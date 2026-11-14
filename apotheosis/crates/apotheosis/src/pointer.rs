// SPDX-License-Identifier: AGPL-3.0-only
//! Pointer geometry and the click-count state machine (T-05 §5). Pure: no I/O, no clock of its
//! own (the caller passes the time), so the gesture rules are tested without a compositor.

use std::time::{Duration, Instant};

use apoth_grid::{SelKind, Side};
use apoth_input::{Mods, MouseButton};

/// evdev button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// The protocol button for an evdev code; `None` for codes the terminal protocols cannot carry.
pub(crate) fn button_of(code: u32) -> Option<MouseButton> {
    match code {
        BTN_LEFT => Some(MouseButton::Left),
        BTN_MIDDLE => Some(MouseButton::Middle),
        BTN_RIGHT => Some(MouseButton::Right),
        BTN_SIDE => Some(MouseButton::Extra(8)),
        BTN_EXTRA => Some(MouseButton::Extra(9)),
        _ => None,
    }
}

/// Where the text area sits in the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geometry {
    /// Left and top padding in physical pixels (the top one includes the overlay bar).
    pub(crate) pad: (u32, u32),
    /// Cell size in physical pixels.
    pub(crate) cell: (u32, u32),
    pub(crate) scale: f64,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
}

/// A pointer position resolved to the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) col: usize,
    pub(crate) row: usize,
    /// Which half of the cell (decides whether the cell is inside a selection end).
    pub(crate) side: Side,
    /// Pixel position inside the text area (for SGR-Pixels).
    pub(crate) px: (u32, u32),
    /// The pointer is above the text area.
    pub(crate) above: bool,
    /// The pointer is below the text area.
    pub(crate) below: bool,
}

impl Geometry {
    /// Resolve a logical surface position. Positions outside the text area clamp to its edge
    /// cell, with `above` / `below` set so a drag can autoscroll.
    pub(crate) fn hit(&self, x: f64, y: f64) -> Hit {
        let cw = f64::from(self.cell.0.max(1));
        let ch = f64::from(self.cell.1.max(1));
        let rx = x * self.scale - f64::from(self.pad.0);
        let ry = y * self.scale - f64::from(self.pad.1);
        let width = cw * self.cols as f64;
        let height = ch * self.rows as f64;
        let above = ry < 0.0;
        let below = ry >= height;
        let (col, side) = Self::axis(rx, cw, self.cols);
        let (row, _) = Self::axis(ry, ch, self.rows);
        let clamp_px = |v: f64, max: f64| v.clamp(0.0, (max - 1.0).max(0.0)) as u32;
        Hit {
            col,
            row,
            side,
            px: (clamp_px(rx, width), clamp_px(ry, height)),
            above,
            below,
        }
    }

    fn axis(v: f64, size: f64, n: usize) -> (usize, Side) {
        if n == 0 || v < 0.0 {
            return (0, Side::Left);
        }
        let idx = (v / size).floor();
        if idx >= n as f64 {
            return (n - 1, Side::Right);
        }
        let frac = v / size - idx;
        (
            idx as usize,
            if frac < 0.5 { Side::Left } else { Side::Right },
        )
    }
}

/// Counts consecutive presses: at most three, within the interval, on (nearly) the same cell.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClickTracker {
    last: Option<(Instant, (usize, usize), u8)>,
}

impl ClickTracker {
    /// Register a press at `cell`; the click count it makes (1, 2 or 3; a fourth starts over).
    pub(crate) fn press(&mut self, now: Instant, cell: (usize, usize), interval: Duration) -> u8 {
        let count = match self.last {
            Some((t, (c, r), n))
                if now.saturating_duration_since(t) <= interval
                    && r == cell.1
                    && c.abs_diff(cell.0) <= 1
                    && n < 3 =>
            {
                n + 1
            }
            _ => 1,
        };
        self.last = Some((now, cell, count));
        count
    }

    pub(crate) fn reset(&mut self) {
        self.last = None;
    }
}

