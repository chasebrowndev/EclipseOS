// SPDX-License-Identifier: AGPL-3.0-only

//! Damped springs for Fog's motion (FOG §Visual design, "Motion"; §Performance
//! model: 120-200 ms, starting on the input event).
//!
//! Stepped in closed form, so any frame interval is stable: a long frame
//! lands where the spring would have been, it never explodes. A spring only
//! moves the picture; state changes happen at once and input is never held
//! for it.

use std::time::Duration;

/// Response shape: angular frequency and damping ratio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// Natural angular frequency, rad/s.
    pub omega: f32,
    /// Damping ratio: 1 is critical, below 1 overshoots a little.
    pub zeta: f32,
}

impl Params {
    /// Critically damped, settled (within 1%) after `settle`.
    pub fn critical(settle: Duration) -> Params {
        // (1 + wt) e^-wt = 0.01 at wt = 6.64.
        Params {
            omega: 6.64 / settle.as_secs_f32().max(1e-3),
            zeta: 1.0,
        }
    }

    /// A touch of overshoot: the "liquid" pop for sheets.
    pub fn bouncy(settle: Duration) -> Params {
        Params {
            omega: 6.0 / settle.as_secs_f32().max(1e-3),
            zeta: 0.72,
        }
    }
}

/// A value chasing a target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
}

/// Close enough to stop animating.
const REST: f32 = 1e-3;

impl Spring {
    /// At rest at `v`.
    pub const fn at(v: f32) -> Spring {
        Spring {
            value: v,
            velocity: 0.0,
            target: v,
        }
    }

    /// Move towards `target`, keeping the current velocity.
    pub fn to(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump to `target` (reduce-motion, or a first placement).
    pub fn snap(&mut self, target: f32) {
        *self = Spring::at(target);
    }

    pub fn settled(&self) -> bool {
        // Relative for large values (a cursor 10 000 rows down), where one
        // f32 step is near REST.
        let rest = REST.max(self.target.abs() * 1e-6);
        (self.value - self.target).abs() < rest && self.velocity.abs() < REST * 10.0
    }

    /// Advance by `dt`.
    pub fn step(&mut self, dt: Duration, p: Params) {
        if self.settled() {
            self.snap(self.target);
            return;
        }
        let t = dt.as_secs_f32().min(0.25);
        let (w, z) = (p.omega, p.zeta);
        let x0 = self.value - self.target;
        let v0 = self.velocity;
        let (x, v) = if z >= 1.0 {
            let e = (-w * t).exp();
            let c = v0 + w * x0;
            ((x0 + c * t) * e, (v0 - w * c * t) * e)
        } else {
            let wd = w * (1.0 - z * z).sqrt();
            let e = (-z * w * t).exp();
            let (s, c) = (wd * t).sin_cos();
            (
                e * (x0 * c + (v0 + z * w * x0) / wd * s),
                e * (v0 * c - (w * w * x0 + z * w * v0) / wd * s),
            )
        };
        self.value = self.target + x;
        self.velocity = v;
        if self.settled() {
            self.snap(self.target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);

    fn run(mut s: Spring, p: Params, dt: Duration, n: usize) -> (Spring, Option<usize>) {
        let mut done = None;
        for i in 0..n {
            s.step(dt, p);
            if done.is_none() && s.settled() {
                done = Some(i + 1);
            }
        }
        (s, done)
    }

    #[test]
    fn critical_settles_in_its_window_without_overshoot() {
        let p = Params::critical(Duration::from_millis(160));
        let mut s = Spring::at(0.0);
        s.to(1.0);
        let mut max: f32 = 0.0;
        for _ in 0..30 {
            s.step(FRAME, p);
            max = max.max(s.value);
        }
        assert!(max <= 1.0 + 1e-6, "overshot to {max}");
        let (_, done) = run(
            {
                let mut s = Spring::at(0.0);
                s.to(1.0);
                s
            },
            p,
            FRAME,
            60,
        );
        // 1% at 160 ms; fully at rest (0.1%) shortly after.
        let frames = done.unwrap();
        assert!((9..=16).contains(&frames), "{frames} frames");
    }

    #[test]
    fn bouncy_overshoots_a_little_and_settles() {
        let p = Params::bouncy(Duration::from_millis(180));
        let mut s = Spring::at(0.0);
        s.to(1.0);
        let mut max: f32 = 0.0;
        for _ in 0..60 {
            s.step(FRAME, p);
            max = max.max(s.value);
        }
        assert!(max > 1.0 && max < 1.06, "peak {max}");
        assert!(s.settled() && s.value == 1.0);
    }

    #[test]
    fn frame_rate_does_not_change_the_path() {
        let p = Params::critical(Duration::from_millis(160));
        let mut a = Spring::at(0.0);
        let mut b = Spring::at(0.0);
        a.to(1.0);
        b.to(1.0);
        for _ in 0..3 {
            a.step(Duration::from_millis(20), p);
        }
        b.step(Duration::from_millis(60), p);
        assert!(
            (a.value - b.value).abs() < 1e-4,
            "{} vs {}",
            a.value,
            b.value
        );
    }

    #[test]
    fn a_huge_frame_lands_at_rest_not_beyond() {
        let p = Params::bouncy(Duration::from_millis(120));
        let mut s = Spring::at(0.0);
        s.to(5.0);
        s.step(Duration::from_secs(10), p);
        assert!((s.value - 5.0).abs() < 0.05, "{}", s.value);
        s.step(Duration::from_secs(10), p);
        assert!(s.settled());
    }

    #[test]
    fn retargeting_keeps_momentum_and_snap_does_not() {
        let p = Params::critical(Duration::from_millis(160));
        let mut s = Spring::at(0.0);
        s.to(10.0);
        s.step(FRAME, p);
        let v = s.velocity;
        assert!(v > 0.0);
        s.to(0.0);
        assert_eq!(s.velocity, v);
        s.snap(3.0);
        assert!(s.settled() && s.value == 3.0);
    }
}
