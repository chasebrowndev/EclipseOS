// SPDX-License-Identifier: AGPL-3.0-only
//! The bar in motion (ADR 0065): what the solver's targets look like on the
//! way there.
//!
//! [`crate::layout::solve`] says where everything *should* be; this module
//! holds where everything *is*. Every chip and every widget carries an
//! animated width and an animated presence, retargeted (never restarted)
//! whenever the solver changes its mind, so a window opening, a title
//! growing, a widget compressing or a player stopping all glide rather than
//! jump. Positions are not animated separately: the row flows, so a cell's x
//! is the sum of the animated widths before it and moves with them.
//!
//! Content never reflows mid-flight. A chip's rung is chosen from its
//! *target* width and drawn at that width under a clip whose edge is the
//! animated one, so a label is uncovered or covered, never re-wrapped.
//!
//! The fold slide (`app::FoldState`) keeps its configured `bar.fold-curve`
//! from rest and hands an interrupted slide to an [`Animated`] spring; the
//! eye's darts (`eye::Iris`) keep their own tween.

use std::collections::HashMap;
use std::time::Instant;

use eclipse_ui::motion::{Animated, Motion};
use eclipse_ui::tokens::motion as tok;

use crate::layout::{ChipOut, Pin, WidgetOut};
use crate::model::Window;

/// One task chip: a live window, or a ghost of one animating out.
#[derive(Debug, Clone)]
pub struct Chip {
    pub window: Window,
    pub width: Animated,
    pub presence: Animated,
    /// The width the chip's content is laid out at: its target, so it never
    /// reflows while the width moves.
    pub content: f32,
    /// Closed; drawn only until its presence reaches zero.
    pub gone: bool,
    /// The content width of the face being faded out, while its rung
    /// changes: a label that appears or goes cross-fades, never pops.
    pub prev: Option<f32>,
    /// How far the current face has faded in over `prev`'s.
    pub swap: Animated,
}

impl Chip {
    /// The glass's width this frame.
    pub fn visible(&self) -> f32 {
        (self.width.value().max(0.0) * self.presence.value().clamp(0.0, 1.0)).round()
    }

    fn animating(&self) -> bool {
        self.width.animating() || self.presence.animating() || self.swap.animating()
    }

    /// The face's rung at content width `width`.
    fn detail(&self, width: f32) -> crate::layout::Detail {
        crate::layout::chip_detail(self.window.label(), self.window.name(), width)
    }

    /// Lay the face out at `to`. A change of rung (a label arriving, going,
    /// or turning from title to name) cross-fades from the face on screen;
    /// a change inside one rung only moves the edge.
    fn relay(&mut self, to: f32, snap: bool, now: Instant) {
        if to == self.content {
            return;
        }
        let changed = self.detail(to) != self.detail(self.content);
        if changed && !snap {
            // The outgoing face keeps the ink it has on screen now, so a
            // change arriving mid-fade continues rather than restarts.
            let shown = self.swap.value().clamp(0.0, 1.0);
            self.prev = Some(self.content);
            self.swap.snap(1.0 - shown);
            self.swap.set_target(1.0, now);
        } else if snap {
            self.prev = None;
            self.swap.snap(1.0);
        }
        self.content = to;
    }
}

/// One widget's animated body width (grip excluded) and presence.
#[derive(Debug, Clone, Copy)]
pub struct Widget {
    pub extent: Animated,
    pub presence: Animated,
}

/// A grip being dragged.
#[derive(Debug, Clone)]
pub struct Drag {
    pub key: String,
    /// The body width the press found.
    pub start: f32,
    /// Travel since the press; negative is the reveal direction.
    pub dx: f32,
    at: Instant,
    /// Body width per second, smoothed.
    pub velocity: f32,
}

impl Drag {
    pub fn new(key: String, start: f32, now: Instant) -> Drag {
        Drag {
            key,
            start,
            dx: 0.0,
            at: now,
            velocity: 0.0,
        }
    }

    /// The body width the finger asks for.
    pub fn live(&self) -> f32 {
        (self.start - self.dx).max(0.0)
    }

    /// Follow the finger to `dx`.
    pub fn follow(&mut self, dx: f32, now: Instant) {
        let dt = now.saturating_duration_since(self.at).as_secs_f32();
        if dt > 0.0 {
            // Leftward travel widens the body, so the body's speed is the
            // travel's, negated.
            let inst = -(dx - self.dx) / dt;
            self.velocity = tok::VELOCITY_BLEND * inst + (1.0 - tok::VELOCITY_BLEND) * self.velocity;
        }
        self.dx = dx;
        self.at = now;
    }

