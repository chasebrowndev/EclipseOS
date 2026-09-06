// SPDX-License-Identifier: AGPL-3.0-only
use smithay::{
    delegate_xdg_shell,
    desktop::{PopupKind, Window},
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::protocol::wl_seat::WlSeat,
    },
    utils::Serial,
    wayland::shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
};

use crate::state::HeliosState;

impl XdgShellHandler for HeliosState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Activated);
        });
        let window = Window::new_wayland_window(surface);
        crate::shell::place_new_window(self, window);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let found = self
            .space
            .elements()
            .find(|w| w.toplevel().map(|t| t == &surface).unwrap_or(false))
            .cloned();
        if let Some(w) = found {
            crate::shell::unmap_window(self, &w);
        } else {
            crate::shell::refocus_topmost(self);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        if let Err(e) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?e, "failed to track popup");
        }
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // Popup grabs land with the full shell in M3.
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        surface.send_repositioned(token);
    }
}

delegate_xdg_shell!(HeliosState);
