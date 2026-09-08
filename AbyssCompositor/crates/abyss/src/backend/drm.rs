// SPDX-License-Identifier: AGPL-3.0-only
//! Production backend (COMP-01 §3, F-04): KMS via libseat + udev + libinput,
//! GLES rendering onto GBM buffers through EGL.
//!
//! M3 scope: single GPU, every connected connector driven as its own output,
//! runtime connector hotplug through udev, and a headless fallback output so
//! the compositor survives with nothing plugged in (COMP-03 §5). Everything
//! that touches hardware logs and continues rather than panicking.

use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant},
};

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
            DrmDevice, DrmDeviceFd, DrmEvent, DrmEventTime, DrmNode, NodeType, VrrSupport,
        },
        egl::{context::EGLContext, display::EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{gles::GlesRenderer, ImportDma},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{UdevBackend, UdevEvent},
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
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::Display,
    },
    utils::{DeviceFd, Scale, Transform},
    wayland::{
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState},
        presentation::Refresh,
        socket::ListeningSocketSource,
    },
};

use crate::{
    backend::gpu,
    config::Config,
    outputs::OutputKind,
    render::{
        collect_elements, presentation_feedback, send_dmabuf_feedback, update_primary_scanout,
        AbyssRenderElement, SurfaceFeedback,
    },
    state::{client_state, AbyssState},
};

const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Size of the headless fallback output used when no connector is present.
const FALLBACK_SIZE: (i32, i32) = (1920, 1080);

/// Per-frame user data: the presentation callbacks waiting on that page flip.
type FrameData = Option<smithay::desktop::utils::OutputPresentationFeedback>;

pub type AbyssDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, FrameData, DrmDeviceFd>;

/// One scanout pipeline: connector → CRTC → [`DrmCompositor`].
pub struct DrmOutput {
    /// Handle into [`crate::outputs::Outputs`].
    pub id: u64,
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    pub output: Output,
    pub compositor: AbyssDrmCompositor,
    /// Set while a re-render timer is armed, so we never stack timers.
    retry_armed: bool,
    /// A frame is queued and we are waiting for its VBlank.
    frame_pending: bool,
    /// Content changed since the last composite; render at the next chance.
    needs_render: bool,
    /// Render + scanout dmabuf tranches for this output (COMP-02 §2).
    feedback: Option<SurfaceFeedback>,
    /// `vrr` requested for this output in the config or over the socket.
    vrr_config: bool,
    /// The connector advertises adaptive sync.
    vrr_capable: bool,
}

/// Everything the DRM backend needs to keep alive between callbacks.
pub struct DrmData {
    pub session: LibSeatSession,
    pub loop_handle: LoopHandle<'static, AbyssState>,
    pub drm: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    pub renderer: GlesRenderer,
    pub outputs: Vec<DrmOutput>,
    /// The headless stand-in, live only while no connector is.
    fallback: Option<u64>,
    cursor: crate::render::cursor::Fallback,
    /// Open libinput devices, so a config reload can reconfigure them.
    pub input_devices: Vec<smithay::reexports::input::Device>,
    /// Render node of the primary GPU, the `main_device` of every feedback.
    render_node: libc::dev_t,
}

impl super::Backend for DrmData {
    fn import_dmabuf(&mut self, buf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.renderer.import_dmabuf(buf, None).is_ok()
    }

    fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(?err, vt, "VT switch failed");
        }
    }
}

