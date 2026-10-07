// SPDX-License-Identifier: AGPL-3.0-only
//! The commit slot's arming rules (COMP-19 §6–§7). TCB.
//!
//! A slot commits a task on a physical Enter, which relaxes COMP-10's "Enter
//! never grants". The relaxation is only safe while every §6 condition has
//! held, unbroken, for `slot_arm_ms`. This module is that rule and nothing
//! else: the protocol object, the drawing and the `create_task` round trip
//! live elsewhere and ask [`Arming`] what the slot is.
//!
//! ```text
//!   any condition false ─► Disarmed / Suspended / Refused (timer cleared)
//!   all true since t0    ─► Previewing until now - t0 ≥ arm_ms, then Armed
//! ```
//!
//! Time is passed in, in milliseconds, so every edge is testable.

/// `slot_arm_ms` bounds (COMP-19 §6). Policy owns the value; anything outside
/// is clamped rather than trusted.
pub const ARM_MIN_MS: u64 = 300;
pub const ARM_MAX_MS: u64 = 2_000;
pub const ARM_DEFAULT_MS: u64 = 500;

/// The §6 conditions, sampled by the caller each time any of them may have
/// changed. `geometry` is the host's position, size and scale folded into
/// one value: any change to it resets the timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conditions {
    /// The preview `policyd` returned, by id; `None` while one is pending.
    pub preview: Option<u64>,
    pub preview_refused: bool,
    pub policy_available: bool,
    pub host_focused: bool,
    pub fully_visible: bool,
    pub geometry: u64,
    pub fits: bool,
    pub fullscreen: bool,
    pub unlocked: bool,
    pub modal_showing: bool,
}

/// The slot's state as `eclipse_commit_slot_v1.state` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Previewing,
    Armed,
    Disarmed(Disarm),
    Suspended,
    Refused,
}

/// Reason codes, never rule ids (A-05 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disarm {
    Overflow,
    PolicyUnavailable,
    Locked,
}

/// What an unmodified physical Enter in the host does (COMP-19 §6–§7).
/// There is no "pass to client": while a slot exists the client never
/// receives one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enter {
    Commit {
        preview: u64,
    },
    /// Overflow or fullscreen: open the modal commit card instead (§7).
    OpenModal,
    /// Not ready: dropped, never queued, and the slot pulses.
    Drop,
}

#[derive(Debug)]
pub struct Arming {
    arm_ms: u64,
    /// The conditions the timer started under, and when.
    since: Option<(Conditions, u64)>,
    last: Option<Conditions>,
}

impl Arming {
    pub fn new(arm_ms: u64) -> Arming {
        Arming {
            arm_ms: arm_ms.clamp(ARM_MIN_MS, ARM_MAX_MS),
            since: None,
            last: None,
        }
    }

    /// Records the current conditions. A change to any of them restarts the
    /// timer; so does a draft edit, which arrives as a new `preview`.
    pub fn observe(&mut self, c: Conditions, now_ms: u64) {
        self.last = Some(c);
        let ready = blocker(&c).is_none();
        match self.since {
            Some((was, _)) if ready && was == c => {}
            _ if ready => self.since = Some((c, now_ms)),
            _ => self.since = None,
        }
    }

    pub fn state(&self, now_ms: u64) -> State {
        let Some(c) = self.last else {
            return State::Previewing;
        };
        if let Some(s) = blocker(&c) {
            return s;
        }
        match self.since {
            Some((_, t0)) if now_ms.saturating_sub(t0) >= self.arm_ms => State::Armed,
            _ => State::Previewing,
        }
    }

    /// The answer to an unmodified, physical-origin Enter (or a physical
    /// click on the commit control) at `now_ms`. The caller has already
    /// dropped every non-physical one (§3).
    pub fn enter(&self, now_ms: u64) -> Enter {
        let Some(c) = self.last else {
            return Enter::Drop;
        };
        match self.state(now_ms) {
            State::Armed => match c.preview {
                Some(preview) => Enter::Commit { preview },
                None => Enter::Drop,
            },
            State::Disarmed(Disarm::Overflow) => Enter::OpenModal,
            State::Suspended if c.fullscreen && c.host_focused => Enter::OpenModal,
            _ => Enter::Drop,
        }
    }
}

