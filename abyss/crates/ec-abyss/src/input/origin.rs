// SPDX-License-Identifier: AGPL-3.0-only
//! Input origin (COMP-19 §3, F-05 amending COMP-04): where an input event was
//! created, so a protected surface can take human input only.
//!
//! Origin, not seat, is the check. An agent's input routed over the human seat
//! under a compat lock arrives *on the human seat*; a seat test would let it
//! through, which is the hole this exists to close.
//!
//! # How it is carried
//!
//! The single-threaded core handles one event at a time, so the origin is held
//! in [`AbyssState::input_origin`] for exactly the duration of the entry point
//! that created the event ([`AbyssState::with_origin`]) and is read at the
//! delivery points. The default, outside any entry point, is `None`, and every
//! delivery reads `None` as [`Origin::Injected`]: an event nobody tagged is
//! never taken for human input (fail-closed).
//!
//! | Entry point | Origin |
//! |---|---|
//! | `process_input_event` (libinput, winit) | `Physical` |
//! | `eclipse_agent_seat_v1` | `AgentSeat`, at its own delivery points |
//! | compat-lock routed agent input | `AgentCompat` |
//! | `zwlr_virtual_pointer_v1` | `Virtual` |
//! | control-socket `type_text` / `click_at` (not built yet) | `Scripted` |
//! | `input/inject.rs`, conformance harness, tests | untagged, so `Injected` |

use crate::state::AbyssState;

/// Where an input event was created (COMP-19 §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A libinput device on the human seat.
    Physical,
    /// An agent seat (COMP-04 §3).
    AgentSeat,
    /// Agent input routed over the human seat under a compat lock (COMP-04 §8).
    AgentCompat,
    /// `zwlr_virtual_pointer`, `zwp_virtual_keyboard`.
    Virtual,
    /// `type_text`, `click_at` (COMP-13 §2.2).
    Scripted,
    /// Test and headless injection.
    Injected,
}

impl Origin {
    /// May an event of this origin reach a protected surface? `Physical`
    /// only; `Injected` too in a `wlcs` build, which compiles the path in so
    /// the conformance suite can drive a window that happens to be protected.
    /// A release build has no such path and a test asserts it.
    #[inline]
    pub fn reaches_protected(self) -> bool {
        match self {
            Origin::Physical => true,
            #[cfg(feature = "wlcs")]
            Origin::Injected => true,
            _ => false,
        }
    }

    /// The S-04 / F-13 name.
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Physical => "physical",
            Origin::AgentSeat => "agent_seat",
            Origin::AgentCompat => "agent_compat",
            Origin::Virtual => "virtual",
            Origin::Scripted => "scripted",
            Origin::Injected => "injected",
        }
    }

    /// An agent drove this (the audited origins, COMP-19 §3).
    #[inline]
    pub fn is_agent(self) -> bool {
        matches!(self, Origin::AgentSeat | Origin::AgentCompat)
    }
}

impl AbyssState {
    /// Run `f` with `origin` as the origin of every event it creates, then
    /// restore the previous one.
    pub fn with_origin<R>(&mut self, origin: Origin, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.input_origin.replace(origin);
        let r = f(self);
        self.input_origin = prev;
        r
    }

    /// The origin to judge a delivery by: the tagged one, else `Injected`.
    #[inline]
    pub fn delivery_origin(&self) -> Origin {
        self.input_origin.unwrap_or(Origin::Injected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_physical_reaches_a_protected_surface() {
        for o in [
            Origin::AgentSeat,
            Origin::AgentCompat,
            Origin::Virtual,
            Origin::Scripted,
        ] {
            assert!(!o.reaches_protected(), "{o:?}");
        }
        assert!(Origin::Physical.reaches_protected());
    }

    /// The `injected`-accepting path exists exactly when `wlcs` is on.
    #[test]
    fn injected_is_accepted_only_with_wlcs() {
        assert_eq!(Origin::Injected.reaches_protected(), cfg!(feature = "wlcs"));
    }
}
