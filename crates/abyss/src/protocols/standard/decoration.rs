// SPDX-License-Identifier: AGPL-3.0-only
//! `zxdg_decoration_manager_v1` (COMP-06 §1) — who draws the window frame.
//!
//! Abyss draws borders itself (COMP-05), so the answer is always server-side:
//! a client-drawn titlebar would be a second, unaudited source of window chrome
//! and would not match the compositor's own focus indication. The spec says
//! prefer server-side and honour client-side; a client that asks for client-side
//! explicitly gets it, everything else — including "no preference" — gets SSD.

use smithay::{
    delegate_xdg_decoration,
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    wayland::shell::xdg::{decoration::XdgDecorationHandler, ToplevelSurface},
};

use crate::state::AbyssState;

fn set_mode(toplevel: ToplevelSurface, mode: Mode) {
    toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

impl XdgDecorationHandler for AbyssState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        set_mode(toplevel, Mode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        // Only an explicit client-side request is honoured as such.
        let mode = match mode {
            Mode::ClientSide => Mode::ClientSide,
            _ => Mode::ServerSide,
        };
        set_mode(toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_mode(toplevel, Mode::ServerSide);
    }
}

delegate_xdg_decoration!(AbyssState);
