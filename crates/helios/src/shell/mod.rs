// SPDX-License-Identifier: AGPL-3.0-only
//! Window placement and focus. M1: every toplevel fills the output and the
//! newest one is focused. Layouts (dwindle/master/floating) arrive in M3.

use smithay::{
    desktop::{PopupKind, Window},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::SERIAL_COUNTER,
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::state::HeliosState;

pub fn place_new_window(state: &mut HeliosState, window: Window) {
    let geometry = state
        .space
        .outputs()
        .next()
        .and_then(|o| state.space.output_geometry(o))
        .unwrap_or_default();
    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|s| s.size = Some(geometry.size));
    }
    state.space.map_element(window.clone(), geometry.loc, true);
    focus_window(state, &window);
}

pub fn focus_window(state: &mut HeliosState, window: &Window) {
    let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) else { return };
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
}

pub fn refocus_topmost(state: &mut HeliosState) {
    let top = state.space.elements().next_back().cloned();
    match top {
        Some(w) => focus_window(state, &w),
        None => {
            let keyboard = state.seat.get_keyboard().unwrap();
            keyboard.set_focus(state, None, SERIAL_COUNTER.next_serial());
        }
    }
}

/// Send the initial configure once a toplevel has committed its role, and
/// keep popups positioned.
pub fn handle_commit(state: &mut HeliosState, surface: &WlSurface) {
    state.popups.commit(surface);

    if let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel().map(|t| t.wl_surface() == surface).unwrap_or(false))
        .cloned()
    {
        let initial_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });
        if !initial_sent {
            window.toplevel().unwrap().send_configure();
        }
        return;
    }

    if let Some(popup) = state.popups.find_popup(surface) {
        let PopupKind::Xdg(ref xdg) = popup else { return };
        if !xdg.is_initial_configure_sent() {
            let _ = xdg.send_configure();
        }
    }
}
