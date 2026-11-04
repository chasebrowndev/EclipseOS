// SPDX-License-Identifier: AGPL-3.0-only
//! Production backend (COMP-01 §3, F-04): KMS via libseat + udev + libinput,
//! GLES rendering onto GBM buffers through EGL.
//!
//! M3 scope: every connected connector driven as its own output, runtime
//! connector hotplug through udev, and a headless fallback output so the
//! compositor survives with nothing plugged in (COMP-03 §5). Everything that
//! touches hardware logs and continues rather than panicking.
//!
//! Hybrid graphics (ADR 0084): every GPU with a connector composites its own
//! outputs with its own renderer and scans them out against its own planes. One
//! *primary* GPU also owns explicit sync, capture and the default dmabuf
//! feedback. Each output's dmabuf feedback leads with the GPU that renders it,
//! so a window on that output allocates for that GPU.
//!
//! "Render then copy" is the fallback: the primary renders the output into an
//! offscreen dmabuf, which the output's own GPU imports and blits into its
//! scanout buffer. It carries a frame when a visible client buffer could not be
//! imported by the output's GPU (native compositing resumes on the next frame
//! the buffers import), and it carries every frame of a secondary's outputs in
//! `dedicated` GPU mode. A secondary that fails to come up is skipped with a
//! warning; it never stops the primary starting.

use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use smithay::{
    backend::{
        allocator::{
            dmabuf::AsDmabuf,
            gbm::{GbmAllocator, GbmBuffer, GbmBufferFlags, GbmDevice},
            Format, Fourcc, Modifier, Slot, Swapchain,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode,
            NodeType, VrrSupport,
        },
        egl::{context::EGLContext, display::EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::OutputDamageTracker,
            element::{texture::TextureRenderElement, Id, Kind, RenderElementStates},
            gles::{GlesRenderer, GlesTexture},
            utils::DamageBag,
            Bind, ExportMem, ImportDma, ImportMem, Renderer, Texture, TextureMapping,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{UdevBackend, UdevEvent},
    },
    output::{Mode as OutputMode, Output, OutputModeSource, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic, timer::TimeoutAction, timer::Timer, EventLoop, Interest, LoopHandle,
            Mode as CalloopMode, PostAction, RegistrationToken,
        },
        drm::control::{connector, crtc, Device as _, Mode as DrmMode, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::Display,
    },
    utils::{Buffer as BufferCoord, DeviceFd, Physical, Rectangle, Scale, Size, Transform},
    wayland::{
        dmabuf::{DmabufFeedbackBuilder, DmabufState},
        drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState},
        presentation::Refresh,
        socket::ListeningSocketSource,
        tablet_manager::{TabletDescriptor, TabletSeatTrait},
    },
};

use crate::{
    backend::gpu,
    config::{Config, GpuMode},
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
    /// Index into [`DrmData::devices`] of the GPU this connector hangs off.
    pub device: usize,
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    pub output: Output,
    pub compositor: AbyssDrmCompositor,
    /// Set while a re-render timer is armed, so we never stack timers.
    retry_armed: bool,
    /// Frames in a row that could not be rendered or queued. Only throttles the
    /// log ([`log_failure_now`]); it returns to zero once a frame is queued.
    commit_failures: u32,
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
    /// Render-then-copy state (ADR 0084); `Some` once an output on a secondary
    /// device has needed the copy path. Kept after a fallback frame so the
    /// next one does not reallocate.
    pass: Option<SecondaryPass>,
    /// Frames of this output are rendered on the primary and copied (the
    /// compositor takes the finished frame as a static texture). Always so with
    /// `always_copy`; otherwise only while a visible buffer is unimportable on
    /// the output's own GPU.
    copying: bool,
    /// Secondary device, copy by default (ADR 0086) or `dedicated`: the copy
    /// path is the only path.
    always_copy: bool,
    /// Format and modifiers of the transfer buffer both GPUs share, when
    /// there are any; without them the fallback is unavailable.
    copy_format: Option<(Fourcc, Vec<Modifier>)>,
}

/// What a secondary output needs to be fed by the primary GPU (ADR 0084).
struct SecondaryPass {
    /// Offscreen dmabufs allocated on the primary's GBM, in a format and
    /// modifier both GPUs understand ([`gpu::pick_transfer_format`]).
    swapchain: Swapchain<GbmAllocator<DrmDeviceFd>>,
    /// The primary pass: renders the output's elements into the swapchain.
    tracker: OutputDamageTracker,
    size: Size<i32, Physical>,
    /// One element identity for the whole life of the output, so the secondary
    /// compositor diffs frame against frame and copies only what changed.
    element_id: Id,
    /// Primary-pass damage, in the coordinates of the transfer texture.
    damage: DamageBag<i32, BufferCoord>,
    /// The next frame must redraw and re-copy everything (after a refused
    /// frame, a resume or a mode change).
    force_full: bool,
    /// The transfer buffer the secondary GPU may still be reading; kept out of
    /// the swapchain until that output's next VBlank.
    held: Option<Slot<GbmBuffer>>,
    /// Importing the dmabuf failed, so frames go through system memory.
    cpu_copy: bool,
}

/// The secondary-side texture imported from one swapchain slot, kept in the
/// slot's user data so a buffer is imported once for its whole life.
struct SlotTexture(RefCell<Option<GlesTexture>>);

impl SecondaryPass {
    fn new(
        primary_gbm: &GbmDevice<DrmDeviceFd>,
        format: Fourcc,
        modifiers: Vec<Modifier>,
        size: Size<i32, Physical>,
        output: &Output,
    ) -> Self {
        // A linear transfer buffer must actually be linear: with only the flags
        // to go on, GBM may otherwise pick a tiled layout the other GPU cannot read.
        let mut flags = GbmBufferFlags::RENDERING;
        if modifiers == [Modifier::Linear] {
            flags |= GbmBufferFlags::LINEAR;
        }
        Self {
            swapchain: Swapchain::new(
                GbmAllocator::new(primary_gbm.clone(), flags),
                size.w.max(1) as u32,
                size.h.max(1) as u32,
                format,
                modifiers,
            ),
            tracker: OutputDamageTracker::from_output(output),
            size,
            element_id: Id::new(),
            damage: DamageBag::default(),
            force_full: true,
            held: None,
            cpu_copy: false,
        }
    }

    /// Follow a mode switch: new swapchain size, and the secondary compositor's
    /// static mode source (the primary pass already applied scale and
    /// transform, so the secondary sees a plain 1:1 texture).
    fn fit(&mut self, size: Size<i32, Physical>, compositor: &mut AbyssDrmCompositor) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.swapchain.resize(size.w.max(1) as u32, size.h.max(1) as u32);
        self.held = None;
        self.force_full = true;
        compositor.set_output_mode_source(static_source(size));
    }
}

/// The mode source of a secondary output's compositor: the finished frame, 1:1.
fn static_source(size: Size<i32, Physical>) -> OutputModeSource {
    OutputModeSource::Static {
        size,
        scale: Scale::from(1.0),
        transform: Transform::Normal,
    }
}

/// One DRM device on the seat (ADR 0084).
pub struct GpuDevice {
    pub dev_id: libc::dev_t,
    /// The device that renders. Its renderer is [`DrmData::renderer`].
    pub primary: bool,
    /// `cardN`, for logs and output-name disambiguation.
    pub tag: String,
    /// The render node, the `main_device` of feedback for surfaces this GPU
    /// composites.
    pub render_node: libc::dev_t,
    pub drm: DrmDevice,
    pub gbm: GbmDevice<DrmDeviceFd>,
    /// EGL/GLES on this device's GBM: it composites this device's outputs and
    /// blits fallback frames. `None` on the primary, whose renderer is
    /// [`DrmData::renderer`].
    pub renderer: Option<GlesRenderer>,
    /// The device's DRM notifier in the event loop.
    token: RegistrationToken,
}

/// Everything the DRM backend needs to keep alive between callbacks.
pub struct DrmData {
    pub session: LibSeatSession,
    pub loop_handle: LoopHandle<'static, AbyssState>,
    /// Every DRM device we drive; index 0 is the primary.
    pub devices: Vec<GpuDevice>,
    /// The primary's GBM device (a clone of `devices[0].gbm`).
    pub gbm: GbmDevice<DrmDeviceFd>,
    /// The primary's renderer: composites the primary's outputs, renders the
    /// copy path's frames and serves capture.
    pub renderer: GlesRenderer,
    /// `misc.gpu-mode` as started (restart-only).
    pub gpu_mode: GpuMode,
    /// `misc.secondary-compositing` as started (restart-only, ADR 0086).
    pub secondary_compositing: bool,
    pub outputs: Vec<DrmOutput>,
    /// The primary GPU was removed; there is nothing left to render with.
    primary_lost: bool,
    /// The headless stand-in, live only while no connector is.
    fallback: Option<u64>,
    cursor: crate::render::cursor::Fallback,
    /// Open libinput devices, so a config reload can reconfigure them.
    pub input_devices: Vec<smithay::reexports::input::Device>,
    /// Frame-budget watch that sheds animations (COMP-14 §6).
    shed: crate::render::anim::ShedMonitor,
}

