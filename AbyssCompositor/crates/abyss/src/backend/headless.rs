// SPDX-License-Identifier: AGPL-3.0-only
//! Headless backend (COMP-01 §10): one virtual output, no display server, no
//! KMS device, and no requirement that a GPU exist at all.
//!
//! This is the CI path. Everything above the [`Backend`](super::Backend) trait
//! is backend-agnostic, so the protocol, policy and `wlcs` suites run here with
//! no host compositor and no connector — which is what makes most of the
//! codebase testable on a cloud runner (F-07 §3, COMP-15 §1).
//!
//! Rendering is real: an EGL device (a hardware render node when the machine
//! has one, Mesa's software device otherwise) backs a [`GlesRenderer`] that
//! composites into an offscreen texture. A test that asserts on pixels —
//! redaction in particular — needs an actual pass to have happened, so a
//! backend that skipped the render would fail the suites it exists to run.
//! Nothing is scanned out; the composited texture is simply dropped unless a
//! capture reads it back.
//!
//! There is no input device. Events reach a headless session through the
//! injection path (COMP-04) alone.

use anyhow::{Context, Result};
use smithay::{
    backend::{
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, Bind, ImportDma, ImportEgl, Offscreen},
    },
    desktop::layer_map_for_output,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, Mode as CalloopMode, PostAction,
        },
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{Display, Resource},
    },
    utils::{Point, Rectangle, Size, Transform},
    wayland::{
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        presentation::Refresh,
        seat::WaylandFocus,
        socket::ListeningSocketSource,
    },
};

use crate::{
    config::Config,
    render::{collect_elements, presentation_feedback, send_frames, update_primary_scanout},
    state::{client_state, AbyssState},
};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Default virtual output size when `--size` is not given.
pub const DEFAULT_SIZE: (i32, i32) = (1280, 800);

/// Output size for the wlcs harness.
///
/// wlcs parks a 400x500 test window at (500, 500) and expects popups anchored
/// to its bottom edge to be on-screen and clickable, which needs a little over
/// 1000 rows. The interactive default is shorter, so the conformance run gets
/// its own mode rather than every popup test failing on geometry the tests
/// never asked about.
pub const WLCS_SIZE: (i32, i32) = (1280, 1024);
/// Frame clock. Fixed rather than derived from anything real: a headless run
/// has no vblank, and a benchmark or a conformance test wants the same cadence
/// on every runner (COMP-14 §5).
const REFRESH_MHZ: i32 = 60_000;
const FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);

/// The live headless backend, owned by [`AbyssState`].
///
/// Holds the renderer and the offscreen target it draws into. A newtype so
/// that no `smithay::backend::renderer` path escapes this module.
pub struct HeadlessData {
    renderer: GlesRenderer,
    target: smithay::backend::renderer::gles::GlesTexture,
    size: Size<i32, smithay::utils::Buffer>,
}

impl super::Backend for HeadlessData {
    fn import_dmabuf(&mut self, buf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.renderer.import_dmabuf(buf, None).is_ok()
    }
}

