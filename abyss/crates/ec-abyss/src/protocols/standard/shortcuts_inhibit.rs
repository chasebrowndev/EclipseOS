// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_keyboard_shortcuts_inhibit_v1` (COMP-04 §5, COMP-06 §1).
//!
//! Honoured: an inhibitor is activated as soon as it is created, and while the
//! keyboard focus is the inhibiting surface the input filter forwards keys that
//! would otherwise run a compositor binding. Nothing is delivered to a surface
//! that is not focused, so a background client cannot take the keys.
//!
//! The reserved chords (`agent-override`, `agent-terminate`, COMP-04 §6) and
//! the VT-switch keys are decided before this check and can never be inhibited;
//! see [`passes_to_client`].

use smithay::{
    delegate_keyboard_shortcuts_inhibit,
    wayland::keyboard_shortcuts_inhibit::{
        KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
        KeyboardShortcutsInhibitorSeat,
    },
};

use crate::{input::Action, protocols::standard::seat::KeyboardFocusTarget, state::AbyssState};

impl KeyboardShortcutsInhibitHandler for AbyssState {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        tracing::debug!("keyboard shortcuts inhibitor created");
        inhibitor.activate();
    }

    fn inhibitor_destroyed(&mut self, _inhibitor: KeyboardShortcutsInhibitor) {
        tracing::debug!("keyboard shortcuts inhibitor destroyed");
    }
}

delegate_keyboard_shortcuts_inhibit!(AbyssState);

/// Whether an active inhibitor belongs to the surface holding keyboard focus.
/// Reads only; allocates nothing (input hot path).
pub fn focus_inhibits(state: &AbyssState) -> bool {
    let Some(keyboard) = state.seat.get_keyboard() else {
        return false;
    };
    let surface = match keyboard.current_focus() {
        Some(KeyboardFocusTarget::Wl(s)) => s,
        Some(KeyboardFocusTarget::X11(x)) => match x.wl_surface() {
            Some(s) => s,
            None => return false,
        },
        None => return false,
    };
    state
        .seat
        .keyboard_shortcuts_inhibitor_for_surface(&surface)
        .is_some_and(|i| i.is_active())
}

/// While focus inhibits, may this bound action still run? Only the reserved
/// chords: everything else goes to the client.
pub fn survives_inhibit(action: &Action) -> bool {
    matches!(
        action,
        Action::AgentOverride | Action::AgentTerminate | Action::AgentAttention
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_reserved_chords_survive() {
        assert!(survives_inhibit(&Action::AgentOverride));
        assert!(survives_inhibit(&Action::AgentTerminate));
        assert!(survives_inhibit(&Action::AgentAttention));
        assert!(!survives_inhibit(&Action::ToggleOverview));
    }
}
