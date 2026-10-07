// SPDX-License-Identifier: AGPL-3.0-only
//! The input side of a protected surface (COMP-19 §3, §6). Called from
//! `input/` and `shell/focus.rs` at each point an event is about to reach a
//! surface; nothing here decides policy, it applies two rules:
//!
//! 1. **Physical only.** An event whose [`Origin`] may not reach a protected
//!    surface is dropped before delivery, counted for `input_refused`, and
//!    audited when an agent sent it ([`super::refuse`]).
//! 2. **The slot's input is the compositor's** (§6). While a slot exists on
//!    the focused protected surface a physical, unmodified Return or KP_Enter
//!    is consumed (press and release) and handed to [`super::hooks::enter`]; a
//!    physical button press inside the slot's rectangle is consumed and
//!    handed to [`super::hooks::click`]; nothing inside the rectangle reaches the
//!    client at all, not even hover.
//!
//! Every function is a single length check while nothing is protected, and
//! none allocates.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point};

use super::{chain_contains, has_protected, is_protected, refuse, slot_rect};
use crate::input::Origin;
use crate::protocols::standard::seat::KeyboardFocusTarget;
use crate::state::AbyssState;

/// `Return` and `KP_Enter` (xkb keysyms).
const KEY_RETURN: u32 = 0xff0d;
const KEY_KP_ENTER: u32 = 0xff8d;

/// What [`button`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Deliver as usual.
    Pass,
    /// Do not deliver: refused, or the release of a press a slot consumed.
    Drop,
    /// A physical press inside a slot's rectangle: not delivered; the caller
    /// still does its click-to-focus, then calls [`super::hooks::click`].
    Slot { slot: u64, x: i32, y: i32 },
}

/// Who is judging a pointer-focus choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A device or injected event moved the pointer: judge by its origin.
    Event,
    /// The scene changed under a stationary pointer: judge by whoever last
    /// moved it.
    Refresh,
}

/// The slot, and the point in its own coordinates, that `pos` is inside, when
/// the topmost surface there (`under`) is the slot's host or part of its
/// tree.
fn slot_hit(state: &AbyssState, under: &WlSurface, pos: Point<f64, Logical>) -> Option<(u64, i32, i32)> {
    for s in &state.protected.slots {
        let Some(host) = state.protected.surface(s.host) else {
            continue;
        };
        let related = chain_contains(&state.popups, under, &host.surface)
            || chain_contains(&state.popups, &host.surface, under);
        if !related {
            continue;
        }
        let Some(rect) = slot_rect(state, s.id) else {
            continue;
        };
        if rect.to_f64().contains(pos) {
            return Some((
                s.id,
                (pos.x - f64::from(rect.loc.x)) as i32,
                (pos.y - f64::from(rect.loc.y)) as i32,
            ));
        }
    }
    None
}

/// The surface the human seat's keyboard has focus on, if it is a Wayland one.
fn keyboard_focus(state: &AbyssState) -> Option<WlSurface> {
    match state.seat.get_keyboard()?.current_focus()? {
        KeyboardFocusTarget::Wl(s) => Some(s),
        KeyboardFocusTarget::X11(_) => None,
    }
}

/// The slot whose host is, or is under or over, the keyboard focus `focus`.
fn slot_for_focus(state: &AbyssState, focus: &WlSurface) -> Option<u64> {
    state.protected.slots.iter().find_map(|s| {
        let host = &state.protected.surface(s.host)?.surface;
        (chain_contains(&state.popups, focus, host) || chain_contains(&state.popups, host, focus))
            .then_some(s.id)
    })
}

/// The slot whose host holds the human seat's keyboard focus, if any: the
/// §6 "host toplevel has keyboard focus" condition the trusted side samples.
pub fn focused_slot(state: &AbyssState) -> Option<u64> {
    slot_for_focus(state, &keyboard_focus(state)?)
}

/// Pointer motion, touch or a tablet tool is about to be aimed at `under`.
/// Returns what it may actually be aimed at: a protected surface is hidden
/// (`None`, so the client gets a `leave`, never a `motion`) from an origin
/// that may not reach it, and so is the slot's rectangle from everyone.
pub fn filter_under(
    state: &mut AbyssState,
    under: Option<(WlSurface, Point<f64, Logical>)>,
    pos: Point<f64, Logical>,
    mode: Mode,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    if state.protected.is_empty() {
        return under;
    }
    let (surface, at) = under?;
    if !is_protected(state, &surface) {
        return Some((surface, at));
    }
    let allowed = match mode {
        Mode::Event => {
            let origin = state.delivery_origin();
            if !origin.reaches_protected() {
                refuse(state, origin, &surface);
                return None;
            }
            true
        }
        Mode::Refresh => state.protected.pointer_trusted,
    };
    if !allowed || slot_hit(state, &surface, pos).is_some() {
        return None;
    }
    Some((surface, at))
}