    /// A press and release that barely moved.
    pub fn is_tap(&self) -> bool {
        self.dx.abs() < tok::TAP_SLOP
    }
}

/// Where a released drag comes to rest: the nearest of `rests` to where its
/// velocity would carry it.
pub fn settle(extent: f32, velocity: f32, rests: &[(Pin, f32)]) -> Option<Pin> {
    let aim = extent + velocity * tok::FLING_LOOKAHEAD_S;
    rests
        .iter()
        .min_by(|a, b| (a.1 - aim).abs().total_cmp(&(b.1 - aim).abs()))
        .map(|(p, _)| *p)
}

/// Everything on the bar that moves.
#[derive(Debug)]
pub struct Bar {
    pub chips: Vec<Chip>,
    pub widgets: HashMap<String, Widget>,
    pub drag: Option<Drag>,
    /// What the human last did to each widget, and the content category it
    /// was done to (see `widgets::category`).
    pub pins: HashMap<String, (Pin, String)>,
    /// How far Now Playing's current cover has developed in over its slot.
    pub art: Animated,
    /// The cover `art` is fading in, by the service's key.
    art_key: Option<String>,
    motion: Motion,
    /// Whether anything has been placed yet: the first layout lands at once.
    laid_out: bool,
    /// The workspace the chips were laid out for; a switch lands at once.
    workspace: Option<usize>,
}

impl Default for Bar {
    fn default() -> Self {
        Bar {
            chips: Vec::new(),
            widgets: HashMap::new(),
            drag: None,
            pins: HashMap::new(),
            art: Animated::new(0.0, Motion::DEFAULT),
            art_key: None,
            motion: Motion::DEFAULT,
            laid_out: false,
            workspace: None,
        }
    }
}

impl Bar {
    pub fn motion(&self) -> Motion {
        self.motion
    }

    /// Adopt `bar.motion.*`. Anything in flight lands.
    pub fn set_motion(&mut self, motion: Motion) {
        self.motion = motion;
        self.art.set_motion(motion);
        for c in &mut self.chips {
            c.width.set_motion(motion);
            c.presence.set_motion(motion);
            c.swap.set_motion(motion);
        }
        for w in self.widgets.values_mut() {
            w.extent.set_motion(motion);
            w.presence.set_motion(motion);
        }
    }

    /// Whether the next retarget should land at once rather than move.
    ///
    /// A grip under the finger snaps everything: the dragged body follows
    /// 1:1, so a neighbour still easing toward its target would run into it.
    pub fn snaps(&self, workspace: Option<usize>) -> bool {
        let dragging = self.drag.as_ref().is_some_and(|d| d.dx != 0.0);
        !self.laid_out || self.workspace != workspace || self.motion.snaps() || dragging
    }

    /// Now Playing's cover is `key` now. A new cover develops in from its
    /// placeholder — it lands after the track, into a slot already reserved,
    /// so it must never pop in. The first layout, and motion off, land at
    /// once; no cover is the placeholder at once (there is nothing left to
    /// fade out).
    pub fn retarget_art(&mut self, key: Option<&str>, now: Instant) {
        if self.art_key.as_deref() == key {
            return;
        }
        self.art_key = key.map(str::to_owned);
        match key {
            Some(_) if self.laid_out && !self.motion.snaps() => {
                self.art.snap(0.0);
                self.art.set_target(1.0, now);
            }
            Some(_) => self.art.snap(1.0),
            None => self.art.snap(0.0),
        }
    }