impl DrmData {
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
             (F-04 §2); abyss has no implicit-sync fallback."
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
pub fn scan_connectors(state: &mut AbyssState) {
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
    state: &mut AbyssState,
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
    let global = output.create_global::<AbyssState>(&dh);
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
    let compositor = AbyssDrmCompositor::new(
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

    // Scanout tranche: the formats this output's primary plane can actually
    // scan out, advertised above the render tranche so a full-screen client
    // allocates something we can hand straight to KMS (COMP-02 §2).
    let feedback = state.dmabuf_feedback.as_ref().and_then(|render| {
        let plane_formats = compositor.surface().plane_info().formats.clone();
        DmabufFeedbackBuilder::new(drm.render_node, renderer_formats.iter().copied())
            .add_preference_tranche(
                compositor.surface().device_fd().dev_id().ok()?,
                Some(
                    smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags::Scanout,
                ),
                plane_formats,
            )
            .build()
            .ok()
            .map(|scanout| SurfaceFeedback {
                render: render.clone(),
                scanout,
            })
    });

    // COMP-03 §8: VRR is on/off/auto per output; "auto" only engages while a
    // fullscreen surface owns the output, handled in render_output.
    let vrr_support = compositor
        .vrr_supported(handle)
        .unwrap_or(VrrSupport::NotSupported);
    let vrr_config = state.config.output_rule(&name, &identity).vrr.unwrap_or(false)
        && matches!(vrr_support, VrrSupport::Supported);

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
            feedback,
            vrr_config,
            vrr_capable: matches!(vrr_support, VrrSupport::Supported),
        });
    }
    tracing::info!(
        output = %name,
        width = wl_mode.size.w,
        height = wl_mode.size.h,
        refresh_mhz = wl_mode.refresh,
        vrr = ?vrr_support,
        "output configured"
    );
    Ok(())
}