/// Remember whether the pointer's position now comes from an origin that may
/// reach a protected surface. Called for every motion.
#[inline]
pub fn note_pointer_origin(state: &mut AbyssState) {
    if !state.protected.is_empty() {
        state.protected.pointer_trusted = state.delivery_origin().reaches_protected();
    }
}

/// A pointer button is about to be delivered.
pub fn button(state: &mut AbyssState, button: u32, pressed: bool) -> Button {
    let bit = 1u32 << (button & 31);
    // The release of a press a slot took, whatever changed since.
    if !pressed && state.protected.consumed_buttons & bit != 0 {
        state.protected.consumed_buttons &= !bit;
        return Button::Drop;
    }
    if state.protected.is_empty() {
        return Button::Pass;
    }
    let origin = state.delivery_origin();
    let pos = state.pointer_location;
    if let Some((s, _)) = crate::shell::surface_under(state, pos) {
        if is_protected(state, &s) {
            if !origin.reaches_protected() {
                refuse(state, origin, &s);
                return Button::Drop;
            }
            if pressed && origin == Origin::Physical {
                if let Some((slot, x, y)) = slot_hit(state, &s, pos) {
                    state.protected.consumed_buttons |= bit;
                    return Button::Slot { slot, x, y };
                }
            }
        }
    }
    // The button goes to the pointer's focus, which may predate the check
    // above (a physical motion put it on the surface, then something else
    // pressed).
    if let Some(f) = state.seat.get_pointer().and_then(|p| p.current_focus()) {
        if is_protected(state, &f) && !origin.reaches_protected() {
            refuse(state, origin, &f);
            return Button::Drop;
        }
    }
    Button::Pass
}

/// A scroll frame is about to be delivered to the pointer's focus. True: drop.
pub fn axis(state: &mut AbyssState) -> bool {
    if state.protected.is_empty() {
        return false;
    }
    let origin = state.delivery_origin();
    if origin.reaches_protected() {
        return false;
    }
    match state.seat.get_pointer().and_then(|p| p.current_focus()) {
        Some(f) if is_protected(state, &f) => {
            refuse(state, origin, &f);
            true
        }
        _ => false,
    }
}

/// The Enter keysyms as a bit of `consumed_enter`.
fn enter_bit(sym: u32) -> Option<u8> {
    match sym {
        KEY_RETURN => Some(1),
        KEY_KP_ENTER => Some(2),
        _ => None,
    }
}

/// A key the bindings did not take is about to be forwarded to the keyboard
/// focus. `sym` is its keysym, `plain` is true when no Shift, Ctrl, Alt, Logo
/// or level-3/5 shift is held. True: do not deliver it.
pub fn key(state: &mut AbyssState, pressed: bool, sym: u32, plain: bool, time: u32) -> bool {
    // The release of an Enter the slot took goes with it, whatever changed
    // since (focus moved, the slot went away): the client never saw the press.
    if !pressed {
        if let Some(bit) = enter_bit(sym) {
            if state.protected.consumed_enter & bit != 0 {
                state.protected.consumed_enter &= !bit;
                return true;
            }
        }
    }
    if state.protected.is_empty() {
        return false;
    }
    let Some(focus) = keyboard_focus(state) else {
        return false;
    };
    let origin = state.delivery_origin();
    if !origin.reaches_protected() {
        if is_protected(state, &focus) || has_protected(state, &focus) {
            refuse(state, origin, &focus);
            return true;
        }
        return false;
    }
    // Enter isolation is for the human's own keyboard only; an `injected`
    // key under `wlcs` is the conformance suite's.
    if origin != Origin::Physical {
        return false;
    }
    let Some(bit) = enter_bit(sym) else {
        return false;
    };
    let Some(slot) = slot_for_focus(state, &focus) else {
        return false;
    };
    // Shift+Enter and the other modified Enters are the client's (newlines).
    if pressed && plain {
        state.protected.consumed_enter |= bit;
        super::calls::enter(state, slot, time);
        return true;
    }
    false
}

/// Keyboard focus is about to move to `surface`. False: it may not. Focus may
/// enter a protected surface only through physical input, or a compositor
/// change that is not an input event at all (a window mapping, closing,
/// the lock lifting); an event of any other origin may not take it there.
pub fn focus_allowed(state: &mut AbyssState, surface: &WlSurface) -> bool {
    if state.protected.is_empty() {
        return true;
    }
    let Some(origin) = state.input_origin else {
        return true;
    };
    if origin.reaches_protected() {
        return true;
    }
    if is_protected(state, surface) || has_protected(state, surface) {
        refuse(state, origin, surface);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_keysyms_map_to_distinct_bits() {
        assert_eq!(enter_bit(KEY_RETURN), Some(1));
        assert_eq!(enter_bit(KEY_KP_ENTER), Some(2));
        assert_eq!(enter_bit(0x61), None);
    }
}
