// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_pointer_constraints_v1` (COMP-06 §1) — pointer lock and confinement.
//!
//! Required for mouse-look in games and for drawing apps that confine the
//! cursor. A constraint only ever restricts where the human's own pointer may
//! go inside a surface that already has pointer focus; it grants no reach, so it
//! is ungated. Smithay activates a constraint only while the surface is focused
//! and drops it on focus loss, which is the property that keeps it safe.

use smithay::{
    delegate_pointer_constraints,
    input::pointer::PointerHandle,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    wayland::pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler},
};

use crate::state::AbyssState;

impl PointerConstraintsHandler for AbyssState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Activate immediately if the constrained surface already has the pointer:
        // a game that locks on startup should not need a second click.
        let focused = pointer.current_focus().is_some_and(|f| &f == surface);
        if focused {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        // Honour the hint only while the lock is actually active, so a background
        // client cannot warp the human's pointer.
        let active = with_pointer_constraint(surface, pointer, |c| c.is_some_and(|c| c.is_active()));
        if active {
            self.pointer_location = location;
        }
    }
}

delegate_pointer_constraints!(AbyssState);
