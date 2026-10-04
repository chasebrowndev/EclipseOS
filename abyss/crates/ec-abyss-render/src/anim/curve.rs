// SPDX-License-Identifier: AGPL-3.0-only
//! The shape of one animation leg (COMP-02 §9).
//!
//! Every animated quantity is a *displacement* that decays to zero: a window
//! drawn 300px left of its target, at 0.92 of its size, at alpha −1 from
//! opaque. A leg answers one question: given the displacement `d0` and
//! velocity `v0` it was released with, what are they `elapsed` later? Both
//! answers are linear in `(d0, v0)`, so a leg is evaluated once per frame
//! into a [`Response`] and applied to every channel of every track on it —
//! no allocation, and no per-channel curve maths.
//!
//! Everything is a closed-form function of the clock. Nothing is integrated
//! frame by frame, so `sync` can run once per output per frame and every
//! output sees the same value, and a retarget reads the exact position and
//! velocity at that instant and starts a new leg from them: no jump.
//!
//! The tween curves are `ec_ui::motion`'s cubics, so a bar chip and a window
//! on the same curve move alike. `spring` is `ec_ui::motion`'s critically
//! damped spring with the same tuning; `bounce` is the underdamped sibling.

use std::time::{Duration, Instant};

pub use ec_abyss_config::animations::Curve;

/// The residual a spring is allowed at `duration`, as a fraction of the
/// distance it had to travel (`ec_ui::motion::SETTLE`).
pub const SETTLE: f64 = 0.005;

/// `ω·duration` for the critically damped `spring` that, released from rest,
/// has [`SETTLE`] of its distance left at `duration`: the root of
/// `(1 + k)·e^-k = SETTLE`. Copied from `ec_ui::motion::SPRING_K`.
pub const SPRING_K: f64 = 7.43;

/// Damping ratio of `bounce`: underdamped, so it overshoots and rings once
/// or twice before settling.
pub const BOUNCE_ZETA: f64 = 0.55;

/// `ω·duration` for `bounce`: its envelope `e^(-ζωt)` is down to [`SETTLE`]
/// at `duration`, i.e. `ζ·k = ln(1 / SETTLE)`.
pub const BOUNCE_K: f64 = 9.6333;

/// A leg's response at one instant, as the four coefficients of
/// `d(t) = d0·pd + v0·pv` and `v(t) = d0·vd + v0·vv`.
///
/// Velocities are in displacement units per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Response {
    pub pd: f64,
    pub pv: f64,
    pub vd: f64,
    pub vv: f64,
}

impl Response {
    /// Landed: no displacement, no velocity.
    pub const REST: Response = Response {
        pd: 0.0,
        pv: 0.0,
        vd: 0.0,
        vv: 0.0,
    };

    /// Not started: everything still where it was released.
    pub const START: Response = Response {
        pd: 1.0,
        pv: 0.0,
        vd: 0.0,
        vv: 1.0,
    };

    pub fn pos(&self, d0: f64, v0: f64) -> f64 {
        d0 * self.pd + v0 * self.pv
    }

    pub fn vel(&self, d0: f64, v0: f64) -> f64 {
        d0 * self.vd + v0 * self.vv
    }
}

/// Ease `t` in `0.0..=1.0` under a tween curve. The springs have no ease of
/// their own (their shape depends on the release velocity); asked anyway,
/// they answer with their step response from rest.
pub fn ease(curve: Curve, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        Curve::Linear => t,
        Curve::EaseIn => t * t * t,
        Curve::EaseOut => 1.0 - (1.0 - t).powi(3),
        Curve::EaseInOut => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (2.0 - 2.0 * t).powi(3) / 2.0
            }
        }
        Curve::Spring | Curve::Bounce => 1.0 - response(curve, t, 1.0).pd,
    }
}

