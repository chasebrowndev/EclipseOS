// SPDX-License-Identifier: AGPL-3.0-only
//! Root compositor state. Single-threaded, owned by the calloop loop.

use std::{sync::Arc, time::Instant};

use smithay::{
    desktop::{PopupManager, Space, Window},
    input::{Seat, SeatState},
    reexports::{
        calloop::LoopSignal,
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            Display, DisplayHandle,
        },
    },
    utils::{Logical, Point},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        input_method::{InputMethodManagerState, PopupSurface},
        output::OutputManagerState,
        selection::{
            data_device::DataDeviceState, primary_selection::PrimarySelectionState,
            wlr_data_control::DataControlState,
        },
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState},
        shm::ShmState,
        socket::ListeningSocketSource,
        text_input::TextInputManagerState,
    },
};

pub struct HeliosState {
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
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

    /// The human seat (`seat0`). Agent seats arrive in Phase 2.
    pub seat: Seat<Self>,

    /// Parsed configuration (COMP-13). Never fails to load; falls back to defaults.
    pub config: crate::config::Config,
    /// Workspaces 1..=10 on the primary output.
    pub workspaces: Vec<crate::shell::workspace::Workspace>,
    /// 0-based index into `workspaces`.
    pub active_workspace: usize,
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
    #[cfg(feature = "drm")]
    pub dmabuf_state: Option<smithay::wayland::dmabuf::DmabufState>,
}

impl HeliosState {
    pub fn new(
        display: &Display<Self>,
        loop_signal: LoopSignal,
        socket: &ListeningSocketSource,
        config: crate::config::Config,
    ) -> Self {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&dh);
        // Data control reads every selection: allowlisted, fail-closed (ADR 0022).
        let data_control_state = DataControlState::new::<Self, _>(
            &dh,
            Some(&primary_selection_state),
            crate::protocols::standard::data_control::allow_filter(
                dh.clone(),
                config.clipboard.data_control_allow.clone(),
            ),
        );
        let text_input_manager_state = TextInputManagerState::new::<Self>(&dh);
        let input_method_manager_state = InputMethodManagerState::new::<Self, _>(&dh, |_| true);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        // Repeat defaults match COMP-04 until config lands (M6).
        seat.add_keyboard(Default::default(), 300, 40)
            .expect("default xkb keymap must load");
        seat.add_pointer();

        Self {
            start_time: Instant::now(),
            display_handle: dh,
            loop_signal,
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
            text_input_manager_state,
            input_method_manager_state,
            clipboard: None,
            dnd_icon: None,
            input_method_popup: None,
            seat_state,
            seat,
            pointer_location: (0.0, 0.0).into(),
            workspaces: crate::shell::workspace::new_set(),
            active_workspace: 0,
            focus: None,
            borders: crate::render::BorderStore::default(),
            config,
            #[cfg(feature = "drm")]
            drm: None,
            #[cfg(feature = "drm")]
            dmabuf_state: None,
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
