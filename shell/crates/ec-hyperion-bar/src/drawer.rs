// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's reveal drawers: wifi, bluetooth (and, in later changes, audio and
//! the calendar). The same mechanism as the start menu (`menu.rs`) — the bar
//! surface itself grows, and a panel rolls out of the pill under (or, on a
//! bottom bar, over) the cell that opened it — instead of a popup surface.
//!
//! The panel's extent follows an [`Animated`] height in pixels. A change in
//! what the content needs ([`Drawer::retarget_height`]) moves the same
//! animation to a new target: the drawer never closes and reopens, so a scan
//! that adds three networks grows the sheet instead of flickering it.
//!
//! This file is the state and its pure rules; the bar surface's size, input
//! region and keyboard mode are `app.rs`'s, and the drawing is `view.rs`'s.

use std::time::Instant;

use ec_ui::motion::{Animated, Motion};
use ec_ui::tokens::{bar, drawer};

use crate::menu::{Rect, BIG};

/// What a drawer is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawerKind {
    Wifi,
    Bluetooth,
    Audio,
    Calendar,
}

impl DrawerKind {
    /// The panel's width. One width for every kind today; a kind that needs
    /// more room (the calendar) says so here and the region follows.
    pub fn width(self) -> f32 {
        match self {
            DrawerKind::Wifi | DrawerKind::Bluetooth | DrawerKind::Audio | DrawerKind::Calendar => drawer::W,
        }
    }
}

pub struct Drawer {
    /// What the panel holds. Kept while it closes, so the view has something
    /// to draw as it rolls back in; look at [`Drawer::showing`] first.
    pub kind: Option<DrawerKind>,
    /// Where the drawer is going: open or closed.
    pub open: bool,
    /// The panel's left edge, surface-local, already clamped so the panel fits
    /// the surface (see [`panel_left`]).
    left: f32,
    /// The panel's height fully out: what the content needs right now.
    pub height: f32,
    /// Pixels of the panel the surface holds: follows `height` while open and
    /// zero while closed.
    pub reveal: Animated,
    /// The input-region boxes last sent to the surface (see `app::sync_region`).
    pub region: Option<Vec<Rect>>,
}

impl Drawer {
    pub fn new(motion: Motion) -> Self {
        Drawer {
            kind: None,
            open: false,
            left: 0.0,
            height: 0.0,
            reveal: Animated::new(0.0, motion),
            region: None,
        }
    }

    /// Open `kind` with its panel at `left`, `full` pixels tall. When another
    /// kind is already out the panel changes hands without closing.
    pub fn open(&mut self, kind: DrawerKind, left: f32, full: f32, now: Instant) {
        self.kind = Some(kind);
        self.open = true;
        self.left = left;
        self.height = full;
        self.reveal.set_target(full, now);
    }

    pub fn close(&mut self, now: Instant) {
        self.open = false;
        self.reveal.set_target(0.0, now);
    }

    /// Open `kind`, or close it if it is the one already out. Returns whether
    /// a drawer is open afterwards.
    pub fn toggle(&mut self, kind: DrawerKind, left: f32, full: f32, now: Instant) -> bool {
        if self.is_open(kind) {
            self.close(now);
            false
        } else {
            self.open(kind, left, full, now);
            true
        }
    }

    /// Move an open drawer to a new full height without closing it. Returns
    /// whether anything moved.
    pub fn retarget_height(&mut self, full: f32, now: Instant) -> bool {
        if !self.open || (self.height - full).abs() < 0.5 {
            return false;
        }
        self.height = full;
        self.reveal.set_target(full, now);
        true
    }

    /// Open, or still rolling in.
    pub fn showing(&self) -> bool {
        self.open || self.reveal.value() > 0.0
    }

    pub fn is_open(&self, kind: DrawerKind) -> bool {
        self.open && self.kind == Some(kind)
    }

    /// What the panel holds, while it is out or rolling in.
    pub fn which(&self) -> Option<DrawerKind> {
        self.kind
    }

    /// The panel's left edge in surface pixels.
    pub fn anchor_x(&self) -> f32 {
        self.left
    }

    /// How much of the panel the surface holds right now, in whole pixels,
    /// never more than the sheet's `full_h` (`view::drawer_height`). The view
    /// clips its sheet to this and the surface is this much taller.
    pub fn extent(&self, full_h: u32) -> u32 {
        self.reveal.value().clamp(0.0, full_h as f32).round() as u32
    }

    /// How far out, `0..=1`, for fades that ride the slide.
    pub fn progress(&self, full_h: u32) -> f32 {
        if full_h == 0 {
            return 0.0;
        }
        (self.reveal.value() / full_h as f32).clamp(0.0, 1.0)
    }

    /// The panel's width: its kind's.
    pub fn width(&self) -> f32 {
        self.kind.map_or(0.0, DrawerKind::width)
    }
}

/// The panel's left edge for a cell centred at `center` on a surface
/// `surface_w` wide: centred under the cell, kept inside the surface.
pub fn panel_left(kind: DrawerKind, center: f32, surface_w: f32) -> f32 {
    let w = kind.width();
    (center - w / 2.0).min(surface_w - w).max(0.0).round()
}