/// `d ease / dt` for a tween curve, `t` in `0.0..=1.0`.
fn ease_slope(curve: Curve, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        Curve::Linear => 1.0,
        Curve::EaseIn => 3.0 * t * t,
        Curve::EaseOut => 3.0 * (1.0 - t) * (1.0 - t),
        Curve::EaseInOut => {
            if t < 0.5 {
                12.0 * t * t
            } else {
                3.0 * (2.0 - 2.0 * t).powi(2)
            }
        }
        // Not reached: the springs are answered in closed form.
        Curve::Spring | Curve::Bounce => 0.0,
    }
}

/// The response of a leg on `curve`, `elapsed` seconds into a leg `total`
/// seconds long. Past the end, or on a zero-length leg, it has landed.
pub fn response(curve: Curve, elapsed: f64, total: f64) -> Response {
    if total <= 0.0 || elapsed >= total {
        return Response::REST;
    }
    let t = elapsed.max(0.0);
    match curve {
        Curve::Spring => {
            // x'' = -ω²x - 2ωx', critically damped:
            // x(t) = (x0 + (v0 + ωx0)·t)·e^-ωt.
            let w = SPRING_K / total;
            let e = (-w * t).exp();
            Response {
                pd: (1.0 + w * t) * e,
                pv: t * e,
                vd: -w * w * t * e,
                vv: (1.0 - w * t) * e,
            }
        }
        Curve::Bounce => {
            // Underdamped: x(t) = e^-αt·(x0·cos ωd·t + (v0 + α·x0)/ωd · sin ωd·t),
            // α = ζω, ωd = ω·√(1 − ζ²).
            let w = BOUNCE_K / total;
            let a = BOUNCE_ZETA * w;
            let wd = w * (1.0 - BOUNCE_ZETA * BOUNCE_ZETA).sqrt();
            let e = (-a * t).exp();
            let (sin, cos) = (wd * t).sin_cos();
            Response {
                pd: e * (cos + a / wd * sin),
                pv: e * sin / wd,
                vd: -e * sin * w * w / wd,
                vv: e * (cos - a / wd * sin),
            }
        }
        tween => {
            // A tween carries no velocity in: it starts from rest wherever
            // it is. It reports the velocity it has, so a spring retargeted
            // off it keeps it.
            let p = t / total;
            Response {
                pd: 1.0 - ease(tween, p),
                pv: 0.0,
                vd: -ease_slope(tween, p) / total,
                vv: 0.0,
            }
        }
    }
}

/// One leg: a curve run over a duration from a start instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Leg {
    pub start: Instant,
    pub duration: Duration,
    pub curve: Curve,
}

impl Leg {
    pub fn new(start: Instant, duration_ms: u32, curve: Curve) -> Self {
        Self {
            start,
            duration: Duration::from_millis(duration_ms as u64),
            curve,
        }
    }

