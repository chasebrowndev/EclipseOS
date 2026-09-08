// SPDX-License-Identifier: AGPL-3.0-only
use smithay::{
    delegate_seat,
    input::{pointer::CursorImageStatus, Seat, SeatHandler, SeatState},
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    wayland::seat::WaylandFocus,
};

use crate::state::AbyssState;

impl SeatHandler for AbyssState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|s| s.wl_surface().map(|s| s.client())).flatten();
        smithay::wayland::selection::data_device::set_data_device_focus(
            &self.display_handle,
            _seat,
            client.clone(),
        );
        smithay::wayland::selection::primary_selection::set_primary_focus(
            &self.display_handle,
            _seat,
            client,
        );
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // Named shapes from `wp_cursor_shape_v1` arrive here too, so this one
        // path covers both. The winit backend draws the host cursor and ignores
        // this; the DRM backend composites it (`render::cursor`).
        self.cursor_status = image;
    }
}

delegate_seat!(AbyssState);
