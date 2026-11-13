// SPDX-License-Identifier: AGPL-3.0-only
//! The one state struct every `Dispatch` impl runs on (T-09 §1): globals, windows, seats,
//! outputs and the event queue handed to the application.
#![allow(unreachable_pub)] // `pub` fields of crate-private structs

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1;
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1;
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{
    Mode as WireDecorationMode, ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

use crate::caps::{BlurPath, CapabilityTable, choose_blur};
use crate::clock::{Stamp, Timerfd};
use crate::event::{ConfigureEvent, OutputId, OutputInfo, SeatId, WindowId, WlEvent};
use crate::scale::{self, Scale, ScaleMode};
use crate::seat::SeatState;
use crate::transfer::TransferLimits;
use crate::window::{
    CsdLayout, CsdMetrics, DecorationMode, DecorationPref, FrameKind, Rect, WindowStates,
    csd_layout, detect_abyss, requested_mode, resolve_app_id, resolve_frame,
};

/// Errors from connecting or driving the connection.
#[derive(Debug, thiserror::Error)]
pub enum WlError {
    /// Could not connect to the compositor.
    #[error("cannot connect to the Wayland display: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    /// The registry could not be read.
    #[error("registry: {0}")]
    Registry(#[from] wayland_client::globals::GlobalError),
    /// A required global is missing, or older than required.
    #[error("required global `{0}` is missing or too old")]
    MissingGlobal(&'static str),
    /// Dispatch failed (protocol error or bad message).
    #[error("dispatch: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    /// Backend I/O.
    #[error("wayland backend: {0}")]
    Backend(#[from] wayland_backend::client::WaylandError),
    /// I/O.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// No such window.
    #[error("no such window")]
    UnknownWindow,
    /// No such seat.
    #[error("no such seat")]
    UnknownSeat,
}

/// Client configuration.
#[derive(Clone, Debug)]
pub struct WlConfig {
    /// Default app id override (`--app-id`, profile). Policy in [`resolve_app_id`].
    pub app_id: Option<String>,
    /// Force the Abyss/non-Abyss decision; `None` detects from `XDG_CURRENT_DESKTOP`.
    pub on_abyss: Option<bool>,
    /// Clipboard transfer cap and timeout.
    pub transfer_limits: TransferLimits,
    /// Lines per wheel detent.
    pub axis_multiplier: f64,
    /// `keyboard.repeat-rate` override (Hz; 0 disables).
    pub repeat_rate: Option<u32>,
    /// `keyboard.repeat-delay` override (ms).
    pub repeat_delay_ms: Option<u32>,
}

impl Default for WlConfig {
    fn default() -> Self {
        WlConfig {
            app_id: None,
            on_abyss: None,
            transfer_limits: TransferLimits::default(),
            axis_multiplier: 1.0,
            repeat_rate: None,
            repeat_delay_ms: None,
        }
    }
}

/// What to create.
#[derive(Clone, Debug)]
pub struct WindowSpec {
    /// Title (from the window template).
    pub title: String,
    /// App id override for this window.
    pub app_id: Option<String>,
    /// `window.decorations`.
    pub decorations: DecorationPref,
    /// Minimum size in logical px.
    pub min_size: Option<(u32, u32)>,
    /// CSD sizes.
    pub csd: CsdMetrics,
}

impl Default for WindowSpec {
    fn default() -> Self {
        WindowSpec {
            title: String::new(),
            app_id: None,
            decorations: DecorationPref::Auto,
            min_size: None,
            csd: CsdMetrics::default(),
        }
    }
}

/// What to blur.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlurRequest {
    /// One box over the window geometry (always used on Abyss).
    WindowGeometry,
    /// The glass rects (off Abyss), surface-local logical px.
    Rects(Vec<Rect>),
}

pub(crate) struct Globals {
    pub compositor: WlCompositor,
    #[allow(dead_code)] // bound because T-09 §3 makes it fatal; subsurfaces arrive with T-08 media
    pub subcompositor: WlSubcompositor,
    #[allow(dead_code)] // bound for the shm cursor fallback and CSD-less shm users
    pub shm: WlShm,
    pub wm_base: XdgWmBase,
    pub data_manager: Option<wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager>,
    pub decoration: Option<ZxdgDecorationManagerV1>,
    pub fractional: Option<WpFractionalScaleManagerV1>,
    pub viewporter: Option<WpViewporter>,
    pub text_input: Option<ZwpTextInputManagerV3>,
    pub primary: Option<ZwpPrimarySelectionDeviceManagerV1>,
    pub cursor_shape: Option<WpCursorShapeManagerV1>,
    pub presentation: Option<WpPresentation>,
    pub bg_effect: Option<ExtBackgroundEffectManagerV1>,
    pub kwin_blur: Option<OrgKdeKwinBlurManager>,
}

#[derive(Default)]
pub(crate) struct PendingConfigure {
    pub size: Option<(u32, u32)>,
    pub bounds: Option<(u32, u32)>,
    pub states: WindowStates,
}

pub(crate) struct Window {
    pub id: WindowId,
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
    pub decoration: Option<ZxdgToplevelDecorationV1>,
    pub fractional: Option<WpFractionalScaleV1>,
    pub viewport: Option<WpViewport>,
    pub effect: Option<ExtBackgroundEffectSurfaceV1>,
    pub kwin_blur: Option<OrgKdeKwinBlur>,
    pub pref: DecorationPref,
    pub configured_mode: Option<DecorationMode>,
    pub frame: FrameKind,
    pub states: WindowStates,
    pub pending: PendingConfigure,
    pub wm_capabilities: Vec<u32>,
    pub configured: bool,
    /// Window geometry size from the last configure (logical).
    pub window_size: (u32, u32),
    /// Surface size (logical): the geometry, plus CSD shadow margins.
    pub logical: (u32, u32),
    pub csd_metrics: CsdMetrics,
    pub csd_layout: Option<CsdLayout>,
    pub scale_mode: ScaleMode,
    pub fractional_pref: Option<Scale>,
    pub preferred_buffer: Option<i32>,
    pub applied_buffer_scale: i32,
    pub entered: Vec<u32>,
    pub scale: Scale,
    pub blur_request: Option<BlurRequest>,
    pub blur_path: BlurPath,
    pub extras: crate::extras::WindowExtras,
}

pub(crate) struct OutputState {
    pub proxy: WlOutput,
    pub info: OutputInfo,
    pub announced: bool,
}

/// All client state. Driven by [`crate::WlSource`]; used through [`crate::WlHandle`].
pub struct WlState {
    pub(crate) conn: Connection,
    pub(crate) qh: QueueHandle<WlState>,
    pub(crate) cfg: WlConfig,
    pub(crate) on_abyss: bool,
    pub(crate) globals: Globals,
    pub(crate) extras: crate::extras::Extras,
    pub(crate) caps: CapabilityTable,
    /// Registry name -> interface, to resolve `global_remove`.
    pub(crate) registry_names: HashMap<u32, String>,
    pub(crate) windows: Vec<Window>,
    pub(crate) next_window: u32,
    pub(crate) seats: Vec<SeatState>,
    pub(crate) outputs: Vec<OutputState>,
    pub(crate) events: VecDeque<WlEvent>,
    /// Client stamp of the dispatch in progress.
    pub(crate) stamp: Stamp,
    pub(crate) presentation_clock: Option<u32>,
    pub(crate) next_feedback: u64,
    pub(crate) next_transfer: u64,
    pub(crate) ext_blur_cap: bool,
    pub(crate) blur_warned: bool,
    pub(crate) timer: Rc<Timerfd>,
    pub(crate) repeat_filter: Box<dyn Fn(u32) -> bool>,
}

impl std::fmt::Debug for WlState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WlState")
            .field("windows", &self.windows.len())
            .field("seats", &self.seats.len())
            .field("outputs", &self.outputs.len())
            .field("on_abyss", &self.on_abyss)
            .finish_non_exhaustive()
    }
}

fn bind_required<I, U>(
    globals: &GlobalList,
    qh: &QueueHandle<WlState>,
    versions: std::ops::RangeInclusive<u32>,
    udata: U,
    name: &'static str,
) -> Result<I, WlError>
where
    I: Proxy + 'static,
    U: Send + Sync + 'static,
    WlState: wayland_client::Dispatch<I, U>,
{
    globals
        .bind::<I, WlState, U>(qh, versions, udata)
        .map_err(|_| WlError::MissingGlobal(name))
}

fn bind_optional<I, U>(
    globals: &GlobalList,
    qh: &QueueHandle<WlState>,
    versions: std::ops::RangeInclusive<u32>,
    udata: U,
) -> Option<I>
where
    I: Proxy + 'static,
    U: Send + Sync + 'static,
    WlState: wayland_client::Dispatch<I, U>,
{
    globals.bind::<I, WlState, U>(qh, versions, udata).ok()
}

impl WlState {
    /// Connect, bind, and run two roundtrips so seats, outputs and capabilities are known.
    /// Events produced meanwhile (keymaps, outputs) stay queued for the first dispatch.
    pub(crate) fn connect(
        cfg: WlConfig,
    ) -> Result<(Connection, EventQueue<WlState>, WlState), WlError> {
        let conn = Connection::connect_to_env()?;
        let (list, mut queue) = registry_queue_init::<WlState>(&conn)?;
        let qh = queue.handle();

        // Fatal if absent (T-09 §3). xdg_wm_base v6+: `suspended` and `wm_capabilities`.
        let compositor: WlCompositor = bind_required(&list, &qh, 4..=6, (), "wl_compositor")?;
        let subcompositor: WlSubcompositor =
            bind_required(&list, &qh, 1..=1, (), "wl_subcompositor")?;
        let shm: WlShm = bind_required(&list, &qh, 1..=1, (), "wl_shm")?;
        let wm_base: XdgWmBase = bind_required(&list, &qh, 6..=7, (), "xdg_wm_base")?;

        // Optional: bound opportunistically.
        let globals = Globals {
            compositor,
            subcompositor,
            shm,
            wm_base,
            data_manager: bind_optional(&list, &qh, 1..=3, ()),
            decoration: bind_optional(&list, &qh, 1..=1, ()),
            fractional: bind_optional(&list, &qh, 1..=1, ()),
            viewporter: bind_optional(&list, &qh, 1..=1, ()),
            text_input: bind_optional(&list, &qh, 1..=1, ()),
            primary: bind_optional(&list, &qh, 1..=1, ()),
            cursor_shape: bind_optional(&list, &qh, 1..=1, ()),
            presentation: bind_optional(&list, &qh, 1..=1, ()),
            bg_effect: bind_optional(&list, &qh, 1..=1, ()),
            kwin_blur: bind_optional(&list, &qh, 1..=1, ()),
        };

        let on_abyss = cfg
            .on_abyss
            .unwrap_or_else(|| detect_abyss(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref()));

        let mut caps = CapabilityTable::new();
        let mut registry_names = HashMap::new();
        list.contents().with_list(|l| {
            for g in l {
                registry_names.insert(g.name, g.interface.clone());
                caps.set_present(&g.interface, g.version);
            }
        });
        let g = &globals;
        for (iface, ver, bound) in [
            ("wl_compositor", g.compositor.version(), true),
            ("wl_subcompositor", g.subcompositor.version(), true),
            ("wl_shm", g.shm.version(), true),
            ("xdg_wm_base", g.wm_base.version(), true),
            (
                "wl_data_device_manager",
                g.data_manager.as_ref().map_or(0, |p| p.version()),
                g.data_manager.is_some(),
            ),
            (
                "zxdg_decoration_manager_v1",
                g.decoration.as_ref().map_or(0, |p| p.version()),
                g.decoration.is_some(),
            ),
            (
                "wp_fractional_scale_manager_v1",
                g.fractional.as_ref().map_or(0, |p| p.version()),
                g.fractional.is_some(),
            ),
            (
                "wp_viewporter",
                g.viewporter.as_ref().map_or(0, |p| p.version()),
                g.viewporter.is_some(),
            ),
            (
                "zwp_text_input_manager_v3",
                g.text_input.as_ref().map_or(0, |p| p.version()),
                g.text_input.is_some(),
            ),
            (
                "zwp_primary_selection_device_manager_v1",
                g.primary.as_ref().map_or(0, |p| p.version()),
                g.primary.is_some(),
            ),
            (
                "wp_cursor_shape_manager_v1",
                g.cursor_shape.as_ref().map_or(0, |p| p.version()),
                g.cursor_shape.is_some(),
            ),
            (
                "wp_presentation",
                g.presentation.as_ref().map_or(0, |p| p.version()),
                g.presentation.is_some(),
            ),
            (
                "ext_background_effect_manager_v1",
                g.bg_effect.as_ref().map_or(0, |p| p.version()),
                g.bg_effect.is_some(),
            ),
            (
                "org_kde_kwin_blur_manager",
                g.kwin_blur.as_ref().map_or(0, |p| p.version()),
                g.kwin_blur.is_some(),
            ),
        ] {
            if bound {
                caps.set_bound(iface, ver);
            }
        }

        let mut state = WlState {
            conn: conn.clone(),
            qh: qh.clone(),
            cfg,
            on_abyss,
            globals,
            extras: crate::extras::Extras::default(),
            caps,
            registry_names,
            windows: Vec::new(),
            next_window: 1,
            seats: Vec::new(),
            outputs: Vec::new(),
            events: VecDeque::new(),
            stamp: Stamp::now(),
            presentation_clock: None,
            next_feedback: 1,
            next_transfer: 1,
            ext_blur_cap: false,
            blur_warned: false,
            timer: Rc::new(Timerfd::new()?),
            repeat_filter: Box::new(crate::repeat::default_key_repeats),
        };

        // Seats and outputs come and go; bind what exists now.
        let initial: Vec<(u32, String, u32)> = list.contents().with_list(|l| {
            l.iter()
                .map(|g| (g.name, g.interface.clone(), g.version))
                .collect()
        });
        for (name, iface, version) in initial {
            state.bind_dynamic(list.registry(), name, &iface, version);
            state.bind_extra(list.registry(), name, &iface, version);
        }

        queue.roundtrip(&mut state)?;
        queue.roundtrip(&mut state)?;

        if let Some(missing) = state.caps.missing_fatal() {
            return Err(WlError::MissingGlobal(missing));
        }
        if state.seats.is_empty() {
            return Err(WlError::MissingGlobal("wl_seat"));
        }
        tracing::info!(
            abyss = state.on_abyss,
            seats = state.seats.len(),
            outputs = state.outputs.len(),
            fractional = state.caps.is_bound("wp_fractional_scale_manager_v1"),
            "wayland connected"
        );
        Ok((conn, queue, state))
    }

    /// Bind seats and outputs; note everything else in the capability table.
    pub(crate) fn bind_dynamic(
        &mut self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        name: u32,
        interface: &str,
        version: u32,
    ) {
        match interface {
            "wl_seat" => {
                crate::seat::bind_seat(self, registry, name, version);
                self.caps.set_bound("wl_seat", version.min(9));
            }
            "wl_output" => {
                if version < 2 {
                    tracing::warn!(name, "wl_output older than v2 ignored (no done event)");
                    return;
                }
                let proxy =
                    registry.bind::<WlOutput, _, _>(name, version.min(4), &self.qh, OutputId(name));
                self.caps.set_bound("wl_output", version.min(4));
                self.outputs.push(OutputState {
                    proxy,
                    info: OutputInfo {
                        id: name,
                        scale: 1,
                        ..Default::default()
                    },
                    announced: false,
                });
            }
            _ => {}
        }
    }

    /// Bind one of the optional extras (`crate::extras`) and record it in the capability table.
    pub(crate) fn bind_extra(
        &mut self,
        registry: &wayland_client::protocol::wl_registry::WlRegistry,
        name: u32,
        interface: &str,
        version: u32,
    ) {
        if let Some(v) = self
            .extras
            .bind(registry, &self.qh, name, interface, version)
        {
            self.caps.set_bound(interface, v);
            if interface == "zwp_input_timestamps_manager_v1" {
                for si in 0..self.seats.len() {
                    self.attach_timestamps(si);
                }
            }
        }
    }

    pub(crate) fn win_index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    pub(crate) fn emit(&mut self, ev: WlEvent) {
        self.events.push_back(ev);
    }

    pub(crate) fn emit_input(&mut self, seat: SeatId, kind: crate::event::InputKind) {
        self.events
            .push_back(WlEvent::Input(crate::event::InputEvent {
                stamp: self.stamp,
                seat,
                kind,
            }));
    }

    // ----- windows -------------------------------------------------------------------

    /// Create a toplevel. The initial commit carries no buffer; the first buffer is the
    /// renderer's first frame (T-09 §7). The first configure arrives as an event.
    pub(crate) fn create_window(&mut self, spec: WindowSpec) -> WindowId {
        let id = WindowId(self.next_window);
        self.next_window += 1;
        let qh = &self.qh;

        let surface = self.globals.compositor.create_surface(qh, id);
        let xdg_surface = self.globals.wm_base.get_xdg_surface(&surface, qh, id);
        let toplevel = xdg_surface.get_toplevel(qh, id);

        let requested = spec.app_id.as_deref().or(self.cfg.app_id.as_deref());
        let choice = resolve_app_id(requested, self.on_abyss);
        if choice.rejected {
            tracing::warn!(
                "app-id override ignored: on Abyss it must start with org.eclipseos.Apotheosis"
            );
        }
        toplevel.set_app_id(choice.id);
        toplevel.set_title(crate::window::clamp_title(&spec.title));
        if let Some((w, h)) = spec.min_size.map(crate::window::clamp_size) {
            toplevel.set_min_size(w as i32, h as i32);
        }

        let decoration = self.globals.decoration.as_ref().map(|m| {
            let d = m.get_toplevel_decoration(&toplevel, qh, id);
            d.set_mode(match requested_mode(spec.decorations) {
                DecorationMode::Server => WireDecorationMode::ServerSide,
                DecorationMode::Client => WireDecorationMode::ClientSide,
            });
            d
        });

        let scale_mode = scale::choose_mode(
            self.globals.fractional.is_some(),
            self.globals.viewporter.is_some(),
        );
        let (fractional, viewport) = if scale_mode == ScaleMode::Fractional {
            (
                self.globals
                    .fractional
                    .as_ref()
                    .map(|m| m.get_fractional_scale(&surface, qh, id)),
                self.globals
                    .viewporter
                    .as_ref()
                    .map(|v| v.get_viewport(&surface, qh, ())),
            )
        } else {
            (None, None)
        };

        let frame = resolve_frame(spec.decorations, self.globals.decoration.is_some(), None);
        self.windows.push(Window {
            id,
            surface: surface.clone(),
            xdg_surface,
            toplevel,
            decoration,
            fractional,
            viewport,
            effect: None,
            kwin_blur: None,
            pref: spec.decorations,
            configured_mode: None,
            frame,
            states: WindowStates::default(),
            pending: PendingConfigure::default(),
            wm_capabilities: Vec::new(),
            configured: false,
            window_size: (0, 0),
            logical: (0, 0),
            csd_metrics: spec.csd,
            csd_layout: None,
            scale_mode,
            fractional_pref: None,
            preferred_buffer: None,
            applied_buffer_scale: 1,
            entered: Vec::new(),
            scale: Scale::ONE,
            blur_request: None,
            blur_path: BlurPath::Tint,
            extras: crate::extras::WindowExtras::default(),
        });

        let wi = self.windows.len() - 1;
        self.reevaluate_blur(wi, true);
        // Role assigned; commit without a buffer to get the initial configure.
        surface.commit();
        id
    }

    /// Destroy in the order swapchain (renderer), roles, `wl_surface` (INTEGRATION-61).
    /// The renderer must have dropped its swapchain before this is called.
    pub(crate) fn destroy_window(&mut self, id: WindowId) {
        let Some(wi) = self.win_index(id) else { return };
        let mut w = self.windows.remove(wi);
        crate::extras::destroy_window_extras(&mut w.extras);
        if let Some(e) = w.effect {
            e.destroy();
        }
        if let Some(b) = w.kwin_blur {
            if let Some(m) = &self.globals.kwin_blur {
                m.unset(&w.surface);
            }
            b.release();
        }
        if let Some(f) = w.fractional {
            f.destroy();
        }
        if let Some(v) = w.viewport {
            v.destroy();
        }
        if let Some(d) = w.decoration {
            d.destroy();
        }
        w.toplevel.destroy();
        w.xdg_surface.destroy();
        w.surface.destroy();
        for s in &mut self.seats {
            s.window_destroyed(id);
        }
    }

    /// Recompute layout after a configure, a metrics change or a size change: CSD geometry
    /// and input region, viewport destination, blur box. All double-buffered state, applied
    /// by the renderer's next commit.
    pub(crate) fn relayout(&mut self, wi: usize) {
        let on_abyss = self.on_abyss;
        let w = &mut self.windows[wi];
        let (gw, gh) = w.window_size;
        if gw == 0 || gh == 0 {
            return;
        }
        if w.frame == FrameKind::Client {
            let layout = csd_layout(on_abyss, w.window_size, &w.csd_metrics, w.states);
            w.logical = layout.surface;
            w.xdg_surface.set_window_geometry(
                layout.geometry.x,
                layout.geometry.y,
                layout.geometry.w,
                layout.geometry.h,
            );
            let region: WlRegion = self.globals.compositor.create_region(&self.qh, ());
            region.add(
                layout.input.x,
                layout.input.y,
                layout.input.w,
                layout.input.h,
            );
            w.surface.set_input_region(Some(&region));
            region.destroy();
            w.csd_layout = Some(layout);
        } else {
            w.logical = w.window_size;
            if w.csd_layout.take().is_some() {
                w.surface.set_input_region(None);
            }
        }
        self.apply_scale(wi);
        self.apply_blur_region(wi);
    }

    /// Re-derive the scale and push it to the surface; emit `ScaleChanged` on a change.
    pub(crate) fn apply_scale(&mut self, wi: usize) {
        let entered_scales: Vec<i32> = self.windows[wi]
            .entered
            .iter()
            .filter_map(|n| {
                self.outputs
                    .iter()
                    .find(|o| o.info.id == *n)
                    .map(|o| o.info.scale)
            })
            .collect();
        let w = &mut self.windows[wi];
        let int = scale::integer_scale(w.preferred_buffer, &entered_scales);
        let new = scale::effective(w.scale_mode, w.fractional_pref, int);
        match w.scale_mode {
            ScaleMode::Fractional => {
                if let Some(vp) = &w.viewport
                    && w.logical.0 > 0
                    && w.logical.1 > 0
                {
                    vp.set_destination(w.logical.0 as i32, w.logical.1 as i32);
                }
            }
            ScaleMode::Integer => {
                let n = new.integer_ceil();
                if n != w.applied_buffer_scale {
                    w.surface.set_buffer_scale(n);
                    w.applied_buffer_scale = n;
                }
            }
        }
        if new != w.scale {
            w.scale = new;
            let (window, scale, mode) = (w.id, new, w.scale_mode);
            self.events.push_back(WlEvent::ScaleChanged {
                window,
                scale,
                mode,
            });
        }
    }

    // ----- blur (T-09 §5) --------------------------------------------------------------

    /// Evaluate the blur ladder for a window and (re)create the right object. `initial`
    /// suppresses the change event for the first evaluation.
    pub(crate) fn reevaluate_blur(&mut self, wi: usize, initial: bool) {
        let ext_ok = self.globals.bg_effect.is_some() && self.ext_blur_cap;
        let kwin_ok = self.globals.kwin_blur.is_some();
        let path = choose_blur(self.on_abyss, ext_ok, kwin_ok);
        let w = &mut self.windows[wi];
        let old = w.blur_path;
        if path == old && !initial {
            return;
        }
        // Tear down what the old path created.
        if path != BlurPath::Ext
            && let Some(e) = w.effect.take()
        {
            e.destroy();
        }
        if path != BlurPath::Kwin
            && let Some(b) = w.kwin_blur.take()
        {
            if let Some(m) = &self.globals.kwin_blur {
                m.unset(&w.surface);
            }
            b.release();
        }
        w.blur_path = path;
        if path == BlurPath::Ext
            && w.effect.is_none()
            && let Some(m) = &self.globals.bg_effect
        {
            w.effect = Some(m.get_background_effect(&w.surface, &self.qh, ()));
        }
        if path == BlurPath::Kwin
            && w.kwin_blur.is_none()
            && let Some(m) = &self.globals.kwin_blur
        {
            w.kwin_blur = Some(m.create(&w.surface, &self.qh, ()));
        }
        let window = w.id;
        if path != BlurPath::Ext && !self.blur_warned {
            // INTEGRATION-09: one warning when blur degrades.
            self.blur_warned = true;
            tracing::warn!(
                ?path,
                abyss = self.on_abyss,
                "compositor blur unavailable; using fallback"
            );
        }
        if !initial {
            self.events
                .push_back(WlEvent::BlurPathChanged { window, path });
        }
        self.apply_blur_region(wi);
    }

    /// Push the requested blur region to the active path. On Abyss it is always one box
    /// over the window geometry. The region is double-buffered: it takes effect with the
    /// frame that draws it.
    pub(crate) fn apply_blur_region(&mut self, wi: usize) {
        let on_abyss = self.on_abyss;
        let w = &self.windows[wi];
        let Some(req) = w.blur_request.clone() else {
            return;
        };
        if w.blur_path == BlurPath::Tint {
            return;
        }
        let geometry = w.csd_layout.map_or(
            Rect {
                x: 0,
                y: 0,
                w: w.logical.0 as i32,
                h: w.logical.1 as i32,
            },
            |l| l.geometry,
        );
        let rects: Vec<Rect> = match req {
            BlurRequest::Rects(r) if !on_abyss => r,
            _ => vec![geometry],
        };
        let rects: Vec<Rect> = rects.into_iter().filter(|r| r.w > 0 && r.h > 0).collect();
        let region = if rects.is_empty() {
            None
        } else {
            let region: WlRegion = self.globals.compositor.create_region(&self.qh, ());
            for r in &rects {
                region.add(r.x, r.y, r.w, r.h);
            }
            Some(region)
        };
        match w.blur_path {
            BlurPath::Ext => {
                if let Some(e) = &w.effect {
                    e.set_blur_region(region.as_ref());
                }
            }
            BlurPath::Kwin => {
                if let Some(b) = &w.kwin_blur {
                    b.set_region(region.as_ref());
                    b.commit();
                }
            }
            BlurPath::Tint => {}
        }
        if let Some(r) = region {
            r.destroy();
        }
    }

    /// The compositor's blur capability bit changed.
    pub(crate) fn set_ext_blur_capability(&mut self, blur: bool) {
        if self.ext_blur_cap == blur {
            return;
        }
        self.ext_blur_cap = blur;
        for wi in 0..self.windows.len() {
            self.reevaluate_blur(wi, false);
        }
    }

    /// Configure applied: states, frame owner, layout. Returns the event for the app.
    pub(crate) fn apply_configure(&mut self, wi: usize, serial: u32) -> ConfigureEvent {
        let decoration_global = self.globals.decoration.is_some();
        let w = &mut self.windows[wi];
        w.xdg_surface.ack_configure(serial);
        w.states = w.pending.states;
        if let Some(sz) = w.pending.size {
            w.window_size = sz;
        }
        w.frame = resolve_frame(w.pref, decoration_global, w.configured_mode);
        let first = !w.configured;
        w.configured = true;
        let ev = ConfigureEvent {
            window: w.id,
            serial,
            size: w.pending.size,
            bounds: w.pending.bounds,
            states: w.states,
            frame: w.frame,
            first,
        };
        self.relayout(wi);
        ev
    }
}
