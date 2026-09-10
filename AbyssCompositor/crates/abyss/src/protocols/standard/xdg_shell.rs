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

use crate::state::AbyssState;

impl XdgShellHandler for AbyssState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // A new toplevel breaks an active popup grab (COMP-06 §4).
        crate::shell::popup_grab_dismiss(self);
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

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        // The initial `xdg_popup.configure` sent from `handle_commit` must
        // carry real geometry (COMP-06 §4), so seed it from the positioner
        // before unconstraining it against the parent's output.
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        crate::shell::unconstrain_popup(self, &surface);
        if let Err(e) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?e, "failed to track popup");
        }
    }

    /// COMP-05 §3: `xdg_toplevel.move` starts a pointer grab that drags the
    /// window; it is honoured only for a serial the client really was sent.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        let wl_surface = surface.wl_surface().clone();
        let Some(window) = crate::shell::window_for_surface(self, &wl_surface) else {
            return;
        };
        crate::input::grabs::start_move(self, window, &wl_surface, serial);
    }

    /// COMP-05 §3: `xdg_toplevel.resize` drags the named edges; the opposite
    /// edges stay put.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let wl_surface = surface.wl_surface().clone();
        let Some(window) = crate::shell::window_for_surface(self, &wl_surface) else {
            return;
        };
        crate::input::grabs::start_resize(self, window, &wl_surface, serial, edges);
    }

    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        crate::shell::popup_grab_start(self, surface);
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        crate::shell::popup_gone(self, &surface);
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        crate::shell::unconstrain_popup(self, &surface);
        // `send_repositioned` already emits the configure pair; a second
        // `send_configure` would double it.
        surface.send_repositioned(token);
        crate::shell::publish_popup_geometry(&surface);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        crate::shell::maximize_toplevel(self, &surface);
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        crate::shell::unmaximize_toplevel(self, &surface);
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        crate::shell::fullscreen_toplevel(self, &surface, output.as_ref());
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        crate::shell::unfullscreen_toplevel(self, &surface);
    }
}

delegate_xdg_shell!(AbyssState);
