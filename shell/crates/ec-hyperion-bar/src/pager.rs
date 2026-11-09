// SPDX-License-Identifier: AGPL-3.0-only
//! The pager as the desktop grid under `mode "de"` (COMP-05 §7.1): the pure
//! half.
//!
//! `get_desktop_grid` says how many `rows` × `columns` of desktops there are
//! and which is active on each output. Desktops are numbered row-major from 1,
//! and that number is what `switch_workspace` takes. A single-row grid keeps
//! the pager's old square tiles; a taller one packs `rows` rows of narrower,
//! shorter tiles into the height of one task chip, so the pill never grows.
//!
//! [`Grid`] is the one place the geometry lives: the view lays tiles out from
//! it and [`Grid::index_at`] reads the same numbers back, so what is drawn
//! and what is hit cannot disagree.
//!
//! The bar does not draw every desktop. [`shown`] picks the occupied ones and
//! the active one of this bar's output; [`Grid::size_shown`] is the width the
//! bar reserves for those survivors, and `slot_rect` places them, wrapping at
//! `columns`, so the drawn pager and the room the task strip is given agree.

use ec_ui::tokens::bar;

/// The desktops a bar draws from its grid: `active` (the one its output is on)
/// and every desktop `occupied` reports, in number order. Empty desktops that
/// are not active are left out; they are reached by the wheel or the keyboard,
/// and the grid is a pager, not a ruler of ten.
pub fn shown(count: usize, active: Option<usize>, occupied: impl Fn(usize) -> bool) -> Vec<usize> {
    (1..=count)
        .filter(|&index| active == Some(index) || occupied(index))
        .collect()
}

/// The grid's shape and the tile it is drawn with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub rows: usize,
    pub columns: usize,
}

impl Grid {
    /// A grid of at least one desktop.
    pub fn new(rows: usize, columns: usize) -> Grid {
        Grid {
            rows: rows.max(1),
            columns: columns.max(1),
        }
    }

    pub fn of(g: &ec_ipc::desktop::DesktopGrid) -> Grid {
        Grid::new(g.rows, g.columns)
    }

    /// How many desktops.
    pub fn count(&self) -> usize {
        self.rows * self.columns
    }

    /// The air between two tiles: across, then down.
    pub fn gap(&self) -> (f32, f32) {
        if self.rows == 1 {
            (bar::GAP, 0.0)
        } else {
            (bar::PAGER_GRID_GAP, bar::PAGER_GRID_GAP)
        }
    }

    /// One tile's width and height.
    pub fn tile(&self) -> (f32, f32) {
        if self.rows == 1 {
            return (bar::PAGER_W, bar::PAGER_H);
        }
        let rows = self.rows as f32;
        let h = (bar::TASK_H - self.gap().1 * (rows - 1.0)) / rows;
        (bar::PAGER_GRID_W, h.floor())
    }

    /// The whole grid's width and height.
    pub fn size(&self) -> (f32, f32) {
        let ((w, h), (gx, gy)) = (self.tile(), self.gap());
        let (c, r) = (self.columns as f32, self.rows as f32);
        (c * w + (c - 1.0) * gx, r * h + (r - 1.0) * gy)
    }

    /// The desktop (1-based, row-major) drawn at `row`, `col`. The view builds
    /// its tiles (and each tile's `Switch`) from this, so the click path and
    /// [`Grid::index_at`] share one numbering.
    pub fn index_of(&self, row: usize, col: usize) -> usize {
        row * self.columns + col + 1
    }

    /// The width and height of a grid that draws only `shown` of its tiles, in
    /// the wrapped layout `slot_rect` places them: `min(shown, columns)` across,
    /// as many rows down as that needs. Zero tiles is zero air.
    pub fn size_shown(&self, shown: usize) -> (f32, f32) {
        if shown == 0 {
            return (0.0, 0.0);
        }
        let ((w, h), (gx, gy)) = (self.tile(), self.gap());
        let cols = shown.min(self.columns) as f32;
        let rows = shown.div_ceil(self.columns) as f32;
        (cols * w + (cols - 1.0) * gx, rows * h + (rows - 1.0) * gy)
    }