/// The first §6/§7 condition that keeps the slot from arming, as the state
/// it puts the slot in. `None` when all hold.
fn blocker(c: &Conditions) -> Option<State> {
    if !c.policy_available {
        return Some(State::Disarmed(Disarm::PolicyUnavailable));
    }
    if !c.unlocked {
        return Some(State::Disarmed(Disarm::Locked));
    }
    if c.preview_refused {
        return Some(State::Refused);
    }
    if !c.fits {
        return Some(State::Disarmed(Disarm::Overflow));
    }
    if c.fullscreen || !c.host_focused || !c.fully_visible || c.modal_showing {
        return Some(State::Suspended);
    }
    if c.preview.is_none() {
        return Some(State::Previewing);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> Conditions {
        Conditions {
            preview: Some(7),
            preview_refused: false,
            policy_available: true,
            host_focused: true,
            fully_visible: true,
            geometry: 1,
            fits: true,
            fullscreen: false,
            unlocked: true,
            modal_showing: false,
        }
    }

    #[test]
    fn arms_only_after_the_delay_and_commits_the_shown_preview() {
        let mut a = Arming::new(ARM_DEFAULT_MS);
        a.observe(good(), 1_000);
        assert_eq!(a.state(1_499), State::Previewing);
        assert_eq!(a.enter(1_499), Enter::Drop, "not yet armed: dropped, not queued");
        assert_eq!(a.state(1_500), State::Armed);
        assert_eq!(a.enter(1_500), Enter::Commit { preview: 7 });
    }

    /// COMP-19 §9 "Arming": each condition, violated alone, prevents arming,
    /// and restoring it restarts the full delay.
    #[test]
    fn each_condition_alone_prevents_arming_and_resets_the_timer() {
        let breaks: [fn(&mut Conditions); 9] = [
            |c| c.preview = None,
            |c| c.preview_refused = true,
            |c| c.policy_available = false,
            |c| c.host_focused = false,
            |c| c.fully_visible = false,
            |c| c.fits = false,
            |c| c.fullscreen = true,
            |c| c.unlocked = false,
            |c| c.modal_showing = true,
        ];
        for (i, f) in breaks.iter().enumerate() {
            let mut a = Arming::new(ARM_DEFAULT_MS);
            a.observe(good(), 0);
            let mut bad = good();
            f(&mut bad);
            a.observe(bad, 400);
            assert_ne!(a.state(10_000), State::Armed, "condition {i}");
            assert!(!matches!(a.enter(10_000), Enter::Commit { .. }), "condition {i}");
            a.observe(good(), 600);
            assert_eq!(
                a.state(1_099),
                State::Previewing,
                "condition {i}: timer restarted"
            );
            assert_eq!(a.state(1_100), State::Armed, "condition {i}");
        }
    }

    #[test]
    fn a_move_or_a_draft_change_inside_the_delay_does_not_commit() {
        let mut a = Arming::new(ARM_DEFAULT_MS);
        a.observe(good(), 0);
        a.observe(
            Conditions {
                geometry: 2,
                ..good()
            },
            400,
        );
        assert_eq!(a.enter(600), Enter::Drop, "moved at 400");
        a.observe(
            Conditions {
                preview: Some(8),
                geometry: 2,
                ..good()
            },
            800,
        );
        assert_eq!(a.enter(1_200), Enter::Drop, "draft changed at 800");
        assert_eq!(
            a.enter(1_300),
            Enter::Commit { preview: 8 },
            "the preview now shown"
        );
    }

    #[test]
    fn overflow_and_fullscreen_fall_back_to_the_modal_card() {
        let mut a = Arming::new(ARM_DEFAULT_MS);
        a.observe(
            Conditions {
                fits: false,
                ..good()
            },
            0,
        );
        assert_eq!(a.state(5_000), State::Disarmed(Disarm::Overflow));
        assert_eq!(a.enter(5_000), Enter::OpenModal);
        a.observe(
            Conditions {
                fullscreen: true,
                ..good()
            },
            0,
        );
        assert_eq!(a.state(5_000), State::Suspended);
        assert_eq!(a.enter(5_000), Enter::OpenModal);
        a.observe(
            Conditions {
                fullscreen: true,
                host_focused: false,
                ..good()
            },
            0,
        );
        assert_eq!(a.enter(5_000), Enter::Drop, "unfocused: nothing");
    }

    #[test]
    fn policy_loss_never_arms_and_the_delay_is_clamped() {
        let mut a = Arming::new(10);
        a.observe(good(), 0);
        assert_eq!(a.state(299), State::Previewing, "clamped up to 300");
        assert_eq!(a.state(300), State::Armed);
        a.observe(
            Conditions {
                policy_available: false,
                ..good()
            },
            400,
        );
        assert_eq!(a.state(60_000), State::Disarmed(Disarm::PolicyUnavailable));
        assert_eq!(Arming::new(u64::MAX).arm_ms, ARM_MAX_MS);
    }
}
