// SPDX-License-Identifier: AGPL-3.0-only
//! Root compositor state. Single-threaded, owned by the calloop loop.

use std::{collections::HashSet, sync::Arc, time::Instant};

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{Seat, SeatState},
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            Display, DisplayHandle,
        },
    },
    utils::{Clock, Logical, Monotonic, Point},
    wayland::{
        alpha_modifier::AlphaModifierState,
        compositor::{CompositorClientState, CompositorState},
        content_type::ContentTypeState,
        cursor_shape::CursorShapeManagerState,
        foreign_toplevel_list::ForeignToplevelListState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        idle_notify::IdleNotifierState,
        input_method::{InputMethodManagerState, PopupSurface},
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        security_context::SecurityContextState,
        selection::{
            data_device::DataDeviceState, primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        session_lock::SessionLockManagerState,
        shell::xdg::decoration::XdgDecorationState,
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState},
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        socket::ListeningSocketSource,
        tablet_manager::TabletManagerState,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
        xdg_foreign::XdgForeignState,
        xwayland_shell::XWaylandShellState,
    },
};

pub struct AbyssState {
    pub start_time: Instant,
    /// CLOCK_MONOTONIC, the clock `wp_presentation` timestamps are given in.
    pub clock: Clock<Monotonic>,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub loop_handle: LoopHandle<'static, Self>,
    pub socket_name: String,

    pub space: Space<Window>,
    pub popups: PopupManager,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    #[allow(dead_code)] // holds the xdg_output global alive
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    #[allow(dead_code)] // holds the zwlr_data_control_manager_v1 global alive
    pub data_control_state: DataControlState,
    #[allow(dead_code)] // holds the zwp_text_input_manager_v3 global alive
    pub text_input_manager_state: TextInputManagerState,
    #[allow(dead_code)] // holds the zwp_input_method_manager_v2 global alive
    pub input_method_manager_state: InputMethodManagerState,
    pub seat_state: SeatState<Self>,
    #[allow(dead_code)] // holds the wp_fractional_scale_manager_v1 global alive
    pub fractional_scale_state: FractionalScaleManagerState,
    #[allow(dead_code)] // holds the wp_viewporter global alive
    pub viewporter_state: ViewporterState,
    #[allow(dead_code)] // holds the wp_presentation global alive
    pub presentation_state: PresentationState,
    #[allow(dead_code)] // holds the wp_single_pixel_buffer_manager_v1 global alive
    pub single_pixel_buffer_state: SinglePixelBufferState,
    #[allow(dead_code)] // holds the wp_content_type_manager_v1 global alive
    pub content_type_state: ContentTypeState,
    #[allow(dead_code)] // holds the wp_alpha_modifier_v1 global alive
    pub alpha_modifier_state: AlphaModifierState,
    #[allow(dead_code)] // holds the zwp_relative_pointer_manager_v1 global alive
    pub relative_pointer_state: RelativePointerManagerState,
    #[allow(dead_code)] // holds the zwp_pointer_gestures_v1 global alive
    pub pointer_gestures_state: PointerGesturesState,
    #[allow(dead_code)] // holds the wp_cursor_shape_manager_v1 global alive
    pub cursor_shape_state: CursorShapeManagerState,
    #[allow(dead_code)] // holds the zwp_tablet_manager_v2 global alive
    pub tablet_state: TabletManagerState,
    #[allow(dead_code)] // holds the zwp_pointer_constraints_v1 global alive
    pub pointer_constraints_state: PointerConstraintsState,
    #[allow(dead_code)] // holds the zxdg_decoration_manager_v1 global alive
    pub xdg_decoration_state: XdgDecorationState,
    #[allow(dead_code)] // holds the wp_security_context_manager_v1 global alive
    pub security_context_state: SecurityContextState,
    pub activation_state: XdgActivationState,
    pub xdg_foreign_state: XdgForeignState,
    pub foreign_toplevel_list: ForeignToplevelListState,
    pub gamma_control: crate::protocols::standard::gamma_control::GammaControlState,
    pub output_management: crate::protocols::standard::output_management::OutputManagementState,
    /// Windows that asked for focus and were refused (COMP-05 §5).
    pub urgent: Vec<smithay::desktop::Window>,

