// SPDX-License-Identifier: AGPL-3.0-only
//! Motion: easings, tweens, a critically damped spring, and one
//! [`Animated`] value that hides which of them is driving it.
//!
//! The bar (ADR 0065) animates every chip and widget's x, width and opacity
//! toward a layout solver's targets, and those targets change mid-flight — a
//! window opens while a widget is still folding. The one property that makes
//! that feel fluid is **continuity under retarget**: a value that is moving
//! never jumps. A tween retargets from wherever it currently is; a spring
//! additionally keeps its velocity, so the direction change is smooth too.
//!
//! Everything here is a pure function of the `Instant`s it is handed. There is
//! no clock, no subscription and no allocation: a caller ticks its values with
//! the frame's `now` and keeps the frame clock running only while
//! [`Animated::animating`] is true for something.

use std::time::{Duration, Instant};

/// `t` as-is.
pub fn linear(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// Cubic ease-in: slow off the mark, fast onto it.
pub fn ease_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t
}

/// Cubic ease-out: fast off the mark, settling onto the target.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Cubic ease-in-out: no snap at either end.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// The residual a spring is allowed at `duration`, as a fraction of the
/// distance it had to travel. Half a percent: on a 200px width that is one
/// pixel, which is where the eye stops seeing motion.
const SETTLE: f32 = 0.005;

/// `ω·duration` for a critically damped spring that, released from rest, has
/// [`SETTLE`] of its distance left at `duration`: the root of
/// `(1 + k)·e^-k = SETTLE`.
const SPRING_K: f32 = 7.43;

/// The floor under the settle threshold, for a spring that was retargeted to
/// (almost) where it already was.
const SETTLE_FLOOR: f32 = 1e-4;

/// The normalised step response of a critically damped spring released from
/// rest, scaled so that it lands within [`SETTLE`] at `t = 1`. Used when a
/// spring has to be expressed as an ease — a [`Tween`] on [`Curve::Spring`].
pub fn spring_ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t >= 1.0 {
        return 1.0;
    }
    let x = SPRING_K * t;
    1.0 - (1.0 + x) * (-x).exp()
}

/// The shape of a movement. Mirrors `bar.motion.curve` exactly — the
/// compositor's `animations` curve names plus `spring`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Curve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// A critically damped spring: retargets keep velocity. The default,
    /// because it is the only curve that is smooth when interrupted.
    #[default]
    Spring,
}

impl Curve {
    /// Every curve, in config order.
    pub const ALL: [Curve; 5] = [
        Curve::Linear,
        Curve::EaseIn,
        Curve::EaseOut,
        Curve::EaseInOut,
        Curve::Spring,
    ];

    /// The config spelling (`ease-in-out`), or `None` for anything else.
    pub fn parse(s: &str) -> Option<Curve> {
        Curve::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Curve::Linear => "linear",
            Curve::EaseIn => "ease-in",
            Curve::EaseOut => "ease-out",
            Curve::EaseInOut => "ease-in-out",
            Curve::Spring => "spring",
        }
    }

    /// Ease `t` in `0.0..=1.0`.
    pub fn ease(self, t: f32) -> f32 {
        match self {
            Curve::Linear => linear(t),
            Curve::EaseIn => ease_in(t),
            Curve::EaseOut => ease_out(t),
            Curve::EaseInOut => ease_in_out(t),
            Curve::Spring => spring_ease(t),
        }
    }
}

/// How things move: `bar.motion.{enabled, curve, duration-ms}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Motion {
    pub enabled: bool,
    pub curve: Curve,
    pub duration: Duration,
}

impl Motion {
    /// The shipped default: on, spring, [`crate::tokens::motion::DURATION_MS`].
    pub const DEFAULT: Motion = Motion {
        enabled: true,
        curve: Curve::Spring,
        duration: Duration::from_millis(crate::tokens::motion::DURATION_MS),
    };

    /// Off, or zero-length: every change lands at once.
    pub const SNAP: Motion = Motion {
        enabled: false,
        curve: Curve::Linear,
        duration: Duration::ZERO,
    };

    /// Whether a change under this motion lands immediately.
    pub fn snaps(&self) -> bool {
        !self.enabled || self.duration.is_zero()
    }
}

impl Default for Motion {
    fn default() -> Self {
        Motion::DEFAULT
    }
}

/// One eased value from `from` to `to` over `duration`, starting at `start`.
///
/// Lands exactly on `to` and reports [`Tween::done`], which is what lets a
/// frame clock stop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub start: Instant,
    pub duration: Duration,
    pub curve: Curve,
}

impl Tween {
    pub fn new(from: f32, to: f32, start: Instant, duration: Duration, curve: Curve) -> Tween {
        Tween {
            from,
            to,
            start,
            duration,
            curve,
        }
    }

