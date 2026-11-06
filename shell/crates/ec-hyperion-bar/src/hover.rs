// SPDX-License-Identifier: AGPL-3.0-only
//! The hover-preview debounce (`mode "de"`, COMP-05 §7.1): the pure half.
//!
//! The compositor draws the preview card; the bar only says *when* and
//! *where*. It says it once: after the pointer has rested on one window's chip
//! for [`DELAY`], one `show_window_preview` goes out. Moving along the chip
//! sends nothing (the chip's enter event arrives once, not per motion), moving
//! onto another chip restarts the wait for that one, and leaving the chip — or
//! the bar — sends one `hide_window_preview` if a card was shown.

use std::time::{Duration, Instant};

/// How long the pointer must rest before the card is asked for.
pub const DELAY: Duration = Duration::from_millis(ec_ui::tokens::bar::PREVIEW_DELAY_MS);

/// What the bar must tell the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Show(u64),
    Hide,
}

#[derive(Debug, Default)]
pub struct Hover {
    /// The chip under the pointer, and since when.
    over: Option<(u64, Instant)>,
    /// The window whose card is up (or whose request is out).
    shown: Option<u64>,
}

impl Hover {
    /// The pointer came onto the chip of `handle`.
    pub fn enter(&mut self, handle: u64, now: Instant) -> Option<Call> {
        if self.over.is_some_and(|(h, _)| h == handle) {
            return None;
        }
        self.over = Some((handle, now));
        // Onto a different chip: the old card goes now, the new one waits.
        self.take_shown()
    }

    /// The pointer left its chip (or the bar). Hides a card that is up.
    pub fn leave(&mut self) -> Option<Call> {
        self.over = None;
        self.take_shown()
    }

    /// A tick of the debounce clock. `Show` once, when the wait is over.
    pub fn due(&mut self, now: Instant) -> Option<Call> {
        let (handle, since) = self.over?;
        if self.shown.is_some() || now.saturating_duration_since(since) < DELAY {
            return None;
        }
        self.shown = Some(handle);
        Some(Call::Show(handle))
    }

    /// A click, a menu or anything else that takes the card down: forget the
    /// hover too, so a card is not asked for again until the pointer re-enters.
    pub fn dismiss(&mut self) -> Option<Call> {
        let call = self.take_shown();
        self.over = None;
        call
    }

    /// Whether the debounce clock needs to run: the pointer is on a chip and
    /// its card has not been asked for.
    pub fn pending(&self) -> bool {
        self.over.is_some() && self.shown.is_none()
    }

    /// The chip under the pointer.
    pub fn over(&self) -> Option<u64> {
        self.over.map(|(h, _)| h)
    }

    pub fn shown(&self) -> Option<u64> {
        self.shown
    }

    fn take_shown(&mut self) -> Option<Call> {
        self.shown.take().map(|_| Call::Hide)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_card_is_asked_for_once_after_the_delay_and_not_before() {
        let t0 = Instant::now();
        let mut h = Hover::default();
        assert_eq!(h.enter(7, t0), None);
        assert!(h.pending());
        assert_eq!(h.due(at(100, t0)), None);
        assert_eq!(h.due(at(399, t0)), None);
        assert_eq!(h.due(at(400, t0)), Some(Call::Show(7)));
        // Ticks after that, and a repeated enter, ask for nothing.
        assert_eq!(h.due(at(900, t0)), None);
        assert_eq!(h.enter(7, at(950, t0)), None);
        assert_eq!(h.due(at(2000, t0)), None);
        assert!(!h.pending());
    }

    #[test]
    fn leaving_before_the_delay_asks_for_nothing_at_all() {
        let t0 = Instant::now();
        let mut h = Hover::default();
        h.enter(7, t0);
        assert_eq!(h.leave(), None);
        assert_eq!(h.due(at(1000, t0)), None);
        assert!(!h.pending());
    }

    #[test]
    fn leaving_a_shown_card_hides_it_once() {
        let t0 = Instant::now();
        let mut h = Hover::default();
        h.enter(7, t0);
        h.due(at(500, t0));
        assert_eq!(h.leave(), Some(Call::Hide));
        assert_eq!(h.leave(), None);
        assert_eq!(h.shown(), None);
    }

    #[test]
    fn moving_to_another_chip_hides_the_old_card_and_restarts_the_wait() {
        let t0 = Instant::now();
        let mut h = Hover::default();
        h.enter(7, t0);
        h.due(at(500, t0));
        assert_eq!(h.enter(8, at(600, t0)), Some(Call::Hide));
        assert_eq!(h.due(at(900, t0)), None);
        assert_eq!(h.due(at(1000, t0)), Some(Call::Show(8)));
    }

    #[test]
    fn a_click_takes_the_card_down_and_it_does_not_come_back_unprompted() {
        let t0 = Instant::now();
        let mut h = Hover::default();
        h.enter(7, t0);
        h.due(at(500, t0));
        assert_eq!(h.dismiss(), Some(Call::Hide));
        assert_eq!(h.due(at(5000, t0)), None);
        assert_eq!(h.over(), None);
    }
}