    /// The human seat (`seat0`). Agent seats arrive in Phase 2.
    pub seat: Seat<Self>,

    /// What the focused client last asked the pointer to look like (COMP-02 §2).
    pub cursor_status: smithay::input::pointer::CursorImageStatus,
    /// Parsed configuration (COMP-13). Never fails to load; falls back to defaults.
    pub config: crate::config::Config,
    /// Every output, each with its own workspace set (COMP-03).
    pub outputs: crate::outputs::Outputs,
    /// The window holding keyboard focus, if any.
    pub focus: Option<Window>,
    /// Per-window border quads, kept alive across frames.
    pub borders: crate::render::BorderStore,

    /// Who set the current clipboard (COMP-06 §4). Never holds contents.
    pub clipboard: Option<crate::protocols::standard::data_device::ClipboardSource>,
    /// Surface dragged under the cursor during a client-initiated DnD.
    pub dnd_icon: Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
    /// The input method's popup surface, when one is mapped.
    pub input_method_popup: Option<PopupSurface>,

    /// Pointer position in the global (logical) coordinate space.
    pub pointer_location: Point<f64, Logical>,

    /// Live only on the DRM backend; `None` under winit/headless.
    #[cfg(feature = "drm")]
    pub drm: Option<Box<crate::backend::drm::DrmData>>,
    /// Live only on the winit backend. Held here so the dmabuf handler can
    /// import into the same renderer that draws the frame.
    #[cfg(feature = "winit")]
    pub winit: Option<Box<crate::backend::winit::WinitData>>,
    /// `None` when no render node exists, in which case there is no global.
    pub dmabuf_state: Option<smithay::wayland::dmabuf::DmabufState>,
    /// Default (render) feedback, sent to every surface that cannot scan out.
    pub dmabuf_feedback: Option<smithay::wayland::dmabuf::DmabufFeedback>,
    /// Explicit sync (COMP-02 §3). `None` when the device has no syncobj eventfd.
    #[cfg(feature = "drm")]
    pub syncobj_state: Option<smithay::wayland::drm_syncobj::DrmSyncobjState>,

    /// Surfaces flagged sensitive (COMP-02 §7). Stub until the policy engine
    /// lands: a surface listed here is never handed to a scanout plane, so the
    /// compositor keeps the pixels it can redact.
    pub sensitive: HashSet<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,

    /// Live capture allowlist, shared with the `zwlr_screencopy_v1` bind
    /// filter. Written by the config reload path.
    pub capture_allow: crate::config::Allowlist,
    /// Live data-control allowlist, shared with the `zwlr_data_control_v1`
    /// bind filter. Written by the config reload path.
    pub clipboard_allow: crate::config::Allowlist,

    /// `zwlr_screencopy_v1` (COMP-06 §3). Holds the global alive; the gate
    /// lives in the dispatch impls.
    #[allow(dead_code)]
    pub screencopy: crate::protocols::standard::screencopy::ScreencopyState,
    /// `ext_image_copy_capture_v1` + `ext_image_capture_source_v1` (COMP-02 §8,
    /// COMP-06 §3). Holds both globals alive and owns the session/frame tables;
    /// the gate is the same `screencopy::decide` (ADR 0030).
    pub image_copy: crate::protocols::standard::image_copy_capture::ImageCopyCaptureState,
    /// Authorised captures awaiting a renderer. Only the backend drains this.
    pub captures: Vec<crate::render::capture::Pending>,
    /// When capture last produced a frame, for the trusted-UI indicator
    /// (COMP-10 §3.6). `None` means nothing has ever been captured.
    pub capture_seen: Option<Instant>,
    /// Process name of the client currently capturing, for the indicator and
    /// the audit log. Never a buffer, never window content.
    pub capture_consumer: Option<String>,

    /// `ext_session_lock_v1` (COMP-10 §5).
    pub session_lock_state: SessionLockManagerState,
    /// Lock surfaces and the compositor-drawn fallback (ADR 0024).
    pub lock: crate::protocols::standard::session_lock::LockState,
    /// Idle clock: last activity, inhibitors, DPMS state (COMP-03 §7).
    pub idle: crate::input::idle::IdleTracker,
    /// `ext_idle_notify_v1`.
    pub idle_notifier: IdleNotifierState<Self>,
    #[allow(dead_code)] // holds the zwp_idle_inhibit_manager_v1 global alive
    pub idle_inhibit_state: IdleInhibitManagerState,
    /// `zwlr_output_power_management_v1`.
    pub output_power: crate::protocols::standard::output_power::OutputPowerState,

