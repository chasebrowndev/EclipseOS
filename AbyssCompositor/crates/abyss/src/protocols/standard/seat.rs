// SPDX-License-Identifier: AGPL-3.0-only
use std::borrow::Cow;

use smithay::{
    backend::input::KeyState,
    delegate_seat,
    input::{
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
        pointer::CursorImageStatus,
        Seat, SeatHandler, SeatState,
    },
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{IsAlive, Serial},
    wayland::seat::WaylandFocus,
    xwayland::X11Surface,
};

use crate::state::AbyssState;

/// What the human seat's keyboard can focus (COMP-07 §1).
///
/// An X11 window has to be focused as its `X11Surface`, not its `wl_surface`:
/// smithay's `KeyboardTarget for X11Surface` is what sets the X input focus
/// (or sends `WM_TAKE_FOCUS`) on enter and clears it on leave. Focusing the
/// bare `wl_surface` skips that, and Chromium-based X clients then drop every
/// key. Everything else — toplevels, layer, popup and lock surfaces — is `Wl`.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyboardFocusTarget {
    Wl(WlSurface),
    X11(X11Surface),
}

impl From<WlSurface> for KeyboardFocusTarget {
    fn from(surface: WlSurface) -> Self {
        Self::Wl(surface)
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Wl(s) => s.alive(),
            Self::X11(s) => s.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Wl(s) => Some(Cow::Borrowed(s)),
            Self::X11(s) => s.wl_surface().map(Cow::Owned),
        }
    }
}

impl KeyboardTarget<AbyssState> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<AbyssState>,
        data: &mut AbyssState,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            Self::Wl(s) => KeyboardTarget::enter(s, seat, data, keys, serial),
            Self::X11(s) => KeyboardTarget::enter(s, seat, data, keys, serial),
        }
    }

    fn leave(&self, seat: &Seat<AbyssState>, data: &mut AbyssState, serial: Serial) {
        match self {
            Self::Wl(s) => KeyboardTarget::leave(s, seat, data, serial),
            Self::X11(s) => KeyboardTarget::leave(s, seat, data, serial),
        }
    }

    fn key(
        &self,
        seat: &Seat<AbyssState>,
        data: &mut AbyssState,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            Self::Wl(s) => KeyboardTarget::key(s, seat, data, key, state, serial, time),
            Self::X11(s) => KeyboardTarget::key(s, seat, data, key, state, serial, time),
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<AbyssState>,
        data: &mut AbyssState,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            Self::Wl(s) => KeyboardTarget::modifiers(s, seat, data, modifiers, serial),
            Self::X11(s) => KeyboardTarget::modifiers(s, seat, data, modifiers, serial),
        }
    }
}

impl SeatHandler for AbyssState {
    type KeyboardFocus = KeyboardFocusTarget;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&KeyboardFocusTarget>) {
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