impl super::Backend for DrmData {
    /// Accepted when any GPU we composite with can import it: an output on
    /// another GPU can still use a buffer the primary cannot (ADR 0084).
    fn import_dmabuf(&mut self, buf: &smithay::backend::allocator::dmabuf::Dmabuf) -> bool {
        self.renderer.import_dmabuf(buf, None).is_ok()
            || self
                .devices
                .iter_mut()
                .filter_map(|d| d.renderer.as_mut())
                .any(|r| r.import_dmabuf(buf, None).is_ok())
    }

    fn change_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(?err, vt, "VT switch failed");
        }
    }
}

impl DrmData {
    fn device_index(&self, dev_id: libc::dev_t) -> Option<usize> {
        self.devices.iter().position(|d| d.dev_id == dev_id)
    }

    /// The output on `dev_id` driving `crtc`. CRTC handles are per device, so
    /// the pair is the key.
    fn index_of(&self, dev_id: libc::dev_t, crtc: crtc::Handle) -> Option<usize> {
        let dev = self.device_index(dev_id)?;
        self.outputs
            .iter()
            .position(|o| o.device == dev && o.crtc == crtc)
    }
}

/// Apply the pointer half of the `input` block to one libinput device, as
/// [`crate::input::device_settings`] resolves it for this device's name.
/// Every setter is optional on the device; a device that does not support a
/// knob reports failure and keeps its default. A setting the config leaves to
/// the device (`scroll-method "default"`, no `calibration`) restores the
/// device's default, so a reload that drops a key undoes it.
pub fn configure_input_device(device: &mut smithay::reexports::input::Device, input: &crate::config::Input) {
    use smithay::reexports::input::{AccelProfile, ClickMethod, ScrollMethod};
    let touchpad = device.config_tap_finger_count() > 0;
    let s = crate::input::device_settings(input, device.name(), touchpad);
    if device.config_accel_is_available() {
        let _ = device.config_accel_set_profile(match s.accel_profile {
            "flat" => AccelProfile::Flat,
            _ => AccelProfile::Adaptive,
        });
        let _ = device.config_accel_set_speed(s.accel_speed);
    }
    let method = match s.scroll_method {
        Some("none") => Some(ScrollMethod::NoScroll),
        Some("two-finger") => Some(ScrollMethod::TwoFinger),
        Some("edge") => Some(ScrollMethod::Edge),
        Some("on-button-down") => Some(ScrollMethod::OnButtonDown),
        _ => device.config_scroll_default_method(),
    };
    if let Some(m) =
        method.filter(|m| *m == ScrollMethod::NoScroll || device.config_scroll_methods().contains(m))
    {
        let _ = device.config_scroll_set_method(m);
    }
    if let Some(t) = s.touchpad {
        let _ = device.config_tap_set_enabled(t.tap_to_click);
        let _ = device.config_tap_set_drag_enabled(t.tap_and_drag);
        let _ = device.config_click_set_method(match t.click_method {
            "button-areas" => ClickMethod::ButtonAreas,
            _ => ClickMethod::Clickfinger,
        });
        let _ = device.config_dwt_set_enabled(t.dwt);
        if device.config_scroll_has_natural_scroll() {
            let _ = device.config_scroll_set_natural_scroll_enabled(t.natural_scroll);
        }
    }
    if device.config_calibration_has_matrix() {
        if let Some(m) = s
            .calibration
            .or_else(|| device.config_calibration_default_matrix())
        {
            let _ = device.config_calibration_set_matrix(m);
        }
    }
    tracing::debug!(
        device = device.name(),
        block = input.devices.iter().any(|d| d.name == device.name()),
        "input device configured"
    );
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
        return mode.vrefresh().saturating_mul(1000).min(i32::MAX as u32) as i32;
    }
    // Saturate rather than truncate: `as i32` on a bad blob can land negative,
    // and a negative refresh reaches `Duration::from_secs_f64` in the VBlank
    // handler.
    ((mode.clock() as u64 * 1_000_000) / denom).min(i32::MAX as u64) as i32
}

