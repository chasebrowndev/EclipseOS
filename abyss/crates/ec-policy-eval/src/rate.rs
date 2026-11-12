// SPDX-License-Identifier: AGPL-3.0-only
//! Grant `rate` constraints, counted (S-01 §4, COMP-08 §10 step 5).
//!
//! A [`Window`] answers "would `n` more events exceed `count` per `per_ms`?"
//! with no allocation and no clock of its own: the caller passes the time.
//!
//! The window is a ring of [`SLOTS`] sub-counters covering `per_ms` plus up
//! to one slot of older history. Counting that extra slot means the total is
//! never *below* the true number of events in the last `per_ms`, so the
//! limiter can refuse slightly early and can never admit late. That is the
//! only direction it is allowed to be wrong in.
//!
//! A clock that steps backwards does not open the window again: time is
//! taken as the latest instant seen.

/// Sub-counters per window. Eight bounds the early refusal to one eighth of
/// the period.
pub const SLOTS: usize = 8;

/// The events a grant may rate-limit that the compositor can count.
///
/// A grant naming any other event is one the compositor cannot enforce, and
/// it is refused at admission rather than admitted with a constraint
/// silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Every acting request that reaches step 5.
    Requests,
    /// Input events the request would deliver to a client.
    InputEvents,
}

impl Event {
    pub fn parse(name: &str) -> Option<Event> {
        match name {
            "requests" => Some(Event::Requests),
            "input_events" => Some(Event::InputEvents),
            _ => None,
        }
    }
}

/// One rate constraint and its running count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    limit: u32,
    /// Width of one slot in ms, at least 1.
    slot_ms: u64,
    /// `counts[i]` belongs to slot `head - i`.
    counts: [u32; SLOTS + 1],
    /// Index of the newest slot (`now_ms / slot_ms` at the last touch).
    head: u64,
}

impl Window {
    /// `None` for a constraint that cannot be counted: zero events or a zero
    /// period. The caller refuses the grant.
    pub fn new(count: u32, per_ms: u64) -> Option<Window> {
        if count == 0 || per_ms == 0 {
            return None;
        }
        Some(Window {
            limit: count,
            slot_ms: per_ms.div_ceil(SLOTS as u64),
            counts: [0; SLOTS + 1],
            head: 0,
        })
    }

    /// Slot of `now_ms`, never earlier than the newest slot already seen.
    fn slot(&self, now_ms: u64) -> u64 {
        (now_ms / self.slot_ms).max(self.head)
    }

    /// Events still counted at slot `at`, without changing anything.
    fn total_at(&self, at: u64) -> u64 {
        let gone = at - self.head;
        self.counts
            .iter()
            .enumerate()
            .filter(|(i, _)| (*i as u64) + gone <= SLOTS as u64)
            .map(|(_, c)| u64::from(*c))
            .sum()
    }

    /// Whether `n` more events fit at `now_ms`. `Err` is how long to wait, in
    /// ms, before they would; a request larger than the whole limit never
    /// fits and is told the full period.
    pub fn fits(&self, now_ms: u64, n: u32) -> Result<(), u64> {
        let at = self.slot(now_ms);
        let room = u64::from(self.limit).saturating_sub(self.total_at(at));
        if u64::from(n) <= room {
            return Ok(());
        }
        if n > self.limit {
            return Err(self.slot_ms * (SLOTS as u64 + 1));
        }
        // Walk forward one slot at a time until enough has aged out. At most
        // SLOTS + 1 steps: by then nothing is counted.
        for ahead in 1..=(SLOTS as u64 + 1) {
            let room = u64::from(self.limit).saturating_sub(self.total_at(at + ahead));
            if u64::from(n) <= room {
                let when = (at + ahead).saturating_mul(self.slot_ms);
                return Err(when.saturating_sub(now_ms).max(1));
            }
        }
        Err(self.slot_ms * (SLOTS as u64 + 1))
    }

