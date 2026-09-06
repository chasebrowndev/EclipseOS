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
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
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
    pub shm_state: ShmState,
    #[allow(dead_code)] // holds the xdg_output global alive
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub seat_state: SeatState<Self>,

    /// The human seat (`seat0`). Agent seats arrive in Phase 2.
    pub seat: Seat<Self>,

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
    ) -> Self {
        let dh = display.handle();
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);
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
            shm_state,
            output_manager_state,
            data_device_state,
            seat_state,
            seat,
            pointer_location: (0.0, 0.0).into(),
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