/// Pick an EGL device and build a renderer on it.
///
/// A render node is preferred so that dmabuf-importing clients work on a
/// runner that does have a GPU; Mesa's software device is accepted when there
/// is none, because a machine with no GPU is exactly the case this backend
/// exists for. Devices are tried in turn rather than trusting the first: an
/// enumerated device can still fail to give a display or a context.
fn open_renderer() -> Result<(GlesRenderer, Option<smithay::backend::drm::DrmNode>)> {
    let mut devices: Vec<EGLDevice> = EGLDevice::enumerate().context("enumerate EGL devices")?.collect();
    if devices.is_empty() {
        anyhow::bail!("no EGL device (install a Mesa software renderer for headless)");
    }
    // Hardware first, software last.
    devices.sort_by_key(|d| d.try_get_render_node().ok().flatten().is_none());
    let mut last_err = None;
    for device in devices {
        let node = device.try_get_render_node().ok().flatten();
        // SAFETY: the display is kept alive by the context, which is kept
        // alive by the renderer, which lives as long as the backend.
        let display = match unsafe { EGLDisplay::new(device) } {
            Ok(d) => d,
            Err(e) => {
                last_err = Some(anyhow::anyhow!("egl display: {e}"));
                continue;
            }
        };
        let context = match EGLContext::new(&display) {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(anyhow::anyhow!("egl context: {e}"));
                continue;
            }
        };
        // SAFETY: the context is not current on any other thread; the
        // compositor is single-threaded.
        match unsafe { GlesRenderer::new(context) } {
            Ok(r) => {
                tracing::info!(node = ?node.as_ref().map(|n| n.dev_path()), "headless renderer up");
                return Ok((r, node));
            }
            Err(e) => last_err = Some(anyhow::anyhow!("gles renderer: {e}")),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no usable EGL device")))
}

/// Events the `wlcs` harness sends from its own thread into the loop.
///
/// The suite drives the compositor from the thread that runs the C++ test, so
/// something has to cross a thread boundary. Only the [`Sender`] does: every
/// event is queued and applied by the loop thread, so the single-threaded-core
/// invariant holds — there is no shared state and no lock.
///
/// Coordinates are plain tuples rather than smithay `Point`s so the harness
/// crate links `abyss` alone and never names a smithay type.
#[derive(Debug)]
pub enum WlcsEvent {
    /// Shut the loop down; the harness joins the thread afterwards.
    Exit,
    /// A client socket wlcs already created. `client_id` is the fd number on
    /// the wlcs side, and is what later events use to name the client.
    NewClient {
        stream: std::os::unix::net::UnixStream,
        client_id: i32,
    },
    /// Place a toplevel or layer surface at an absolute position.
    /// `surface_id` is the
    /// client-side protocol id of its `wl_surface`.
    PositionWindow {
        client_id: i32,
        surface_id: u32,
        location: (i32, i32),
    },
    PointerMoveAbsolute {
        location: (f64, f64),
    },
    PointerMoveRelative {
        delta: (f64, f64),
    },
    PointerButtonDown {
        button_id: i32,
    },
    PointerButtonUp {
        button_id: i32,
    },
    /// wlcs drives one touch device with one point at a time; `slot` is kept
    /// explicit so a multi-touch caller (IPC, agent seats) needs no new event.
    TouchDown {
        slot: u32,
        location: (f64, f64),
    },
    TouchMove {
        slot: u32,
        location: (f64, f64),
    },
    TouchUp {
        slot: u32,
    },
}

/// Build the channel the harness sends on. Exported so the harness crate does
/// not have to depend on the same calloop version by name.
pub fn wlcs_channel() -> (
    smithay::reexports::calloop::channel::Sender<WlcsEvent>,
    smithay::reexports::calloop::channel::Channel<WlcsEvent>,
) {
    smithay::reexports::calloop::channel::channel()
}

pub type WlcsSender = smithay::reexports::calloop::channel::Sender<WlcsEvent>;

/// Run a headless session driven by `wlcs` instead of by a Wayland socket.
///
/// Same compositor, same render loop; the only difference is where clients and
/// input come from. Blocks until the harness sends [`WlcsEvent::Exit`].
pub fn run_wlcs(channel: smithay::reexports::calloop::channel::Channel<WlcsEvent>) -> Result<()> {
    boot(Config::default(), false, false, WLCS_SIZE, Some(channel))
}

pub fn run(config: Config, stats: bool, session: bool, size: (i32, i32)) -> Result<()> {
    boot(config, stats, session, size, None)
}

fn boot(
    config: Config,
    stats: bool,
    session: bool,
    size: (i32, i32),
    wlcs: Option<smithay::reexports::calloop::channel::Channel<WlcsEvent>>,
) -> Result<()> {
    let mut event_loop: EventLoop<'static, AbyssState> = EventLoop::try_new().context("calloop")?;
    let display: Display<AbyssState> = Display::new().context("wayland display")?;
    let socket = ListeningSocketSource::new_auto().context("bind wayland socket")?;
    let mut state = AbyssState::new(
        &display,
        event_loop.get_signal(),
        event_loop.handle(),
        &socket,
        config,
        stats,
    );
    // Not under wlcs: the suite advertises no X11 global, so an Xwayland per
    // compositor tests nothing. It also leaks — wlcs builds one compositor per
    // test in a single process, and each Xwayland child keeps its listening
    // sockets in that process's fd table, exhausting `ulimit -n` partway
    // through the run and taking the whole suite down with it.
    if wlcs.is_none() {
        crate::xwayland::start(&mut state);
    }
    let dh = state.display_handle.clone();

    let (mut renderer, render_node) = open_renderer()?;

    // COMP-02 §2: a dmabuf global only where there is a render node to import
    // into. On the software device there is none, so clients use shm — which
    // is what a GPU-free runner can do anyway.
    match render_node {
        Some(node) => {
            let formats: Vec<_> = renderer
                .egl_context()
                .dmabuf_texture_formats()
                .iter()
                .copied()
                .collect();
            match DmabufFeedbackBuilder::new(node.dev_id(), formats.iter().copied()).build() {
                Ok(feedback) => {
                    let mut dmabuf_state = DmabufState::new();
                    let _global =
                        dmabuf_state.create_global_with_default_feedback::<AbyssState>(&dh, &feedback);
                    if let Err(e) = renderer.bind_wl_display(&dh) {
                        tracing::debug!(?e, "no wl_drm/EGL display binding");
                    }
                    state.dmabuf_state = Some(dmabuf_state);
                    state.dmabuf_feedback = Some(feedback);
                    tracing::info!(node = ?node.dev_path(), "dmabuf feedback advertised");
                }
                Err(e) => tracing::warn!(?e, "building dmabuf feedback; no dmabuf global"),
            }
        }
        None => tracing::info!("software EGL device; no dmabuf global"),
    }

    let mode = Mode {
        size: size.into(),
        refresh: REFRESH_MHZ,
    };
    let output = Output::new(
        "headless".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "abyss".into(),
            model: "headless".into(),
        },
    );
    let global = output.create_global::<AbyssState>(&dh);
    // No flip: an offscreen texture has no scanout convention to match, so the
    // capture path and the on-screen pass agree on orientation for free.
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(mode);
    crate::outputs::register(
        &mut state,
        "headless".into(),
        "headless".into(),
        output.clone(),
        crate::outputs::OutputKind::Physical,
        Some(global),
    );
    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    let buffer_size = mode.size.to_logical(1).to_buffer(1, Transform::Normal);
    let target = Offscreen::create_buffer(&mut renderer, crate::render::capture::FORMAT, buffer_size)
        .map_err(|e| anyhow::anyhow!("offscreen target: {e}"))?;

    let handle = event_loop.handle();
    crate::input::idle::start(&mut state, &handle);
    crate::ipc::start(&mut state, &handle);
    crate::config::watch::start(&mut state, &handle);
    // Registered ahead of the wayland sources on purpose: calloop dispatches in
    // registration order, so a queued PositionWindow wins over client requests
    // that arrived in the same wakeup. wlcs assumes move_surface_to has taken
    // effect before the requests that follow it, and does not roundtrip.
    let under_wlcs = wlcs.is_some();
    if let Some(channel) = wlcs {
        let mut clients: std::collections::HashMap<i32, smithay::reexports::wayland_server::Client> =
            std::collections::HashMap::new();
        handle
            .insert_source(channel, move |event, _, state| match event {
                smithay::reexports::calloop::channel::Event::Msg(e) => wlcs_event(state, &mut clients, e),
                smithay::reexports::calloop::channel::Event::Closed => state.loop_signal.stop(),
            })
            .map_err(|e| anyhow::anyhow!("wlcs channel source: {e}"))?;
    }
    handle
        .insert_source(socket, |stream, _, state| {
            if let Err(e) = state.display_handle.insert_client(stream, client_state()) {
                tracing::warn!(?e, "failed to insert client");
            }
        })
        .map_err(|e| anyhow::anyhow!("socket source: {e}"))?;
    handle
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                // SAFETY: the display is only touched from this callback on the loop thread.
                unsafe { display.get_mut().dispatch_clients(state) }?;
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| anyhow::anyhow!("display source: {e}"))?;

    // Under wlcs the socket exists but nothing connects to it, and the
    // harness runs several compositors at once — exporting a global
    // WAYLAND_DISPLAY would have them fight over one process-wide variable.
    if !under_wlcs {
        std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
    }
    tracing::info!(socket = %state.socket_name, w = size.0, h = size.1, "listening (headless)");

    if session {
        crate::session::import();
    }

    state.headless = Some(Box::new(HeadlessData {
        renderer,
        target,
        size: buffer_size,
    }));

    let out = output.clone();
    handle
        .insert_source(Timer::immediate(), move |_, _, state| {
            let Some(mut data) = state.headless.take() else {
                return TimeoutAction::Drop;
            };
            redraw(state, &mut data, &out, &mut damage_tracker);
            state.headless = Some(data);
            TimeoutAction::ToDuration(FRAME)
        })
        .map_err(|e| anyhow::anyhow!("frame timer: {e}"))?;

    event_loop
        .run(None, &mut state, |state| {
            let _ = state.display_handle.flush_clients();
        })
        .context("event loop")?;
    crate::ipc::cleanup(&state);
    if session {
        crate::session::teardown();
    }
    tracing::info!("abyss exited cleanly");
    Ok(())
}

