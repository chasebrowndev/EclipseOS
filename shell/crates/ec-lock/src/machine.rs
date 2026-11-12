// SPDX-License-Identifier: AGPL-3.0-only
//! The lock's state machine. No iced, no Wayland, no clock of its own: every
//! transition takes `now`, so the tests drive time.
//!
//! ```text
//!   Locking ──surfaces_up──▶ Idle ──submit──▶ Authenticating ──Granted──▶ Unlocked
//!                             ▲                    │
//!                             │                    └─Denied/Unavailable─▶ Failed(n, retry_at)
//!                             └──────────── submit (once now ≥ retry_at) ◀──┘
//! ```
//!
//! The one way to [`Phase::Unlocked`] is [`Machine::finish`] with a
//! [`Verdict::Granted`] for the attempt in flight, and a [`Granted`] can only
//! come from PAM ([`crate::pam`]). Nothing else, no error and no stale
//! verdict, moves the machine there. `Unlocked` is terminal.

use std::time::{Duration, Instant};

use crate::pam::{Granted, Verdict};

/// Why the last attempt failed, for the error line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// PAM refused the password.
    Wrong,
    /// PAM could not check it.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The lock is requested; no surface is up yet.
    Locking,
    /// Locked, waiting for a password.
    Idle,
    /// Attempt `attempt` is with PAM.
    Authenticating { attempt: u64 },
    /// The last attempt failed. `failures` counts every failure so far; the
    /// next attempt is refused until `retry_at`.
    Failed {
        failures: u32,
        retry_at: Instant,
        reason: Reason,
    },
    /// PAM said yes. Terminal.
    Unlocked,
}