/// Which selection a press starts: the geometry from the click count and the modifiers.
/// `triple_line` is `selection.triple-click line` (else the shell-integration output zone).
pub(crate) fn kind_for(count: u8, mods: Mods, triple_line: bool) -> SelKind {
    if mods.contains(Mods::ALT) && count == 1 {
        return SelKind::Block;
    }
    match count {
        0 | 1 => SelKind::Simple,
        2 => SelKind::Word,
        _ if triple_line => SelKind::Line,
        _ => SelKind::Semantic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo() -> Geometry {
        Geometry {
            pad: (10, 20),
            cell: (10, 20),
            scale: 2.0,
            rows: 5,
            cols: 8,
        }
    }

    #[test]
    fn maps_buttons() {
        assert_eq!(button_of(0x110), Some(MouseButton::Left));
        assert_eq!(button_of(0x112), Some(MouseButton::Middle));
        assert_eq!(button_of(0x113), Some(MouseButton::Extra(8)));
        assert_eq!(button_of(0x120), None);
    }

    #[test]
    fn hit_resolves_cell_and_half() {
        let g = geo();
        // Logical (10, 20) is physical (20, 40): 10 px into the text area on both axes.
        let h = g.hit(10.0, 20.0);
        assert_eq!((h.col, h.row), (1, 1));
        assert_eq!(h.side, Side::Left);
        assert_eq!(h.px, (10, 20));
        let h = g.hit(13.0, 20.0); // 16 px into the text area: right half of column 1
        assert_eq!((h.col, h.side), (1, Side::Right));
        assert!(!h.above && !h.below);
    }

    #[test]
    fn hit_clamps_outside_and_flags_edges() {
        let g = geo();
        let h = g.hit(0.0, 0.0); // inside the padding
        assert_eq!((h.col, h.row, h.side), (0, 0, Side::Left));
        assert!(h.above);
        let h = g.hit(5000.0, 5000.0);
        assert_eq!((h.col, h.row, h.side), (7, 4, Side::Right));
        assert!(h.below && !h.above);
        // Pixel position stays inside the text area (SGR-Pixels is 0-based, within it).
        assert_eq!(h.px, (79, 99));
    }

    #[test]
    fn empty_grid_is_safe() {
        let g = Geometry {
            rows: 0,
            cols: 0,
            ..geo()
        };
        let h = g.hit(50.0, 50.0);
        assert_eq!((h.col, h.row), (0, 0));
    }

    #[test]
    fn click_counts_cycle_within_interval_and_distance() {
        let mut c = ClickTracker::default();
        let t0 = Instant::now();
        let iv = Duration::from_millis(400);
        assert_eq!(c.press(t0, (3, 1), iv), 1);
        assert_eq!(c.press(t0 + Duration::from_millis(100), (3, 1), iv), 2);
        assert_eq!(c.press(t0 + Duration::from_millis(200), (4, 1), iv), 3);
        // A fourth press starts over.
        assert_eq!(c.press(t0 + Duration::from_millis(300), (4, 1), iv), 1);
        // Too slow, a different row, or too far: a new first click.
        assert_eq!(c.press(t0 + Duration::from_millis(900), (4, 1), iv), 1);
        assert_eq!(c.press(t0 + Duration::from_millis(1000), (4, 2), iv), 1);
        assert_eq!(c.press(t0 + Duration::from_millis(1100), (9, 2), iv), 1);
        c.reset();
        assert_eq!(c.press(t0 + Duration::from_millis(1150), (9, 2), iv), 1);
    }

    #[test]
    fn selection_kind_follows_count_and_modifiers() {
        assert_eq!(kind_for(1, Mods::NONE, true), SelKind::Simple);
        assert_eq!(kind_for(2, Mods::NONE, true), SelKind::Word);
        assert_eq!(kind_for(3, Mods::NONE, true), SelKind::Line);
        assert_eq!(kind_for(3, Mods::NONE, false), SelKind::Semantic);
        assert_eq!(kind_for(1, Mods::ALT, true), SelKind::Block);
        // A double click with Alt is still a word selection.
        assert_eq!(kind_for(2, Mods::ALT, true), SelKind::Word);
    }
}
