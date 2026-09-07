// SPDX-License-Identifier: AGPL-3.0-only
//! Production backend (COMP-01 §3, F-04): KMS via libseat + udev + libinput,
//! GLES rendering onto GBM buffers through EGL.
//!
//! M3 scope: single GPU, every connected connector driven as its own output,
//! runtime connector hotplug through udev, and a headless fallback output so
//! the compositor survives with nothing plugged in (COMP-03 §5). Everything
//! that touches hardware logs and continues rather than panicking.

use std::{collections::HashSet, path::PathBuf, time::Duration};

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
    outputs::OutputKind,
    render::{collect_elements, HeliosRenderElement},
    state::{client_state, HeliosState},
};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Amber-on-black placeholder pointer. A themed cursor lands with M4.
const CURSOR_COLOR: [f32; 4] = [1.0, 0.72, 0.15, 1.0];
const CURSOR_SIZE: i32 = 12;
/// Size of the headless fallback output used when no connector is present.
const FALLBACK_SIZE: (i32, i32) = (1920, 1080);

pub type HeliosDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// One scanout pipeline: connector → CRTC → [`DrmCompositor`].
pub struct DrmOutput {
    /// Handle into [`crate::outputs::Outputs`].
    pub id: u64,
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    pub output: Output,
    pub compositor: HeliosDrmCompositor,
    /// Set while a re-render timer is armed, so we never stack timers.
    retry_armed: bool,
    /// A frame is queued and we are waiting for its VBlank.
    frame_pending: bool,
    /// Content changed since the last composite; render at the next chance.
    needs_render: bool,
}

/// Everything the DRM backend needs to keep alive between callbacks.
pub struct DrmData {
    pub session: LibSeatSession,
    pub loop_handle: LoopHandle<'static, HeliosState>,
    pub drm: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub renderer: GlesRenderer,
    pub outputs: Vec<DrmOutput>,
    /// The headless stand-in, live only while no connector is.
    fallback: Option<u64>,
    cursor: SolidColorBuffer,
}

impl DrmData {
    pub fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(?err, vt, "VT switch failed");
        }
    }

    fn index_of_crtc(&self, crtc: crtc::Handle) -> Option<usize> {
        self.outputs.iter().position(|o| o.crtc == crtc)
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

/// Refresh rate in mHz, derived from the mode timings (`vrefresh` is only
/// whole Hz and rounds 59.94 to 60).
fn refresh_mhz(mode: &smithay::reexports::drm::control::Mode) -> i32 {
    let denom = mode.hsync().2 as u64 * mode.vsync().2 as u64;
    if denom == 0 {
        return (mode.vrefresh() as i32) * 1000;
    }
    ((mode.clock() as u64 * 1_000_000) / denom) as i32
}

fn connector_name(info: &connector::Info) -> String {
    format!("{}-{}", info.interface().as_str(), info.interface_id())
}

/// The connector's raw EDID blob, when the kernel exposes one. smithay 0.7
/// ships no EDID helper (it lives in `smithay-drm-extras`, not a dependency),
/// so the blob is parsed in tree — see ADR 0023.
fn edid_blob(drm: &DrmDevice, conn: connector::Handle) -> Option<Vec<u8>> {
    let props = drm.get_properties(conn).ok()?;
    for (handle, value) in props.iter() {
        let Ok(info) = drm.get_property(*handle) else {
            continue;
        };
        if info.name().to_str().ok()? != "EDID" {
            continue;
        }
        if *value == 0 {
            return None;
        }
        return drm.get_property_blob(*value).ok();
    }
    None
}

/// A CRTC the connector can drive that no other output is already using.
fn pick_crtc(
    drm: &DrmDevice,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    info: &connector::Info,
    used: &HashSet<crtc::Handle>,
) -> Option<crtc::Handle> {
    // The CRTC the connector is already lit on, if any, is always the cheapest.
    if let Some(enc) = info.current_encoder().and_then(|e| drm.get_encoder(e).ok()) {
        if let Some(crtc) = enc.crtc() {
            if !used.contains(&crtc) {
                return Some(crtc);
            }
        }
    }
    info.encoders()
        .iter()
        .filter_map(|enc| drm.get_encoder(*enc).ok())
        .flat_map(|enc| resources.filter_crtcs(enc.possible_crtcs()))
        .find(|crtc| !used.contains(crtc))
}

/// Diff the kernel's connector list against the outputs we are driving and
/// bring the two back into agreement (COMP-03 §3). Called at startup and on
/// every udev `Changed` event for our GPU.
pub fn scan_connectors(state: &mut HeliosState) {
    let Some(drm) = state.drm.as_ref() else { return };
    let Ok(resources) = drm.drm.resource_handles() else {
        tracing::warn!("reading DRM resources");
        return;
    };

    let mut connected: Vec<(connector::Handle, connector::Info)> = Vec::new();
    for handle in resources.connectors() {
        match drm.drm.get_connector(*handle, true) {
            Ok(info) if info.state() == connector::State::Connected && !info.modes().is_empty() => {
                connected.push((*handle, info))
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "reading connector"),
        }
    }
    let present: HashSet<connector::Handle> = connected.iter().map(|(h, _)| *h).collect();

    // --- departures ---------------------------------------------------------
    let gone: Vec<(usize, u64)> = drm
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| !present.contains(&o.connector))
        .map(|(i, o)| (i, o.id))
        .collect();
    for (i, id) in gone.into_iter().rev() {
        if let Some(drm) = state.drm.as_mut() {
            // Dropping the DrmOutput releases its CRTC for reuse.
            drm.outputs.remove(i);
        }
        crate::outputs::unregister(state, id);
    }

    // --- arrivals -----------------------------------------------------------
    for (handle, info) in connected {
        let known = state
            .drm
            .as_ref()
            .is_some_and(|d| d.outputs.iter().any(|o| o.connector == handle));
        if known {
            continue;
        }
        if let Err(err) = add_connector(state, &resources, handle, &info) {
            tracing::warn!(connector = %connector_name(&info), ?err, "could not bring up connector");
        }
    }

    sync_fallback(state);
}