    /// Point the chips at the solver's answer. `live` is the strip in order;
    /// the first `out.len()` of it are shown and the rest are in `+N`.
    pub fn retarget_chips(
        &mut self,
        live: &[&Window],
        out: &[ChipOut],
        workspace: Option<usize>,
        now: Instant,
    ) {
        let snap = self.snaps(workspace);
        let motion = self.motion;
        let old = std::mem::take(&mut self.chips);
        let mut next: Vec<Chip> = Vec::with_capacity(live.len());
        for (i, w) in live.iter().enumerate() {
            let target = out.get(i).map(|o| o.width);
            let existing = old.iter().position(|c| c.window.handle == w.handle && !c.gone);
            let mut chip = match existing {
                Some(at) => old[at].clone(),
                None => {
                    // Born at its own width, and grown in by presence.
                    let width = target.unwrap_or_default();
                    Chip {
                        window: (*w).clone(),
                        width: Animated::new(width, motion),
                        presence: Animated::new(0.0, motion),
                        content: width,
                        gone: false,
                        prev: None,
                        swap: Animated::new(1.0, motion),
                    }
                }
            };
            chip.window = (*w).clone();
            let presence = if target.is_some() { 1.0 } else { 0.0 };
            if let Some(t) = target {
                chip.relay(t, snap, now);
                aim(&mut chip.width, t, snap, now);
            }
            aim(&mut chip.presence, presence, snap, now);
            next.push(chip);
        }
        // Ghosts: a closed window's chip stays where it was, closing.
        if !snap {
            for (i, c) in old.iter().enumerate() {
                if next.iter().any(|n| n.window.handle == c.window.handle && !n.gone)
                    || (c.gone && !c.animating())
                {
                    continue;
                }
                let mut ghost = c.clone();
                ghost.gone = true;
                aim(&mut ghost.presence, 0.0, false, now);
                let after = old[..i]
                    .iter()
                    .rev()
                    .find_map(|p| next.iter().position(|n| n.window.handle == p.window.handle));
                let at = after.map_or(0, |a| a + 1);
                next.insert(at, ghost);
            }
        }
        self.chips = next;
        self.laid_out = true;
        self.workspace = workspace;
    }

    /// Point the widgets at the solver's answer: `(key, present, out)` in
    /// `bar.widgets.order`.
    pub fn retarget_widgets(&mut self, widgets: &[(String, bool, WidgetOut)], snap: bool, now: Instant) {
        let motion = self.motion;
        self.widgets
            .retain(|k, _| widgets.iter().any(|(key, _, _)| key == k));
        for (key, present, out) in widgets {
            let dragged = self.drag.as_ref().is_some_and(|d| d.key == *key);
            let w = self.widgets.entry(key.clone()).or_insert_with(|| Widget {
                extent: Animated::new(out.extent, motion),
                presence: Animated::new(0.0, motion),
            });
            if *present {
                // A widget arriving from nothing arrives at its own width.
                if w.presence.value() <= 0.0 || dragged || snap {
                    w.extent.snap(out.extent);
                } else {
                    aim(&mut w.extent, out.extent, false, now);
                }
                aim(&mut w.presence, 1.0, snap, now);
            } else {
                // Leaving: the width stays, the presence closes over it.
                aim(&mut w.presence, 0.0, snap, now);
            }
        }
    }

    /// Advance everything to `now`, and forget ghosts that have closed.
    pub fn tick(&mut self, now: Instant) {
        for c in &mut self.chips {
            c.width.tick(now);
            c.presence.tick(now);
            c.swap.tick(now);
            if !c.swap.animating() {
                c.prev = None;
            }
        }
        self.chips.retain(|c| !(c.gone && !c.animating()));
        for w in self.widgets.values_mut() {
            w.extent.tick(now);
            w.presence.tick(now);
        }
        self.art.tick(now);
    }

    /// Whether another frame is needed.
    pub fn animating(&self) -> bool {
        self.drag.is_some()
            || self.art.animating()
            || self.chips.iter().any(Chip::animating)
            || self
                .widgets
                .values()
                .any(|w| w.extent.animating() || w.presence.animating())
    }

    /// Let go of the grip: move `key`'s body to `target` at the finger's speed.
    pub fn fling(&mut self, key: &str, target: f32, velocity: f32, now: Instant) {
        if let Some(w) = self.widgets.get_mut(key) {
            w.extent.fling(target, velocity, now);
        }
    }
}