    /// Where the `slot`th drawn tile (0-based, in the order [`shown`] gives)
    /// sits: `(x, y, width, height)` from the top-left, wrapping at `columns`.
    /// Every desktop drawn is the same tile it is on the full grid.
    pub fn slot_rect(&self, slot: usize) -> (f32, f32, f32, f32) {
        let (row, col) = (slot / self.columns, slot % self.columns);
        let ((w, h), (gx, gy)) = (self.tile(), self.gap());
        (col as f32 * (w + gx), row as f32 * (h + gy), w, h)
    }

    /// Where the tile of desktop `index` (1-based, row-major) sits on the full
    /// grid: `(x, y, width, height)` from the grid's top-left.
    pub fn rect_of(&self, index: usize) -> Option<(f32, f32, f32, f32)> {
        if index == 0 || index > self.count() {
            return None;
        }
        Some(self.slot_rect(index - 1))
    }

    /// The desktop under `(x, y)`, from the grid's top-left. `None` in the air
    /// between tiles and outside the grid.
    pub fn index_at(&self, x: f32, y: f32) -> Option<usize> {
        let ((w, h), (gx, gy)) = (self.tile(), self.gap());
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (col, row) = ((x / (w + gx)).floor() as usize, (y / (h + gy)).floor() as usize);
        if col >= self.columns || row >= self.rows {
            return None;
        }
        let (in_x, in_y) = (x - col as f32 * (w + gx), y - row as f32 * (h + gy));
        (in_x < w && in_y < h).then_some(row * self.columns + col + 1)
    }
}

