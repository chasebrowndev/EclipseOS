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

/// A hover fade: an opacity easing toward 1 while shown and toward 0 while hidden. A reversal
/// mid-way continues from the current level, so a pointer flicking in and out never jumps.
/// Times are milliseconds on any monotonic clock the caller keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fader {
    shown: bool,
    /// When the current fade started; `None` once settled (or never moved).
    start_ms: Option<u64>,
}

impl Fader {
    /// Hidden and settled.
    pub const fn hidden() -> Fader {
        Fader {
            shown: false,
            start_ms: None,
        }
    }

    /// Fade toward shown or hidden from `now_ms`.
    pub fn set(&mut self, shown: bool, now_ms: u64, mode: MotionMode) {
        if shown == self.shown {
            if !self.animating(now_ms, mode) {
                self.start_ms = None;
            }
            return;
        }
        let level = self.level(now_ms, mode);
        self.shown = shown;
        if mode.instant() {
            self.start_ms = None;
            return;
        }
        // The eased progress that gives the current level on the new curve, inverted.
        let eased = if shown { level } else { 1.0 - level };
        let p = 1.0 - (1.0 - eased.clamp(0.0, 1.0)).cbrt();
        let back = (p * tok::FADE_MS as f32) as u64;
        self.start_ms = Some(now_ms.saturating_sub(back));
    }

    /// Opacity in `0..=1` at `now_ms`.
    pub fn level(&self, now_ms: u64, mode: MotionMode) -> f32 {
        let e = self
            .start_ms
            .map_or(1.0, |s| fade(now_ms.saturating_sub(s), mode));
        if self.shown { e } else { 1.0 - e }
    }

    /// Whether the owner must request another frame.
    pub fn animating(&self, now_ms: u64, mode: MotionMode) -> bool {
        !mode.instant()
            && self
                .start_ms
                .is_some_and(|s| now_ms.saturating_sub(s) < tok::FADE_MS)
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

/// The caret's blink at `elapsed_ms` since the cursor last moved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    /// Opacity in `0..=1`.
    pub alpha: f32,
    /// A fade is running: the owner needs a frame now.
    pub fading: bool,
    /// Milliseconds until the next change, while holding; `None` when settled for good.
    pub next_ms: Option<u64>,
}

/// An eased blink (smoothstep both ways), solid at rest. Only `Full` motion blinks; `Reduced`
/// and `None` keep the caret steady. After `CARET_IDLE_MS` it rests solid and asks for no
/// frames.
pub fn caret_blink(elapsed_ms: u64, mode: MotionMode) -> Caret {
    let mut c = caret_phase(elapsed_ms, mode);
    // Wake at the idle edge too, so the caret never rests mid-blink.
    c.next_ms = c
        .next_ms
        .map(|n| n.min(tok::CARET_IDLE_MS.saturating_sub(elapsed_ms)));
    c
}

fn caret_phase(elapsed_ms: u64, mode: MotionMode) -> Caret {
    let solid = Caret {
        alpha: 1.0,
        fading: false,
        next_ms: None,
    };
    if mode != MotionMode::Full || elapsed_ms >= tok::CARET_IDLE_MS {
        return solid;
    }
    let cycle = tok::CARET_ON_MS + 2 * tok::CARET_FADE_MS + tok::CARET_OFF_MS;
    let ph = elapsed_ms % cycle;
    let smooth = |t: u64| {
        let x = t as f32 / tok::CARET_FADE_MS as f32;
        x * x * (3.0 - 2.0 * x)
    };
    let out_at = tok::CARET_ON_MS;
    let off_at = out_at + tok::CARET_FADE_MS;
    let in_at = off_at + tok::CARET_OFF_MS;
    if ph < out_at {
        Caret {
            next_ms: Some(out_at - ph),
            ..solid
        }
    } else if ph < off_at {
        Caret {
            alpha: 1.0 - smooth(ph - out_at),
            fading: true,
            next_ms: None,
        }
    } else if ph < in_at {
        Caret {
            alpha: 0.0,
            fading: false,
            next_ms: Some(in_at - ph),
        }
    } else {
        Caret {
            alpha: smooth(ph - in_at),
            fading: true,
            next_ms: None,
        }
    }
}

#[cfg(test)]
mod caret_tests {
    use super::*;

    #[test]
    fn blink_is_eased_and_settles() {
        let m = MotionMode::Full;
        assert_eq!(caret_blink(0, m).alpha, 1.0);
        let mid = caret_blink(tok::CARET_ON_MS + tok::CARET_FADE_MS / 2, m);
        assert!(mid.fading && (mid.alpha - 0.5).abs() < 0.01);
        assert_eq!(
            caret_blink(tok::CARET_ON_MS + tok::CARET_FADE_MS + 1, m).alpha,
            0.0
        );
        // Reduced, none and idle are steady and request nothing.
        for c in [
            caret_blink(700, MotionMode::Reduced),
            caret_blink(700, MotionMode::None),
            caret_blink(tok::CARET_IDLE_MS, m),
        ] {
            assert_eq!((c.alpha, c.fading, c.next_ms), (1.0, false, None));
        }
    }
}