    /// Progress `0.0..=1.0` at `now`. A zero-length tween is already done.
    pub fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32();
        t.clamp(0.0, 1.0)
    }

    pub fn value_at(&self, now: Instant) -> f32 {
        let t = self.progress(now);
        if t >= 1.0 {
            return self.to;
        }
        self.from + (self.to - self.from) * self.curve.ease(t)
    }

    pub fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

/// A critically damped spring: the fastest approach to `target` that never
/// overshoots.
///
/// Integrated exactly (the closed form of `x'' = -ω²(x - T) - 2ωx'`), so a
/// long frame or a stalled clock cannot make it explode or ring, and a step
/// of `a + b` equals a step of `a` then one of `b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    /// Angular frequency, from the settle duration.
    omega: f32,
    /// How far this leg had to go, the yardstick for [`Spring::settled`].
    span: f32,
}

impl Spring {
    /// A spring at rest at `value`, tuned so that released from rest it is
    /// within half a percent of its target after `settle`.
    pub fn new(value: f32, settle: Duration) -> Spring {
        let secs = settle.as_secs_f32().max(f32::EPSILON);
        Spring {
            value,
            velocity: 0.0,
            target: value,
            omega: SPRING_K / secs,
            span: 0.0,
        }
    }

    /// Aim at `target` from wherever the spring is, at whatever speed it is
    /// moving. Value and velocity are untouched — that is the whole contract.
    pub fn retarget(&mut self, target: f32) {
        self.target = target;
        self.span = (self.value - target).abs() + self.velocity.abs() / self.omega;
    }

    /// Advance by `dt`.
    pub fn step(&mut self, dt: Duration) {
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return;
        }
        let w = self.omega;
        let t = dt.as_secs_f32();
        let e0 = self.value - self.target;
        let v0 = self.velocity;
        let c = v0 + w * e0;
        let decay = (-w * t).exp();
        let e = (e0 + c * t) * decay;
        self.velocity = (v0 - w * c * t) * decay;
        self.value = self.target + e;
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
        }
    }

    /// Close enough, and slow enough, that another frame would not move a
    /// pixel.
    pub fn settled(&self) -> bool {
        let eps = (self.span * SETTLE).max(SETTLE_FLOOR);
        (self.value - self.target).abs() <= eps && self.velocity.abs() / self.omega <= eps
    }
}

/// What an [`Animated`] can move. Implemented for `f32`; the trait is the
/// door for a colour or a point without reworking the driver.
pub trait Animatable: Copy + PartialEq {
    fn to_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
}

impl Animatable for f32 {
    fn to_f32(self) -> f32 {
        self
    }
    fn from_f32(v: f32) -> Self {
        v
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drive {
    Rest,
    Tween(Tween),
    Spring(Spring),
}

/// A value that moves toward its target under a [`Motion`].
///
/// The one API a view needs: [`set_target`](Animated::set_target) when the
/// solver changes its mind, [`tick`](Animated::tick) once per frame,
/// [`value`](Animated::value) to draw, [`animating`](Animated::animating) to
/// decide whether to ask for another frame.
///
/// Retargeting mid-flight never jumps. Under a tween curve the new leg starts
/// from the current value; under [`Curve::Spring`] it also keeps the current
/// velocity. With motion off, or a zero duration, every change snaps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Animated<T: Animatable = f32> {
    motion: Motion,
    value: f32,
    target: f32,
    drive: Drive,
    last: Option<Instant>,
    _unit: std::marker::PhantomData<T>,
}

impl<T: Animatable> Animated<T> {
    /// At rest at `value`.
    pub fn new(value: T, motion: Motion) -> Self {
        let v = value.to_f32();
        Animated {
            motion,
            value: v,
            target: v,
            drive: Drive::Rest,
            last: None,
            _unit: std::marker::PhantomData,
        }
    }

    pub fn motion(&self) -> Motion {
        self.motion
    }

    /// Adopt a new motion (a config reload). A value in flight lands at its
    /// target at once, rather than switching curves halfway through a leg.
    pub fn set_motion(&mut self, motion: Motion) {
        if motion != self.motion {
            self.motion = motion;
            self.snap(T::from_f32(self.target));
        }
    }

    /// Move toward `target`, starting at `now`.
    pub fn set_target(&mut self, target: T, now: Instant) {
        let target = target.to_f32();
        // Bring the current leg up to `now` first, so the new one starts from
        // where the value really is.
        self.tick(now);
        if target == self.target && !self.animating() {
            return;
        }
        self.target = target;
        if self.motion.snaps() {
            self.snap(T::from_f32(target));
            return;
        }
        self.drive = match (self.drive, self.motion.curve) {
            (Drive::Spring(mut s), Curve::Spring) => {
                s.retarget(target);
                Drive::Spring(s)
            }
            (_, Curve::Spring) => {
                let mut s = Spring::new(self.value, self.motion.duration);
                s.retarget(target);
                Drive::Spring(s)
            }
            (_, curve) => Drive::Tween(Tween::new(self.value, target, now, self.motion.duration, curve)),
        };
        self.last = Some(now);
        if self.value == target {
            if let Drive::Tween(_) = self.drive {
                self.drive = Drive::Rest;
            }
        }
    }

