// SPDX-License-Identifier: AGPL-3.0-only
//! `xdg_toplevel_icon_v1` (COMP-06 §1, T-17 §8).
//!
//! Smithay keeps the icon in the surface's double-buffered cached state, so a
//! toplevel's icon is latched on its next `wl_surface.commit` and dies with the
//! surface. abyss stores nothing of its own. The shell reads the themed icon
//! name through `get_windows` (`icon`), see [`icon_name`]; pixel buffers are
//! not forwarded over the control socket. An icon is a client's own claim about
//! itself, so it is advisory chrome only and never feeds identity or policy.

use smithay::{
    delegate_xdg_toplevel_icon,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        compositor::with_states,
        xdg_toplevel_icon::{ToplevelIconCachedState, XdgToplevelIconHandler},
    },
};

use crate::state::AbyssState;

/// Largest icon size advertised to clients, logical px.
pub const ICON_SIZE: i32 = 64;

impl XdgToplevelIconHandler for AbyssState {}

delegate_xdg_toplevel_icon!(AbyssState);

/// The themed icon name the client set on this toplevel, as committed.
pub fn icon_name(surface: &WlSurface) -> Option<String> {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<ToplevelIconCachedState>()
            .current()
            .icon_name()
            .map(str::to_owned)
    })
}
