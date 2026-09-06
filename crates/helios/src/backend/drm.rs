// SPDX-License-Identifier: AGPL-3.0-only
//! Production backend (COMP-01 §3, F-04): KMS via libseat + udev + libinput,
//! GLES rendering onto GBM buffers through EGL.
//!
//! M1 scope: single GPU, single output (the first connected connector), no
//! multi-GPU import path and no runtime hotplug beyond logging. Everything
//! that touches hardware logs and continues rather than panicking.

use std::{path::PathBuf, time::Duration};

use anyhow::{anyhow, Context, Result};
use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
        },
        egl::{context::EGLContext, display::EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            element::{
                solid::{SolidColorBuffer, SolidColorRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{primary_gpu, UdevBackend, UdevEvent},
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic, timer::TimeoutAction, timer::Timer, EventLoop, Interest, LoopHandle,
            Mode as CalloopMode, PostAction,
        },
        drm::control::{connector, crtc, Device as _, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Scale, Transform},
    wayland::{dmabuf::DmabufState, socket::ListeningSocketSource},
};

use crate::{
    config::Config,
    render::{collect_elements, HeliosRenderElement},
    state::{client_state, HeliosState},
};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Amber-on-black placeholder pointer. A themed cursor lands with M3.
const CURSOR_COLOR: [f32; 4] = [1.0, 0.72, 0.15, 1.0];
const CURSOR_SIZE: i32 = 12;

pub type HeliosDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// Everything the DRM backend needs to keep alive between callbacks.
pub struct DrmData {
    pub session: LibSeatSession,
    pub loop_handle: LoopHandle<'static, HeliosState>,
    pub drm: DrmDevice,
    pub renderer: GlesRenderer,
    pub compositor: Option<HeliosDrmCompositor>,
    pub output: Option<Output>,
    pub crtc: Option<crtc::Handle>,
    cursor: SolidColorBuffer,
    /// Set while a re-render timer is armed, so we never stack timers.
    retry_armed: bool,
    /// A frame is queued and we are waiting for its VBlank.
    frame_pending: bool,
    /// Content changed since the last composite; render at the next chance.
    needs_render: bool,
}

impl DrmData {
    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(?err, vt, "VT switch failed");
        }
    }
}

/// Refuse to run on NVIDIA without `nvidia-drm.modeset=1` (F-04 §2).
fn check_nvidia_modeset(node: &DrmNode) -> Result<()> {
    let driver = match smithay::backend::udev::driver(node.dev_id()) {
        Ok(Some(d)) => d,
        Ok(None) => return Ok(()),
        Err(err) => {
            tracing::warn!(?err, "could not determine DRM driver; continuing");
            return Ok(());
        }
    };
    if driver != "nvidia" {
        return Ok(());
    }
    // The sysfs param is root-only readable (0400) on Arch; only fail on a
    // readable, explicit "N". An unreadable param is not evidence.
    let modeset = match std::fs::read_to_string("/sys/module/nvidia_drm/parameters/modeset") {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "cannot read nvidia_drm modeset param; continuing");
            return Ok(());
        }
    };
    if modeset.trim() != "N" {
        Ok(())
    } else {
        Err(anyhow!(
            "NVIDIA GPU without kernel modesetting. Boot with `nvidia-drm.modeset=1` \
             (F-04 §2); helios has no implicit-sync fallback."
        ))
    }
}

