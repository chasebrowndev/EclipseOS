// SPDX-License-Identifier: AGPL-3.0-only
//! Geometry-only window animations (COMP-02 §9).
//!
//! The shell always maps a window at its *target* location, so `scene` and
//! `get_tree` report where a window is going, never where it is mid-flight —
//! an agent can never click at an interpolated position. Animation lives
//! entirely in this store: it remembers each window's previous target and
//! hands the render path a shrinking offset to draw at.

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

/// Per-window animation state, kept between frames.
#[derive(Default)]
pub struct AnimStore {
    moves: HashMap<Window, Move>,
    /// Last target seen for each live window, animating or not.
    targets: HashMap<Window, Point<i32, Logical>>,
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
    pub fn sync(&mut self, space: &Space<Window>, config: &Config) {
        let curve = config.animations.get("windows");
        let now = Instant::now();

        let live: Vec<(Window, Point<i32, Logical>)> = space
            .elements()
            .filter_map(|w| space.element_location(w).map(|l| (w.clone(), l)))
            .collect();
        self.targets.retain(|w, _| live.iter().any(|(l, _)| l == w));
        self.moves.retain(|w, _| live.iter().any(|(l, _)| l == w));

        for (window, target) in live {
            let previous = self.targets.insert(window.clone(), target);
            let Some(anim) = curve else {
                // Animations off: the window is simply drawn where it is.
                self.moves.remove(&window);
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
        self.running = !self.moves.is_empty();
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
    fn an_unanimated_window_has_no_offset() {
        let store = AnimStore::default();
        // No entry means no move; the window draws at its target.
        assert!(store.moves.is_empty());
        assert!(!store.running());
    }
}
