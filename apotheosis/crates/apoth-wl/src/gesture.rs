// SPDX-License-Identifier: AGPL-3.0-only
//! Pointer axis accumulation and touch gestures (T-09 §6): axis events use `axis_value120`
//! where present, accumulated to lines with a multiplier; `axis_stop` ends kinetic
//! accumulation. Touch: tap = click, drag = scroll.

/// Surface-local pixels that count as one line when a source gives only pixel values.
pub const PX_PER_LINE: f64 = 10.0;

/// Largest line multiplier honoured.
const MAX_MULTIPLIER: f64 = 1000.0;

fn sane_multiplier(m: f64, fallback: f64) -> f64 {
    if m.is_finite() {
        m.clamp(-MAX_MULTIPLIER, MAX_MULTIPLIER)
    } else {
        fallback
    }
}

/// Accumulates fractional lines for one axis.
#[derive(Clone, Copy, Debug)]
pub struct AxisAccum {
    multiplier: f64,
    acc: f64,
}

impl AxisAccum {
    /// New with a line multiplier (1.0 = one line per detent).
    pub fn new(multiplier: f64) -> AxisAccum {
        AxisAccum {
            multiplier: sane_multiplier(multiplier, 1.0),
            acc: 0.0,
        }
    }

    /// Change the multiplier; the accumulated remainder is kept.
    /// A non-finite value is ignored (the previous multiplier stays).
    pub fn set_multiplier(&mut self, m: f64) {
        self.multiplier = sane_multiplier(m, self.multiplier);
    }

    fn take_whole(&mut self) -> i32 {
        if !self.acc.is_finite() {
            // A NaN/inf input (e.g. a bad pixel delta) must not poison the accumulator.
            self.acc = 0.0;
            return 0;
        }
        let whole = self.acc.trunc();
        self.acc -= whole;
        whole as i32
    }

    /// Add `axis_value120` (120 = one detent); returns whole lines now due (signed).
    pub fn push_value120(&mut self, v120: i32) -> i32 {
        self.acc += f64::from(v120) / 120.0 * self.multiplier;
        self.take_whole()
    }

    /// Add a pixel value (compositors without value120, touchpads); returns whole lines due.
    pub fn push_px(&mut self, px: f64) -> i32 {
        self.acc += px / PX_PER_LINE * self.multiplier;
        self.take_whole()
    }

    /// `axis_stop`: drop the remainder.
    pub fn stop(&mut self) {
        self.acc = 0.0;
    }

    /// The fractional remainder (for tests and kinetic scroll displays).
    pub fn remainder(&self) -> f64 {
        self.acc
    }
}

/// Movement in logical pixels before a touch becomes a scroll.
pub const TAP_SLOP: f64 = 10.0;
/// Longest touch that still counts as a tap.
pub const TAP_MAX_MS: u32 = 500;

#[derive(Clone, Copy, Debug)]
struct Active {
    id: i32,
    start: (f64, f64),
    last: (f64, f64),
    t0: u32,
    scrolling: bool,
    poisoned: bool,
}

/// What a touch update produced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
    /// A tap at a position (logical px).
    Tap {
        /// X.
        x: f64,
        /// Y.
        y: f64,
    },
    /// Drag-scroll delta in logical px (finger movement; the consumer maps direction).
    Scroll {
        /// X delta.
        dx: f64,
        /// Y delta.
        dy: f64,
    },
}

/// Single-finger tap/drag recognizer. A second finger poisons the gesture: nothing is
/// emitted until all fingers lift (multi-touch is not a v1 gesture).
#[derive(Debug, Default)]
pub struct TouchTracker {
    active: Option<Active>,
    fingers: u32,
}

impl TouchTracker {
    /// New.
    pub fn new() -> TouchTracker {
        TouchTracker::default()
    }

    /// Finger down. `time_ms` is the Wayland event time (only differences are used).
    pub fn down(&mut self, id: i32, x: f64, y: f64, time_ms: u32) {
        self.fingers += 1;
        match &mut self.active {
            Some(a) => a.poisoned = true,
            None => {
                self.active = Some(Active {
                    id,
                    start: (x, y),
                    last: (x, y),
                    t0: time_ms,
                    scrolling: false,
                    poisoned: false,
                })
            }
        }
    }

    /// Finger moved.
    pub fn motion(&mut self, id: i32, x: f64, y: f64) -> Option<Gesture> {
        let a = self.active.as_mut()?;
        if a.id != id || a.poisoned {
            return None;
        }
        let from = if a.scrolling {
            a.last
        } else {
            let (dx, dy) = (x - a.start.0, y - a.start.1);
            if dx.hypot(dy) < TAP_SLOP {
                return None;
            }
            a.scrolling = true;
            a.start
        };
        a.last = (x, y);
        Some(Gesture::Scroll {
            dx: x - from.0,
            dy: y - from.1,
        })
    }

