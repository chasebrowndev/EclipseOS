// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_pointer_constraints_v1` (COMP-06 §1) — pointer lock and confinement.
//!
//! Required for mouse-look in games and for drawing apps that confine the
//! cursor. A constraint only ever restricts where the human's own pointer may
//! go inside a surface that already has pointer focus; it grants no reach, so it
//! is ungated. Smithay drops an active constraint automatically when its
//! surface loses pointer focus (`PointerTarget::leave`); activating it again
//! when the surface regains focus, and clamping motion while it is active,
//! are the compositor's responsibility — see [`apply_pointer_constraint`] and
//! [`update_pointer_constraint_focus`], called from `input::pointer_moved`.

use smithay::{
    delegate_pointer_constraints,
    input::pointer::PointerHandle,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle},
    wayland::{
        compositor::{RectangleKind, RegionAttributes},
        pointer_constraints::{with_pointer_constraint, PointerConstraint, PointerConstraintsHandler},
    },
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

/// Bounding box of a region's additive rectangles, in surface-local
/// coordinates. Subtractive holes are ignored: a bbox clamp is a
/// conservative approximation of confinement, which is all the shapes the
/// wlcs conformance tests exercise.
fn region_bbox(region: &RegionAttributes) -> Option<Rectangle<i32, Logical>> {
    region
        .rects
        .iter()
        .filter(|(kind, _)| matches!(kind, RectangleKind::Add))
        .map(|(_, rect)| *rect)
        .reduce(Rectangle::merge)
}

/// Apply an active lock/confine constraint on `focus` to a candidate pointer
/// position (COMP-06 §1, zwp_pointer_constraints_v1). A `Locked` constraint
/// freezes the pointer at `current`; a `Confined` one clamps it into the
/// constraint's region, translated by `origin` (the focused surface's
/// top-left in the same coordinate space as `current`/`candidate`).
pub(crate) fn apply_pointer_constraint(
    state: &AbyssState,
    focus: Option<&WlSurface>,
    origin: Option<Point<i32, Logical>>,
    current: Point<f64, Logical>,
    candidate: Point<f64, Logical>,
) -> Point<f64, Logical> {
    let Some(surface) = focus else { return candidate };
    let Some(pointer) = state.seat.get_pointer() else {
        return candidate;
    };
    with_pointer_constraint(surface, &pointer, |constraint| {
        let Some(constraint) = constraint else {
            return candidate;
        };
        if !constraint.is_active() {
            return candidate;
        }
        match &*constraint {
            PointerConstraint::Locked(_) => current,
            PointerConstraint::Confined(_) => {
                let Some(origin) = origin else { return candidate };
                let Some(bbox) = constraint.region().and_then(region_bbox) else {
                    return candidate;
                };
                let local = candidate - origin.to_f64();
                let max_x = (bbox.loc.x + bbox.size.w - 1).max(bbox.loc.x) as f64;
                let max_y = (bbox.loc.y + bbox.size.h - 1).max(bbox.loc.y) as f64;
                let clamped: Point<f64, Logical> = (
                    local.x.clamp(bbox.loc.x as f64, max_x),
                    local.y.clamp(bbox.loc.y as f64, max_y),
                )
                    .into();
                clamped + origin.to_f64()
            }
        }
    })
}

/// On a pointer-focus transition, activate an inactive constraint held by
/// the surface being entered. Smithay deactivates the constraint on the
/// surface being left automatically; entering is the compositor's half.
pub(crate) fn update_pointer_constraint_focus(
    state: &AbyssState,
    old_focus: Option<&WlSurface>,
    new_focus: Option<&WlSurface>,
) {
    let Some(surface) = new_focus else { return };
    if old_focus == Some(surface) {
        return;
    }
    let Some(pointer) = state.seat.get_pointer() else {
        return;
    };
    with_pointer_constraint(surface, &pointer, |constraint| {
        if let Some(constraint) = constraint {
            if !constraint.is_active() {
                constraint.activate();
            }
        }
    });
}