    /// Jump to `value` and stop.
    pub fn snap(&mut self, value: T) {
        let v = value.to_f32();
        self.value = v;
        self.target = v;
        self.drive = Drive::Rest;
        self.last = None;
    }

    /// Advance to `now`. Cheap and idempotent at rest.
    pub fn tick(&mut self, now: Instant) {
        match &mut self.drive {
            Drive::Rest => {}
            Drive::Tween(tw) => {
                self.value = tw.value_at(now);
                if tw.done(now) {
                    self.value = self.target;
                    self.drive = Drive::Rest;
                }
            }
            Drive::Spring(s) => {
                let dt = self
                    .last
                    .map(|l| now.saturating_duration_since(l))
                    .unwrap_or(Duration::ZERO);
                s.step(dt);
                self.value = s.value;
                if s.settled() {
                    self.value = self.target;
                    self.drive = Drive::Rest;
                }
            }
        }
        self.last = Some(now);
    }

    pub fn value(&self) -> T {
        T::from_f32(self.value)
    }

    pub fn target(&self) -> T {
        T::from_f32(self.target)
    }

    /// Current velocity in units per second. Zero at rest and for tweens,
    /// which carry none across a retarget.
    pub fn velocity(&self) -> f32 {
        match self.drive {
            Drive::Spring(s) => s.velocity,
            _ => 0.0,
        }
    }