/// Frame period for a mode's mHz refresh rate.
///
/// `Duration::from_secs_f64` panics on a negative or non-finite argument, and
/// this runs in the VBlank handler on every page flip, so a mode reporting a
/// nonsensical refresh must fall back rather than take the compositor down the
/// moment the screen lights up. The period is `1000 / mHz` seconds: 60_000 mHz
/// is a 16.6 ms frame.
fn frame_period(refresh_mhz: i32) -> Duration {
    if refresh_mhz <= 0 {
        return Duration::from_millis(16);
    }
    Duration::from_secs_f64(1_000f64 / refresh_mhz as f64)
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

/// Rescan every device (COMP-03 §3). Called at startup and after a VT switch.
pub fn scan_connectors(state: &mut AbyssState) {
    let n = state.drm.as_ref().map_or(0, |d| d.devices.len());
    for dev in 0..n {
        scan_device(state, dev);
    }
    sync_fallback(state);
}

/// Disambiguate a connector name that another device already uses (ADR 0084).
/// Kernel connector names count per card, so two GPUs can both own a `DP-1`.
/// The device enumerated first (the primary, then secondaries in rank order)
/// keeps the plain name; a later one gets `@cardN`, which is deterministic for
/// a given set of devices whatever is plugged in.
fn disambiguate(name: &str, taken: &[String], tag: &str) -> String {
    if taken.iter().any(|t| t == name) {
        format!("{name}@{tag}")
    } else {
        name.to_string()
    }
}

/// Names of every connector (plugged or not) on the devices before `dev`.
fn names_before(drm: &DrmData, dev: usize) -> Vec<String> {
    let mut names = Vec::new();
    for d in &drm.devices[..dev] {
        let Ok(res) = d.drm.resource_handles() else {
            continue;
        };
        for h in res.connectors() {
            if let Ok(info) = d.drm.get_connector(*h, false) {
                names.push(connector_name(&info));
            }
        }
    }
    names
}

/// Diff one device's connector list against the outputs we are driving on it
/// and bring the two back into agreement (COMP-03 §3). Called at startup and on
/// every udev `Changed` event for that GPU.
fn scan_device(state: &mut AbyssState, dev: usize) {
    let Some(drm) = state.drm.as_ref() else { return };
    if drm.primary_lost {
        return;
    }
    let Some(device) = drm.devices.get(dev) else {
        return;
    };
    let Ok(resources) = device.drm.resource_handles() else {
        tracing::warn!(device = %device.tag, "reading DRM resources");
        return;
    };

    let mut connected: Vec<(connector::Handle, connector::Info)> = Vec::new();
    for handle in resources.connectors() {
        match device.drm.get_connector(*handle, true) {
            Ok(info) if info.state() == connector::State::Connected && !info.modes().is_empty() => {
                connected.push((*handle, info))
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(device = %device.tag, ?err, "reading connector"),
        }
    }
    let present: HashSet<connector::Handle> = connected.iter().map(|(h, _)| *h).collect();

    // --- departures ---------------------------------------------------------
    let gone: Vec<(usize, u64)> = drm
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.device == dev && !present.contains(&o.connector))
        .map(|(i, o)| (i, o.id))
        .collect();
    // Drop every departing DrmOutput first, then stand the fallback up *before*
    // unregistering, so that unplugging the last monitor has somewhere to re-home
    // its windows instead of silently dropping them.
    for (i, _) in gone.iter().rev() {
        if let Some(drm) = state.drm.as_mut() {
            // Dropping the DrmOutput releases its CRTC for reuse.
            drm.outputs.remove(*i);
        }
    }
    if !gone.is_empty() {
        sync_fallback(state);
        for (_, id) in gone {
            crate::outputs::unregister(state, id);
        }
    }

    // --- arrivals -----------------------------------------------------------
    for (handle, info) in connected {
        let known = state
            .drm
            .as_ref()
            .is_some_and(|d| d.outputs.iter().any(|o| o.device == dev && o.connector == handle));
        if known {
            continue;
        }
        if let Err(err) = add_connector(state, dev, &resources, handle, &info) {
            tracing::warn!(connector = %connector_name(&info), ?err, "could not bring up connector");
        }
    }

    sync_fallback(state);
}

/// Bring up one connector: build its [`Output`], register it, and create the
/// [`DrmCompositor`] that scans it out.
fn add_connector(
    state: &mut AbyssState,
    dev: usize,
    resources: &smithay::reexports::drm::control::ResourceHandles,
    handle: connector::Handle,
    info: &connector::Info,
) -> Result<()> {
    let dh = state.display_handle.clone();
    let drm = state.drm.as_ref().ok_or_else(|| anyhow!("no DRM backend"))?;
    let device = &drm.devices[dev];
    let name = disambiguate(&connector_name(info), &names_before(drm, dev), &device.tag);
    let identity = crate::outputs::identity(edid_blob(&device.drm, handle).as_deref(), &name);

    let used: HashSet<crtc::Handle> = drm
        .outputs
        .iter()
        .filter(|o| o.device == dev)
        .map(|o| o.crtc)
        .collect();
    let crtc = pick_crtc(&device.drm, resources, info, &used)
        .ok_or_else(|| anyhow!("no free CRTC for connector {name}"))?;

    // Config, then the remembered layout, then the connector's preferred mode.
    let wanted = state
        .config
        .output_rule(&name, &identity)
        .mode
        .or_else(|| state.outputs.saved_for(&identity).and_then(|s| s.mode));
    // A connector can report zero modes (a live link that has not read EDID
    // yet); indexing [0] would panic mid-hotplug, so bail instead.
    let preferred = info
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| info.modes().first())
        .copied()
        .ok_or_else(|| anyhow!("connector {name} reports no modes"))?;
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
    let edid = edid_blob(&device.drm, handle).and_then(|b| crate::outputs::edid::parse(&b));
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
    let secondary = !drm.devices[dev].primary;
    // The formats the compositor may render into: the renderer of the GPU that
    // composites this output (ADR 0084) -- the device's own, in `hybrid`.
    let renderer_formats = if secondary {
        drm.devices[dev]
            .renderer
            .as_ref()
            .ok_or_else(|| anyhow!("secondary {} has no renderer", drm.devices[dev].tag))?
            .egl_context()
            .dmabuf_render_formats()
            .clone()
    } else {
        drm.renderer.egl_context().dmabuf_render_formats().clone()
    };
    // A buffer both GPUs understand, for the copy path: always for `dedicated`,
    // the per-frame fallback otherwise (absent, there is no fallback).
    let copy_format = if secondary {
        let primary_render: Vec<Format> = drm
            .renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();
        let secondary_texture: Vec<Format> = drm.devices[dev]
            .renderer
            .as_ref()
            .map(|r| r.egl_context().dmabuf_texture_formats().iter().copied().collect())
            .unwrap_or_default();
        gpu::pick_transfer_format(&primary_render, &secondary_texture)
    } else {
        None
    };
    let always_copy = gpu::output_copies(
        drm.gpu_mode,
        secondary,
        drm.secondary_compositing,
        copy_format.is_some(),
    );
    if always_copy && copy_format.is_none() {
        anyhow::bail!(
            "no dmabuf format shared between the primary GPU and {}",
            drm.devices[dev].tag
        );
    }
    if secondary {
        match (&copy_format, always_copy) {
            (Some((fourcc, modifiers)), true) => tracing::info!(
                output = %name,
                device = %drm.devices[dev].tag,
                ?fourcc,
                ?modifiers,
                "secondary output: render on primary, copy to {}",
                drm.devices[dev].tag
            ),
            (_, true) => {}
            (Some(_), false) => tracing::info!(
                output = %name,
                device = %drm.devices[dev].tag,
                "output composited on {}; render-on-primary copy kept as the fallback",
                drm.devices[dev].tag
            ),
            (None, false) => tracing::warn!(
                output = %name,
                device = %drm.devices[dev].tag,
                "no dmabuf format shared with the primary GPU: no fallback for buffers {} cannot import",
                drm.devices[dev].tag
            ),
        }
    }
    let mode_source = if always_copy {
        static_source(wl_mode.size)
    } else {
        OutputModeSource::from(&output)
    };
    let pass = match (&copy_format, always_copy) {
        (Some((fourcc, modifiers)), true) => Some(SecondaryPass::new(
            &drm.gbm,
            *fourcc,
            modifiers.clone(),
            wl_mode.size,
            &output,
        )),
        _ => None,
    };
    let device = &mut drm.devices[dev];
    let surface = device
        .drm
        .create_surface(crtc, mode, &[handle])
        .context("creating DRM surface")?;
    let allocator = GbmAllocator::new(
        device.gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(device.gbm.clone(), None);
    let cursor_size = device.drm.cursor_size();
    let gbm = device.gbm.clone();
    let compositor = AbyssDrmCompositor::new(
        mode_source,
        surface,
        None,
        allocator,
        exporter,
        // Opaque variants are listed too: a driver that advertises only
        // XRGB8888 on its primary plane would otherwise fail the modeset
        // outright and surface as a black screen behind a single warning.
        [
            Fourcc::Abgr8888,
            Fourcc::Argb8888,
            Fourcc::Xbgr8888,
            Fourcc::Xrgb8888,
        ],
        renderer_formats.iter().copied(),
        cursor_size,
        Some(gbm),
    )
    .context("DrmCompositor::new")?;

    // Dmabuf feedback for the surfaces this output mostly shows (COMP-02 §2,
    // ADR 0084).
    let feedback = state
        .dmabuf_feedback
        .as_ref()
        .and_then(|default| output_feedback(drm, default, dev, always_copy, &compositor));

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
            device: dev,
            crtc,
            connector: handle,
            output,
            compositor,
            retry_armed: false,
            commit_failures: 0,
            frame_pending: false,
            needs_render: true,
            feedback,
            vrr_config,
            vrr_capable: matches!(vrr_support, VrrSupport::Supported),
            copying: always_copy,
            always_copy,
            copy_format,
            pass,
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

/// What each GPU we composite with can sample, in device order.
fn feedback_gpus(drm: &DrmData) -> Vec<gpu::FeedbackGpu> {
    drm.devices
        .iter()
        .map(|d| gpu::FeedbackGpu {
            render_node: d.render_node,
            texture: d
                .renderer
                .as_ref()
                .unwrap_or(&drm.renderer)
                .egl_context()
                .dmabuf_texture_formats()
                .iter()
                .copied()
                .collect(),
        })
        .collect()
}

/// A `DmabufFeedback` from a tranche plan ([`gpu::plan_feedback`]).
fn build_feedback(
    main_device: libc::dev_t,
    plan: &[gpu::Tranche],
) -> Option<smithay::wayland::dmabuf::DmabufFeedback> {
    use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
    let mut builder = DmabufFeedbackBuilder::new(main_device, std::iter::empty());
    for t in plan {
        builder = builder.add_preference_tranche(
            t.target,
            t.scanout.then_some(TrancheFlags::Scanout),
            t.formats.iter().copied(),
        );
    }
    builder.build().ok()
}

/// The feedback for surfaces whose main output is composited by `drm.devices[device]`.
///
/// One GPU in play: exactly the pre-hybrid shape -- the default feedback, and
/// for scanout a scanout tranche of the plane formats over the GPU's render
/// formats. Several: that GPU's formats first, then the formats every GPU can
/// import ([`gpu::plan_feedback`], ADR 0084), the scanout tranche omitted where
/// frames are copied (a copied frame never scans a client buffer out). A
/// surface is sent the render or the scanout variant per frame, and re-sent
/// when its main output changes GPU.
fn output_feedback(
    drm: &DrmData,
    default: &smithay::wayland::dmabuf::DmabufFeedback,
    device: usize,
    always_copy: bool,
    compositor: &AbyssDrmCompositor,
) -> Option<SurfaceFeedback> {
    use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
    let scanout = (!always_copy)
        .then(|| {
            Some((
                compositor.surface().device_fd().dev_id().ok()?,
                compositor
                    .surface()
                    .plane_info()
                    .formats
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
            ))
        })
        .flatten();
    let gpus = feedback_gpus(drm);
    let home = if always_copy { 0 } else { device };
    let main_device = gpus.get(home)?.render_node;
    if gpus.len() == 1 {
        let scanout = match scanout {
            Some((target, formats)) => {
                let render_formats = drm.renderer.egl_context().dmabuf_render_formats().clone();
                DmabufFeedbackBuilder::new(main_device, render_formats.iter().copied())
                    .add_preference_tranche(target, Some(TrancheFlags::Scanout), formats)
                    .build()
                    .ok()?
            }
            None => default.clone(),
        };
        return Some(SurfaceFeedback {
            render: default.clone(),
            scanout,
        });
    }
    let render = build_feedback(main_device, &gpu::plan_feedback(home, &gpus, None))?;
    let scanout = match scanout {
        Some((target, formats)) => build_feedback(
            main_device,
            &gpu::plan_feedback(home, &gpus, Some((target, &formats))),
        )?,
        None => render.clone(),
    };
    Some(SurfaceFeedback { render, scanout })
}

/// Rebuild every output's feedback against the GPUs now present, after one
/// went away: they must stop advertising a shared-format tranche for it.
fn refresh_feedback(state: &mut AbyssState) {
    let Some(default) = state.dmabuf_feedback.clone() else {
        return;
    };
    let Some(drm) = state.drm.as_mut() else { return };
    let fresh: Vec<_> = drm
        .outputs
        .iter()
        .map(|o| output_feedback(drm, &default, o.device, o.always_copy, &o.compositor))
        .collect();
    for (o, f) in drm.outputs.iter_mut().zip(fresh) {
        o.feedback = f;
    }
}

/// A GPU disappeared (COMP-03 §3, ADR 0084). A secondary takes only its own
/// outputs with it. The primary's fd is dead and nothing can render without it,
/// so every output goes and we land on the headless fallback rather than
/// re-scanning a device that is no longer there.
fn teardown_gpu(state: &mut AbyssState, dev_id: libc::dev_t) {
    let Some(drm) = state.drm.as_mut() else { return };
    let Some(di) = drm.device_index(dev_id) else {
        return;
    };
    let primary = drm.devices[di].primary;
    let mut ids = Vec::new();
    for o in std::mem::take(&mut drm.outputs) {
        if primary || o.device == di {
            ids.push(o.id);
        } else {
            drm.outputs.push(o);
        }
    }
    if primary {
        drm.primary_lost = true;
    } else {
        let dev = drm.devices.remove(di);
        drm.loop_handle.remove(dev.token);
        for o in drm.outputs.iter_mut().filter(|o| o.device > di) {
            o.device -= 1;
        }
        // The removed GPU's parked GL objects (programs, textures) go with it.
        use smithay::backend::renderer::Renderer as _;
        let keep: Vec<_> = std::iter::once(drm.renderer.context_id())
            .chain(
                drm.devices
                    .iter()
                    .filter_map(|d| d.renderer.as_ref().map(|r| r.context_id())),
            )
            .collect();
        state.borders.retain_contexts(&keep);
        state.capture_fx.retain_contexts(&keep);
        state.annotations.retain_contexts(&keep);
        state.region_select.retain_contexts(&keep);
        refresh_feedback(state);
        tracing::warn!(device = %dev.tag, outputs = ids.len(), "secondary GPU removed; its outputs torn down");
    }
    if ids.is_empty() {
        return;
    }
    sync_fallback(state);
    for id in ids {
        crate::outputs::unregister(state, id);
    }
    if primary {
        tracing::warn!("render GPU removed; outputs torn down");
    }
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
fn resolve_render_device(requested: Option<&str>, seat_name: &str, mode: GpuMode) -> Result<PathBuf> {
    let Some(req) = requested else {
        return auto_select_render_device(seat_name, mode);
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

/// The automatic path (COMP-01 §4, ADR 0084): rank every DRM device on the
/// seat, then pick the primary. With several GPUs the one lighting a connected
/// built-in panel wins, then `boot_vga`, then the ranking. The whole ranked list
/// is logged with the reason, as the spec requires — a wrong pick is a black
/// screen, and the log is the only way to see why it happened.
fn auto_select_render_device(seat_name: &str, mode: GpuMode) -> Result<PathBuf> {
    let ranked = gpu::probe_seat(seat_name).context("enumerating GPUs")?;
    for (i, g) in ranked.iter().enumerate() {
        tracing::info!(rank = i + 1, "GPU candidate: {}", g.describe());
    }
    let best = gpu::select_primary(&ranked, mode)
        .map(|i| &ranked[i])
        .ok_or_else(|| {
            anyhow!("no GPU on seat '{seat_name}' exposes a render node; abyss needs one to composite")
        })?;
    if ranked.iter().all(|g| g.connectors_total == 0) {
        anyhow::bail!("no GPU on seat '{seat_name}' has any connector");
    }
    tracing::info!(
        path = %best.card.display(),
        driver = %best.driver,
        internal_panel = best.has_internal_panel(),
        boot_vga = best.boot_vga,
        gpu_mode = mode.name(),
        "selected primary GPU: best of {} (dedicated mode: the discrete GPU; else connected built-in panel, then boot_vga, then class/VRAM/PCI-address)",
        ranked.len()
    );
    Ok(best.card.clone())
}

/// Everything opened for one DRM device.
struct OpenedDevice {
    node: DrmNode,
    drm: DrmDevice,
    notifier: DrmDeviceNotifier,
    gbm: GbmDevice<DrmDeviceFd>,
    /// Shared with the DRM and GBM devices; the primary also needs it for syncobj.
    fd: DrmDeviceFd,
}

/// Open a card node through the session, and the DRM and GBM devices on it.
fn open_device(session: &mut LibSeatSession, path: &Path) -> Result<OpenedDevice> {
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|err| anyhow!("opening {}: {err}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let node = DrmNode::from_file(&fd).context("resolving DRM node")?;
    let (drm, notifier) = DrmDevice::new(fd.clone(), true).context("DrmDevice::new")?;
    let gbm = GbmDevice::new(fd.clone()).context("GbmDevice::new")?;
    Ok(OpenedDevice {
        node,
        drm,
        notifier,
        gbm,
        fd,
    })
}

/// EGL and GLES on a GBM device.
fn gles_on(gbm: &GbmDevice<DrmDeviceFd>) -> Result<GlesRenderer> {
    // SAFETY: the gbm device outlives the display (both live in DrmData /
    // the caller's scope for the lifetime of the process).
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }.context("EGLDisplay::new")?;
    let egl_context = EGLContext::new(&egl_display).context("EGLContext::new")?;
    // SAFETY: the context is current on this (single) thread only.
    unsafe { GlesRenderer::new(egl_context) }.context("GlesRenderer::new")
}

/// Bring up one secondary GPU: device, GBM, a blit renderer. Any failure is the
/// caller's to log; a secondary never stops the primary starting (ADR 0084).
fn open_secondary(session: &mut LibSeatSession, path: &Path) -> Result<(OpenedDevice, GlesRenderer)> {
    let opened = open_device(session, path)?;
    check_nvidia_modeset(&opened.node)?;
    let renderer = gles_on(&opened.gbm)?;
    Ok((opened, renderer))
}

/// Route one device's DRM events (page flips) into the loop; the device is
/// named by id rather than index, because indices shift when a GPU goes away.
fn insert_drm_source(
    handle: &LoopHandle<'static, AbyssState>,
    notifier: DrmDeviceNotifier,
    dev_id: libc::dev_t,
) -> Result<RegistrationToken> {
    handle
        .insert_source(notifier, move |event, meta, state| match event {
            DrmEvent::VBlank(crtc) => on_vblank(state, dev_id, crtc, meta.as_ref()),
            DrmEvent::Error(err) => tracing::error!(?err, "DRM event error"),
        })
        .map_err(|err| anyhow!("inserting drm source: {err}"))
}

/// A page flip completed on `crtc` of device `dev_id`.
fn on_vblank(
    state: &mut AbyssState,
    dev_id: libc::dev_t,
    crtc: crtc::Handle,
    meta: Option<&DrmEventMetadata>,
) {
    let clock_now = state.clock.now();
    let pending = if let Some(drm) = state.drm.as_mut() {
        match drm.index_of(dev_id, crtc) {
            Some(i) => {
                let o = &mut drm.outputs[i];
                let refresh = o
                    .output
                    .current_mode()
                    .map(|m| frame_period(m.refresh))
                    .unwrap_or_else(|| Duration::from_millis(16));
                match o.compositor.frame_submitted() {
                    // wp_presentation: report the real page-flip
                    // timestamp and sequence the kernel gave us.
                    Ok(Some(Some(mut feedback))) => {
                        let (time, seq) = match meta {
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
                // The flip is done, so the secondary GPU has finished reading
                // the transfer buffer it was fed from.
                if let Some(pass) = o.pass.as_mut() {
                    pass.held = None;
                }
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
    let gpu_mode = config.misc.gpu_mode;
    let secondary_compositing = config.misc.secondary_compositing;
    tracing::info!(
        gpu_mode = gpu_mode.name(),
        "GPU mode (misc.gpu-mode, restart to change)"
    );
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
    crate::trusted_ui::socket::start(&mut state, &handle);
    // COMP-10 §2: every prompt shows the personal secret, once one is set.
    state.trusted_ui.phrase = crate::trusted_ui::phrase::load();
    crate::config::watch::start(&mut state, &handle);
    crate::addons::start(&mut state, &handle);
    crate::xwayland::start(&mut state);

    // --- GPU discovery -----------------------------------------------------
    let udev = UdevBackend::new(&seat_name).context("udev backend")?;
    let gpu_path: PathBuf = resolve_render_device(requested_device.as_deref(), &seat_name, gpu_mode)?;

    let mut session_for_open = session.clone();
    let OpenedDevice {
        node,
        drm: drm_device,
        notifier: drm_notifier,
        gbm,
        fd: device_fd_for_syncobj,
    } = open_device(&mut session_for_open, &gpu_path)?;
    check_nvidia_modeset(&node)?;
    let our_device = node.dev_id();
    let renderer = gles_on(&gbm)?;

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
    // smithay 0.7.0's probe is `match eventfd(..) { Ok(_) => unreachable!(), .. }`
    // (drm_syncobj/mod.rs:73): a driver that accepts the deliberately bogus
    // handle panics the compositor at startup with no diagnostic. Treat a panic
    // there as "no explicit sync" instead.
    let syncobj_supported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        supports_syncobj_eventfd(&device_fd_for_syncobj)
    }))
    .unwrap_or_else(|_| {
        tracing::warn!("syncobj eventfd probe panicked; disabling explicit sync");
        false
    });
    if syncobj_supported {
        state.syncobj_state = Some(DrmSyncobjState::new::<AbyssState>(
            &state.display_handle,
            device_fd_for_syncobj.clone(),
        ));
        tracing::info!("explicit sync (wp_linux_drm_syncobj_v1) enabled");
    } else {
        tracing::warn!("driver has no syncobj eventfd support; no explicit sync global");
    }

    let primary_token = insert_drm_source(&handle, drm_notifier, our_device)?;
    let mut devices = vec![GpuDevice {
        dev_id: our_device,
        primary: true,
        tag: format!("card{}", node.minor()),
        render_node,
        drm: drm_device,
        gbm: gbm.clone(),
        renderer: None,
        token: primary_token,
    }];

    // Secondary GPUs (ADR 0084): every other device on the seat with a
    // connector. Each is optional; one that will not come up is logged and left
    // dark, and the primary carries on.
    let primary_id = node
        .node_with_type(NodeType::Primary)
        .and_then(|r| r.ok())
        .unwrap_or(node)
        .dev_id();
    match gpu::probe_seat(&seat_name) {
        Ok(ranked) => {
            for g in ranked.iter() {
                let Ok(other) = DrmNode::from_path(&g.card) else {
                    continue;
                };
                if other.dev_id() == primary_id || other.dev_id() == our_device {
                    // `dedicated`: everything the compositor starts runs on the
                    // discrete GPU (the launcher and bar do the same through
                    // ec-services) -- but only when it is a second GPU to
                    // offload to; a lone discrete GPU is already the default.
                    if gpu_mode == GpuMode::Dedicated && g.class == gpu::Class::Discrete && !g.boot_vga {
                        state.dedicated_env = gpu::offload_env(g);
                    }
                    continue;
                }
                if !gpu::opens_as_secondary(g, gpu_mode) {
                    if g.connectors_total > 0 {
                        tracing::info!(
                            path = %g.card.display(),
                            "GPU left closed by integrated mode; its outputs stay dark"
                        );
                    }
                    continue;
                }
                match open_secondary(&mut session_for_open, &g.card) {
                    Ok((opened, sec_renderer)) => {
                        let dev_id = opened.node.dev_id();
                        match insert_drm_source(&handle, opened.notifier, dev_id) {
                            Ok(token) => {
                                tracing::info!(
                                    path = %g.card.display(),
                                    driver = %g.driver,
                                    "secondary GPU: {}",
                                    g.describe()
                                );
                                devices.push(GpuDevice {
                                    dev_id,
                                    primary: false,
                                    tag: format!("card{}", opened.node.minor()),
                                    render_node: opened
                                        .node
                                        .node_with_type(NodeType::Render)
                                        .and_then(|r| r.ok())
                                        .unwrap_or(opened.node)
                                        .dev_id(),
                                    drm: opened.drm,
                                    gbm: opened.gbm,
                                    renderer: Some(sec_renderer),
                                    token,
                                });
                            }
                            Err(err) => {
                                tracing::warn!(path = %g.card.display(), ?err, "secondary GPU skipped");
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(path = %g.card.display(), ?err, "secondary GPU skipped");
                    }
                }
            }
        }
        Err(err) => tracing::warn!(?err, "enumerating secondary GPUs"),
    }

    state.drm = Some(Box::new(DrmData {
        session: session.clone(),
        loop_handle: handle.clone(),
        devices,
        gbm,
        renderer,
        gpu_mode,
        secondary_compositing,
        primary_lost: false,
        outputs: Vec::new(),
        fallback: None,
        cursor: crate::render::cursor::Fallback::default(),
        input_devices: Vec::new(),
        shed: Default::default(),
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
    // libseat revokes the evdev fds on a VT switch away; the session handler
    // below suspends and resumes this context so the devices come back.
    let mut libinput_session = libinput.clone();
    handle
        .insert_source(LibinputInputBackend::new(libinput), |event, _, state| {
            // Devices are configured as they appear, and remembered so a config
            // reload can reach the ones already open (COMP-13 §1.2).
            match &event {
                InputEvent::DeviceAdded { device } => {
                    let mut device = device.clone();
                    configure_input_device(&mut device, &state.config.input);
                    // Advertise a tablet on the seat as it appears (COMP-06
                    // §1), so a client learns of it before the first stroke.
                    // Fully qualified: libinput's inherent `has_capability`
                    // takes its own enum and would shadow the trait method.
                    if smithay::backend::input::Device::has_capability(
                        &device,
                        smithay::backend::input::DeviceCapability::TabletTool,
                    ) {
                        let dh = state.display_handle.clone();
                        state
                            .seat
                            .tablet_seat()
                            .add_tablet::<AbyssState>(&dh, &TabletDescriptor::from(&device));
                    }
                    if let Some(drm) = state.drm.as_mut() {
                        drm.input_devices.push(device);
                    }
                }
                InputEvent::DeviceRemoved { device } => {
                    if smithay::backend::input::Device::has_capability(
                        device,
                        smithay::backend::input::DeviceCapability::TabletTool,
                    ) {
                        state
                            .seat
                            .tablet_seat()
                            .remove_tablet(&TabletDescriptor::from(device));
                        state.tablet_in_use = None;
                    }
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
                libinput_session.suspend();
                if let Some(drm) = state.drm.as_mut() {
                    // Every device loses master together (ADR 0084).
                    for d in drm.devices.iter_mut() {
                        d.drm.pause();
                    }
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session activated");
                if libinput_session.resume().is_err() {
                    tracing::error!("resuming libinput after VT switch");
                }
                if let Some(drm) = state.drm.as_mut() {
                    for d in drm.devices.iter_mut() {
                        if let Err(err) = d.drm.activate(false) {
                            tracing::error!(?err, device = %d.tag, "reactivating DRM device");
                        }
                    }
                    for o in drm.outputs.iter_mut() {
                        if let Err(err) = o.compositor.reset_state() {
                            tracing::error!(?err, "resetting compositor state");
                        }
                        o.retry_armed = false;
                        o.commit_failures = 0;
                        o.frame_pending = false;
                        o.needs_render = true;
                        if let Some(pass) = o.pass.as_mut() {
                            pass.force_full = true;
                            pass.held = None;
                        }
                    }
                }
                // A connector may have changed while we were away.
                scan_connectors(state);
                crate::outputs::ensure_one_enabled(state);
                render(state);
            }
        })
        .map_err(|err| anyhow!("inserting session source: {err}"))?;

    handle
        .insert_source(udev, move |event, _, state| {
            // Every known GPU is handled by id; an unknown one that shows up
            // at runtime is ignored (ADR 0084: no runtime GPU hotplug).
            let known =
                |state: &AbyssState, id: libc::dev_t| state.drm.as_ref().and_then(|d| d.device_index(id));
            let rescan = match event {
                UdevEvent::Added { device_id, path } => {
                    tracing::info!(?device_id, path = %path.display(), "GPU added");
                    let dev = known(state, device_id);
                    if dev.is_none() {
                        tracing::info!(
                            ?device_id,
                            "GPU not in use at startup; ignoring (no runtime GPU hotplug)"
                        );
                    }
                    dev
                }
                UdevEvent::Changed { device_id } => known(state, device_id),
                UdevEvent::Removed { device_id } => {
                    tracing::info!(?device_id, "GPU removed");
                    // The fd is dead; re-scanning it would only log errors.
                    // Tear its outputs down (all of them, if it was the
                    // primary, falling back to headless).
                    teardown_gpu(state, device_id);
                    None
                }
            };
            if let Some(dev) = rescan {
                scan_device(state, dev);
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
        crate::protocols::agent::flush(state);
    })?;
    crate::ipc::cleanup(&state);
    crate::trusted_ui::socket::cleanup(&state);
    crate::protocols::agent::cleanup(&state);
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
    let prompt = crate::trusted_ui::holds_seat(state);
    crate::render::capture::service(state, &mut drm.renderer, prompt);
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

/// How long to wait before re-rendering a frame that could not be queued: about
/// one frame at 60 Hz.
///
/// Deliberately not backed off. A refused commit usually clears on its own —
/// the next frame carries different damage and goes through — so the retry is
/// what gets the screen updating again, and stretching it stretches the stall
/// (it was tried, with a one second cap, and made the lag worse). What a run of
/// failures must not do is flood the log; see [`log_failure_now`].
const RETRY_DELAY: Duration = Duration::from_millis(16);

/// Whether the `failures`-th consecutive failed frame should be logged: the
/// first, then each time the run length reaches a power of two (1, 2, 4, 8 …).
/// A commit the kernel keeps refusing would otherwise log every retry.
fn log_failure_now(failures: u32) -> bool {
    failures.is_power_of_two()
}

/// Whether the frame after this one has to redraw the whole output instead of
/// only what changed. True for a discarded empty frame and for a refused one;
/// the call site says why each needs it.
fn needs_full_redraw(empty: bool, failed: bool) -> bool {
    empty || failed
}

/// Consecutive refused frames after which an output stops using KMS planes for
/// its next frame.
const PLANE_FALLBACK_AFTER: u32 = 3;

/// The [`FrameFlags`] for a frame, given how many frames in a row were refused.
/// A commit that keeps failing despite the full redraw is being poisoned by a
/// plane-assigned element the compositor cannot see inside, so composite
/// everything onto the primary plane instead: its damage is the output's own,
/// built from rectangles the damage tracker has already clamped.
fn frame_flags_for(failures: u32, base: FrameFlags) -> FrameFlags {
    if failures >= PLANE_FALLBACK_AFTER {
        FrameFlags::empty()
    } else {
        base
    }
}

/// One line per element: id, geometry, source and how many damage rectangles it
/// reports. Only built for a frame that follows a refusal.
fn describe_elements(elements: &[crate::render::overscan::OutputElement], scale: Scale<f64>) -> String {
    use smithay::backend::renderer::element::Element;
    elements
        .iter()
        .map(|e| {
            format!(
                "{:?} geo={:?} src={:?} damage={}",
                e.id(),
                e.geometry(scale),
                e.src(),
                e.damage_since(scale, None).len(),
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Composite and page-flip one output. A no-op when the session is inactive.
fn render_output(state: &mut AbyssState, index: usize) {
    // Every frame path (VBlank, schedule, retry) lands here, not in `render`.
    crate::shell::tick_retile_hold(state);
    // Commit slots follow their window this frame, not 50 ms later.
    crate::trusted_ui::commit::tick(state);
    crate::trusted_ui::indicator::tick(state);
    let capture_active = state.capture_active();
    let prompt = crate::trusted_ui::holds_seat(state);
    // Over a slot card or an indicator too, the compositor's arrow replaces
    // the client cursor.
    let arrow = prompt
        || crate::trusted_ui::pointer_on_card(state)
        || crate::trusted_ui::indicator::at(state, state.pointer_location);
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
    // A fullscreen toplevel drops the `Top` layer below the window stack.
    let fullscreen = match state.outputs.get(out_id).map(|e| e.output.clone()) {
        Some(out) => crate::shell::output_has_fullscreen(state, &out),
        None => false,
    };
    let Some(drm) = state.drm.as_mut() else { return };
    let Some(entry) = drm.outputs.get(index) else {
        return;
    };
    let output = entry.output.clone();
    let crtc = entry.crtc;
    let dev_id = drm.devices[entry.device].dev_id;
    let (home, always_copy, can_fall_back) = (entry.device, entry.always_copy, entry.copy_format.is_some());
    // ADR 0084: an output composites on its own GPU. Only when a visible client
    // buffer cannot be imported there does this frame go down the copy path (the
    // primary renders, the output's GPU blits); the next frame tries natively
    // again. `dedicated` mode copies every frame of a secondary's output.
    let copy = if always_copy {
        true
    } else if home != 0 && can_fall_back && !state.lock.locked {
        drm.devices[home]
            .renderer
            .as_mut()
            .is_some_and(|r| crate::render::surfaces_missing_texture(&state.space, &output, r))
    } else {
        false
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let output_loc = state
        .space
        .output_geometry(&output)
        .map(|g| g.loc)
        .unwrap_or_default();

    // The pointer lives in the global space; elements are output-local.
    let cursor_pos = state.pointer_location - output_loc.to_f64();
    // A trusted prompt (COMP-10) is front-most, with the compositor's own
    // arrow above it: the client cursor surface is not drawn while a prompt
    // holds the seat, so nothing a client chose sits on top of the prompt.
    let mut elements: Vec<AbyssRenderElement> = Vec::new();
    // Every element is built with the renderer that will draw it: the output's
    // own GPU, or the primary on a copy frame. A buffer imported on one GPU is
    // no use on another, and the stores' GPU objects follow the renderer.
    let DrmData {
        renderer: primary_renderer,
        devices,
        cursor: drm_cursor,
        ..
    } = &mut **drm;
    let renderer: &mut GlesRenderer = if copy || home == 0 {
        primary_renderer
    } else {
        match devices[home].renderer.as_mut() {
            Some(r) => r,
            None => primary_renderer,
        }
    };
    if arrow {
        elements.extend(crate::render::cursor::elements(
            renderer,
            &smithay::input::pointer::CursorImageStatus::default_named(),
            drm_cursor,
            cursor_pos,
            scale,
        ));
    }
    elements.extend(crate::trusted_ui::elements(
        renderer,
        &mut state.trusted_ui,
        state.lock.locked,
        &output,
        output_loc,
    ));
    // Trusted UI, above the cursor and never drawn into a capture target.
    elements.extend(crate::render::capture::indicator(&output, capture_active));
    // The region selector (COMP-18 §1.3): untrusted too, but above the
    // annotation pass it is about to feed.
    elements.extend(crate::render::select::selector_elements(
        renderer,
        &mut state.region_select,
        &output,
        output_loc,
        &state.config,
    ));
    // Annotations (COMP-18): untrusted, so below the indicator -- the list is
    // front-to-back -- but above the cursor and above everything client-drawn.
    // They go in here, at `annotations_at`, once everything below them has
    // been collected: their glass panels blur what is behind them.
    let annotations_at = elements.len();
    if !arrow {
        elements.extend(crate::render::cursor::elements(
            renderer,
            &state.cursor_status,
            drm_cursor,
            cursor_pos,
            scale,
        ));
    }
    // Radiant drop guides (COMP-05 §3.1): above the windows, below the
    // annotations and the cursor.
    elements.extend(crate::render::drop::drop_elements(
        state.tile_drag.as_ref(),
        state.config.general.drop_guides,
        state.config.general.drop_guide_color,
        &output,
        output_loc,
    ));
    if state.lock.locked {
        elements.extend(crate::protocols::standard::session_lock::lock_elements(
            renderer,
            &mut state.lock,
            &output,
        ));
    } else {
        elements.extend(collect_elements(
            renderer,
            &state.space,
            &mut state.borders,
            &output,
            &state.config,
            state.focus.as_ref(),
            state.input_method_popup.as_ref(),
            fullscreen,
        ));
    }
    let behind = elements.split_off(annotations_at);
    elements.extend(crate::render::annotation::annotation_elements(
        renderer,
        &mut state.annotations,
        &output,
        output_loc,
        &state.config,
        Some(&behind),
    ));
    elements.extend(behind);
    // Overscan compensation (COMP-03 §2): scale the finished scene into the
    // inset rect and leave the margins black. The markers go on afterwards so
    // they stay in raw framebuffer pixels — they exist to show where the
    // desktop's edge landed, so scaling them with it would defeat the point.
    let overscan = crate::outputs::overscan_of(state, out_id);
    let mode_size = crate::outputs::mode_size(&output);
    let calibrating = state
        .outputs
        .get(out_id)
        .map(|e| e.calibrating.is_some())
        .unwrap_or(false);
    let mut elements = crate::render::overscan::frame(elements, overscan, mode_size, scale);
    if calibrating {
        crate::render::overscan::calibration_markers(overscan, mode_size, &mut elements);
    }
    let Some(drm) = state.drm.as_mut() else { return };

    // Annotation and selector fades schedule frames only while in flight.
    // A held retile keeps frames coming so the tick sees its ghost finish.
    let animating = state.borders.anim.running()
        || state.annotations.animating()
        || state.region_select.animating()
        || matches!(state.retile_hold, crate::shell::RetileHold::Held { .. });

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
    // An inset output composites every frame by definition — a scanned-out
    // client buffer would land at the panel's edge, outside the inset rect.
    let base_flags = if state.config.render.direct_scanout && !redact && overscan.is_zero() {
        FrameFlags::DEFAULT
    } else {
        FrameFlags::empty()
    };
    let prior_failures = drm.outputs[index].commit_failures;
    let flags = frame_flags_for(prior_failures, base_flags);
    // A retry after a refusal is the moment worth describing: the elements are
    // still in hand, and one line names whichever fed the kernel a bad rectangle.
    let diagnostic = log_failure_now(prior_failures.saturating_add(1))
        .then_some(prior_failures)
        .filter(|failures| *failures > 0)
        .map(|_| describe_elements(&elements, scale));

    // Moving between native and copy composition changes what the output's
    // compositor is fed (the output's own scene, or one finished texture).
    if copy != drm.outputs[index].copying {
        set_route(drm, index, copy);
    }
    let flags = if copy { FrameFlags::empty() } else { flags };

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
    if !copy {
        // The output's own GPU composites it and plans its own planes.
        let DrmData {
            renderer: primary_renderer,
            devices,
            outputs,
            ..
        } = &mut **drm;
        let renderer: &mut GlesRenderer = if home == 0 {
            primary_renderer
        } else {
            match devices[home].renderer.as_mut() {
                Some(r) => r,
                None => primary_renderer,
            }
        };
        match outputs[index]
            .compositor
            .render_frame(renderer, &elements, CLEAR, flags)
        {
            Ok(result) if result.is_empty => empty = true,
            Ok(result) => states = Some(result.states.clone()),
            Err(err) => {
                tracing::warn!(?err, "rendering frame");
                failed = true;
            }
        }
    } else {
        // ADR 0084: the primary rendered the elements; the output's GPU blits.
        let secondary = drm.devices.get_mut(home).and_then(|d| d.renderer.as_mut());
        match secondary {
            Some(secondary) => {
                match render_secondary(&mut drm.renderer, secondary, &mut drm.outputs[index], &elements) {
                    Ok(SecondaryFrame::Skipped) => {}
                    Ok(SecondaryFrame::Empty) => empty = true,
                    Ok(SecondaryFrame::Frame(s)) => states = Some(s),
                    Err(err) => {
                        tracing::warn!(?err, "rendering secondary frame");
                        failed = true;
                    }
                }
            }
            None => failed = true,
        }
    }
    let render_time = render_start.elapsed();
    drop(elements);
    let surface_feedback = drm.outputs[index].feedback.as_ref().map(|f| SurfaceFeedback {
        render: f.render.clone(),
        // A copied frame never scans a client buffer out (ADR 0084).
        scanout: if copy { f.render.clone() } else { f.scanout.clone() },
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
                let failures = drm.outputs[index].commit_failures.saturating_add(1);
                if log_failure_now(failures) {
                    tracing::warn!(?err, failures, elements = diagnostic.as_deref(), "queueing frame");
                }
                failed = true;
            }
        }
        let frame_time = render_time + submit_start.elapsed();
        state.stats.record(render_time, submit_start.elapsed());
        // COMP-14 §6: a sustained run of missed frames sheds animations a
        // step, a longer run of headroom restores one.
        if let Some(drm) = state.drm.as_mut() {
            let period = frame_period(output.current_mode().map_or(0, |m| m.refresh));
            // A transition shader drawn in a run of missed frames is switched off.
            state.borders.anim.shaders.frame_result(frame_time > period);
            if let Some(level) = drm.shed.observe(frame_time, period) {
                tracing::info!(
                    ?level,
                    frame_us = frame_time.as_micros() as u64,
                    "animation shedding"
                );
                state.borders.anim.set_shed(level);
            }
        }
        state.stats.maybe_report();
    }

    let Some(drm) = state.drm.as_mut() else { return };
    if needs_full_redraw(empty, failed) {
        if let Some(pass) = drm.outputs[index].pass.as_mut() {
            // The transfer buffers and the secondary compositor's history now
            // disagree; redraw and re-copy everything next time.
            pass.force_full = true;
        }
        let compositor = &mut drm.outputs[index].compositor;
        // An empty frame is discarded rather than submitted, but the damage
        // tracker still recorded it. That desynchronises the damage history
        // from the swapchain slot ages, and every later frame then restores
        // the wrong regions (stale content, cursor trails). Clearing the ages
        // forces the next frame to be a full redraw, which resyncs both.
        //
        // A refused frame needs the same redraw, for a different reason. The
        // `FB_DAMAGE_CLIPS` blob lives in the plane's pending config, and a
        // frame with no fresh damage reuses the previous config wholesale. So
        // a blob the kernel rejects — one degenerate rectangle is enough, and
        // it rejects the whole commit — is latched: the retry re-renders,
        // finds nothing newly damaged, resubmits the same blob and is refused
        // again, at [`RETRY_DELAY`], for as long as the screen holds still.
        // Measured on eDP-1: 448 consecutive refusals over eight seconds, one
        // `invalid damage clip 40 298 2840 298` every time. A full redraw
        // forces a fresh blob, so a bad one costs a frame instead of the
        // display (COMP-02 §4).
        compositor.reset_buffer_ages();
    }

    let entry = &mut drm.outputs[index];
    // A running animation asks for the next frame here: leaving `needs_render`
    // set makes this VBlank's completion schedule another render, and it
    // clears itself the frame the last move finishes (COMP-02 §9).
    entry.needs_render = animating;
    entry.frame_pending = queued;
    if queued {
        entry.commit_failures = 0;
    } else if failed {
        entry.commit_failures = entry.commit_failures.saturating_add(1);
    }
    let arm = !queued && !entry.retry_armed && failed;
    if arm {
        // The frame had damage but could not be queued: retry within a frame
        // ([`RETRY_DELAY`], not backed off). Never re-arm for an empty frame —
        // an idle render_frame spin pushes empty entries into the damage
        // tracker's history while the swapchain slot ages stand still, which
        // desynchronises the two and leaves stale content on screen.
        entry.retry_armed = true;
        let handle = drm.loop_handle.clone();
        if let Err(err) = handle.insert_source(Timer::from_duration(RETRY_DELAY), move |_, _, state| {
            let i = state.drm.as_mut().and_then(|drm| {
                let i = drm.index_of(dev_id, crtc)?;
                drm.outputs[i].retry_armed = false;
                Some(i)
            });
            if let Some(i) = i {
                render_output(state, i);
            }
            TimeoutAction::Drop
        }) {
            tracing::warn!(?err, "arming re-render timer");
            if let Some(drm) = state.drm.as_mut() {
                if let Some(i) = drm.index_of(dev_id, crtc) {
                    drm.outputs[i].retry_armed = false;
                }
            }
        }
    }

    let time = state.start_time.elapsed();
    crate::render::send_frames(&state.space, &output, time);
    state.space.refresh();
    state.popups.cleanup();
    // Deliberately no `flush_clients()` here. `render_output` is reachable
    // from inside a client request: a destructor request such as
    // `xdg_toplevel.destroy` runs `toplevel_destroyed` -> `unmap_window` ->
    // `arrange` -> `damage_all` -> `schedule_render` -> here, all while
    // libwayland is still dispatching that request. `wl_display_flush_clients`
    // destroys any client whose socket write fails, and a client that closed
    // its socket right after sending `destroy` does fail. That frees the very
    // `wl_resource` libwayland is about to destroy itself when the request
    // returns, and the second destructor runs on a freed `ResourceUserData`.
    // The event loop flushes after every dispatch round, which is both safe
    // and soon enough; see the `run` callback above.
}

/// ADR 0084: switch output `index` between composing natively on its own GPU
/// and the copy path. The output's compositor is told what it is being fed (its
/// own scene, or one finished texture); changing that restarts its damage
/// tracking, so the first frame after a switch is a full one.
fn set_route(drm: &mut DrmData, index: usize, copy: bool) {
    let out = &mut drm.outputs[index];
    let size = crate::outputs::mode_size(&out.output);
    if copy {
        if out.pass.is_none() {
            let Some((fourcc, modifiers)) = out.copy_format.clone() else {
                return;
            };
            out.pass = Some(SecondaryPass::new(&drm.gbm, fourcc, modifiers, size, &out.output));
        }
        if let Some(pass) = out.pass.as_mut() {
            pass.force_full = true;
            pass.held = None;
            pass.fit(size, &mut out.compositor);
        }
        out.compositor.set_output_mode_source(static_source(size));
    } else {
        out.compositor
            .set_output_mode_source(OutputModeSource::from(&out.output));
    }
    out.compositor.reset_buffer_ages();
    out.copying = copy;
    tracing::debug!(
        output = %out.output.name(),
        copy,
        "output composition route changed (copy = rendered on the primary GPU, blitted by the output's own)"
    );
}

/// What a secondary-output frame came to.
enum SecondaryFrame {
    /// The primary pass had no damage; nothing to copy or queue.
    Skipped,
    /// The secondary compositor discarded the frame as empty.
    Empty,
    /// A frame is ready to queue; the states are the primary pass's, which are
    /// what presentation feedback and frame callbacks are derived from.
    Frame(RenderElementStates),
}

/// ADR 0084 "render then copy" for one secondary output. The primary renders
/// `elements` into an offscreen dmabuf; the secondary imports that dmabuf and
/// its own [`DrmCompositor`] draws it as one full-output texture carrying the
/// primary pass's damage. The caller queues the frame.
///
/// Sync: the primary's render fence is waited on before the secondary touches
/// the buffer (a blocking wait, but the frame was just submitted and this is
/// one GPU hop; explicit cross-device fences are future work). Going the other
/// way, the transfer buffer is held until the output's VBlank so the
/// primary does not overwrite it under the secondary's blit.
fn render_secondary(
    primary: &mut GlesRenderer,
    secondary: &mut GlesRenderer,
    out: &mut DrmOutput,
    elements: &[crate::render::overscan::OutputElement],
) -> Result<SecondaryFrame> {
    let size = crate::outputs::mode_size(&out.output);
    let pass = out
        .pass
        .as_mut()
        .ok_or_else(|| anyhow!("not a secondary output"))?;
    pass.fit(size, &mut out.compositor);

    let slot = pass
        .swapchain
        .acquire()
        .map_err(|err| anyhow!("allocating a transfer buffer: {err}"))?
        .ok_or_else(|| anyhow!("transfer swapchain exhausted"))?;
    let age = if pass.force_full { 0 } else { slot.age() as usize };
    // `Slot::export` keeps the dmabuf in the slot, so this is the same
    // buffer (and the same EGL image) every time the slot comes round.
    let dmabuf = slot
        .export()
        .map_err(|err| anyhow!("exporting the transfer buffer: {err}"))?;

    // A clone shares the underlying buffer; `bind` wants it by `&mut`.
    let mut bind_to = dmabuf.clone();
    let mut target = primary
        .bind(&mut bind_to)
        .map_err(|err| anyhow!("binding the transfer buffer: {err}"))?;
    let result = pass
        .tracker
        .render_output(primary, &mut target, age, elements, CLEAR)
        .map_err(|err| anyhow!("primary pass: {err:?}"))?;
    let sync = result.sync.clone();
    let states = result.states.clone();
    // The pass reports damage accumulated over the slot's age, a superset of
    // what changed since the last frame the secondary saw, which is safe.
    let damage: Vec<Rectangle<i32, BufferCoord>> = result
        .damage
        .map(|d| {
            d.iter()
                .map(|r| Rectangle::new((r.loc.x, r.loc.y).into(), (r.size.w, r.size.h).into()))
                .collect()
        })
        .unwrap_or_default();
    pass.swapchain.submitted(&slot);
    if damage.is_empty() {
        pass.force_full = false;
        return Ok(SecondaryFrame::Skipped);
    }
    pass.damage.add(damage);

    // The secondary-side texture: imported once per slot.
    slot.userdata()
        .insert_if_missing(|| SlotTexture(RefCell::new(None)));
    let cached = slot
        .userdata()
        .get::<SlotTexture>()
        .and_then(|c| c.0.borrow().clone());
    let mut texture = None;
    if !pass.cpu_copy {
        texture = match cached {
            Some(t) => Some(t),
            None => match secondary.import_dmabuf(&dmabuf, None) {
                Ok(t) => {
                    if let Some(c) = slot.userdata().get::<SlotTexture>() {
                        *c.0.borrow_mut() = Some(t.clone());
                    }
                    Some(t)
                }
                Err(err) => {
                    tracing::warn!(
                        output = %out.output.name(),
                        ?err,
                        "secondary GPU cannot import the transfer buffer; copying through system memory"
                    );
                    pass.cpu_copy = true;
                    None
                }
            },
        };
    }
    let texture = match texture {
        Some(t) => {
            if sync.wait().is_err() {
                tracing::debug!("waiting for the primary pass was interrupted");
            }
            t
        }
        None => {
            // glReadPixels is synchronous, so no fence wait is needed here.
            let region = Rectangle::from_size((size.w, size.h).into());
            let mapping = primary
                .copy_framebuffer(&target, region, Fourcc::Abgr8888)
                .map_err(|err| anyhow!("reading back the transfer buffer: {err}"))?;
            let flipped = mapping.flipped();
            let bytes = primary
                .map_texture(&mapping)
                .map_err(|err| anyhow!("mapping the read-back: {err}"))?;
            secondary
                .import_memory(bytes, Fourcc::Abgr8888, (size.w, size.h).into(), flipped)
                .map_err(|err| anyhow!("uploading to the secondary GPU: {err}"))?
        }
    };
    drop(target);

    let full = Rectangle::from_size(texture.size());
    let element = TextureRenderElement::from_texture_with_damage(
        pass.element_id.clone(),
        secondary.context_id(),
        (0.0, 0.0),
        texture,
        1,
        Transform::Normal,
        None,
        None,
        None,
        // The pass clears to opaque black, so the whole frame is opaque.
        Some(vec![full]),
        pass.damage.snapshot(),
        Kind::Unspecified,
    );
    // No direct scanout on a secondary output (ADR 0084): the one element is
    // always composited into the compositor's own buffer.
    let elements = [element];
    let outcome = match out
        .compositor
        .render_frame(secondary, &elements, CLEAR, FrameFlags::empty())
    {
        Ok(r) if r.is_empty => Ok(SecondaryFrame::Empty),
        Ok(_) => Ok(SecondaryFrame::Frame(states)),
        Err(err) => Err(anyhow!("secondary compositor: {err:?}")),
    };
    // Only a queued frame is read at a VBlank; an empty or failed one never
    // gets one, so holding its slot would starve the swapchain.
    if matches!(outcome, Ok(SecondaryFrame::Frame(_))) {
        pass.held = Some(slot);
    }
    pass.force_full = false;
    outcome
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

/// Put one output's scanout surface on a new mode. `false` means nothing
/// changed: the connector advertises no matching mode, or the surface refused
/// the switch. The `Output` must not be updated when this returns `false`.
pub fn set_mode(state: &mut AbyssState, id: u64, mode: OutputMode) -> bool {
    let Some(drm) = state.drm.as_mut() else {
        return true;
    };
    let Some(index) = drm.outputs.iter().position(|o| o.id == id) else {
        // Not a DRM output (the headless fallback); nothing to reprogram.
        return true;
    };
    let connector = drm.outputs[index].connector;
    let info = match drm.devices[drm.outputs[index].device]
        .drm
        .get_connector(connector, false)
    {
        Ok(info) => info,
        Err(e) => {
            tracing::warn!(id, error = %e, "reading connector modes");
            return false;
        }
    };
    let matches_size = |m: &DrmMode| m.size().0 as i32 == mode.size.w && m.size().1 as i32 == mode.size.h;
    let Some(found) = info
        .modes()
        .iter()
        .find(|m| matches_size(m) && refresh_mhz(m) == mode.refresh)
        .or_else(|| info.modes().iter().find(|m| matches_size(m)))
        .copied()
    else {
        tracing::warn!(id, w = mode.size.w, h = mode.size.h, "connector has no such mode");
        return false;
    };
    if let Err(e) = drm.outputs[index].compositor.use_mode(found) {
        tracing::warn!(id, error = %e, "surface refused the mode switch");
        return false;
    }
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
    let info = drm.devices[output.device].drm.get_crtc(output.crtc).ok()?;
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
    match drm.devices[output.device].drm.set_gamma(output.crtc, r, g, b) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 0033, kept fatal by ADR 0064: an explicit render device that does
    /// not resolve refuses, whether it came from the config, `--render-device`
    /// or `ECLIPSE_RENDER_DEVICE` (all three land in `misc.render_device`).
    #[test]
    fn an_unresolvable_render_device_refuses() {
        assert!(resolve_render_device(Some("/nonexistent/dri/card9"), "seat0", GpuMode::Hybrid).is_err());
        assert!(resolve_render_device(Some("pci:ffff:ff:ff.7"), "seat0", GpuMode::Hybrid).is_err());
    }

    /// ADR 0084: a repeated connector name on a later device is tagged with its
    /// card; the first owner and non-colliding names keep the plain name.
    #[test]
    fn a_colliding_connector_name_is_tagged_with_its_card() {
        let taken = vec!["DP-1".to_string(), "eDP-1".to_string()];
        assert_eq!(disambiguate("DP-1", &taken, "card0"), "DP-1@card0");
        assert_eq!(disambiguate("HDMI-A-2", &taken, "card0"), "HDMI-A-2");
        assert_eq!(disambiguate("DP-1", &[], "card0"), "DP-1");
    }

    #[test]
    fn a_failed_frame_is_retried_within_one_frame() {
        // A refused commit clears on its own; the retry must not be the slow part.
        assert!(RETRY_DELAY <= Duration::from_millis(17));
    }

    #[test]
    fn a_run_of_failures_is_logged_at_powers_of_two() {
        let logged: Vec<u32> = (1..=20).filter(|n| log_failure_now(*n)).collect();
        assert_eq!(logged, [1, 2, 4, 8, 16]);
    }

    #[test]
    fn a_refused_frame_redraws_in_full_so_the_retry_carries_new_damage() {
        // Without this the retry finds nothing newly damaged, reuses the plane
        // config holding the blob the kernel just rejected, and is refused
        // again every RETRY_DELAY until something else on screen moves.
        assert!(needs_full_redraw(false, true));
        assert!(needs_full_redraw(true, false));
        assert!(!needs_full_redraw(false, false));
    }

    #[test]
    fn repeated_refusals_drop_plane_assignment_and_a_success_restores_it() {
        assert_eq!(frame_flags_for(0, FrameFlags::DEFAULT), FrameFlags::DEFAULT);
        assert_eq!(
            frame_flags_for(PLANE_FALLBACK_AFTER - 1, FrameFlags::DEFAULT),
            FrameFlags::DEFAULT
        );
        assert_eq!(
            frame_flags_for(PLANE_FALLBACK_AFTER, FrameFlags::DEFAULT),
            FrameFlags::empty()
        );
        assert_eq!(
            frame_flags_for(PLANE_FALLBACK_AFTER, FrameFlags::empty()),
            FrameFlags::empty()
        );
    }

    #[test]
    fn a_counter_that_never_reset_still_logs_rarely() {
        assert!(log_failure_now(1 << 20));
        assert!(!log_failure_now((1 << 20) + 1));
        assert!(!log_failure_now(0));
    }
}
