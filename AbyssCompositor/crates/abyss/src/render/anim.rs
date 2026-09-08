// SPDX-License-Identifier: AGPL-3.0-only
//! Geometry-only window animations (COMP-02 §9).
//!
//! The shell always maps a window at its *target* location, so `scene` and
//! `get_tree` report where a window is going, never where it is mid-flight —
//! an agent can never click at an interpolated position. Animation lives
//! entirely in this store: it remembers each window's previous target and
//! hands the render path a shrinking offset to draw at.
//!
//! The same store drives the other two window-scoped animations. `fade` ramps
//! a newly mapped window's alpha from zero; there is no fade-out, because a
//! closing window is gone from the space before the next frame and the
//! compositor never holds a dead client's buffers to animate. `border`
//! crossfades the border colour on focus change, and `workspaces` slides the
//! windows of a newly activated workspace in from the edge of the output.
//! Like `fade`, it has no outgoing half: the old workspace's windows are
//! already unmapped by the time the frame is drawn.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::desktop::{Space, Window};
use smithay::utils::{Logical, Point};

use crate::config::Config;

/// One window's in-flight move.
struct Move {
    /// Where the window was drawn when the target changed.
    from: Point<i32, Logical>,
    target: Point<i32, Logical>,
    start: Instant,
    duration: Duration,
    curve: Curve,
}

#[derive(Clone, Copy)]
enum Curve {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Curve {
    fn parse(name: &str) -> Self {
        match name {
            "linear" => Curve::Linear,
            "ease-in" => Curve::EaseIn,
            "ease-in-out" => Curve::EaseInOut,
            // `ease-out` is the config default and the fallback for anything
            // the parser let through.
            _ => Curve::EaseOut,
        }
    }

    fn apply(self, t: f64) -> f64 {
        match self {
            Curve::Linear => t,
            Curve::EaseIn => t * t,
            Curve::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            Curve::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - 2.0 * (1.0 - t) * (1.0 - t)
                }
            }
        }
    }
}

/// A ramp from 0 to 1 over `duration`, used by `fade` and `border`.
struct Ramp {
    start: Instant,
    duration: Duration,
    curve: Curve,
}

impl Ramp {
    fn new(anim: &crate::config::Animation, now: Instant) -> Self {
        Self {
            start: now,
            duration: Duration::from_millis(anim.duration_ms as u64),
            curve: Curve::parse(&anim.curve),
        }
    }

    fn value(&self) -> f32 {
        let total = self.duration.as_secs_f64();
        if total <= 0.0 {
            return 1.0;
        }
        self.curve
            .apply((self.start.elapsed().as_secs_f64() / total).clamp(0.0, 1.0)) as f32
    }

    fn done(&self, now: Instant) -> bool {
        now.duration_since(self.start) >= self.duration
    }
}

/// Per-window animation state, kept between frames.
#[derive(Default)]
pub struct AnimStore {
    moves: HashMap<Window, Move>,
    /// Last target seen for each live window, animating or not.
    targets: HashMap<Window, Point<i32, Logical>>,
    /// Fade-in ramps for windows mapped since the last frame.
    fades: HashMap<Window, Ramp>,
    /// Border colour crossfades, with the focus state each was headed towards.
    borders: HashMap<Window, (Ramp, bool)>,
    /// Focus state each window was last drawn with, animating or not.
    focused: HashMap<Window, bool>,
    running: bool,
}

impl AnimStore {
    /// True while at least one window is still moving. The backends keep
    /// repainting for as long as this holds and stop the moment it clears, so
    /// an idle compositor is idle (COMP-02 §9).
    pub fn running(&self) -> bool {
        self.running
    }

    /// Note this frame's target locations and retire finished moves.
    ///
    /// Cheap and idempotent: a multi-output frame calls it once per output and
    /// gets the same answer, because everything here is derived from the clock.
    pub fn sync(&mut self, space: &Space<Window>, config: &Config, focus: Option<&Window>) {
        let curve = config.animations.get("windows");
        let now = Instant::now();

        let live: Vec<(Window, Point<i32, Logical>)> = space
            .elements()
            .filter_map(|w| space.element_location(w).map(|l| (w.clone(), l)))
            .collect();
        self.targets.retain(|w, _| live.iter().any(|(l, _)| l == w));
        self.moves.retain(|w, _| live.iter().any(|(l, _)| l == w));
        self.fades.retain(|w, _| live.iter().any(|(l, _)| l == w));
        self.borders.retain(|w, _| live.iter().any(|(l, _)| l == w));
        self.focused.retain(|w, _| live.iter().any(|(l, _)| l == w));

        // `fade`: a window not seen last frame starts at zero alpha. The check
        // is against `targets`, which is updated below, so it has to happen
        // first.
        if let Some(anim) = config.animations.get("fade") {
            for (window, _) in &live {
                if !self.targets.contains_key(window) {
                    self.fades.insert(window.clone(), Ramp::new(anim, now));
                }
            }
        } else {
            self.fades.clear();
        }

        // `border`: crossfade whenever a window's focus state flips. A window
        // seen for the first time takes its colour immediately.
        let border_anim = config.animations.get("border");
        for (window, _) in &live {
            let active = focus == Some(window);
            match self.focused.insert(window.clone(), active) {
                Some(was) if was != active => {
                    if let Some(anim) = border_anim {
                        self.borders
                            .insert(window.clone(), (Ramp::new(anim, now), active));
                    }
                }
                _ => {}
            }
        }
        if border_anim.is_none() {
            self.borders.clear();
        }

        for (window, target) in live {
            let previous = self.targets.insert(window.clone(), target);
            let Some(anim) = curve else {
                // `windows` off: never start a move from a target change. Any
                // move already in flight is left to finish — it can only have
                // come from `workspaces`, which is a separate setting.
                continue;
            };
            match previous {
                // A window that was already here has not moved.
                Some(p) if p == target => {}
                // First frame for this window: it appears at its target rather
                // than flying in from the origin.
                None => {}
                Some(previous) => {
                    // Fly in from where the window is currently drawn, so a
                    // move retargeted mid-flight does not jump.
                    let from = previous + self.offset(&window);
                    self.moves.insert(
                        window,
                        Move {
                            from,
                            target,
                            start: now,
                            duration: Duration::from_millis(anim.duration_ms as u64),
                            curve: Curve::parse(&anim.curve),
                        },
                    );
                }
            }
        }

        self.moves.retain(|_, m| now.duration_since(m.start) < m.duration);
        self.fades.retain(|_, r| !r.done(now));
        self.borders.retain(|_, (r, _)| !r.done(now));
        self.running = !self.moves.is_empty() || !self.fades.is_empty() || !self.borders.is_empty();
    }