    /// `xwayland_shell_v1`: how XWayland associates X windows with surfaces.
    pub xwayland_shell_state: XWaylandShellState,
    /// The X11 trust domain (COMP-07). Empty until XWayland is ready.
    pub xwayland: crate::xwayland::XWaylandState,

    /// Frame timing (COMP-14 §2).
    pub stats: crate::render::stats::FrameStats,

    /// Human control socket (COMP-13 §2).
    pub ipc: crate::ipc::IpcState,
    /// Deadline for the debounced config reload, `None` when nothing is
    /// pending (COMP-13 §1.2).
    pub config_dirty: Option<Instant>,
    /// The debounce timer's source, so repeated edits reuse one timer.
    pub config_timer: Option<smithay::reexports::calloop::RegistrationToken>,
}

impl AbyssState {
    /// The live backend, as the [`Backend`](crate::backend::Backend) trait.
    ///
    /// Exactly one of the two fields is ever `Some`, so the order here is a
    /// formality rather than a preference. `None` means no backend is up yet.
    pub fn backend_mut(&mut self) -> Option<&mut dyn crate::backend::Backend> {
        #[cfg(feature = "drm")]
        if let Some(drm) = self.drm.as_mut() {
            return Some(&mut **drm);
        }
        #[cfg(feature = "winit")]
        if let Some(winit) = self.winit.as_mut() {
            return Some(&mut **winit);
        }
        None
    }

    /// Is a client capturing right now? Drives the trusted-UI indicator.
    pub fn capture_active(&self) -> bool {
        self.capture_seen
            .is_some_and(|t| t.elapsed() < crate::render::capture::INDICATOR_LINGER)
    }

    pub fn new(
        display: &Display<Self>,
        loop_signal: LoopSignal,
        loop_handle: LoopHandle<'static, Self>,
        socket: &ListeningSocketSource,
        config: crate::config::Config,
        stats: bool,
    ) -> Self {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(
            &dh,
            crate::protocols::standard::screencopy::EXTRA_SHM_FORMATS.to_vec(),
        );
        // Live handles, shared with the two global filters so that a config
        // reload retunes them without a restart (ADR 0022 amendment).
        let clipboard_allow = crate::config::Allowlist::new(config.clipboard.data_control_allow.clone());
        let capture_allow = crate::config::Allowlist::new(config.capture.allow.clone());
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        // Data control reads every selection: allowlisted, fail-closed (ADR 0022).
        let data_control_state = DataControlState::new::<Self, _>(
            &dh,
            Some(&primary_selection_state),
            crate::protocols::standard::data_control::allow_filter(dh.clone(), clipboard_allow.clone()),
        );
        let text_input_manager_state = TextInputManagerState::new::<Self>(&dh);
        let input_method_manager_state = InputMethodManagerState::new::<Self, _>(&dh, |_| true);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        // CLOCK_MONOTONIC: the clock every backend timestamps frames against.
        let presentation_state = PresentationState::new::<Self>(&dh, libc::CLOCK_MONOTONIC as u32);
        // Passive globals: no handler, no policy, no gate (COMP-06 §1).
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&dh);
        let content_type_state = ContentTypeState::new::<Self>(&dh);
        let alpha_modifier_state = AlphaModifierState::new::<Self>(&dh);
        let relative_pointer_state = RelativePointerManagerState::new::<Self>(&dh);
        let pointer_gestures_state = PointerGesturesState::new::<Self>(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(&dh);
        let tablet_state = TabletManagerState::new::<Self>(&dh);
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let activation_state = XdgActivationState::new::<Self>(&dh);
        let xdg_foreign_state = XdgForeignState::new::<Self>(&dh);
        let foreign_toplevel_list = ForeignToplevelListState::new::<Self>(&dh);
        let gamma_control = crate::protocols::standard::gamma_control::GammaControlState::new(&dh);
        let output_management =
            crate::protocols::standard::output_management::OutputManagementState::new(&dh);
        // A sandboxed client may not mint further sandbox identities for itself.
        let security_context_state = SecurityContextState::new::<Self, _>(&dh, |client| {
            client
                .get_data::<ClientState>()
                .is_none_or(|s| s.security_context.is_none())
        });
        // Any client may lock the session; a lock only ever removes access.
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);
        let idle_notifier = IdleNotifierState::<Self>::new(&dh, loop_handle.clone());
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(&dh);
        let xwayland_shell_state = XWaylandShellState::new::<Self>(&dh);
        let output_power = crate::protocols::standard::output_power::OutputPowerState::new(&dh);
        // Capture reads every pixel of an output: allowlisted, fail-closed.
        let screencopy =
            crate::protocols::standard::screencopy::ScreencopyState::new(&dh, capture_allow.clone());
        let image_copy = crate::protocols::standard::image_copy_capture::ImageCopyCaptureState::new(
            &dh,
            capture_allow.clone(),
        );
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        // Keyboard settings come from the `input` block (COMP-13 §1.2); a bad
        // layout there must not leave the seat without a keyboard at all.
        let xkb = crate::input::xkb_config(&config.input);
        let (delay, rate) = (config.input.repeat_delay, config.input.repeat_rate);
        seat.add_keyboard(xkb, delay, rate)
            .or_else(|err| {
                tracing::error!(
                    ?err,
                    layout = config.input.kb_layout,
                    "falling back to the default keymap"
                );
                seat.add_keyboard(Default::default(), delay, rate)
            })
            .expect("default xkb keymap must load");
        seat.add_pointer();