/// One frame into the offscreen target.
///
/// Deliberately the same shape as the winit redraw: same element collection,
/// same damage tracker, same frame callbacks and presentation feedback. What a
/// test observes here is what a nested session would have shown, minus the
/// submit — there is nowhere to submit to.
fn redraw(
    state: &mut AbyssState,
    data: &mut HeadlessData,
    out: &Output,
    damage_tracker: &mut OutputDamageTracker,
) {
    // An output resized by the IPC layer needs a target of the new size before
    // anything is drawn into it; a stale one would crop or stretch the frame.
    if let Some(mode) = out.current_mode() {
        let want = mode.size.to_logical(1).to_buffer(1, Transform::Normal);
        if want != data.size {
            match Offscreen::create_buffer(&mut data.renderer, crate::render::capture::FORMAT, want) {
                Ok(t) => {
                    data.target = t;
                    data.size = want;
                }
                Err(e) => {
                    tracing::error!(?e, "resize offscreen target");
                    return;
                }
            }
        }
    }

    let frame_start = std::time::Instant::now();
    let rendered = {
        let renderer = &mut data.renderer;
        // A fullscreen toplevel drops the `Top` layer below the window stack.
        let fullscreen = crate::shell::output_has_fullscreen(state, out);
        let mut elements = if state.lock.locked {
            crate::protocols::standard::session_lock::lock_elements(renderer, &mut state.lock, out)
        } else {
            collect_elements(
                renderer,
                &state.space,
                &mut state.borders,
                out,
                &state.config,
                state.focus.as_ref(),
                state.input_method_popup.as_ref(),
                fullscreen,
            )
        };
        // Trusted UI, drawn on top of everything and never into a capture.
        elements.splice(
            0..0,
            crate::render::capture::indicator(out, state.capture_active()),
        );
        let mut fb = match Bind::bind(renderer, &mut data.target) {
            Ok(fb) => fb,
            Err(e) => {
                tracing::error!(?e, "bind");
                return;
            }
        };
        // Age 0: the target is never presented, so its previous contents carry
        // no information and a full repaint of the damaged region is correct.
        match damage_tracker.render_output(renderer, &mut fb, 0, &elements, CLEAR) {
            Ok(r) => r.states,
            Err(e) => {
                tracing::error!(?e, "render");
                return;
            }
        }
    };
    let render_time = frame_start.elapsed();
    update_primary_scanout(&state.space, out, &rendered);
    crate::render::capture::service(state, &mut data.renderer);

    // There is no page flip, so the frame is "presented" the moment it is
    // composited, and never with a hardware-completion flag.
    let now = state.clock.now();
    let mut feedback = presentation_feedback(&state.space, out, &rendered);
    feedback.presented::<_, smithay::utils::Monotonic>(
        now,
        Refresh::fixed(FRAME),
        0,
        wp_presentation_feedback::Kind::Vsync,
    );
    state.stats.record(render_time, std::time::Duration::ZERO);
    state.stats.maybe_report();
    let time = state.start_time.elapsed();
    send_frames(&state.space, out, time);
    state.space.refresh();
    state.popups.cleanup();
}

