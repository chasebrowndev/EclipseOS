// SPDX-License-Identifier: AGPL-3.0-only

//! Rolling context for questions that span screens (scroll, page turn).
//!
//! Holds **redacted OCR text only**, one line at a time, from recent reads:
//! never pixels, never anything unredacted, never logged (the lines are not
//! passed to `tracing` anywhere). It is bounded three ways, so it cannot grow
//! into a second copy of everything the user looked at:
//!
//! - a line cap and a byte cap, oldest dropped first;
//! - a TTL: a line not seen again within `ttl_ms` is forgotten, which is also
//!   what makes an idle screen stop contributing;
//! - overlap de-duplication: a new screen whose top lines are the buffer's
//!   bottom lines (a scroll that moved by part of a screen) appends only what
//!   is new, so a stitched question is not repeated per frame.
//!
//! The model sees this as a secondary section; the current screen stays
//! primary (see `answer::user_prompt`).

use std::collections::VecDeque;

/// The three bounds, from `oracle-eyes.kdl`'s `context` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_lines: usize,
    pub max_bytes: usize,
    pub ttl_ms: u64,
}

#[derive(Debug)]
pub struct Rolling {
    limits: Limits,
    /// (last seen, text), oldest first.
    lines: VecDeque<(u64, String)>,
    bytes: usize,
}

impl Rolling {
    pub fn new(limits: Limits) -> Rolling {
        Rolling {
            limits,
            lines: VecDeque::new(),
            bytes: 0,
        }
    }

    /// Take in one screen's lines and return the context it adds to: the
    /// buffered lines that were there **before** this screen, minus the
    /// overlap the screen repeats. Taken before merging, so the caller's
    /// prompt never repeats the current screen back to the model.
    pub fn absorb<S: AsRef<str>>(&mut self, screen: &[S], now_ms: u64) -> Vec<String> {
        self.expire(now_ms);
        let overlap = self.overlap(screen);
        let before: Vec<String> = self
            .lines
            .iter()
            .take(self.lines.len() - overlap)
            .map(|(_, t)| t.clone())
            .collect();

        // The overlap was seen again: refresh it, so a page still on screen
        // does not age out from under the user.
        let start = self.lines.len() - overlap;
        for entry in self.lines.iter_mut().skip(start) {
            entry.0 = now_ms;
        }
        for s in screen.iter().skip(overlap) {
            let t = s.as_ref();
            if t.trim().is_empty() {
                continue;
            }
            self.bytes += t.len();
            self.lines.push_back((now_ms, t.to_string()));
        }
        self.trim();
        before
    }

    /// Lines of the buffer, oldest first. Test inspection only: the prompt
    /// gets the pre-screen context `absorb` returns, never the whole buffer,
    /// so the current screen is not sent back to the model.
    #[cfg(test)]
    pub fn lines(&self) -> Vec<String> {
        self.lines.iter().map(|(_, t)| t.clone()).collect()
    }

    /// New bounds, live (a config reload). Shrinking trims at once.
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
        self.trim();
    }

    /// Drop everything. Automatic mode's reset and any new question use this.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
    }

    /// Length of the longest tail of the buffer that is the head of `screen`.
    fn overlap<S: AsRef<str>>(&self, screen: &[S]) -> usize {
        let max = self.lines.len().min(screen.len());
        (1..=max)
            .rev()
            .find(|&k| {
                let tail = self.lines.iter().skip(self.lines.len() - k);
                tail.zip(screen.iter().take(k))
                    .all(|((_, t), s)| t == s.as_ref())
            })
            .unwrap_or(0)
    }

    fn expire(&mut self, now_ms: u64) {
        let ttl = self.limits.ttl_ms;
        let before = self.lines.len();
        self.lines
            .retain(|(at, _)| now_ms.saturating_sub(*at) < ttl);
        if self.lines.len() != before {
            self.bytes = self.lines.iter().map(|(_, t)| t.len()).sum();
        }
    }

    fn trim(&mut self) {
        while self.lines.len() > self.limits.max_lines || self.bytes > self.limits.max_bytes {
            match self.lines.pop_front() {
                Some((_, t)) => self.bytes -= t.len(),
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lim(max_lines: usize, max_bytes: usize, ttl_ms: u64) -> Limits {
        Limits {
            max_lines,
            max_bytes,
            ttl_ms,
        }
    }

    #[test]
    fn a_scroll_appends_only_the_new_lines() {
        let mut r = Rolling::new(lim(100, 10_000, 60_000));
        assert!(r.absorb(&["q1", "a)", "b)"], 0).is_empty());
        // The second screen repeats "a)" and "b)" at its top, then goes on.
        let before = r.absorb(&["a)", "b)", "c)"], 10);
        assert_eq!(before, ["q1"], "the repeated lines are not context");
        assert_eq!(r.lines(), ["q1", "a)", "b)", "c)"]);
    }

    #[test]
    fn a_screen_already_held_adds_nothing() {
        let mut r = Rolling::new(lim(100, 10_000, 60_000));
        r.absorb(&["x", "y"], 0);
        r.absorb(&["x", "y"], 5);
        assert_eq!(r.lines(), ["x", "y"]);
    }

    #[test]
    fn the_line_cap_drops_the_oldest() {
        let mut r = Rolling::new(lim(3, 10_000, 60_000));
        r.absorb(&["one", "two"], 0);
        r.absorb(&["three", "four"], 1);
        assert_eq!(r.lines(), ["two", "three", "four"]);
    }

    #[test]
    fn the_byte_cap_drops_the_oldest_too() {
        let mut r = Rolling::new(lim(100, 10, 60_000));
        r.absorb(&["aaaaa", "bbbbb"], 0);
        r.absorb(&["ccccc"], 1);
        assert_eq!(r.lines(), ["bbbbb", "ccccc"]);
    }

    #[test]
    fn lines_expire_after_the_ttl() {
        let mut r = Rolling::new(lim(100, 10_000, 1000));
        r.absorb(&["old"], 0);
        let before = r.absorb(&["new"], 1000);
        assert!(before.is_empty(), "a line exactly one TTL old is gone");
        assert_eq!(r.lines(), ["new"]);
    }

    #[test]
    fn a_repeated_screen_refreshes_its_lines() {
        let mut r = Rolling::new(lim(100, 10_000, 1000));
        r.absorb(&["keep"], 0);
        r.absorb(&["keep"], 900);
        r.absorb(&["other"], 1500);
        assert_eq!(
            r.lines(),
            ["keep", "other"],
            "refreshed at 900, not expired"
        );
    }

    #[test]
    fn clear_forgets_everything() {
        let mut r = Rolling::new(lim(100, 10_000, 60_000));
        r.absorb(&["a", "b"], 0);
        r.clear();
        assert!(r.lines().is_empty());
        assert!(r.absorb(&["c"], 1).is_empty());
    }

    #[test]
    fn blank_lines_are_not_kept() {
        let mut r = Rolling::new(lim(100, 10_000, 60_000));
        r.absorb(&["a", "   ", "b"], 0);
        assert_eq!(r.lines(), ["a", "b"]);
    }
}
