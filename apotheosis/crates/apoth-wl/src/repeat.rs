// SPDX-License-Identifier: AGPL-3.0-only
//! Key repeat timing (T-09 §6). The compositor only reports `repeat_info`; repeats are
//! generated here, on our clock. Cancelled on release, on focus loss and when another
//! repeating key goes down. `keyboard.repeat-rate` / `keyboard.repeat-delay` override.

use crate::clock::Stamp;

/// Rate and delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepeatInfo {
    /// Repeats per second; 0 disables repeat.
    pub rate_hz: u32,
    /// Delay before the first repeat.
    pub delay_ms: u32,
}

/// Used when the compositor sent no `repeat_info` (an unspecified default; T-09 is silent).
pub const FALLBACK: RepeatInfo = RepeatInfo {
    rate_hz: 25,
    delay_ms: 600,
};

/// Highest repeat rate honoured; a hostile `repeat_info` cannot exceed it.
pub const MAX_RATE_HZ: u32 = 1000;

/// Upper bound of repeats reported by one poll, so a stalled loop never floods the pty.
pub const MAX_CATCH_UP: u32 = 8;

/// Evdev keycodes of keys that never repeat when no keymap-derived filter is installed:
/// shifts, controls, alts, supers, caps/num/scroll lock.
const NON_REPEATING: [u32; 13] = [29, 42, 54, 56, 58, 69, 70, 97, 100, 125, 126, 464, 465];

/// Default repeat filter: every key except modifiers and locks. The consumer replaces it
/// with `xkb_keymap_key_repeats` once it has parsed the keymap.
pub fn default_key_repeats(evdev_key: u32) -> bool {
    !NON_REPEATING.contains(&evdev_key)
}

/// A repeat that is due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tick {
    /// Evdev keycode.
    pub key: u32,
    /// How many repeat events are due (1 normally; more if the loop was late).
    pub count: u32,
}

#[derive(Clone, Copy, Debug)]
struct Held {
    key: u32,
    next: Stamp,
}

/// The repeat state machine for one seat.
#[derive(Debug, Default)]
pub struct Repeater {
    from_compositor: Option<RepeatInfo>,
    override_rate: Option<u32>,
    override_delay: Option<u32>,
    held: Option<Held>,
}

impl Repeater {
    /// New, with no compositor info and no override.
    pub fn new() -> Repeater {
        Repeater::default()
    }

    /// Record `wl_keyboard.repeat_info`.
    pub fn set_compositor(&mut self, info: RepeatInfo) {
        self.from_compositor = Some(info);
    }

    /// Set the config overrides.
    pub fn set_overrides(&mut self, rate_hz: Option<u32>, delay_ms: Option<u32>) {
        self.override_rate = rate_hz;
        self.override_delay = delay_ms;
    }

    /// The rate and delay in force.
    pub fn effective(&self) -> RepeatInfo {
        let base = self.from_compositor.unwrap_or(FALLBACK);
        RepeatInfo {
            rate_hz: self.override_rate.unwrap_or(base.rate_hz).min(MAX_RATE_HZ),
            delay_ms: self.override_delay.unwrap_or(base.delay_ms),
        }
    }

    fn interval_ns(rate_hz: u32) -> u64 {
        1_000_000_000 / u64::from(rate_hz.max(1))
    }

    /// A repeating key went down at `now`. Replaces any key already repeating. Returns the
    /// first deadline, or `None` when repeat is disabled.
    pub fn press(&mut self, key: u32, now: Stamp) -> Option<Stamp> {
        let info = self.effective();
        if info.rate_hz == 0 {
            self.held = None;
            return None;
        }
        let next = now.plus_ms(u64::from(info.delay_ms));
        self.held = Some(Held { key, next });
        Some(next)
    }

    /// A key went up. Cancels repeat only if it is the repeating key.
    pub fn release(&mut self, key: u32) {
        if self.held.is_some_and(|h| h.key == key) {
            self.held = None;
        }
    }

    /// Stop repeating (focus loss, seat capability loss).
    pub fn cancel(&mut self) {
        self.held = None;
    }

    /// The next deadline, if a key is repeating.
    pub fn next_deadline(&self) -> Option<Stamp> {
        self.held.map(|h| h.next)
    }

    /// The key currently repeating.
    pub fn held_key(&self) -> Option<u32> {
        self.held.map(|h| h.key)
    }