pub fn run(config: Config) -> Result<()> {
    let mut event_loop: EventLoop<'static, HeliosState> =
        EventLoop::try_new().context("calloop event loop")?;
    let display: Display<HeliosState> = Display::new().context("wayland display")?;
    let handle = event_loop.handle();

    let (session, session_notifier) =
        LibSeatSession::new().context("libseat session (is seatd running, or logind available?)")?;
    let seat_name = session.seat();
    tracing::info!(seat = %seat_name, "libseat session acquired");

    let socket = ListeningSocketSource::new_auto().context("wayland socket")?;
    let mut state = HeliosState::new(&display, event_loop.get_signal(), &socket, config);

    // --- GPU discovery -----------------------------------------------------
    let udev = UdevBackend::new(&seat_name).context("udev backend")?;
    let gpu_path: PathBuf = primary_gpu(&seat_name)
        .context("querying primary GPU")?
        .ok_or_else(|| anyhow!("no GPU found on seat '{seat_name}'"))?;
    tracing::info!(path = %gpu_path.display(), "primary GPU");

    let mut session_for_open = session.clone();
    let fd = session_for_open
        .open(
            &gpu_path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|err| anyhow!("opening {}: {err}", gpu_path.display()))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let node = DrmNode::from_file(&device_fd).context("resolving DRM node")?;
    check_nvidia_modeset(&node)?;

    let (drm_device, drm_notifier) = DrmDevice::new(device_fd.clone(), true).context("DrmDevice::new")?;
    let gbm = GbmDevice::new(device_fd).context("GbmDevice::new")?;

    // SAFETY: the gbm device outlives the display (both live in DrmData /
    // this function's scope for the lifetime of the process).
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }.context("EGLDisplay::new")?;
    let egl_context = EGLContext::new(&egl_display).context("EGLContext::new")?;
    // SAFETY: the context is current on this (single) thread only.
    let renderer = unsafe { GlesRenderer::new(egl_context) }.context("GlesRenderer::new")?;

    // dmabuf is the expected client buffer path (F-04); shm still works.
    let dmabuf_formats = renderer.egl_context().dmabuf_texture_formats().clone();
    let mut dmabuf_state = DmabufState::new();
    let _dmabuf_global =
        dmabuf_state.create_global::<HeliosState>(&state.display_handle, dmabuf_formats.iter().copied());
    state.dmabuf_state = Some(dmabuf_state);

    state.drm = Some(Box::new(DrmData {
        session: session.clone(),
        loop_handle: handle.clone(),
        drm: drm_device,
        renderer,
        compositor: None,
        output: None,
        crtc: None,
        cursor: SolidColorBuffer::new((CURSOR_SIZE, CURSOR_SIZE), CURSOR_COLOR),
        retry_armed: false,
        frame_pending: false,
        needs_render: false,
    }));

    if let Err(err) = init_output(&mut state, &gbm) {
        tracing::error!(?err, "no usable output");
        return Err(err);
    }

    // --- input -------------------------------------------------------------
    let mut libinput = Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| anyhow!("libinput could not assign seat '{seat_name}'"))?;
    handle
        .insert_source(LibinputInputBackend::new(libinput), |event, _, state| {
            state.process_input_event(event);
            schedule_render(state);
        })
        .map_err(|err| anyhow!("inserting libinput source: {err}"))?;

    // --- session / drm / udev sources --------------------------------------
    handle
        .insert_source(session_notifier, move |event, &mut (), state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused (VT switch away)");
                if let Some(drm) = state.drm.as_mut() {
                    drm.drm.pause();
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session activated");
                if let Some(drm) = state.drm.as_mut() {
                    if let Err(err) = drm.drm.activate(false) {
                        tracing::error!(?err, "reactivating DRM device");
                    }
                    if let Some(compositor) = drm.compositor.as_mut() {
                        if let Err(err) = compositor.reset_state() {
                            tracing::error!(?err, "resetting compositor state");
                        }
                    }
                    drm.retry_armed = false;
                    drm.frame_pending = false;
                    drm.needs_render = true;
                }
                render(state);
            }
        })
        .map_err(|err| anyhow!("inserting session source: {err}"))?;

    handle
        .insert_source(drm_notifier, move |event, _meta, state| match event {
            DrmEvent::VBlank(_crtc) => {
                let pending = if let Some(drm) = state.drm.as_mut() {
                    if let Some(compositor) = drm.compositor.as_mut() {
                        if let Err(err) = compositor.frame_submitted() {
                            tracing::warn!(?err, "frame_submitted");
                        }
                    }
                    drm.frame_pending = false;
                    drm.needs_render
                } else {
                    false
                };
                if pending {
                    render(state);
                }
            }
            DrmEvent::Error(err) => tracing::error!(?err, "DRM event error"),
        })
        .map_err(|err| anyhow!("inserting drm source: {err}"))?;

    handle
        .insert_source(udev, |event, _, _state| match event {
            UdevEvent::Added { device_id, path } => {
                tracing::info!(?device_id, path = %path.display(), "GPU added (hotplug lands in M3)")
            }
            UdevEvent::Changed { device_id } => {
                tracing::info!(?device_id, "GPU changed (hotplug lands in M3)")
            }
            UdevEvent::Removed { device_id } => {
                tracing::info!(?device_id, "GPU removed (hotplug lands in M3)")
            }
        })
        .map_err(|err| anyhow!("inserting udev source: {err}"))?;

    // --- wayland -----------------------------------------------------------
    handle
        .insert_source(socket, |stream, _, state| {
            if let Err(err) = state.display_handle.insert_client(stream, client_state()) {
                tracing::warn!(?err, "rejecting client");
            }
        })
        .map_err(|err| anyhow!("inserting socket source: {err}"))?;

    handle
        .insert_source(
            Generic::new(display, Interest::READ, CalloopMode::Level),
            |_, display, state| {
                // SAFETY: the display is only ever dispatched from this loop.
                unsafe { display.get_mut().dispatch_clients(state) }?;
                schedule_render(state);
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| anyhow!("inserting display source: {err}"))?;

    // SAFETY: single-threaded; nothing else reads the environment concurrently.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &state.socket_name) };
    tracing::info!(socket = %state.socket_name, "helios ready on DRM");

    render(&mut state);
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    Ok(())
}