    /// Slide `windows` in from `from`, an offset in logical pixels relative to
    /// where each already sits. Used by `workspaces` on a workspace switch;
    /// the windows are mapped at their targets first, so nothing outside the
    /// render path sees the offset (COMP-02 §9).
    pub fn slide(
        &mut self,
        space: &Space<Window>,
        anim: &crate::config::Animation,
        windows: &[Window],
        from: Point<i32, Logical>,
    ) {
        let now = Instant::now();
        for window in windows {
            let Some(target) = space.element_location(window) else {
                continue;
            };
            self.moves.insert(
                window.clone(),
                Move {
                    from: target + from,
                    target,
                    start: now,
                    duration: Duration::from_millis(anim.duration_ms as u64),
                    curve: Curve::parse(&anim.curve),
                },
            );
            // `sync` only starts a move when a window's target *changes*, so
            // seed the target too or the next frame would treat this as a
            // first sighting and drop the slide.
            self.targets.insert(window.clone(), target);
        }
        self.running |= !self.moves.is_empty();
    }

    /// How far from its target this window should be drawn, in logical pixels.
    pub fn offset(&self, window: &Window) -> Point<i32, Logical> {
        let Some(m) = self.moves.get(window) else {
            return (0, 0).into();
        };
        let elapsed = m.start.elapsed().as_secs_f64();
        let total = m.duration.as_secs_f64();
        if total <= 0.0 {
            return (0, 0).into();
        }
        let remaining = 1.0 - m.curve.apply((elapsed / total).clamp(0.0, 1.0));
        let dx = (m.from.x - m.target.x) as f64 * remaining;
        let dy = (m.from.y - m.target.y) as f64 * remaining;
        (dx.round() as i32, dy.round() as i32).into()
    }

    /// The alpha multiplier for a window still fading in; 1.0 once it is up.
    pub fn fade(&self, window: &Window) -> f32 {
        self.fades.get(window).map_or(1.0, Ramp::value)
    }

    /// True while any window is fading, i.e. whether the per-window render
    /// path has to be taken even with no decoration effect configured.
    pub fn fading(&self) -> bool {
        !self.fades.is_empty()
    }

    /// The border colour to draw, crossfading on focus change.
    pub fn border_color(&self, window: &Window, active: [f32; 4], inactive: [f32; 4]) -> [f32; 4] {
        let (from, to, t) = match self.borders.get(window) {
            // `towards` is where the focus went; the ramp runs from the other.
            Some((ramp, true)) => (inactive, active, ramp.value()),
            Some((ramp, false)) => (active, inactive, ramp.value()),
            None if self.focused.get(window).copied().unwrap_or(false) => return active,
            None => return inactive,
        };
        std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_at_zero_and_end_at_one() {
        for c in [Curve::Linear, Curve::EaseIn, Curve::EaseOut, Curve::EaseInOut] {
            assert!(c.apply(0.0).abs() < 1e-9);
            assert!((c.apply(1.0) - 1.0).abs() < 1e-9);
            // Monotonic across the middle.
            assert!(c.apply(0.25) <= c.apply(0.75));
        }
    }

    #[test]
    fn a_ramp_runs_from_zero_to_one() {
        let now = Instant::now();
        let ramp = Ramp {
            start: now - Duration::from_millis(500),
            duration: Duration::from_millis(1000),
            curve: Curve::Linear,
        };
        assert!((ramp.value() - 0.5).abs() < 0.05);
        assert!(!ramp.done(now));
        assert!(ramp.done(now + Duration::from_millis(600)));
        // A zero-length ramp is over before it starts.
        let instant = Ramp {
            start: now,
            duration: Duration::ZERO,
            curve: Curve::Linear,
        };
        assert!((instant.value() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn an_unanimated_window_has_no_offset() {
        let store = AnimStore::default();
        // No entry means no move; the window draws at its target.
        assert!(store.moves.is_empty());
        assert!(!store.running());
    }
}