    /// Collect repeats due at `now` and advance the deadline.
    pub fn poll(&mut self, now: Stamp) -> Option<Tick> {
        let rate = self.effective().rate_hz;
        let held = self.held.as_mut()?;
        if now < held.next || rate == 0 {
            return None;
        }
        let interval = Self::interval_ns(rate);
        let behind = now.0 - held.next.0;
        let due = behind / interval + 1;
        let count = due.min(u64::from(MAX_CATCH_UP)) as u32;
        // Skip whatever exceeds the cap so we resume on the grid instead of replaying it.
        held.next = Stamp(held.next.0.saturating_add(due.saturating_mul(interval)));
        Some(Tick {
            key: held.key,
            count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absurd_rate_is_clamped_and_never_panics() {
        let mut r = Repeater::new();
        r.set_compositor(RepeatInfo {
            rate_hz: 2_000_000_000,
            delay_ms: 300,
        });
        assert_eq!(r.effective().rate_hz, MAX_RATE_HZ);
        r.press(30, Stamp(0));
        let t = r.poll(Stamp(1_000_000_000)).unwrap();
        assert!(t.count >= 1);
        assert!(r.next_deadline().unwrap().0 > 1_000_000_000);
    }

    fn rep(rate: u32, delay: u32) -> Repeater {
        let mut r = Repeater::new();
        r.set_compositor(RepeatInfo {
            rate_hz: rate,
            delay_ms: delay,
        });
        r
    }

    #[test]
    fn first_repeat_after_delay_then_on_interval() {
        let mut r = rep(20, 500); // 50 ms interval
        let t0 = Stamp(1_000_000_000);
        assert_eq!(r.press(30, t0), Some(t0.plus_ms(500)));
        assert_eq!(r.poll(t0.plus_ms(499)), None);
        assert_eq!(r.poll(t0.plus_ms(500)), Some(Tick { key: 30, count: 1 }));
        assert_eq!(r.next_deadline(), Some(t0.plus_ms(550)));
        assert_eq!(r.poll(t0.plus_ms(549)), None);
        assert_eq!(r.poll(t0.plus_ms(550)), Some(Tick { key: 30, count: 1 }));
    }

    #[test]
    fn late_polls_catch_up_capped() {
        let mut r = rep(20, 0);
        let t0 = Stamp(0).plus_ms(1000);
        r.press(1, t0);
        // 3 intervals late: due at +0, +50, +100, +150 = 4 repeats
        assert_eq!(r.poll(t0.plus_ms(150)), Some(Tick { key: 1, count: 4 }));
        // very late: capped, and the deadline lands back on the grid in the future
        let t1 = t0.plus_ms(100_000);
        let tick = r.poll(t1).unwrap();
        assert_eq!(tick.count, MAX_CATCH_UP);
        assert!(r.next_deadline().unwrap() > t1);
    }

    #[test]
    fn release_of_other_key_does_not_cancel() {
        let mut r = rep(25, 100);
        r.press(10, Stamp(1));
        r.release(11);
        assert_eq!(r.held_key(), Some(10));
        r.release(10);
        assert_eq!(r.held_key(), None);
        assert_eq!(r.next_deadline(), None);
    }

    #[test]
    fn another_repeating_key_replaces() {
        let mut r = rep(25, 100);
        r.press(10, Stamp(1));
        r.press(11, Stamp(2));
        assert_eq!(r.held_key(), Some(11));
    }

    #[test]
    fn cancel_on_focus_loss() {
        let mut r = rep(25, 100);
        r.press(10, Stamp(1));
        r.cancel();
        assert_eq!(r.poll(Stamp(u64::MAX / 2)), None);
    }

    #[test]
    fn overrides_win_and_rate_zero_disables() {
        let mut r = rep(25, 600);
        r.set_overrides(Some(10), Some(200));
        assert_eq!(
            r.effective(),
            RepeatInfo {
                rate_hz: 10,
                delay_ms: 200
            }
        );
        r.set_overrides(Some(0), None);
        assert_eq!(r.press(1, Stamp(5)), None);
        assert_eq!(r.held_key(), None);
        r.set_overrides(None, None);
        assert_eq!(
            r.effective(),
            RepeatInfo {
                rate_hz: 25,
                delay_ms: 600
            }
        );
    }

    #[test]
    fn fallback_when_compositor_is_silent() {
        let r = Repeater::new();
        assert_eq!(r.effective(), FALLBACK);
    }

    #[test]
    fn modifiers_do_not_repeat_by_default() {
        assert!(!default_key_repeats(42)); // left shift
        assert!(!default_key_repeats(29)); // left ctrl
        assert!(default_key_repeats(30)); // a
        assert!(default_key_repeats(14)); // backspace
    }
}
