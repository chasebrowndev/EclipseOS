// SPDX-License-Identifier: AGPL-3.0-only
//! Tile placement and its springs.
//!
//! A tile's rectangle is not a number the view reads off the layout: it is a
//! spring that chases the layout's rectangle, so a change of output size (or of
//! layout) reads as liquid, not as a cut. On appearance each tile rises and
//! fades in, staggered in layout order. A damped spring with a small overshoot
//! settles quickly and bounces once, which is the bubbly, floaty motion the
//! screen wants, without being slow.
//!
//! Pure: no iced, no clock of its own. The caller passes the instant of each
//! frame, so the tests run without a window.

use std::time::{Duration, Instant};

use ec_layout::{Kind, Layout};
use iced::Size;

use crate::tokens::{motion, TILE_GAP};

/// One damped spring: `pos` chases `target` with velocity `vel`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub pos: f32,
    pub vel: f32,
    pub target: f32,
}

impl Spring {
    pub const fn at(v: f32) -> Self {
        Spring {
            pos: v,
            vel: 0.0,
            target: v,
        }
    }

    /// Advance by `dt` seconds, in substeps so a long frame stays stable.
    pub fn step(&mut self, dt: f32) {
        let h = dt / motion::SUBSTEPS as f32;
        for _ in 0..motion::SUBSTEPS {
            let a = motion::STIFFNESS * (self.target - self.pos) - motion::DAMPING * self.vel;
            self.vel += a * h;
            self.pos += self.vel * h;
        }
    }

    pub fn settled(&self) -> bool {
        (self.pos - self.target).abs() < motion::REST_POS && self.vel.abs() < motion::REST_VEL
    }
}

/// Where one tile is this frame, for the view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// 0 before the tile appears, 1 once it is in place. May overshoot briefly.
    pub fade: f32,
    /// Px the tile is still below its place (it rises to 0).
    pub rise: f32,
}

#[derive(Debug, Clone)]
struct Item {
    kind: Kind,
    /// x, y, w, h in px.
    pos: [Spring; 4],
    /// 0 → 1 as the tile appears.
    enter: Spring,
    appear: Instant,
}

/// The tiles of one layout on one output size, animated.
#[derive(Debug)]
pub struct Stage {
    layout: Layout,
    size: (f32, f32),
    /// Whether a real output size has been seen. The first one snaps: the
    /// window's default size is a guess, not a place to animate from.
    sized: bool,
    items: Vec<Item>,
    last: Option<Instant>,
}

impl Stage {
    pub fn new(layout: Layout, size: Size, now: Instant) -> Self {
        let mut s = Stage {
            layout,
            size: (size.width, size.height),
            sized: false,
            items: Vec::new(),
            last: None,
        };
        s.reconcile(now);
        s
    }

    /// The output changed size: every tile springs to its new place.
    pub fn resize(&mut self, size: Size, now: Instant) {
        self.size = (size.width, size.height);
        self.reconcile(now);
        if !self.sized {
            self.sized = true;
            for it in &mut self.items {
                for s in &mut it.pos {
                    s.pos = s.target;
                    s.vel = 0.0;
                }
            }
        }
    }

    /// Bring the springs to a frame at `now`.
    pub fn step(&mut self, now: Instant) {
        let dt = match self.last {
            Some(prev) => now
                .saturating_duration_since(prev)
                .as_secs_f32()
                .min(motion::MAX_DT),
            None => motion::FIRST_DT,
        };
        self.last = Some(now);
        for it in &mut self.items {
            it.enter.target = if now >= it.appear { 1.0 } else { 0.0 };
            it.enter.step(dt);
            for s in &mut it.pos {
                s.step(dt);
            }
        }
    }

    /// Whether any tile is still moving or still waiting to appear. The view
    /// asks for frames only while this is true.
    pub fn animating(&self, now: Instant) -> bool {
        self.items
            .iter()
            .any(|it| now < it.appear || !it.enter.settled() || it.pos.iter().any(|s| !s.settled()))
    }