    /// Counts `n` events at `now_ms`. Call only after [`fits`](Self::fits)
    /// said yes; the count saturates rather than wraps.
    pub fn record(&mut self, now_ms: u64, n: u32) {
        let at = self.slot(now_ms);
        let gone = at - self.head;
        if gone > 0 {
            let shift = usize::try_from(gone).unwrap_or(usize::MAX).min(SLOTS + 1);
            self.counts.copy_within(0..SLOTS + 1 - shift, shift);
            self.counts[..shift].fill(0);
            self.head = at;
        }
        self.counts[0] = self.counts[0].saturating_add(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spend(w: &mut Window, now: u64, n: u32) -> Result<(), u64> {
        w.fits(now, n)?;
        w.record(now, n);
        Ok(())
    }

    #[test]
    fn uncountable_constraints_are_refused() {
        assert!(Window::new(0, 1000).is_none());
        assert!(Window::new(5, 0).is_none());
        assert_eq!(Event::parse("requests"), Some(Event::Requests));
        assert_eq!(Event::parse("input_events"), Some(Event::InputEvents));
        assert_eq!(Event::parse("capture_frames"), None);
    }

    #[test]
    fn the_limit_holds_inside_one_period() {
        let mut w = Window::new(3, 800).unwrap();
        for t in [0, 10, 20] {
            assert_eq!(spend(&mut w, t, 1), Ok(()));
        }
        assert!(spend(&mut w, 30, 1).is_err());
        assert!(spend(&mut w, 799, 1).is_err());
    }

    #[test]
    fn never_more_than_the_limit_in_any_sliding_period() {
        // Brute force: hammer the window at every millisecond and check the
        // admitted events against the true sliding count.
        let (limit, per) = (5u32, 80u64);
        let mut w = Window::new(limit, per).unwrap();
        let mut admitted: Vec<u64> = Vec::new();
        for t in 0..2_000u64 {
            if t % 3 != 0 && spend(&mut w, t, 1).is_ok() {
                admitted.push(t);
            }
            let inside = admitted.iter().filter(|&&a| t - a < per).count();
            assert!(
                inside <= limit as usize,
                "{inside} events in the period ending at {t}"
            );
        }
        // And it is a limiter, not a wall: the budget does come back.
        assert!(admitted.len() > 2 * limit as usize);
    }

    #[test]
    fn retry_after_is_honest() {
        let mut w = Window::new(2, 800).unwrap();
        assert_eq!(spend(&mut w, 0, 2), Ok(()));
        let wait = spend(&mut w, 100, 1).unwrap_err();
        // Waiting less is still refused; waiting that long is admitted.
        assert!(spend(&mut w, 100 + wait - 1, 1).is_err());
        assert_eq!(spend(&mut w, 100 + wait, 1), Ok(()));
    }

    #[test]
    fn a_request_larger_than_the_limit_never_fits() {
        let w = Window::new(4, 400).unwrap();
        assert!(w.fits(0, 5).is_err());
        assert!(w.fits(1_000_000, 5).is_err());
        assert_eq!(w.fits(0, 4), Ok(()));
    }

    #[test]
    fn a_clock_stepping_back_does_not_refill() {
        let mut w = Window::new(2, 800).unwrap();
        assert_eq!(spend(&mut w, 10_000, 2), Ok(()));
        assert!(spend(&mut w, 0, 1).is_err());
        assert!(spend(&mut w, 9_000, 1).is_err());
    }

    #[test]
    fn a_long_silence_empties_the_window() {
        let mut w = Window::new(2, 800).unwrap();
        assert_eq!(spend(&mut w, 0, 2), Ok(()));
        assert_eq!(spend(&mut w, u64::MAX / 2, 2), Ok(()));
    }

    #[test]
    fn a_period_shorter_than_the_slot_count_still_counts() {
        let mut w = Window::new(1, 3).unwrap();
        assert_eq!(spend(&mut w, 0, 1), Ok(()));
        assert!(spend(&mut w, 1, 1).is_err());
        assert_eq!(spend(&mut w, 100, 1), Ok(()));
    }
}