/// The input region of a bar with a drawer out, surface-local: the panel's
/// box at `x`, `w` wide, and the pill. Abyss draws the glass as the smooth
/// union of these boxes, so this is also the shape the blur takes.
///
/// As with the start menu, a top bar's boxes do not depend on the surface's
/// height — the panel runs off the bottom and is cut there — so one region
/// holds for the whole slide. A bottom bar's pill sits at `height - PILL_H`.
pub fn region(top: bool, x: i32, w: i32, height: u32) -> Vec<Rect> {
    let pill_h = bar::PILL_H as i32;
    let h = height as i32;
    if top {
        vec![(x, 0, w, BIG), (0, 0, BIG, pill_h)]
    } else {
        vec![(x, 0, w, h), (0, h - pill_h, BIG, pill_h)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapped() -> Drawer {
        Drawer::new(Motion::SNAP)
    }

    #[test]
    fn a_new_drawer_is_not_showing() {
        let d = snapped();
        assert!(!d.showing());
        assert_eq!(d.extent(200), 0);
        assert_eq!(d.which(), None);
    }

    #[test]
    fn the_extent_follows_the_reveal() {
        let mut d = Drawer::new(Motion::default());
        let t0 = Instant::now();
        d.open(DrawerKind::Wifi, 300.0, 200.0, t0);
        assert!(d.showing());
        assert!(d.reveal.animating());
        // Not out yet; the slide has only just started.
        assert!(d.extent(200) < 200);
        d.reveal.snap(100.0);
        assert_eq!(d.extent(200), 100);
        assert!((d.progress(200) - 0.5).abs() < 1e-6);
        d.reveal.snap(200.0);
        assert_eq!(d.extent(200), 200);
        // A spring can overshoot; the surface never grows past the sheet.
        d.reveal.snap(230.0);
        assert_eq!(d.extent(200), 200);
    }

    #[test]
    fn closing_keeps_showing_until_it_is_in() {
        let mut d = Drawer::new(Motion::default());
        let t0 = Instant::now();
        d.open(DrawerKind::Bluetooth, 300.0, 120.0, t0);
        d.reveal.snap(120.0);
        d.close(t0);
        assert!(!d.open);
        assert!(d.showing(), "still rolling in");
        assert_eq!(
            d.which(),
            Some(DrawerKind::Bluetooth),
            "the view still has its content"
        );
        d.reveal.snap(0.0);
        assert!(!d.showing());
    }

    #[test]
    fn a_height_change_animates_and_never_closes() {
        let mut d = Drawer::new(Motion::default());
        let t0 = Instant::now();
        d.open(DrawerKind::Wifi, 300.0, 100.0, t0);
        d.reveal.snap(100.0);
        assert!(d.retarget_height(180.0, t0));
        assert!(d.open, "still open");
        assert_eq!(d.which(), Some(DrawerKind::Wifi));
        assert_eq!(d.reveal.target(), 180.0);
        // It starts from where it was rather than jumping.
        assert_eq!(d.extent(180), 100);
        assert!(d.reveal.animating());
        // The same height again moves nothing.
        assert!(!d.retarget_height(180.0, t0));
    }

    #[test]
    fn a_height_change_snaps_with_motion_off() {
        let mut d = snapped();
        let t0 = Instant::now();
        d.open(DrawerKind::Wifi, 300.0, 100.0, t0);
        assert_eq!(d.extent(100), 100);
        d.retarget_height(60.0, t0);
        assert_eq!(d.extent(60), 60);
        assert!(d.open);
    }

    #[test]
    fn a_closed_drawer_ignores_a_height_change() {
        let mut d = snapped();
        let t0 = Instant::now();
        d.open(DrawerKind::Wifi, 300.0, 100.0, t0);
        d.close(t0);
        assert!(!d.retarget_height(180.0, t0));
        assert!(!d.showing());
        assert_eq!(d.extent(180), 0, "it does not reopen");
    }

    #[test]
    fn the_same_cell_toggles_and_another_takes_over() {
        let mut d = snapped();
        let t0 = Instant::now();
        assert!(d.toggle(DrawerKind::Wifi, 300.0, 100.0, t0));
        assert!(d.is_open(DrawerKind::Wifi));
        assert!(!d.toggle(DrawerKind::Wifi, 300.0, 100.0, t0));
        assert!(!d.open);
        assert!(d.toggle(DrawerKind::Wifi, 300.0, 100.0, t0));
        // The other cell while this one is out: the panel changes hands
        // without closing.
        assert!(d.toggle(DrawerKind::Bluetooth, 340.0, 140.0, t0));
        assert!(d.is_open(DrawerKind::Bluetooth));
        assert!(!d.is_open(DrawerKind::Wifi));
        assert_eq!(d.anchor_x(), 340.0);
        assert_eq!(d.extent(140), 140);
    }

    #[test]
    fn the_panel_is_centred_under_the_cell_and_kept_on_the_surface() {
        let w = drawer::W;
        let k = DrawerKind::Wifi;
        assert_eq!(panel_left(k, 700.0, 1440.0), (700.0 - w / 2.0).round());
        // Near the right end it stops at the edge.
        assert_eq!(panel_left(k, 1430.0, 1440.0), (1440.0 - w).round());
        // Near the left end, at the surface's origin.
        assert_eq!(panel_left(k, 10.0, 1440.0), 0.0);
        // A surface narrower than the panel does not go negative.
        assert_eq!(panel_left(k, 10.0, 100.0), 0.0);
    }

    #[test]
    fn a_top_region_does_not_depend_on_the_height() {
        assert_eq!(region(true, 400, 248, 40), region(true, 400, 248, 400));
        // A bottom bar's pill rides the surface's far edge.
        let [panel, pill] = region(false, 400, 248, 400)[..] else {
            panic!("two boxes")
        };
        assert_eq!(pill.1, 400 - bar::PILL_H as i32);
        assert_eq!(panel, (400, 0, 248, 400));
    }

    #[test]
    fn the_region_holds_the_panel_and_the_whole_pill() {
        let [panel, pill] = region(true, 400, 248, 300)[..] else {
            panic!("two boxes")
        };
        assert_eq!((panel.0, panel.2), (400, 248));
        assert_eq!((pill.0, pill.1, pill.3), (0, 0, bar::PILL_H as i32));
        assert!(pill.2 >= BIG);
    }
}