/// Never leave the human with no output (COMP-03 §5): stand up a headless one
/// when the last connector goes away, and retire it when a real one returns.
fn sync_fallback(state: &mut AbyssState) {
    let Some(drm) = state.drm.as_ref() else { return };
    let physical = !drm.outputs.is_empty();
    let fallback = drm.fallback;
    match (physical, fallback) {
        (false, None) => {
            let (output, _) = crate::outputs::virtual_output("HEADLESS-1", FALLBACK_SIZE);
            let global = output.create_global::<AbyssState>(&state.display_handle.clone());
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

/// COMP-01 §4: resolve the render device to use.
///
/// `None` means "auto" — every DRM device on the seat is ranked by
/// [`crate::backend::gpu`] and the best one wins. An explicit request is
/// either a device path or a `pci:DDDD:BB:DD.F` address resolved through sysfs.
/// An explicit request that does not resolve is a hard error rather than a
/// silent fallback (ADR 0033): a typo that quietly lands on the wrong GPU is
/// worse than a refusal to start.
fn resolve_render_device(requested: Option<&str>, seat_name: &str) -> Result<PathBuf> {
    let Some(req) = requested else {
        return auto_select_render_device(seat_name);
    };
    let path = match req.strip_prefix("pci:") {
        Some(addr) => {
            let dir = PathBuf::from("/sys/bus/pci/devices").join(addr).join("drm");
            let mut cards: Vec<PathBuf> = std::fs::read_dir(&dir)
                .with_context(|| format!("render-device 'pci:{addr}' has no DRM node ({})", dir.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name())
                .filter(|n| n.to_string_lossy().starts_with("card"))
                .map(|n| PathBuf::from("/dev/dri").join(n))
                .collect();
            cards.sort();
            cards
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("render-device 'pci:{addr}' exposes no card node"))?
        }
        None => PathBuf::from(req),
    };
    if !path.exists() {
        anyhow::bail!(
            "render-device '{req}' resolved to {} which does not exist",
            path.display()
        );
    }
    Ok(path)
}

/// The COMP-01 §4 automatic path: rank every DRM device on the seat and take
/// the best. The whole ranked list is logged with the reason, as the spec
/// requires — with one render device driving everything, a wrong pick is a
/// black screen, and the log is the only way to see why it happened.
fn auto_select_render_device(seat_name: &str) -> Result<PathBuf> {
    let ranked = gpu::probe_seat(seat_name).context("enumerating GPUs")?;
    let best = ranked
        .first()
        .ok_or_else(|| anyhow!("no GPU found on seat '{seat_name}'"))?;
    for (i, g) in ranked.iter().enumerate() {
        tracing::info!(rank = i + 1, "GPU candidate: {}", g.describe());
    }
    if best.render.is_none() {
        anyhow::bail!(
            "best GPU {} exposes no render node; abyss needs one to composite",
            best.card.display()
        );
    }
    // Multi-GPU is out of scope for v1: outputs on a device we did not select
    // are never lit, so a machine whose connectors all hang off another GPU
    // (a hybrid dGPU laptop) must refuse to start rather than come up blind.
    if best.connectors == 0 {
        if let Some(other) = ranked.iter().find(|g| g.connectors > 0) {
            anyhow::bail!(
                "selected {} has no connectors; the displays are on {} \
                 (multi-GPU and hybrid graphics are not supported in v1 — \
                 set render_device to pin one GPU that has both)",
                best.card.display(),
                other.card.display(),
            );
        }
        anyhow::bail!("no GPU on seat '{seat_name}' has any connector");
    }
    tracing::info!(
        path = %best.card.display(),
        driver = %best.driver,
        "selected render device: best of {} by class/VRAM/PCI-address",
        ranked.len()
    );
    Ok(best.card.clone())
}

pub fn run(config: Config, stats: bool, session_handoff: bool) -> Result<()> {
    let mut event_loop: EventLoop<'static, AbyssState> =
        EventLoop::try_new().context("calloop event loop")?;
    let display: Display<AbyssState> = Display::new().context("wayland display")?;
    let handle = event_loop.handle();

    let (session, session_notifier) =
        LibSeatSession::new().context("libseat session (is seatd running, or logind available?)")?;
    let seat_name = session.seat();
    tracing::info!(seat = %seat_name, "libseat session acquired");

    let requested_device = config.misc.render_device.clone();
    let socket = ListeningSocketSource::new_auto().context("wayland socket")?;
    let mut state = AbyssState::new(
        &display,
        event_loop.get_signal(),
        handle.clone(),
        &socket,
        config,
        stats,
    );

    crate::input::idle::start(&mut state, &handle);
    crate::ipc::start(&mut state, &handle);
    crate::config::watch::start(&mut state, &handle);
    crate::xwayland::start(&mut state);

    // --- GPU discovery -----------------------------------------------------
    let udev = UdevBackend::new(&seat_name).context("udev backend")?;
    let gpu_path: PathBuf = resolve_render_device(requested_device.as_deref(), &seat_name)?;

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
    let device_fd_for_syncobj = device_fd.clone();
    let gbm = GbmDevice::new(device_fd).context("GbmDevice::new")?;

    // SAFETY: the gbm device outlives the display (both live in DrmData /
    // this function's scope for the lifetime of the process).
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }.context("EGLDisplay::new")?;
    let egl_context = EGLContext::new(&egl_display).context("EGLContext::new")?;
    // SAFETY: the context is current on this (single) thread only.
    let renderer = unsafe { GlesRenderer::new(egl_context) }.context("GlesRenderer::new")?;

    // dmabuf is the expected client buffer path (F-04); shm still works.
    // v5 default feedback names the render node and its texture formats; the
    // per-output scanout tranche is added per surface in render_output.
    let render_node = node
        .node_with_type(NodeType::Render)
        .and_then(|r| r.ok())
        .unwrap_or(node)
        .dev_id();
    let dmabuf_formats = renderer.egl_context().dmabuf_texture_formats().clone();
    let mut dmabuf_state = DmabufState::new();
    match DmabufFeedbackBuilder::new(render_node, dmabuf_formats.iter().copied()).build() {
        Ok(feedback) => {
            let _global = dmabuf_state
                .create_global_with_default_feedback::<AbyssState>(&state.display_handle, &feedback);
            state.dmabuf_feedback = Some(feedback);
        }
        Err(err) => {
            tracing::warn!(?err, "dmabuf feedback build failed; advertising formats only");
            let _global = dmabuf_state
                .create_global::<AbyssState>(&state.display_handle, dmabuf_formats.iter().copied());
        }
    }
    state.dmabuf_state = Some(dmabuf_state);

    // Explicit sync (COMP-02 §3). Without syncobj eventfd support in the
    // driver there is no way to wait without blocking the loop, so the global
    // is simply absent and clients fall back to implicit sync.
    if supports_syncobj_eventfd(&device_fd_for_syncobj) {
        state.syncobj_state = Some(DrmSyncobjState::new::<AbyssState>(
            &state.display_handle,
            device_fd_for_syncobj.clone(),
        ));
        tracing::info!("explicit sync (wp_linux_drm_syncobj_v1) enabled");
    } else {
        tracing::warn!("driver has no syncobj eventfd support; no explicit sync global");
    }

    state.drm = Some(Box::new(DrmData {
        session: session.clone(),
        loop_handle: handle.clone(),
        drm: drm_device,
        gbm,
        renderer,
        outputs: Vec::new(),
        fallback: None,
        cursor: crate::render::cursor::Fallback::default(),
        input_devices: Vec::new(),
        render_node,
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
            // Devices are configured as they appear, and remembered so a config
            // reload can reach the ones already open (COMP-13 §1.2).
            match &event {
                InputEvent::DeviceAdded { device } => {
                    let mut device = device.clone();
                    crate::input::configure_device(&mut device, &state.config.input);
                    if let Some(drm) = state.drm.as_mut() {
                        drm.input_devices.push(device);
                    }
                }
                InputEvent::DeviceRemoved { device } => {
                    if let Some(drm) = state.drm.as_mut() {
                        drm.input_devices.retain(|d| d != device);
                    }
                }
                _ => {}
            }
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
        .insert_source(drm_notifier, move |event, meta, state| match event {
            DrmEvent::VBlank(crtc) => {
                let clock_now = state.clock.now();
                let pending = if let Some(drm) = state.drm.as_mut() {
                    match drm.index_of_crtc(crtc) {
                        Some(i) => {
                            let o = &mut drm.outputs[i];
                            let refresh = o
                                .output
                                .current_mode()
                                .map(|m| Duration::from_secs_f64(1_000f64 / m.refresh as f64 / 1_000f64))
                                .unwrap_or_else(|| Duration::from_millis(16));
                            match o.compositor.frame_submitted() {
                                // wp_presentation: report the real page-flip
                                // timestamp and sequence the kernel gave us.
                                Ok(Some(Some(mut feedback))) => {
                                    let (time, seq) = match meta.as_ref() {
                                        Some(m) => (
                                            match m.time {
                                                DrmEventTime::Monotonic(t) => t,
                                                DrmEventTime::Realtime(_) => clock_now.into(),
                                            },
                                            m.sequence as u64,
                                        ),
                                        None => (clock_now.into(), 0),
                                    };
                                    let vrr = o.compositor.vrr_enabled();
                                    feedback.presented::<_, smithay::utils::Monotonic>(
                                        time,
                                        if vrr {
                                            Refresh::Variable(refresh)
                                        } else {
                                            Refresh::fixed(refresh)
                                        },
                                        seq,
                                        wp_presentation_feedback::Kind::Vsync
                                            | wp_presentation_feedback::Kind::HwClock
                                            | wp_presentation_feedback::Kind::HwCompletion,
                                    );
                                }
                                Ok(_) => {}
                                Err(err) => tracing::warn!(?err, "frame_submitted"),
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
    tracing::info!(socket = %state.socket_name, "abyss ready on DRM");
    if session_handoff {
        crate::session::import();
    }

    render(&mut state);
    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    crate::ipc::cleanup(&state);
    if session_handoff {
        crate::session::teardown();
    }
    Ok(())
}

/// Mark every output dirty and composite the ones that are idle. An output
/// with a frame in flight picks the new content up at its VBlank, which keeps
/// `render_frame` calls one-to-one with submitted frames — what smithay's
/// damage tracker assumes.
pub fn schedule_render(state: &mut AbyssState) {
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
    service_captures(state);
}

/// Drain the authorised capture queue. The renderer is moved out of
/// `AbyssState` for the call because `service` needs the state too.
fn service_captures(state: &mut AbyssState) {
    if state.captures.is_empty() {
        return;
    }
    let Some(mut drm) = state.drm.take() else { return };
    crate::render::capture::service(state, &mut drm.renderer);
    state.drm = Some(drm);
}

/// Composite every output. Safe to call at any time.
pub fn render(state: &mut AbyssState) {
    let n = state.drm.as_ref().map_or(0, |d| d.outputs.len());
    for i in 0..n {
        render_output(state, i);
    }
    service_captures(state);
}

/// Composite and page-flip one output. A no-op when the session is inactive.
fn render_output(state: &mut AbyssState, index: usize) {
    let capture_active = state.capture_active();
    let Some(drm) = state.drm.as_mut() else { return };
    if !drm.session.is_active() {
        return;
    }
    let Some(entry) = drm.outputs.get(index) else {
        return;
    };
    // DPMS off: the CRTC is already cleared, and re-arming it here would
    // light the panel back up behind the human's back.
    let out_id = entry.id;
    if !state.outputs.get(out_id).map(|e| e.powered).unwrap_or(true) {
        return;
    }
    let Some(drm) = state.drm.as_mut() else { return };
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
    // Trusted UI, above the cursor and never drawn into a capture target.
    let mut elements: Vec<AbyssRenderElement> = crate::render::capture::indicator(&output, capture_active);
    elements.extend(crate::render::cursor::elements(
        &mut drm.renderer,
        &state.cursor_status,
        &mut drm.cursor,
        cursor_pos,
        scale,
    ));
    if state.lock.locked {
        elements.extend(crate::protocols::standard::session_lock::lock_elements(
            &mut drm.renderer,
            &mut state.lock,
            &output,
        ));
    } else {
        elements.extend(collect_elements(
            &mut drm.renderer,
            &state.space,
            &mut state.borders,
            &output,
            &state.config,
            state.focus.as_ref(),
            state.input_method_popup.as_ref(),
        ));
    }
    let animating = state.borders.anim.running();

    // A surface covering the whole output is both the direct-scanout candidate
    // and the trigger for adaptive sync (COMP-03 §8).
    let candidate = crate::render::scanout_candidate(&state.space, &output, &state.config);
    let vrr_wanted = drm.outputs[index].vrr_config && candidate.is_some();

    // Direct scanout (COMP-02 §2) hands a client buffer to a KMS plane, which
    // means the compositor never touches its pixels — so it cannot redact
    // them. A sensitive surface therefore forces full composition (COMP-02 §7,
    // ADR 0025), as does the config knob being off.
    let redact = candidate
        .as_ref()
        .map(|s| state.sensitive.contains(s))
        .unwrap_or(false);
    let flags = if state.config.render.direct_scanout && !redact {
        FrameFlags::DEFAULT
    } else {
        FrameFlags::empty()
    };

    let compositor = &mut drm.outputs[index].compositor;
    // COMP-03 §8: adaptive sync only while a surface covers the whole output;
    // a windowed desktop on a variable-refresh panel flickers otherwise.
    if vrr_wanted != compositor.vrr_enabled() {
        if let Err(err) = compositor.use_vrr(vrr_wanted) {
            tracing::warn!(?err, vrr = vrr_wanted, "setting adaptive sync");
        }
    }

    let render_start = Instant::now();
    let mut failed = false;
    let mut empty = false;
    let mut states = None;
    match compositor.render_frame(&mut drm.renderer, &elements, CLEAR, flags) {
        Ok(result) if result.is_empty => empty = true,
        Ok(result) => states = Some(result.states.clone()),
        Err(err) => {
            tracing::warn!(?err, "rendering frame");
            failed = true;
        }
    }
    let render_time = render_start.elapsed();
    drop(elements);
    let surface_feedback = drm.outputs[index].feedback.as_ref().map(|f| SurfaceFeedback {
        render: f.render.clone(),
        scanout: f.scanout.clone(),
    });

    // Feedback needs `&state.space`, so the frame data is built outside the
    // `drm` borrow and handed back to `queue_frame`.
    let mut queued = false;
    if let Some(states) = states {
        update_primary_scanout(&state.space, &output, &states);
        if let Some(fb) = surface_feedback.as_ref() {
            send_dmabuf_feedback(&state.space, &output, fb, candidate.as_ref());
        }
        let presentation = presentation_feedback(&state.space, &output, &states);
        let submit_start = Instant::now();
        let Some(drm) = state.drm.as_mut() else { return };
        match drm.outputs[index].compositor.queue_frame(Some(presentation)) {
            Ok(()) => queued = true,
            Err(err) => {
                tracing::warn!(?err, "queueing frame");
                failed = true;
            }
        }
        state.stats.record(render_time, submit_start.elapsed());
        state.stats.maybe_report();
    }

    let Some(drm) = state.drm.as_mut() else { return };
    let compositor = &mut drm.outputs[index].compositor;

    if empty {
        // An empty frame is discarded rather than submitted, but the damage
        // tracker still recorded it. That desynchronises the damage history
        // from the swapchain slot ages, and every later frame then restores
        // the wrong regions (stale content, cursor trails). Clearing the ages
        // forces the next frame to be a full redraw, which resyncs both.
        compositor.reset_buffer_ages();
    }

    let entry = &mut drm.outputs[index];
    // A running animation asks for the next frame here: leaving `needs_render`
    // set makes this VBlank's completion schedule another render, and it
    // clears itself the frame the last move finishes (COMP-02 §9).
    entry.needs_render = animating;
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

/// Turn adaptive sync on or off for one output at runtime (COMP-13 §2.1).
/// `false` means the request was refused: unknown output, or a connector that
/// does not support it. Turning it *off* always succeeds where the output
/// exists, because "off" is what an unsupported connector already is.
pub fn set_vrr(state: &mut AbyssState, id: u64, on: bool) -> bool {
    let Some(drm) = state.drm.as_mut() else {
        return false;
    };
    let Some(index) = drm.outputs.iter().position(|o| o.id == id) else {
        return false;
    };
    if on && !drm.outputs[index].vrr_capable {
        return false;
    }
    drm.outputs[index].vrr_config = on;
    drm.outputs[index].needs_render = true;
    schedule_render(state);
    true
}

/// The size of one gamma ramp channel for an output, or `None` if the output is
/// unknown or its CRTC has no programmable ramp.
pub fn vrr(state: &AbyssState, id: u64) -> bool {
    let Some(drm) = state.drm.as_ref() else {
        return false;
    };
    drm.outputs
        .iter()
        .find(|o| o.id == id)
        .is_some_and(|o| o.vrr_config && o.vrr_capable)
}

pub fn gamma_size(state: &AbyssState, id: u64) -> Option<u32> {
    let drm = state.drm.as_ref()?;
    let output = drm.outputs.iter().find(|o| o.id == id)?;
    let info = drm.drm.get_crtc(output.crtc).ok()?;
    let len = info.gamma_length();
    (len > 0).then_some(len)
}

/// Load a gamma ramp onto one output's CRTC (COMP-03 §7). The three slices must
/// each be [`gamma_size`] long. `false` means the request was refused: unknown
/// output, wrong length, or a driver that rejected the ramp.
pub fn set_gamma(state: &mut AbyssState, id: u64, r: &[u16], g: &[u16], b: &[u16]) -> bool {
    let Some(size) = gamma_size(state, id) else {
        return false;
    };
    if r.len() != size as usize || g.len() != size as usize || b.len() != size as usize {
        return false;
    }
    let Some(drm) = state.drm.as_ref() else {
        return false;
    };
    let Some(output) = drm.outputs.iter().find(|o| o.id == id) else {
        return false;
    };
    match drm.drm.set_gamma(output.crtc, r, g, b) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!(?err, id, "setting gamma ramp");
            false
        }
    }
}

/// DPMS for one output (COMP-03 §7): clear the CRTC on the way down, and let
/// the normal render path bring it back on the way up.
pub fn set_power(state: &mut AbyssState, id: u64, on: bool) {
    let Some(drm) = state.drm.as_mut() else { return };
    let Some(index) = drm.outputs.iter().position(|o| o.id == id) else {
        return;
    };
    if on {
        drm.outputs[index].needs_render = true;
        drm.outputs[index].frame_pending = false;
        render_output(state, index);
    } else if let Err(err) = drm.outputs[index].compositor.clear() {
        tracing::warn!(?err, id, "powering output off");
    } else {
        drm.outputs[index].frame_pending = false;
    }
}