/// The delay before attempt `failures + 1`: 1 s after the first failure,
/// doubling, at most [`MAX_BACKOFF`]. On top of whatever the PAM stack itself
/// imposes (pam_unix's delay, pam_faillock).
pub fn backoff(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let secs = 1u64 << (failures - 1).min(5);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct Machine {
    phase: Phase,
    failures: u32,
    next_attempt: u64,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Machine {
            phase: Phase::Locking,
            failures: 0,
            next_attempt: 1,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// A lock surface is on screen. Only moves `Locking`.
    pub fn surfaces_up(&mut self) {
        if self.phase == Phase::Locking {
            self.phase = Phase::Idle;
        }
    }

    /// Whether [`Machine::submit`] would start an attempt now.
    pub fn can_submit(&self, now: Instant) -> bool {
        match self.phase {
            Phase::Idle => true,
            Phase::Failed { retry_at, .. } => now >= retry_at,
            Phase::Locking | Phase::Authenticating { .. } | Phase::Unlocked => false,
        }
    }

    /// Whether typing goes into the field: not while PAM holds the last
    /// password, and not once unlocked.
    pub fn accepts_input(&self) -> bool {
        matches!(self.phase, Phase::Idle | Phase::Failed { .. })
    }

    /// Start an attempt. `Some(id)` means the caller hands the password to
    /// PAM tagged `id`; `None` means not now (backoff, one already in flight,
    /// not locked yet, or already unlocked).
    pub fn submit(&mut self, now: Instant) -> Option<u64> {
        if !self.can_submit(now) {
            return None;
        }
        let attempt = self.next_attempt;
        self.next_attempt += 1;
        self.phase = Phase::Authenticating { attempt };
        Some(attempt)
    }

    /// PAM answered attempt `attempt`. Returns the grant only when it is the
    /// attempt in flight and PAM said yes; the machine is then `Unlocked`.
    /// A verdict for any other attempt is ignored.
    pub fn finish(&mut self, attempt: u64, verdict: Verdict, now: Instant) -> Option<Granted> {
        let Phase::Authenticating { attempt: current } = self.phase else {
            return None;
        };
        if current != attempt {
            return None;
        }
        let reason = match verdict {
            Verdict::Granted(granted) => {
                self.phase = Phase::Unlocked;
                return Some(granted);
            }
            Verdict::Denied => Reason::Wrong,
            Verdict::Unavailable => Reason::Unavailable,
        };
        self.failures = self.failures.saturating_add(1);
        self.phase = Phase::Failed {
            failures: self.failures,
            retry_at: now + backoff(self.failures),
            reason,
        };
        None
    }

    /// Time left before another attempt is allowed, if any.
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        match self.phase {
            Phase::Failed { retry_at, .. } if retry_at > now => Some(retry_at - now),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pam::granted_for_test;

    fn locked() -> (Machine, Instant) {
        let mut m = Machine::new();
        m.surfaces_up();
        (m, Instant::now())
    }

    #[test]
    fn starts_locking_and_cannot_submit_until_a_surface_is_up() {
        let mut m = Machine::new();
        let t = Instant::now();
        assert_eq!(m.phase(), Phase::Locking);
        assert_eq!(m.submit(t), None);
        assert!(!m.accepts_input());
        m.surfaces_up();
        assert_eq!(m.phase(), Phase::Idle);
    }

    #[test]
    fn success_unlocks() {
        let (mut m, t) = locked();
        let a = m.submit(t).unwrap();
        assert_eq!(m.phase(), Phase::Authenticating { attempt: a });
        assert!(!m.accepts_input(), "the password is with PAM");
        assert_eq!(m.submit(t), None, "one attempt at a time");
        let g = m.finish(a, Verdict::Granted(granted_for_test()), t);
        assert!(g.is_some());
        assert_eq!(m.phase(), Phase::Unlocked);
    }

    #[test]
    fn unlocked_is_terminal() {
        let (mut m, t) = locked();
        let a = m.submit(t).unwrap();
        m.finish(a, Verdict::Granted(granted_for_test()), t);
        m.surfaces_up();
        assert_eq!(m.submit(t), None);
        assert_eq!(m.finish(a, Verdict::Denied, t), None);
        assert_eq!(m.phase(), Phase::Unlocked);
    }

    #[test]
    fn failure_backs_off_and_grows() {
        let (mut m, t) = locked();
        let a = m.submit(t).unwrap();
        assert_eq!(m.finish(a, Verdict::Denied, t), None);
        assert_eq!(
            m.phase(),
            Phase::Failed {
                failures: 1,
                retry_at: t + Duration::from_secs(1),
                reason: Reason::Wrong
            }
        );
        assert!(
            m.accepts_input(),
            "the next password can be typed during the wait"
        );
        assert_eq!(m.submit(t), None, "inside the backoff");
        assert_eq!(m.retry_in(t), Some(Duration::from_secs(1)));
        let t1 = t + Duration::from_secs(1);
        let b = m.submit(t1).unwrap();
        assert_ne!(a, b);
        m.finish(b, Verdict::Denied, t1);
        assert_eq!(m.retry_in(t1), Some(Duration::from_secs(2)));
        assert_eq!(m.failures(), 2);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(0), Duration::ZERO);
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(5), Duration::from_secs(16));
        assert_eq!(backoff(6), MAX_BACKOFF);
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn errors_never_unlock() {
        let (mut m, mut t) = locked();
        for _ in 0..50 {
            t += MAX_BACKOFF;
            let a = m.submit(t).unwrap();
            assert_eq!(m.finish(a, Verdict::Unavailable, t), None);
            assert!(matches!(
                m.phase(),
                Phase::Failed {
                    reason: Reason::Unavailable,
                    ..
                }
            ));
        }
        assert_ne!(m.phase(), Phase::Unlocked);
    }

    #[test]
    fn a_stale_grant_is_ignored() {
        let (mut m, t) = locked();
        let a = m.submit(t).unwrap();
        m.finish(a, Verdict::Denied, t);
        let t1 = t + MAX_BACKOFF;
        let b = m.submit(t1).unwrap();
        // A grant for the earlier attempt arriving late does not unlock.
        assert_eq!(m.finish(a, Verdict::Granted(granted_for_test()), t1), None);
        assert_eq!(m.phase(), Phase::Authenticating { attempt: b });
        // Nor does a grant when nothing is in flight.
        let mut idle = Machine::new();
        idle.surfaces_up();
        assert_eq!(idle.finish(1, Verdict::Granted(granted_for_test()), t), None);
        assert_eq!(idle.phase(), Phase::Idle);
    }
}