/// Bring up one connector: build its [`Output`], register it, and create the
/// [`DrmCompositor`] that scans it out.
fn add_connector(
    state: &mut HeliosState,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    handle: connector::Handle,
    info: &connector::Info,
) -> Result<()> {
    let dh = state.display_handle.clone();
    let name = connector_name(info);
    let drm = state.drm.as_ref().ok_or_else(|| anyhow!("no DRM backend"))?;
    let identity = crate::outputs::identity(edid_blob(&drm.drm, handle).as_deref(), &name);

    let used: HashSet<crtc::Handle> = drm.outputs.iter().map(|o| o.crtc).collect();
    let crtc = pick_crtc(&drm.drm, resources, info, &used)
        .ok_or_else(|| anyhow!("no free CRTC for connector {name}"))?;

    // Config, then the remembered layout, then the connector's preferred mode.
    let wanted = state
        .config
        .output_rule(&name, &identity)
        .mode
        .or_else(|| state.outputs.saved_for(&identity).and_then(|s| s.mode));
    let preferred = info
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .unwrap_or_else(|| info.modes()[0]);
    let mode = wanted
        .and_then(|(w, h, r)| {
            let exact = info
                .modes()
                .iter()
                .find(|m| m.size() == (w as u16, h as u16) && refresh_mhz(m) == r);
            exact
                .or_else(|| info.modes().iter().find(|m| m.size() == (w as u16, h as u16)))
                .copied()
        })
        .unwrap_or(preferred);

    let (phys_w, phys_h) = info.size().unwrap_or((0, 0));
    let edid = edid_blob(&drm.drm, handle).and_then(|b| crate::outputs::edid::parse(&b));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (phys_w as i32, phys_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: edid.as_ref().map_or("Unknown".into(), |e| e.make.clone()),
            model: edid.as_ref().map_or("Unknown".into(), |e| e.model.clone()),
        },
    );
    // Advertise every mode the connector offers so clients (and the persisted
    // layout) can name one other than the preferred.
    for m in info.modes() {
        output.add_mode(OutputMode {
            size: (m.size().0 as i32, m.size().1 as i32).into(),
            refresh: refresh_mhz(m),
        });
    }
    let wl_mode = OutputMode {
        size: (mode.size().0 as i32, mode.size().1 as i32).into(),
        refresh: refresh_mhz(&mode),
    };
    let global = output.create_global::<HeliosState>(&dh);
    output.change_current_state(Some(wl_mode), Some(Transform::Normal), None, Some((0, 0).into()));
    output.set_preferred(wl_mode);

    let drm = state.drm.as_mut().ok_or_else(|| anyhow!("no DRM backend"))?;
    let surface = drm
        .drm
        .create_surface(crtc, mode, &[handle])
        .context("creating DRM surface")?;
    let allocator = GbmAllocator::new(
        drm.gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(drm.gbm.clone(), None);
    let renderer_formats = drm.renderer.egl_context().dmabuf_render_formats().clone();
    let cursor_size = drm.drm.cursor_size();
    let gbm = drm.gbm.clone();
    let compositor = HeliosDrmCompositor::new(
        &output,
        surface,
        None,
        allocator,
        exporter,
        [Fourcc::Abgr8888, Fourcc::Argb8888],
        renderer_formats.iter().copied(),
        cursor_size,
        Some(gbm),
    )
    .context("DrmCompositor::new")?;

    let id = crate::outputs::register(
        state,
        identity,
        name.clone(),
        output.clone(),
        OutputKind::Physical,
        Some(global),
    );

    if let Some(drm) = state.drm.as_mut() {
        drm.outputs.push(DrmOutput {
            id,
            crtc,
            connector: handle,
            output,
            compositor,
            retry_armed: false,
            frame_pending: false,
            needs_render: true,
        });
    }
    tracing::info!(
        output = %name,
        width = wl_mode.size.w,
        height = wl_mode.size.h,
        refresh_mhz = wl_mode.refresh,
        "output configured"
    );
    Ok(())
}