    /// Still moving: the frame clock is needed for this value.
    pub fn animating(&self) -> bool {
        !matches!(self.drive, Drive::Rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUR: Duration = Duration::from_millis(220);
    const FRAME: Duration = Duration::from_millis(16);

    fn motion(curve: Curve) -> Motion {
        Motion {
            enabled: true,
            curve,
            duration: DUR,
        }
    }

    /// Run `a` at 60 Hz from `now` until it settles or `limit` passes;
    /// returns the samples and the time it took.
    fn run(a: &mut Animated, mut now: Instant, limit: Duration) -> (Vec<f32>, Duration) {
        let start = now;
        let mut out = vec![a.value()];
        while a.animating() && now - start < limit {
            now += FRAME;
            a.tick(now);
            out.push(a.value());
        }
        (out, now - start)
    }

    #[test]
    fn easings_pin_their_ends_and_are_monotonic() {
        for c in Curve::ALL {
            assert_eq!(c.ease(0.0), 0.0, "{c:?}");
            assert_eq!(c.ease(1.0), 1.0, "{c:?}");
            let mut prev = 0.0;
            for i in 0..=100 {
                let v = c.ease(i as f32 / 100.0);
                assert!(v >= prev - 1e-6, "{c:?} dipped at {i}");
                prev = v;
            }
        }
    }

    #[test]
    fn curve_names_round_trip_the_config_spelling() {
        for c in Curve::ALL {
            assert_eq!(Curve::parse(c.as_str()), Some(c));
        }
        assert_eq!(Curve::parse("bouncy"), None);
        assert_eq!(Curve::default(), Curve::Spring);
    }

    #[test]
    fn tweens_settle_monotonically_and_land_exactly() {
        for c in [Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::EaseInOut] {
            let t0 = Instant::now();
            let mut a = Animated::new(0.0, motion(c));
            a.set_target(100.0, t0);
            let (samples, took) = run(&mut a, t0, DUR * 4);
            assert!(samples.windows(2).all(|w| w[1] >= w[0]), "{c:?}: {samples:?}");
            assert_eq!(a.value(), 100.0);
            assert!(!a.animating());
            assert!(took <= DUR + FRAME, "{c:?} took {took:?}");
        }
    }

    #[test]
    fn a_spring_settles_within_about_its_duration_without_overshoot() {
        for span in [1.0, 100.0, 3000.0] {
            let t0 = Instant::now();
            let mut a = Animated::new(0.0, motion(Curve::Spring));
            a.set_target(span, t0);
            let (samples, took) = run(&mut a, t0, DUR * 10);
            assert!(
                samples.windows(2).all(|w| w[1] >= w[0]),
                "overshoot or dip: {samples:?}"
            );
            assert!(samples.iter().all(|v| *v <= span));
            assert_eq!(a.value(), span);
            assert!(!a.animating(), "still animating after {took:?}");
            assert!(took <= DUR + DUR / 10 + FRAME, "span {span} took {took:?}");
        }
    }

    #[test]
    fn animating_goes_false_once_settled_and_ticks_are_then_inert() {
        let t0 = Instant::now();
        let mut a = Animated::new(10.0, motion(Curve::Spring));
        assert!(!a.animating());
        a.set_target(20.0, t0);
        assert!(a.animating());
        let (_, took) = run(&mut a, t0, DUR * 10);
        assert!(!a.animating());
        a.tick(t0 + took + Duration::from_secs(5));
        assert_eq!(a.value(), 20.0);
        assert!(!a.animating());
        // Retargeting to where it already is does not start the clock.
        a.set_target(20.0, t0 + took + Duration::from_secs(6));
        assert!(!a.animating());
    }

    #[test]
    fn a_retarget_mid_flight_is_continuous_for_every_curve() {
        for c in Curve::ALL {
            let t0 = Instant::now();
            let mut a = Animated::new(0.0, motion(c));
            a.set_target(100.0, t0);
            let mid = t0 + DUR / 2;
            a.tick(mid);
            let before = a.value();
            assert!(before > 0.0 && before < 100.0, "{c:?} {before}");
            // Reverse: the value at the retarget instant must be unchanged.
            a.set_target(-50.0, mid);
            assert_eq!(a.value(), before, "{c:?} jumped on retarget");
            // And the next frame is a small step, not a leap.
            a.tick(mid + FRAME);
            let step = (a.value() - before).abs();
            assert!(step < 40.0, "{c:?} leapt {step}");
            let (_, _) = run(&mut a, mid + FRAME, DUR * 10);
            assert_eq!(a.value(), -50.0);
        }
    }

    #[test]
    fn a_spring_retarget_keeps_its_velocity() {
        let t0 = Instant::now();
        let mut a = Animated::new(0.0, motion(Curve::Spring));
        a.set_target(100.0, t0);
        let mid = t0 + Duration::from_millis(40);
        a.tick(mid);
        let (x, v) = (a.value(), a.velocity());
        assert!(v > 0.0);
        a.set_target(200.0, mid);
        assert_eq!((a.value(), a.velocity()), (x, v));
        // Reversing keeps the forward velocity too: the value carries on
        // forward for a moment before turning, rather than snapping round.
        let mut b = Animated::new(0.0, motion(Curve::Spring));
        b.set_target(100.0, t0);
        b.tick(mid);
        b.set_target(0.0, mid);
        b.tick(mid + Duration::from_millis(4));
        assert!(b.value() > x, "turned instantly");
    }

    #[test]
    fn spring_steps_compose_exactly() {
        let mut a = Spring::new(0.0, DUR);
        a.retarget(1.0);
        let mut b = a;
        a.step(Duration::from_millis(30));
        b.step(Duration::from_millis(10));
        b.step(Duration::from_millis(20));
        assert!((a.value - b.value).abs() < 1e-5);
        assert!((a.velocity - b.velocity).abs() < 1e-4);
    }

    #[test]
    fn a_huge_frame_neither_explodes_nor_rings() {
        let mut s = Spring::new(0.0, DUR);
        s.retarget(100.0);
        s.step(Duration::from_secs(60));
        assert_eq!(s.value, 100.0);
        assert!(s.settled());
    }

    #[test]
    fn disabled_or_zero_duration_snaps() {
        let t0 = Instant::now();
        for m in [
            Motion::SNAP,
            Motion {
                enabled: false,
                ..Motion::DEFAULT
            },
            Motion {
                duration: Duration::ZERO,
                ..Motion::DEFAULT
            },
            Motion {
                duration: Duration::ZERO,
                curve: Curve::EaseOut,
                enabled: true,
            },
        ] {
            let mut a = Animated::new(0.0, m);
            a.set_target(42.0, t0);
            assert_eq!(a.value(), 42.0, "{m:?}");
            assert!(!a.animating(), "{m:?}");
        }
    }

    #[test]
    fn changing_motion_lands_a_value_in_flight() {
        let t0 = Instant::now();
        let mut a = Animated::new(0.0, Motion::DEFAULT);
        a.set_target(10.0, t0);
        a.set_motion(motion(Curve::Linear));
        assert_eq!(a.value(), 10.0);
        assert!(!a.animating());
    }

    #[test]
    fn a_tween_is_a_pure_function_of_time() {
        let t0 = Instant::now();
        let tw = Tween::new(0.0, 10.0, t0, DUR, Curve::Linear);
        assert_eq!(tw.value_at(t0), 0.0);
        assert!((tw.value_at(t0 + DUR / 2) - 5.0).abs() < 1e-3);
        assert_eq!(tw.value_at(t0 + DUR * 2), 10.0);
        assert!(tw.done(t0 + DUR));
        let zero = Tween::new(0.0, 10.0, t0, Duration::ZERO, Curve::EaseOut);
        assert_eq!(zero.value_at(t0), 10.0);
    }
}
