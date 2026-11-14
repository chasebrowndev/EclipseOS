// SPDX-License-Identifier: AGPL-3.0-only
//! Window geometry (T-08 §4, T-13 §3): where the overlay bar, the tab strip and the content
//! area go in physical pixels, the window's size in cells for the mux, and each placed pane's
//! pixel origin and grid size. Pure arithmetic; main and the tests share it.
//!
//! `apoth-mux` places split panes edge to edge in cells. A pane that has a neighbour to its
//! right (below) gives its last column (row) to the divider, so frames never sit on text.

use apoth_mux::{PaneId, Placed, Size};
use apoth_ui::Rect;
use apoth_ui::frame::TabPosition;
use apoth_ui::tokens::{metrics, space};

/// Largest grid a pane gets (bounds the term's allocation).
pub(crate) const MAX_COLS: u32 = 1000;
pub(crate) const MAX_ROWS: u32 = 500;

/// What the window geometry depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WinMetrics {
    /// Buffer size in physical pixels.
    pub(crate) buffer: (u32, u32),
    /// Physical pixels per logical pixel.
    pub(crate) scale: f64,
    /// Interface zoom (`interface.font-size`, `interface.scale`).
    pub(crate) zoom: f32,
    /// Padding in physical pixels.
    pub(crate) pad: (u32, u32),
    /// Physical pixels the overlay bar takes at the top (0 without one).
    pub(crate) bar: u32,
    /// The tab strip is shown, and where.
    pub(crate) strip: Option<TabPosition>,
    /// Cell size in physical pixels.
    pub(crate) cell: (u32, u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WinLayout {
    /// Physical pixels per interface unit.
    pub(crate) ui: f32,
    /// The whole window in interface units.
    pub(crate) area: Rect,
    /// The tab strip in interface units.
    pub(crate) strip: Option<Rect>,
    /// Top-left of the cell grid area, physical pixels.
    pub(crate) origin: (i32, i32),
    /// The window's size in cells (what the mux lays out).
    pub(crate) size: Size,
}

/// Physical height of the tab strip at interface scale `ui`.
pub(crate) fn strip_px(ui: f32) -> u32 {
    ((metrics::TAB_H + 2.0 * space::XS) * ui).round() as u32
}

pub(crate) fn window(m: &WinMetrics) -> WinLayout {
    let ui = (m.scale * f64::from(m.zoom)) as f32;
    let ui = if ui > 0.0 { ui } else { 1.0 };
    let (bw, bh) = m.buffer;
    let sh = m.strip.map_or(0, |_| strip_px(ui));
    let (strip_y, top) = match m.strip {
        Some(TabPosition::Top) => (m.bar, m.bar + sh),
        Some(TabPosition::Bottom) => (bh.saturating_sub(sh), m.bar),
        None => (0, m.bar),
    };
    let bottom = match m.strip {
        Some(TabPosition::Bottom) => sh,
        _ => 0,
    };
    let x0 = m.pad.0;
    let y0 = top + m.pad.1;
    let w = bw.saturating_sub(2 * m.pad.0);
    let h = bh.saturating_sub(y0 + m.pad.1 + bottom);
    let (cw, ch) = (m.cell.0.max(1), m.cell.1.max(1));
    let size = Size {
        cols: (w / cw).clamp(1, MAX_COLS),
        rows: (h / ch).clamp(1, MAX_ROWS),
    };
    WinLayout {
        ui,
        area: Rect::new(0.0, 0.0, bw as f32 / ui, bh as f32 / ui),
        strip: m
            .strip
            .map(|_| Rect::new(0.0, strip_y as f32 / ui, bw as f32 / ui, sh as f32 / ui)),
        origin: (x0 as i32, y0 as i32),
        size,
    }
}

/// One pane of the shown tab, placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PanePx {
    pub(crate) id: PaneId,
    /// Top-left of its grid, physical pixels.
    pub(crate) origin: (i32, i32),
    /// Its grid (rows, cols).
    pub(crate) dims: (usize, usize),
}

impl PanePx {
    /// The grid's pixel rectangle `(x, y, w, h)`.
    pub(crate) fn rect(&self, cell: (u32, u32)) -> (i32, i32, u32, u32) {
        (
            self.origin.0,
            self.origin.1,
            self.dims.1 as u32 * cell.0,
            self.dims.0 as u32 * cell.1,
        )
    }

    pub(crate) fn contains(&self, cell: (u32, u32), x: f64, y: f64) -> bool {
        let (rx, ry, w, h) = self.rect(cell);
        x >= f64::from(rx)
            && y >= f64::from(ry)
            && x < f64::from(rx) + f64::from(w)
            && y < f64::from(ry) + f64::from(h)
    }
}