/// Pick the first connected connector, build the [`Output`] and the
/// [`DrmCompositor`] driving it.
fn init_output(state: &mut HeliosState, gbm: &GbmDevice<DrmDeviceFd>) -> Result<()> {
    let dh = state.display_handle.clone();
    let drm = state
        .drm
        .as_mut()
        .ok_or_else(|| anyhow!("DRM backend not initialised"))?;

    let resources = drm.drm.resource_handles().context("drm resource handles")?;

    let mut chosen = None;
    for handle in resources.connectors() {
        let info = match drm.drm.get_connector(*handle, false) {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!(?err, "reading connector");
                continue;
            }
        };
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }
        chosen = Some(info);
        break;
    }
    let connector = chosen.ok_or_else(|| anyhow!("no connected connector with modes"))?;

    let name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
    let mode = connector
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .unwrap_or_else(|| connector.modes()[0]);

    // A CRTC the connector's encoders can drive.
    let crtc = connector
        .encoders()
        .iter()
        .filter_map(|enc| drm.drm.get_encoder(*enc).ok())
        .find_map(|enc| resources.filter_crtcs(enc.possible_crtcs()).into_iter().next())
        .ok_or_else(|| anyhow!("no CRTC available for connector {name}"))?;

    let (w, h) = mode.size();
    let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Unknown".into(),
            model: "Unknown".into(),
        },
    );
    let wl_mode = OutputMode {
        size: (w as i32, h as i32).into(),
        refresh: refresh_mhz(&mode),
    };
    let _global = output.create_global::<HeliosState>(&dh);
    output.change_current_state(Some(wl_mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(wl_mode);

    let surface = drm
        .drm
        .create_surface(crtc, mode, &[connector.handle()])
        .context("creating DRM surface")?;

    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), None);
    let renderer_formats = drm.renderer.egl_context().dmabuf_render_formats().clone();
    let cursor_size = drm.drm.cursor_size();

    let compositor = HeliosDrmCompositor::new(
        &output,
        surface,
        None,
        allocator,
        exporter,
        [Fourcc::Abgr8888, Fourcc::Argb8888],
        renderer_formats.iter().copied(),
        cursor_size,
        Some(gbm.clone()),
    )
    .context("DrmCompositor::new")?;

    tracing::info!(
        output = %name,
        width = w,
        height = h,
        refresh_mhz = wl_mode.refresh,
        "output configured"
    );

    drm.compositor = Some(compositor);
    drm.output = Some(output.clone());
    drm.crtc = Some(crtc);

    state.space.map_output(&output, (0, 0));
    state.pointer_location = (w as f64 / 2.0, h as f64 / 2.0).into();
    Ok(())
}