    pub fn response(&self, now: Instant) -> Response {
        response(
            self.curve,
            now.saturating_duration_since(self.start).as_secs_f64(),
            self.duration.as_secs_f64(),
        )
    }

    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start) >= self.duration
    }

    /// `0.0..=1.0` progress through the leg's curve, for a crossfade.
    /// Clamped: a bouncing colour does not overshoot its endpoints.
    pub fn progress(&self, now: Instant) -> f64 {
        (1.0 - self.response(now).pd).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWEENS: [Curve; 4] = [Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::EaseInOut];

    #[test]
    fn tweens_start_at_zero_end_at_one_and_never_turn_back() {
        for c in TWEENS {
            assert!(ease(c, 0.0).abs() < 1e-9, "{c:?}");
            assert!((ease(c, 1.0) - 1.0).abs() < 1e-9, "{c:?}");
            let mut last = 0.0;
            for i in 1..=100 {
                let v = ease(c, i as f64 / 100.0);
                assert!(v >= last - 1e-12, "{c:?} turned back at {i}");
                last = v;
            }
        }
    }

    #[test]
    fn tween_slopes_match_the_curve() {
        for c in TWEENS {
            for i in 1..20 {
                let t = i as f64 / 20.0;
                let h = 1e-6;
                let numeric = (ease(c, t + h) - ease(c, t - h)) / (2.0 * h);
                assert!((numeric - ease_slope(c, t)).abs() < 1e-4, "{c:?} at {t}");
            }
        }
    }

    #[test]
    fn spring_constants_settle_where_they_say() {
        let k = SPRING_K;
        assert!(((1.0 + k) * (-k).exp() - SETTLE).abs() < 1e-4);
        assert!((BOUNCE_ZETA * BOUNCE_K - (1.0 / SETTLE).ln()).abs() < 1e-3);
    }

    #[test]
    fn every_curve_starts_where_released_and_lands() {
        for c in Curve::ALL {
            let r = response(c, 0.0, 0.2);
            assert!((r.pos(10.0, 0.0) - 10.0).abs() < 1e-9, "{c:?}");
            assert_eq!(response(c, 0.2, 0.2), Response::REST, "{c:?}");
            assert_eq!(response(c, 0.0, 0.0), Response::REST, "{c:?}");
            // Within the settle band just before the end.
            let near = response(c, 0.199, 0.2).pos(1.0, 0.0).abs();
            assert!(near < 0.02, "{c:?} still {near} out");
        }
    }

    #[test]
    fn spring_never_overshoots_and_bounce_does() {
        let mut spring_min = f64::MAX;
        let mut bounce_min = f64::MAX;
        for i in 0..200 {
            let t = i as f64 / 1000.0;
            spring_min = spring_min.min(response(Curve::Spring, t, 0.2).pos(1.0, 0.0));
            bounce_min = bounce_min.min(response(Curve::Bounce, t, 0.2).pos(1.0, 0.0));
        }
        assert!(spring_min >= -1e-9);
        assert!(bounce_min < -0.05, "bounce should pass its target");
    }

    #[test]
    fn spring_velocity_is_the_derivative_of_position() {
        for c in [Curve::Spring, Curve::Bounce] {
            for (d0, v0) in [(1.0, 0.0), (0.0, 5.0), (-3.0, 12.0)] {
                for i in 1..19 {
                    let t = i as f64 / 100.0;
                    let h = 1e-6;
                    let p = |t| response(c, t, 0.2).pos(d0, v0);
                    let numeric = (p(t + h) - p(t - h)) / (2.0 * h);
                    let v = response(c, t, 0.2).vel(d0, v0);
                    assert!((numeric - v).abs() < 1e-3 * (1.0 + v.abs()), "{c:?} t={t}");
                }
            }
        }
    }

    #[test]
    fn spring_retarget_is_continuous_in_position_and_velocity() {
        // A leg released at d0 = 100, retargeted 60ms in so its rest point
        // moves 40 further: the new leg starts at the old one's position plus
        // the shift, with the old one's velocity, and its first instants
        // follow on without a jump.
        for c in [Curve::Spring, Curve::Bounce] {
            let total = 0.22;
            let at = response(c, 0.06, total);
            let (d, v) = (at.pos(100.0, 0.0), at.vel(100.0, 0.0));
            let (d1, v1) = (d + 40.0, v);
            let start = response(c, 0.0, total);
            assert!((start.pos(d1, v1) - d1).abs() < 1e-9);
            assert!((start.vel(d1, v1) - v1).abs() < 1e-9);
            // A millisecond on, it has moved by about v·dt: no kink.
            let dt = 1e-3;
            let next = response(c, dt, total).pos(d1, v1);
            assert!((next - (d1 + v1 * dt)).abs() < 0.5, "{c:?}");
        }
    }

    #[test]
    fn a_leg_reports_done_at_its_duration() {
        let now = Instant::now();
        let leg = Leg::new(now, 200, Curve::EaseOut);
        assert!(!leg.done(now));
        assert!(!leg.done(now + Duration::from_millis(199)));
        assert!(leg.done(now + Duration::from_millis(200)));
        assert_eq!(leg.response(now + Duration::from_millis(300)), Response::REST);
        // Clamped even when the curve rings.
        let bounce = Leg::new(now, 200, Curve::Bounce);
        for ms in 0..200 {
            let p = bounce.progress(now + Duration::from_millis(ms));
            assert!((0.0..=1.0).contains(&p));
        }
    }
}