/// Place the tab's panes (stack header lines are skipped: this binary makes no stacks).
pub(crate) fn panes(
    placed: &[Placed],
    size: Size,
    origin: (i32, i32),
    cell: (u32, u32),
) -> Vec<PanePx> {
    placed
        .iter()
        .filter(|p| !p.header)
        .map(|p| {
            let r = p.rect;
            let w = if r.x + r.w < size.cols {
                r.w.saturating_sub(1)
            } else {
                r.w
            };
            let h = if r.y + r.h < size.rows {
                r.h.saturating_sub(1)
            } else {
                r.h
            };
            PanePx {
                id: p.pane,
                origin: (
                    origin.0 + (r.x * cell.0) as i32,
                    origin.1 + (r.y * cell.1) as i32,
                ),
                dims: (h.max(1) as usize, w.max(1) as usize),
            }
        })
        .collect()
}

/// A pane's frame in interface units: its grid outset by a few units, inside the window.
pub(crate) fn frame_rect(p: &PanePx, cell: (u32, u32), ui: f32, area: Rect) -> Rect {
    let (x, y, w, h) = p.rect(cell);
    let out = 3.0;
    let r = Rect::new(
        x as f32 / ui - out,
        y as f32 / ui - out,
        w as f32 / ui + 2.0 * out,
        h as f32 / ui + 2.0 * out,
    );
    let x0 = r.x.max(area.x);
    let y0 = r.y.max(area.y);
    let x1 = r.right().min(area.right());
    let y1 = r.bottom().min(area.bottom());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_mux::Rect as CellRect;

    fn m(strip: Option<TabPosition>, bar: u32) -> WinMetrics {
        WinMetrics {
            buffer: (800, 600),
            scale: 1.0,
            zoom: 1.0,
            pad: (4, 4),
            bar,
            strip,
            cell: (10, 20),
        }
    }

    #[test]
    fn no_strip_is_the_old_single_pane_grid() {
        let l = window(&m(None, 0));
        assert_eq!(l.origin, (4, 4));
        assert_eq!(l.size, Size { cols: 79, rows: 29 });
        assert_eq!(l.strip, None);
    }

    #[test]
    fn strip_and_bar_push_the_grid_down() {
        let sh = strip_px(1.0);
        let l = window(&m(Some(TabPosition::Top), 48));
        assert_eq!(l.origin, (4, (48 + sh + 4) as i32));
        let s = l.strip.unwrap();
        assert_eq!((s.y, s.h), (48.0, sh as f32));
        let b = window(&m(Some(TabPosition::Bottom), 0));
        assert_eq!(b.origin, (4, 4));
        assert_eq!(b.strip.unwrap().bottom(), 600.0);
        assert!(b.size.rows < window(&m(None, 0)).size.rows);
    }

    #[test]
    fn interface_scale_divides_into_units() {
        let mut mm = m(Some(TabPosition::Top), 0);
        mm.scale = 2.0;
        let l = window(&mm);
        assert!((l.ui - 2.0).abs() < f32::EPSILON);
        assert_eq!(l.area, Rect::new(0.0, 0.0, 400.0, 300.0));
    }

    #[test]
    fn dividers_take_a_cell_from_the_left_and_top_panes() {
        let size = Size { cols: 80, rows: 24 };
        let placed = [
            Placed {
                pane: 1,
                rect: CellRect {
                    x: 0,
                    y: 0,
                    w: 40,
                    h: 24,
                },
                header: false,
            },
            Placed {
                pane: 2,
                rect: CellRect {
                    x: 40,
                    y: 0,
                    w: 40,
                    h: 12,
                },
                header: false,
            },
            Placed {
                pane: 3,
                rect: CellRect {
                    x: 40,
                    y: 12,
                    w: 40,
                    h: 12,
                },
                header: false,
            },
        ];
        let p = panes(&placed, size, (4, 4), (10, 20));
        assert_eq!(p[0].dims, (24, 39));
        assert_eq!(p[1].dims, (11, 40));
        assert_eq!(p[2].dims, (12, 40));
        assert_eq!(p[1].origin, (404, 4));
        assert_eq!(p[2].origin, (404, 244));
        assert!(p[1].contains((10, 20), 405.0, 5.0));
        assert!(!p[1].contains((10, 20), 405.0, 4.0 + 11.0 * 20.0));
    }

    #[test]
    fn frames_stay_inside_the_window() {
        let p = PanePx {
            id: 1,
            origin: (0, 0),
            dims: (10, 10),
        };
        let area = Rect::new(0.0, 0.0, 100.0, 200.0);
        let f = frame_rect(&p, (10, 20), 1.0, area);
        assert_eq!((f.x, f.y), (0.0, 0.0));
        assert_eq!((f.w, f.h), (100.0, 200.0));
    }
}