/// Refresh rate in mHz, derived from the mode timings (`vrefresh` is only
/// whole Hz and rounds 59.94 to 60).
fn refresh_mhz(mode: &smithay::reexports::drm::control::Mode) -> i32 {
    let (hsync_start, _, htotal) = mode.hsync();
    let (vsync_start, _, vtotal) = mode.vsync();
    let _ = (hsync_start, vsync_start);
    let denom = htotal as u64 * vtotal as u64;
    if denom == 0 {
        return (mode.vrefresh() as i32) * 1000;
    }
    ((mode.clock() as u64 * 1_000_000) / denom) as i32
}

/// Mark the output as dirty and composite, unless a frame is already in
/// flight — in that case the pending VBlank picks the new content up. This
/// keeps `render_frame` calls one-to-one with submitted frames, which is what
/// smithay's damage tracker assumes.
pub fn schedule_render(state: &mut HeliosState) {
    let Some(drm) = state.drm.as_mut() else { return };
    drm.needs_render = true;
    if drm.frame_pending || drm.retry_armed {
        return;
    }
    render(state);
}

/// Composite and page-flip. Safe to call at any time; a no-op when the
/// session is inactive or no output is configured.
pub fn render(state: &mut HeliosState) {
    let Some(drm) = state.drm.as_mut() else { return };
    if !drm.session.is_active() {
        return;
    }
    let (Some(compositor), Some(output)) = (drm.compositor.as_mut(), drm.output.as_ref()) else {
        return;
    };
    let output = output.clone();
    let scale = Scale::from(output.current_scale().fractional_scale());

    let mut elements: Vec<HeliosRenderElement> =
        vec![HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
            &drm.cursor,
            state.pointer_location.to_physical_precise_round(scale),
            scale,
            1.0,
            Kind::Cursor,
        ))];
    elements.extend(collect_elements(
        &mut drm.renderer,
        &state.space,
        &mut state.borders,
        &output,
        &state.config,
        state.focus.as_ref(),
    ));

    // FrameFlags::empty() forces full composition: F-04 §2 assumes no plane
    // availability. Plane scanout is a probed optimisation for a later
    // milestone.
    let mut failed = false;
    let mut empty = false;
    let queued = match compositor.render_frame(&mut drm.renderer, &elements, CLEAR, FrameFlags::empty()) {
        Ok(result) if result.is_empty => {
            empty = true;
            false
        }
        Ok(_) => match compositor.queue_frame(()) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(?err, "queueing frame");
                failed = true;
                false
            }
        },
        Err(err) => {
            tracing::warn!(?err, "rendering frame");
            failed = true;
            false
        }
    };
    drop(elements);

    if empty {
        // An empty frame is discarded rather than submitted, but the damage
        // tracker still recorded it. That desynchronises the damage history
        // from the swapchain slot ages, and every later frame then restores
        // the wrong regions (stale content, cursor trails). Clearing the ages
        // forces the next frame to be a full redraw, which resyncs both.
        compositor.reset_buffer_ages();
    }

    drm.needs_render = false;
    drm.frame_pending = queued;

    if !queued && !drm.retry_armed && failed {
        // The frame had damage but could not be queued: retry shortly. Never
        // re-arm for an empty frame — an idle render_frame spin pushes empty
        // entries into the damage tracker's history while the swapchain slot
        // ages stand still, which desynchronises the two and leaves stale
        // content on screen.
        drm.retry_armed = true;
        let handle = drm.loop_handle.clone();
        if let Err(err) =
            handle.insert_source(Timer::from_duration(Duration::from_millis(16)), |_, _, state| {
                if let Some(drm) = state.drm.as_mut() {
                    drm.retry_armed = false;
                }
                render(state);
                TimeoutAction::Drop
            })
        {
            tracing::warn!(?err, "arming re-render timer");
            if let Some(drm) = state.drm.as_mut() {
                drm.retry_armed = false;
            }
        }
    }

    let time = state.start_time.elapsed();
    crate::render::send_frames(&state.space, &output, time);
    state.space.refresh();
    state.popups.cleanup();
    let _ = state.display_handle.flush_clients();
}