    /// Every tile's place this frame, in layout order.
    pub fn placed(&self) -> Vec<Placed> {
        self.items
            .iter()
            .map(|it| {
                let [x, y, w, h] = it.pos.map(|s| s.pos);
                let fade = it.enter.pos.clamp(0.0, 1.0);
                Placed {
                    kind: it.kind,
                    x,
                    y,
                    w: w.max(0.0),
                    h: h.max(0.0),
                    fade,
                    rise: (1.0 - it.enter.pos) * motion::RISE,
                }
            })
            .collect()
    }

    /// Match the items to the layout. Springs of a tile that stays are kept, so
    /// it moves from where it is; a new tile starts at its place and rises.
    fn reconcile(&mut self, now: Instant) {
        let mut next = Vec::with_capacity(self.layout.tiles.len());
        for (i, t) in self.layout.tiles.iter().enumerate() {
            let target = target_px(&self.layout, t.kind, self.size);
            let item = match self.items.iter().find(|it| it.kind == t.kind) {
                Some(old) => {
                    let mut it = old.clone();
                    for (s, v) in it.pos.iter_mut().zip(target) {
                        s.target = v;
                    }
                    it
                }
                None => Item {
                    kind: t.kind,
                    pos: target.map(Spring::at),
                    enter: Spring::at(0.0),
                    appear: now + Duration::from_millis(motion::STAGGER_MS * i as u64),
                },
            };
            next.push(item);
        }
        self.items = next;
    }
}

/// A tile's rectangle on an output of `size`, through `Rect::to_px` with the
/// greeter's gap. A kind the layout lacks is a zero box (it is never drawn).
fn target_px(layout: &Layout, kind: Kind, size: (f32, f32)) -> [f32; 4] {
    let Some(r) = layout.get(kind) else {
        return [0.0; 4];
    };
    let w = size.0.max(1.0) as u32;
    let h = size.1.max(1.0) as u32;
    let (x, y, pw, ph) = r.to_px(w, h, TILE_GAP);
    [x as f32, y as f32, pw as f32, ph as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::default()
    }

    #[test]
    fn a_spring_settles_on_its_target_with_a_small_overshoot() {
        let mut s = Spring::at(0.0);
        s.target = 100.0;
        let mut peak = 0.0_f32;
        for _ in 0..240 {
            s.step(1.0 / 60.0);
            peak = peak.max(s.pos);
        }
        assert!(s.settled());
        assert!(peak > 100.0, "bubbly: it overshoots");
        assert!(peak < 115.0, "but only a little");
    }

    #[test]
    fn tiles_appear_in_order_and_settle() {
        let t0 = Instant::now();
        let mut st = Stage::new(layout(), Size::new(1920.0, 1080.0), t0);
        assert!(st.animating(t0));
        assert!(st.placed().iter().all(|p| p.fade == 0.0));
        let mut now = t0;
        for _ in 0..600 {
            now += Duration::from_millis(16);
            st.step(now);
        }
        assert!(!st.animating(now));
        assert!(st
            .placed()
            .iter()
            .all(|p| (p.fade - 1.0).abs() < 1e-3 && p.rise.abs() < 0.5));
    }

    #[test]
    fn the_first_real_size_snaps_and_later_ones_spring() {
        let t0 = Instant::now();
        let mut st = Stage::new(layout(), Size::new(1920.0, 1080.0), t0);
        st.resize(Size::new(800.0, 1200.0), t0);
        let snapped = st.placed();
        let clock = snapped.iter().find(|p| p.kind == Kind::Clock).unwrap();
        assert!(clock.w < 800.0 && clock.w > 0.0);
        st.resize(Size::new(1920.0, 1080.0), t0);
        let before = st.placed();
        let c0 = before.iter().find(|p| p.kind == Kind::Clock).unwrap();
        assert!(c0.w < 800.0, "the new size is a target, not yet a place");
    }

    #[test]
    fn a_layout_with_no_room_for_a_tile_draws_nothing_wrong() {
        assert_eq!(
            target_px(&Layout { tiles: vec![] }, Kind::Clock, (100.0, 100.0)),
            [0.0; 4]
        );
    }
}