    /// Finger up.
    pub fn up(&mut self, id: i32, time_ms: u32) -> Option<Gesture> {
        self.fingers = self.fingers.saturating_sub(1);
        let a = self.active?;
        if a.poisoned {
            // A poisoned gesture stays dead until the last finger lifts.
            if self.fingers == 0 {
                self.active = None;
            } else if a.id == id {
                self.active = Some(Active { id: -1, ..a });
            }
            return None;
        }
        if a.id != id {
            return None;
        }
        self.active = None;
        if a.scrolling || time_ms.wrapping_sub(a.t0) > TAP_MAX_MS {
            return None;
        }
        Some(Gesture::Tap {
            x: a.start.0,
            y: a.start.1,
        })
    }

    /// Compositor cancelled the touch sequence.
    pub fn cancel(&mut self) {
        self.active = None;
        self.fingers = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_multiplier_cannot_kill_scrolling() {
        let mut a = AxisAccum::new(f64::NAN);
        assert_eq!(a.push_value120(120), 1);
        a.set_multiplier(f64::INFINITY);
        assert_eq!(a.push_value120(120), 1);
        assert_eq!(a.push_px(f64::NAN), 0);
        assert_eq!(a.push_value120(120), 1);
    }

    #[test]
    fn value120_accumulates_to_lines() {
        let mut a = AxisAccum::new(1.0);
        assert_eq!(a.push_value120(120), 1);
        assert_eq!(a.push_value120(-120), -1);
        // high-resolution wheel: 30 + 30 + 30 + 30 = one line
        assert_eq!(a.push_value120(30), 0);
        assert_eq!(a.push_value120(30), 0);
        assert_eq!(a.push_value120(30), 0);
        assert_eq!(a.push_value120(30), 1);
    }

    #[test]
    fn multiplier_scales_lines() {
        let mut a = AxisAccum::new(3.0);
        assert_eq!(a.push_value120(120), 3);
        a.set_multiplier(0.5);
        assert_eq!(a.push_value120(120), 0);
        assert_eq!(a.push_value120(120), 1);
    }

    #[test]
    fn px_and_stop() {
        let mut a = AxisAccum::new(1.0);
        assert_eq!(a.push_px(25.0), 2);
        assert!((a.remainder() - 0.5).abs() < 1e-9);
        a.stop();
        assert_eq!(a.remainder(), 0.0);
        assert_eq!(a.push_px(-25.0), -2);
    }

    #[test]
    fn short_still_touch_is_a_tap() {
        let mut t = TouchTracker::new();
        t.down(0, 5.0, 6.0, 1000);
        assert_eq!(t.motion(0, 7.0, 8.0), None);
        assert_eq!(t.up(0, 1100), Some(Gesture::Tap { x: 5.0, y: 6.0 }));
    }

    #[test]
    fn long_touch_is_not_a_tap() {
        let mut t = TouchTracker::new();
        t.down(0, 0.0, 0.0, 1000);
        assert_eq!(t.up(0, 1000 + TAP_MAX_MS + 1), None);
    }

    #[test]
    fn drag_scrolls_without_tap() {
        let mut t = TouchTracker::new();
        t.down(0, 0.0, 0.0, 0);
        // crossing the slop emits the whole displacement from the start
        assert_eq!(
            t.motion(0, 0.0, 12.0),
            Some(Gesture::Scroll { dx: 0.0, dy: 12.0 })
        );
        assert_eq!(
            t.motion(0, 0.0, 20.0),
            Some(Gesture::Scroll { dx: 0.0, dy: 8.0 })
        );
        assert_eq!(t.up(0, 50), None);
    }

    #[test]
    fn second_finger_poisons_until_all_lift() {
        let mut t = TouchTracker::new();
        t.down(0, 0.0, 0.0, 0);
        t.down(1, 50.0, 50.0, 10);
        assert_eq!(t.up(0, 20), None);
        assert_eq!(t.up(1, 30), None);
        // a fresh single touch works again
        t.down(2, 1.0, 1.0, 100);
        assert_eq!(t.up(2, 120), Some(Gesture::Tap { x: 1.0, y: 1.0 }));
    }

    #[test]
    fn cancel_clears() {
        let mut t = TouchTracker::new();
        t.down(0, 0.0, 0.0, 0);
        t.cancel();
        assert_eq!(t.up(0, 10), None);
    }
}