/// Apply one harness event on the loop thread.
///
/// Input goes through [`AbyssState`]'s injection path rather than the seat
/// handle, so a wlcs run exercises the same focus and clamping code a real
/// device would (COMP-04 §6).
fn wlcs_event(
    state: &mut AbyssState,
    clients: &mut std::collections::HashMap<i32, smithay::reexports::wayland_server::Client>,
    event: WlcsEvent,
) {
    let time = state.start_time.elapsed().as_millis() as u32;
    match event {
        WlcsEvent::Exit => state.loop_signal.stop(),
        WlcsEvent::NewClient { stream, client_id } => {
            match state.display_handle.insert_client(stream, client_state()) {
                Ok(client) => {
                    clients.insert(client_id, client);
                }
                Err(e) => tracing::error!(?e, "wlcs: insert client"),
            }
        }
        WlcsEvent::PositionWindow {
            client_id,
            surface_id,
            location,
        } => {
            // wlcs names a window by (its client, the protocol id of its
            // surface); nothing else identifies it across the boundary.
            let client = clients.get(&client_id);
            let window = state
                .space
                .elements()
                .find(|w| {
                    w.wl_surface().is_some_and(|s| {
                        state.display_handle.get_client(s.id()).ok().as_ref() == client
                            && s.id().protocol_id() == surface_id
                    })
                })
                .cloned();
            if let Some(w) = window {
                // `arrange` re-maps every element on every layout pass, so a
                // bare `map_element` would be undone by the next one; pin the
                // window as floating at the requested geometry instead.
                let size = w.geometry().size;
                crate::shell::place_at(state, &w, Rectangle::new(location.into(), size));
                return;
            }
            // A layer surface is not a space element: wlcs positions the
            // parent of a layer-shell popup this way, so the request has to
            // reach the layer map's placement override too.
            let outputs: Vec<_> = state.outputs.iter().map(|e| e.output.clone()).collect();
            for output in outputs {
                let output_loc = state
                    .space
                    .output_geometry(&output)
                    .map(|g| g.loc)
                    .unwrap_or_default();
                let map = layer_map_for_output(&output);
                let found = map.layers().find(|l| {
                    let s = l.wl_surface();
                    state.display_handle.get_client(s.id()).ok().as_ref() == client
                        && s.id().protocol_id() == surface_id
                });
                if let Some(layer) = found {
                    crate::shell::place_layer(&map, layer, Point::from(location) - output_loc);
                    return;
                }
            }
            tracing::warn!(client_id, surface_id, "wlcs: no such window to position");
        }
        WlcsEvent::PointerMoveAbsolute { location } => state.inject_pointer_absolute(location.into(), time),
        WlcsEvent::PointerMoveRelative { delta } => state.inject_pointer_relative(delta.into(), time),
        WlcsEvent::PointerButtonDown { button_id } => {
            state.inject_pointer_button(button_id as u32, true, time)
        }
        WlcsEvent::PointerButtonUp { button_id } => {
            state.inject_pointer_button(button_id as u32, false, time)
        }
        WlcsEvent::TouchDown { slot, location } => state.inject_touch_down(slot, location.into(), time),
        WlcsEvent::TouchMove { slot, location } => state.inject_touch_motion(slot, location.into(), time),
        WlcsEvent::TouchUp { slot } => state.inject_touch_up(slot, time),
    }
}