/// Never leave the human with no output (COMP-03 §5): stand up a headless one
/// when the last connector goes away, and retire it when a real one returns.
fn sync_fallback(state: &mut HeliosState) {
    let Some(drm) = state.drm.as_ref() else { return };
    let physical = !drm.outputs.is_empty();
    let fallback = drm.fallback;
    match (physical, fallback) {
        (false, None) => {
            let (output, _) = crate::outputs::virtual_output("HEADLESS-1", FALLBACK_SIZE);
            let global = output.create_global::<HeliosState>(&state.display_handle.clone());
            let id = crate::outputs::register(
                state,
                "headless".into(),
                "HEADLESS-1".into(),
                output,
                OutputKind::Virtual,
                Some(global),
            );
            if let Some(drm) = state.drm.as_mut() {
                drm.fallback = Some(id);
            }
            tracing::warn!("no connected outputs; running on a headless fallback");
        }
        (true, Some(id)) => {
            if let Some(drm) = state.drm.as_mut() {
                drm.fallback = None;
            }
            crate::outputs::unregister(state, id);
            tracing::info!("headless fallback retired");
        }
        _ => {}
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
    let our_device = node.dev_id();

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
        gbm,
        renderer,
        outputs: Vec::new(),
        fallback: None,
        cursor: SolidColorBuffer::new((CURSOR_SIZE, CURSOR_SIZE), CURSOR_COLOR),
    }));

    scan_connectors(&mut state);
    if let Some(first) = state.outputs.iter().next() {
        if let Some(geo) = state.space.output_geometry(&first.output) {
            state.pointer_location = (
                geo.loc.x as f64 + geo.size.w as f64 / 2.0,
                geo.loc.y as f64 + geo.size.h as f64 / 2.0,
            )
                .into();
        }
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
                    for o in drm.outputs.iter_mut() {
                        if let Err(err) = o.compositor.reset_state() {
                            tracing::error!(?err, "resetting compositor state");
                        }
                        o.retry_armed = false;
                        o.frame_pending = false;
                        o.needs_render = true;
                    }
                }
                // A connector may have changed while we were away.
                scan_connectors(state);
                render(state);
            }
        })
        .map_err(|err| anyhow!("inserting session source: {err}"))?;

    handle
        .insert_source(drm_notifier, move |event, _meta, state| match event {
            DrmEvent::VBlank(crtc) => {
                let pending = if let Some(drm) = state.drm.as_mut() {
                    match drm.index_of_crtc(crtc) {
                        Some(i) => {
                            let o = &mut drm.outputs[i];
                            if let Err(err) = o.compositor.frame_submitted() {
                                tracing::warn!(?err, "frame_submitted");
                            }
                            o.frame_pending = false;
                            o.needs_render.then_some(i)
                        }
                        None => None,
                    }
                } else {
                    None
                };
                if let Some(i) = pending {
                    render_output(state, i);
                }
            }
            DrmEvent::Error(err) => tracing::error!(?err, "DRM event error"),
        })
        .map_err(|err| anyhow!("inserting drm source: {err}"))?;

    handle
        .insert_source(udev, move |event, _, state| {
            let changed = match event {
                UdevEvent::Added { device_id, path } => {
                    tracing::info!(?device_id, path = %path.display(), "GPU added");
                    device_id == our_device
                }
                UdevEvent::Changed { device_id } => device_id == our_device,
                UdevEvent::Removed { device_id } => {
                    tracing::info!(?device_id, "GPU removed");
                    device_id == our_device
                }
            };
            if changed {
                scan_connectors(state);
                schedule_render(state);
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

/// Mark every output dirty and composite the ones that are idle. An output
/// with a frame in flight picks the new content up at its VBlank, which keeps
/// `render_frame` calls one-to-one with submitted frames — what smithay's
/// damage tracker assumes.
pub fn schedule_render(state: &mut HeliosState) {
    let n = state.drm.as_ref().map_or(0, |d| d.outputs.len());
    for i in 0..n {
        let idle = match state.drm.as_mut() {
            Some(drm) => {
                let o = &mut drm.outputs[i];
                o.needs_render = true;
                !o.frame_pending && !o.retry_armed
            }
            None => false,
        };
        if idle {
            render_output(state, i);
        }
    }
}

/// Composite every output. Safe to call at any time.
pub fn render(state: &mut HeliosState) {
    let n = state.drm.as_ref().map_or(0, |d| d.outputs.len());
    for i in 0..n {
        render_output(state, i);
    }
}

/// Composite and page-flip one output. A no-op when the session is inactive.
fn render_output(state: &mut HeliosState, index: usize) {
    let Some(drm) = state.drm.as_mut() else { return };
    if !drm.session.is_active() {
        return;
    }
    let Some(entry) = drm.outputs.get(index) else {
        return;
    };
    let output = entry.output.clone();
    let crtc = entry.crtc;
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = state
        .space
        .output_geometry(&output)
        .map(|g| g.loc)
        .unwrap_or_default();

    // The pointer lives in the global space; elements are output-local.
    let cursor_pos = state.pointer_location - output_loc.to_f64();
    let mut elements: Vec<HeliosRenderElement> =
        vec![HeliosRenderElement::Solid(SolidColorRenderElement::from_buffer(
            &drm.cursor,
            cursor_pos.to_physical_precise_round(scale),
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
        state.input_method_popup.as_ref(),
    ));

    // FrameFlags::empty() forces full composition: F-04 §2 assumes no plane
    // availability. Plane scanout is a probed optimisation for a later
    // milestone.
    let compositor = &mut drm.outputs[index].compositor;
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

    let entry = &mut drm.outputs[index];
    entry.needs_render = false;
    entry.frame_pending = queued;
    let arm = !queued && !entry.retry_armed && failed;
    if arm {
        // The frame had damage but could not be queued: retry shortly. Never
        // re-arm for an empty frame — an idle render_frame spin pushes empty
        // entries into the damage tracker's history while the swapchain slot
        // ages stand still, which desynchronises the two and leaves stale
        // content on screen.
        entry.retry_armed = true;
        let handle = drm.loop_handle.clone();
        if let Err(err) = handle.insert_source(
            Timer::from_duration(Duration::from_millis(16)),
            move |_, _, state| {
                let i = state.drm.as_mut().and_then(|drm| {
                    let i = drm.index_of_crtc(crtc)?;
                    drm.outputs[i].retry_armed = false;
                    Some(i)
                });
                if let Some(i) = i {
                    render_output(state, i);
                }
                TimeoutAction::Drop
            },
        ) {
            tracing::warn!(?err, "arming re-render timer");
            if let Some(drm) = state.drm.as_mut() {
                if let Some(i) = drm.index_of_crtc(crtc) {
                    drm.outputs[i].retry_armed = false;
                }
            }
        }
    }

    let time = state.start_time.elapsed();
    crate::render::send_frames(&state.space, &output, time);
    state.space.refresh();
    state.popups.cleanup();
    let _ = state.display_handle.flush_clients();
}