/// The wheel over the pager: a notch up goes to the previous desktop, a notch
/// down to the next, in the order the numbers run. The compositor decides what
/// an edge does (`general.desktop-wrap`).
pub fn scroll_step(notches: i32) -> Option<ec_ipc::desktop::Direction> {
    use ec_ipc::desktop::Direction;
    match notches.signum() {
        1 => Some(Direction::Left),
        -1 => Some(Direction::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_row_keeps_the_square_tiles() {
        let g = Grid::new(1, 4);
        assert_eq!(g.tile(), (bar::PAGER_W, bar::PAGER_H));
        let (w, h) = g.size();
        assert_eq!(h, bar::PAGER_H);
        assert_eq!(w, 4.0 * bar::PAGER_W + 3.0 * bar::GAP);
    }

    #[test]
    fn a_tall_grid_fits_the_height_of_one_task_chip() {
        for rows in 2..=4 {
            let (_, h) = Grid::new(rows, 5).size();
            assert!(h <= bar::TASK_H, "{rows} rows are {h}px tall");
        }
    }

    #[test]
    fn every_desktop_is_hit_at_the_middle_of_its_own_tile() {
        let g = Grid::new(2, 5);
        for i in 1..=g.count() {
            let (x, y, w, h) = g.rect_of(i).expect("tile");
            assert_eq!(g.index_at(x + w / 2.0, y + h / 2.0), Some(i));
            // Both corners, inside the tile, are the same desktop.
            assert_eq!(g.index_at(x + 0.5, y + 0.5), Some(i));
            assert_eq!(g.index_at(x + w - 0.5, y + h - 0.5), Some(i));
        }
    }

    /// The numbering the view draws and clicks with is the numbering the
    /// geometry hits: tile (row, col) sits where `rect_of` says its desktop is.
    #[test]
    fn the_drawn_tile_is_the_hit_tile() {
        let g = Grid::new(3, 4);
        for r in 0..g.rows {
            for c in 0..g.columns {
                let i = g.index_of(r, c);
                let (x, y, w, h) = g.rect_of(i).expect("tile");
                assert_eq!(g.index_at(x + w / 2.0, y + h / 2.0), Some(i));
                assert_eq!(x, c as f32 * (w + g.gap().0));
                assert_eq!(y, r as f32 * (h + g.gap().1));
            }
        }
    }

    #[test]
    fn desktops_run_along_the_row_then_down() {
        let g = Grid::new(2, 5);
        let (x, y, _, _) = g.rect_of(6).expect("tile");
        // The sixth is the first of the second row.
        assert_eq!(x, 0.0);
        assert!(y > 0.0);
        assert_eq!(g.rect_of(5).map(|r| r.1), Some(0.0));
    }

    #[test]
    fn the_air_between_tiles_and_outside_the_grid_hits_nothing() {
        let g = Grid::new(2, 5);
        let (x, y, w, h) = g.rect_of(1).expect("tile");
        assert_eq!(g.index_at(x + w + 0.5, y + 1.0), None, "gap across");
        assert_eq!(g.index_at(x + 1.0, y + h + 0.5), None, "gap down");
        assert_eq!(g.index_at(-1.0, 1.0), None);
        let (gw, gh) = g.size();
        assert_eq!(g.index_at(gw + 1.0, 1.0), None);
        assert_eq!(g.index_at(1.0, gh + 1.0), None);
        assert_eq!(g.rect_of(0), None);
        assert_eq!(g.rect_of(11), None);
    }

    #[test]
    fn a_degenerate_grid_is_one_desktop() {
        let g = Grid::new(0, 0);
        assert_eq!(g.count(), 1);
        assert_eq!(g.index_at(1.0, 1.0), Some(1));
    }

    #[test]
    fn the_wheel_steps_through_desktops() {
        use ec_ipc::desktop::Direction;
        assert_eq!(scroll_step(1), Some(Direction::Left));
        assert_eq!(scroll_step(-3), Some(Direction::Right));
        assert_eq!(scroll_step(0), None);
    }

    #[test]
    fn shown_keeps_the_active_and_the_occupied_in_number_order() {
        assert_eq!(shown(10, Some(4), |i| i == 2 || i == 9), vec![2, 4, 9]);
        assert_eq!(shown(10, None, |i| i == 1), vec![1]);
        assert_eq!(shown(10, Some(10), |_| false), vec![10]);
        assert!(shown(10, None, |_| false).is_empty());
    }

    /// The bar reserves `size_shown(n)` for `n` survivors, so that must be the
    /// box the first `n` wrapped slots actually cover, at every count.
    #[test]
    fn the_shown_grid_is_as_big_as_the_tiles_it_draws() {
        for (rows, columns) in [(1, 10), (2, 5), (3, 4)] {
            let g = Grid::new(rows, columns);
            assert_eq!(g.size_shown(g.count()), g.size(), "{rows}x{columns} full");
            assert_eq!(g.size_shown(0), (0.0, 0.0));
            for n in 1..=g.count() {
                let (mut right, mut bottom) = (0.0f32, 0.0f32);
                for slot in 0..n {
                    let (x, y, w, h) = g.slot_rect(slot);
                    right = right.max(x + w);
                    bottom = bottom.max(y + h);
                }
                let (w, h) = g.size_shown(n);
                assert!(
                    (w - right).abs() < 0.01,
                    "{rows}x{columns} n={n} width {w} vs {right}"
                );
                assert!(
                    (h - bottom).abs() < 0.01,
                    "{rows}x{columns} n={n} height {h} vs {bottom}"
                );
            }
        }
    }

    /// Survivors wrap at `columns`: the sixth drawn tile starts the second line.
    #[test]
    fn survivors_wrap_at_the_grids_columns() {
        let g = Grid::new(2, 5);
        let (x, y, _, _) = g.slot_rect(5);
        assert_eq!(x, 0.0);
        assert!(y > 0.0);
        assert_eq!(g.size_shown(3).1, g.tile().1, "three survivors are one line");
    }
}
