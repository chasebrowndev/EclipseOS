// SPDX-License-Identifier: AGPL-3.0-only
//! `zxdg_decoration_manager_v1` (COMP-06 §1) — who draws the window frame.
//!
//! Abyss draws borders itself (COMP-05), so the answer is server-side unless
//! the client explicitly asks for client-side: the spec says prefer server-side
//! and honour client-side. A client that asks for client-side gets it and keeps
//! its own titlebar; everything else, "no preference" included, gets SSD.
//!
//! Server-side means different things per mode. `wm` and `hybrid` draw a border
//! and no titlebar. Under `de` the compositor also draws a titlebar above the
//! window (COMP-05 §7.1, `shell::titlebar`), so a change of mode here changes
//! the window's frame: [`set_mode`] re-runs the layout when it does. A client
//! that never binds this protocol draws its own decorations and gets no bar.

use smithay::{
    delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    wayland::shell::xdg::{decoration::XdgDecorationHandler, ToplevelSurface},
};

use crate::state::AbyssState;

fn set_mode(state: &mut AbyssState, toplevel: ToplevelSurface, mode: Mode) {
    let before = toplevel.with_pending_state(|s| s.decoration_mode);
    toplevel.with_pending_state(|s| s.decoration_mode = Some(mode));
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
    // A bar appears or goes with the mode: lay the window out again.
    if before != Some(mode) && state.config.mode.profile().titlebars {
        crate::shell::arrange(state);
    }
}

impl XdgDecorationHandler for AbyssState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        set_mode(self, toplevel, Mode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        // Only an explicit client-side request is honoured as such.
        let mode = match mode {
            Mode::ClientSide => Mode::ClientSide,
            _ => Mode::ServerSide,
        };
        set_mode(self, toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_mode(self, toplevel, Mode::ServerSide);
    }
}

delegate_xdg_decoration!(AbyssState);
