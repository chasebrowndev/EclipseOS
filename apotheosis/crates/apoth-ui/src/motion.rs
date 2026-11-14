// SPDX-License-Identifier: AGPL-3.0-only
//! One spring and one ease helper (UI-04), time-composable: advancing by dt1 then dt2
//! equals advancing by dt1 + dt2, so irregular frame intervals stay correct.
//! Honours `interface.motion`; a settled value requests no frames (T-07 idle rule).

use crate::tokens::motion as tok;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionMode {
    Full,
    Reduced,
    None,
}

impl MotionMode {
    /// `interface.motion system|full|reduced|none`; `system_reduced` is the portal's
    /// reduce-motion setting.
    pub fn resolve(config: &str, system_reduced: bool) -> MotionMode {
        match config {
            "full" => MotionMode::Full,
            "reduced" => MotionMode::Reduced,
            "none" => MotionMode::None,
            _ if system_reduced => MotionMode::Reduced,
            _ => MotionMode::Full,
        }
    }

    /// `none` is a hard cut everywhere; `reduced` keeps fades but springs snap.
    fn instant(self) -> bool {
        self == MotionMode::None
    }
}

/// Cubic ease-out of `t` in `0..=1`.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// A critically damped spring toward a target. `k` is chosen so `DURATION_MS` leaves
/// half a percent of the travel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
}

const SPRING_K: f32 = 7.43;
const SETTLE_EPS: f32 = 0.001;

impl Spring {
    pub fn at(value: f32) -> Spring {
        Spring {
            value,
            velocity: 0.0,
            target: value,
        }
    }

    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < SETTLE_EPS && self.velocity.abs() < SETTLE_EPS
    }

    /// Advance by `dt_ms`. Closed-form for a critically damped oscillator, so it is exact
    /// for any step size.
    pub fn advance(&mut self, dt_ms: u64, mode: MotionMode) {
        if mode != MotionMode::Full {
            self.value = self.target;
            self.velocity = 0.0;
            return;
        }
        if self.settled() {
            self.value = self.target;
            self.velocity = 0.0;
            return;
        }
        let w = SPRING_K / (tok::DURATION_MS as f32 / 1000.0);
        let t = dt_ms as f32 / 1000.0;
        let x0 = self.value - self.target;
        let c2 = self.velocity + w * x0;
        let e = (-w * t).exp();
        self.value = self.target + (x0 + c2 * t) * e;
        self.velocity = (self.velocity - w * c2 * t) * e;
    }

    /// Whether the surface owning this spring must request another frame.
    pub fn animating(&self) -> bool {
        !self.settled()
    }
}

/// A 0..1 fade used by the auto-hide scrollbar and hover transitions.
/// `elapsed_ms` since the fade started; `None` mode is a hard cut.
pub fn fade(elapsed_ms: u64, mode: MotionMode) -> f32 {
    if mode.instant() || elapsed_ms >= tok::FADE_MS {
        return 1.0;
    }
    ease_out(elapsed_ms as f32 / tok::FADE_MS as f32)
}