        Self {
            start_time: Instant::now(),
            clock: Clock::new(),
            display_handle: dh,
            loop_signal,
            loop_handle,
            socket_name: socket.socket_name().to_string_lossy().into_owned(),
            space: Space::default(),
            popups: PopupManager::default(),
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            shm_state,
            output_manager_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            capture_allow,
            clipboard_allow,
            screencopy,
            image_copy,
            captures: Vec::new(),
            capture_seen: None,
            capture_consumer: None,
            text_input_manager_state,
            input_method_manager_state,
            clipboard: None,
            dnd_icon: None,
            input_method_popup: None,
            fractional_scale_state,
            viewporter_state,
            presentation_state,
            single_pixel_buffer_state,
            content_type_state,
            alpha_modifier_state,
            relative_pointer_state,
            pointer_gestures_state,
            cursor_shape_state,
            tablet_state,
            pointer_constraints_state,
            xdg_decoration_state,
            security_context_state,
            activation_state,
            xdg_foreign_state,
            foreign_toplevel_list,
            gamma_control,
            output_management,
            urgent: Vec::new(),
            seat_state,
            seat,
            pointer_location: (0.0, 0.0).into(),
            outputs: crate::outputs::Outputs::new(),
            focus: None,
            borders: crate::render::BorderStore::default(),
            cursor_status: smithay::input::pointer::CursorImageStatus::default_named(),
            config,
            #[cfg(feature = "drm")]
            drm: None,
            #[cfg(feature = "winit")]
            winit: None,
            dmabuf_state: None,
            dmabuf_feedback: None,
            #[cfg(feature = "drm")]
            syncobj_state: None,
            sensitive: HashSet::new(),
            session_lock_state,
            lock: Default::default(),
            idle: Default::default(),
            idle_notifier,
            idle_inhibit_state,
            output_power,
            xwayland_shell_state,
            xwayland: Default::default(),
            stats: crate::render::stats::FrameStats::new(stats),
            ipc: crate::ipc::IpcState::default(),
            config_dirty: None,
            config_timer: None,
        }
    }

    pub fn quit(&mut self) {
        tracing::info!("quit requested");
        self.loop_signal.stop();
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Set when the client connected through a `wp_security_context` socket:
    /// the sandbox engine, app id and instance id it was launched under.
    pub security_context: Option<smithay::wayland::security_context::SecurityContext>,
}

impl ClientData for ClientState {
    fn initialized(&self, id: ClientId) {
        tracing::debug!(?id, "client connected");
    }
    fn disconnected(&self, id: ClientId, reason: DisconnectReason) {
        tracing::debug!(?id, ?reason, "client disconnected");
    }
}

pub fn client_state() -> Arc<ClientState> {
    Arc::new(ClientState::default())
}