fn aim(a: &mut Animated, target: f32, snap: bool, now: Instant) {
    if snap {
        a.snap(target);
    } else if a.target() != target {
        a.set_target(target, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn win(handle: u64) -> Window {
        Window {
            handle,
            app_id: "kitty".into(),
            title: "t".into(),
            workspace: Some(1),
            output: Some(0),
            focused: false,
            minimized: false,
            pid: None,
            trust: crate::model::Trust::Private,
        }
    }

    fn out(n: usize, width: f32) -> Vec<ChipOut> {
        (0..n)
            .map(|i| ChipOut {
                x: i as f32 * width,
                width,
            })
            .collect()
    }

    #[test]
    fn the_first_layout_lands_and_later_ones_move() {
        let (a, b) = (win(1), win(2));
        let mut bar = Bar::default();
        let t0 = Instant::now();
        bar.retarget_chips(&[&a], &out(1, 120.0), Some(1), t0);
        assert_eq!(bar.chips[0].visible(), 120.0);
        assert!(!bar.animating());

        bar.retarget_chips(&[&a, &b], &out(2, 80.0), Some(1), t0);
        assert!(bar.animating());
        assert_eq!(bar.chips[1].visible(), 0.0, "a new chip grows in from nothing");
        assert_eq!(bar.chips[0].content, 80.0, "content is laid out at the target");
        bar.tick(t0 + Duration::from_secs(2));
        assert!(!bar.animating());
        assert_eq!(bar.chips[1].visible(), 80.0);
    }

    #[test]
    fn a_closed_window_closes_in_place_and_is_forgotten() {
        let (a, b, c) = (win(1), win(2), win(3));
        let mut bar = Bar::default();
        let t0 = Instant::now();
        bar.retarget_chips(&[&a, &b, &c], &out(3, 100.0), Some(1), t0);
        bar.retarget_chips(&[&a, &c], &out(2, 100.0), Some(1), t0);
        let order: Vec<u64> = bar.chips.iter().map(|c| c.window.handle).collect();
        assert_eq!(order, vec![1, 2, 3]);
        assert!(bar.chips[1].gone);
        bar.tick(t0 + Duration::from_secs(2));
        assert_eq!(bar.chips.len(), 2);
    }

    #[test]
    fn a_workspace_switch_lands_at_once() {
        let (a, b) = (win(1), win(2));
        let mut bar = Bar::default();
        let t0 = Instant::now();
        bar.retarget_chips(&[&a], &out(1, 100.0), Some(1), t0);
        bar.retarget_chips(&[&b], &out(1, 100.0), Some(2), t0);
        assert!(!bar.animating());
        assert_eq!(bar.chips.len(), 1);
    }

    fn wout(extent: f32) -> WidgetOut {
        WidgetOut {
            x: 0.0,
            width: extent,
            extent,
            state: crate::layout::State::Core,
        }
    }

    /// A compress interrupted mid-flight and sent back open keeps its value
    /// and its speed at the turn, never moves more than a spring's step per
    /// frame, lands exactly, and then stops the clock.
    #[test]
    fn an_interrupted_compress_turns_without_a_jump() {
        let mut bar = Bar::default();
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        bar.retarget_widgets(&[("np".into(), true, wout(200.0))], true, t0);
        bar.retarget_widgets(&[("np".into(), true, wout(0.0))], false, t0);
        for n in (8..=64).step_by(8) {
            bar.tick(ms(n));
        }
        let before = bar.widgets["np"].extent;
        assert!(before.value() > 0.0 && before.value() < 200.0, "mid-flight");
        assert!(before.velocity() < 0.0, "compressing");

        bar.retarget_widgets(&[("np".into(), true, wout(200.0))], false, ms(64));
        let after = bar.widgets["np"].extent;
        assert_eq!(after.value(), before.value(), "no jump at the turn");
        assert_eq!(after.velocity(), before.velocity(), "no velocity kink");

        let mut prev = after.value();
        for n in (72..=1200).step_by(8) {
            bar.tick(ms(n));
            let v = bar.widgets["np"].extent.value();
            assert!((v - prev).abs() < 25.0, "frame at {n}ms leapt {prev} -> {v}");
            prev = v;
        }
        assert_eq!(prev, 200.0, "lands exactly");
        assert!(!bar.animating(), "and the clock stops");
    }

    /// Once everything has landed — widths, presences, a rung cross-fade
    /// and a cover developing in — the bar asks for no more frames.
    #[test]
    fn the_clock_stops_when_settled() {
        let (a, b) = (win(1), win(2));
        let mut bar = Bar::default();
        let t0 = Instant::now();
        bar.retarget_chips(&[&a], &out(1, 400.0), Some(1), t0);
        bar.retarget_art(Some("one"), t0);
        bar.retarget_chips(&[&a, &b], &out(2, 20.0), Some(1), t0);
        bar.retarget_art(Some("two"), t0);
        bar.retarget_widgets(&[("np".into(), true, wout(120.0))], false, t0);
        assert!(bar.animating());
        bar.tick(t0 + Duration::from_secs(3));
        assert!(!bar.animating());
        assert!(
            bar.chips.iter().all(|c| c.prev.is_none()),
            "the outgoing face is dropped"
        );
        assert_eq!(bar.art.value(), 1.0);
    }

    /// Motion off lands every change at once: no frames at all.
    #[test]
    fn motion_off_snaps_everything() {
        let (a, b) = (win(1), win(2));
        let mut bar = Bar::default();
        bar.set_motion(Motion::SNAP);
        let t0 = Instant::now();
        bar.retarget_chips(&[&a], &out(1, 400.0), Some(1), t0);
        bar.retarget_chips(&[&a, &b], &out(2, 20.0), Some(1), t0);
        bar.retarget_art(Some("cover"), t0);
        let snap = bar.snaps(Some(1));
        bar.retarget_widgets(&[("np".into(), true, wout(120.0))], snap, t0);
        bar.retarget_widgets(&[("np".into(), true, wout(0.0))], snap, t0);
        assert!(!bar.animating());
        assert_eq!(bar.chips[1].visible(), 20.0);
        assert!(bar.chips[0].prev.is_none(), "no cross-fade under motion off");
        assert_eq!(bar.widgets["np"].extent.value(), 0.0);
        assert_eq!(bar.art.value(), 1.0);
    }

    /// A chip whose rung changes cross-fades its two faces, and a change
    /// arriving mid-fade continues from the ink on screen.
    #[test]
    fn a_rung_change_cross_fades_and_a_reversal_continues() {
        let a = win(1);
        let mut bar = Bar::default();
        let t0 = Instant::now();
        bar.retarget_chips(&[&a], &out(1, 400.0), Some(1), t0);
        bar.retarget_chips(&[&a], &out(1, 20.0), Some(1), t0);
        assert_eq!(bar.chips[0].prev, Some(400.0));
        assert_eq!(bar.chips[0].swap.value(), 0.0, "the new face starts clear");
        bar.tick(t0 + Duration::from_millis(80));
        let shown = bar.chips[0].swap.value();
        assert!(shown > 0.0 && shown < 1.0);
        bar.retarget_chips(&[&a], &out(1, 400.0), Some(1), t0 + Duration::from_millis(80));
        assert_eq!(bar.chips[0].prev, Some(20.0));
        assert!((bar.chips[0].swap.value() - (1.0 - shown)).abs() < 1e-6);
        // A move inside one rung only moves the edge.
        bar.tick(t0 + Duration::from_secs(2));
        bar.retarget_chips(&[&a], &out(1, 380.0), Some(1), t0 + Duration::from_secs(2));
        assert!(bar.chips[0].prev.is_none());
    }

    /// A cover arriving on the first layout is simply there; one arriving
    /// later develops in over its placeholder; no cover is the placeholder.
    #[test]
    fn a_cover_develops_in_after_the_first_layout() {
        let a = win(1);
        let mut bar = Bar::default();
        let t0 = Instant::now();
        bar.retarget_art(Some("one"), t0);
        assert_eq!(bar.art.value(), 1.0);
        assert!(!bar.animating());
        bar.retarget_chips(&[&a], &out(1, 100.0), Some(1), t0);
        bar.retarget_art(Some("two"), t0);
        assert_eq!(bar.art.value(), 0.0);
        bar.tick(t0 + Duration::from_millis(60));
        let mid = bar.art.value();
        assert!(mid > 0.0 && mid < 1.0);
        bar.retarget_art(Some("two"), t0 + Duration::from_millis(60));
        assert_eq!(bar.art.value(), mid, "the same cover does not restart");
        bar.retarget_art(None, t0 + Duration::from_millis(60));
        assert_eq!(bar.art.value(), 0.0);
        assert!(!bar.animating());
    }

    #[test]
    fn a_flick_carries_past_the_midpoint() {
        let rests = [(Pin::Collapsed, 0.0), (Pin::Open, 100.0), (Pin::Revealed, 200.0)];
        // Released at 40, short of halfway, but moving fast toward open.
        assert_eq!(settle(40.0, 600.0, &rests), Some(Pin::Open));
        assert_eq!(settle(40.0, 0.0, &rests), Some(Pin::Collapsed));
        assert_eq!(settle(160.0, 0.0, &rests), Some(Pin::Revealed));
    }

    #[test]
    fn a_drag_follows_the_finger_and_knows_a_tap() {
        let t0 = Instant::now();
        let mut d = Drag::new("volume".into(), 50.0, t0);
        d.follow(-2.0, t0 + Duration::from_millis(16));
        assert!(d.is_tap());
        d.follow(-30.0, t0 + Duration::from_millis(32));
        assert!(!d.is_tap());
        assert_eq!(d.live(), 80.0);
        assert!(d.velocity > 0.0, "leftward travel widens the body");
    }
}
