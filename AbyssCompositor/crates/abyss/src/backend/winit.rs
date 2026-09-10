// SPDX-License-Identifier: AGPL-3.0-only
//! Nested backend: one output inside a host Wayland/X11 window.

use anyhow::{Context, Result};
use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, ImportDma, ImportEgl},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, Mode as CalloopMode, PostAction},
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::Display,
        winit::{dpi::LogicalSize, window::WindowAttributes},
    },
    utils::Transform,
    wayland::{
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        presentation::Refresh,
        socket::ListeningSocketSource,
    },
};

use crate::{
    config::Config,
    render::{collect_elements, presentation_feedback, send_frames, update_primary_scanout},
    state::{client_state, AbyssState},
};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The live winit backend, owned by [`AbyssState`].
///
/// A newtype rather than the smithay type directly so that no
/// `smithay::backend::winit` path escapes this module (root invariant).
pub struct WinitData(WinitGraphicsBackend<GlesRenderer>);

impl super::Backend for WinitData {
    fn import_dmabuf(&mut self, buf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.0.renderer().import_dmabuf(buf, None).is_ok()
    }
}

pub fn run(config: Config, stats: bool, session: bool) -> Result<()> {
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
    crate::xwayland::start(&mut state);
    let dh = state.display_handle.clone();

    let attrs = WindowAttributes::default()
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_title("abyss")
        .with_visible(true);
    let (mut backend, winit_loop) =
        winit::init_from_attributes::<GlesRenderer>(attrs).map_err(|e| anyhow::anyhow!("winit init: {e}"))?;

    // COMP-02 §2: advertise dmabuf only when the host EGL sits on a real render
    // node. On llvmpipe there is nothing to import into, so no global at all and
    // clients fall back to shm rather than failing every import.
    let render_node = EGLDevice::device_for_display(backend.renderer().egl_context().display())
        .ok()
        .and_then(|d| d.try_get_render_node().ok().flatten());
    match render_node {
        Some(node) => {
            let formats: Vec<_> = backend
                .renderer()
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
                    if let Err(e) = backend.renderer().bind_wl_display(&dh) {
                        tracing::debug!(?e, "no wl_drm/EGL display binding");
                    }
                    state.dmabuf_state = Some(dmabuf_state);
                    state.dmabuf_feedback = Some(feedback);
                    tracing::info!(node = ?node.dev_path(), "dmabuf feedback advertised");
                }
                Err(e) => tracing::warn!(?e, "building dmabuf feedback; no dmabuf global"),
            }
        }
        None => tracing::warn!("host EGL has no render node (llvmpipe?); no dmabuf global"),
    }

    let size = backend.window_size();
    let mode = Mode {
        size,
        refresh: 60_000,
    };
    let output = Output::new(
        "winit".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "abyss".into(),
            model: "winit".into(),
        },
    );
    let global = output.create_global::<AbyssState>(&dh);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);
    crate::outputs::register(
        &mut state,
        "winit".into(),
        "winit".into(),
        output.clone(),
        crate::outputs::OutputKind::Physical,
        Some(global),
    );
    let damage_tracker = OutputDamageTracker::from_output(&output);

    let handle = event_loop.handle();
    crate::input::idle::start(&mut state, &handle);
    crate::ipc::start(&mut state, &handle);
    crate::config::watch::start(&mut state, &handle);
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

    std::env::set_var("WAYLAND_DISPLAY", &state.socket_name);
    tracing::info!(socket = %state.socket_name, "listening");
    if session {
        crate::session::import();
    }

    state.winit = Some(Box::new(WinitData(backend)));
    let out = output.clone();
    let mut damage_tracker = damage_tracker;
    handle
        .insert_source(winit_loop, move |event, _, state| match event {
            WinitEvent::Resized { size, .. } => {
                out.change_current_state(
                    Some(Mode {
                        size,
                        refresh: 60_000,
                    }),
                    None,
                    None,
                    None,
                );
                crate::outputs::relayout(state);
            }
            WinitEvent::Input(ev) => state.process_input_event(ev),
            WinitEvent::CloseRequested => state.quit(),
            WinitEvent::Redraw => {
                let Some(mut data) = state.winit.take() else {
                    return;
                };
                redraw(state, &mut data.0, &out, &mut damage_tracker);
                data.0.window().request_redraw();
                state.winit = Some(data);
            }
            WinitEvent::Focus(_) => {}
        })
        .map_err(|e| anyhow::anyhow!("winit source: {e}"))?;

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

/// Service queued captures without waiting for the host's next frame.
///
/// The nested backend composites only when the host compositor delivers a
/// frame callback, and a host that has the abyss window occluded or on
/// another workspace delivers none. A screen-capture consumer must not stall
/// on that: a queued `copy` renders its own offscreen pass and never touches
/// the on-screen framebuffer, so it can be serviced from an idle instead.
/// The DRM backend has no equivalent problem — there `damage_all` schedules a
/// real render.
pub fn service_captures(state: &mut AbyssState) {
    if state.captures.is_empty() {
        return;
    }
    let Some(mut data) = state.winit.take() else {
        return;
    };
    crate::render::capture::service(state, data.0.renderer());
    state.winit = Some(data);
}

/// One frame on the nested backend.
fn redraw(
    state: &mut AbyssState,
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
    out: &Output,
    damage_tracker: &mut OutputDamageTracker,
) {
    let frame_start = std::time::Instant::now();
    let age = backend.buffer_age().unwrap_or(0);
    let rendered = {
        let (renderer, mut fb) = match backend.bind() {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(?e, "bind");
                return;
            }
        };
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
        match damage_tracker.render_output(renderer, &mut fb, age, &elements, CLEAR) {
            Ok(r) => (r.damage.map(|d| d.to_vec()), r.states),
            Err(e) => {
                tracing::error!(?e, "render");
                return;
            }
        }
    };
    let render_time = frame_start.elapsed();
    let submit_start = std::time::Instant::now();
    let (damage, states) = rendered;
    update_primary_scanout(&state.space, out, &states);
    if let Some(damage) = damage {
        if let Err(e) = backend.submit(Some(&damage)) {
            tracing::error!(?e, "submit");
        }
    }
    // Authorised captures are serviced after submit, so a capture never delays
    // the frame the user is looking at.
    crate::render::capture::service(state, backend.renderer());
    // Best effort: winit gives us no page-flip timestamp, so the submit time is
    // reported without a hardware-completion flag.
    let now = state.clock.now();
    let mut feedback = presentation_feedback(&state.space, out, &states);
    feedback.presented::<_, smithay::utils::Monotonic>(
        now,
        Refresh::fixed(std::time::Duration::from_millis(16)),
        0,
        wp_presentation_feedback::Kind::Vsync,
    );
    state.stats.record(render_time, submit_start.elapsed());
    state.stats.maybe_report();
    let time = state.start_time.elapsed();
    send_frames(&state.space, out, time);
    state.space.refresh();
    state.popups.cleanup();
}
