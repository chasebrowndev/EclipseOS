// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop grid under `mode "de"` (COMP-05 §2, §7.1).
//!
//! Desktops are the human workspaces, `workspace::COUNT` of them, and nothing
//! else: there is no second model. The grid is only a way to walk them, laid
//! out row by row (`general.desktop-rows` rows, `ceil(COUNT / rows)` columns),
//! so desktop 1 is the top-left cell and a short last row simply has no cells
//! past the last desktop. Agent workspaces are not human workspaces and never
//! appear here.
//!
//! Switching goes through [`super::switch_workspace`], so sticky windows,
//! focus, animation and the `workspace` event behave exactly as they do for
//! the numbered binds and `switch_workspace`.

use crate::input::Direction;

use crate::state::AbyssState;

use super::{de, workspace};

/// The grid's shape. Plain data: `rows` is clamped to `1..=COUNT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub rows: usize,
    pub columns: usize,
}

impl Grid {
    pub fn new(rows: i32) -> Grid {
        let rows = (rows.max(1) as usize).min(workspace::COUNT);
        Grid {
            rows,
            columns: workspace::COUNT.div_ceil(rows),
        }
    }

    pub fn of(state: &AbyssState) -> Grid {
        Grid::new(state.config.general.desktop_rows)
    }

    fn present(&self, row: usize, col: usize) -> bool {
        row < self.rows && col < self.columns && row * self.columns + col < workspace::COUNT
    }

    /// The desktop (0-based) one step from `from` toward `dir`, or `None` when
    /// there is nowhere to go: an edge without `wrap`, or a row or column of
    /// one cell. A wrap skips the cells a short last row does not have.
    pub fn step(&self, from: usize, dir: Direction, wrap: bool) -> Option<usize> {
        if from >= workspace::COUNT {
            return None;
        }
        let (row, col) = (from / self.columns, from % self.columns);
        let (dr, dc): (isize, isize) = match dir {
            Direction::Left => (0, -1),
            Direction::Right => (0, 1),
            Direction::Up => (-1, 0),
            Direction::Down => (1, 0),
        };
        let (mut r, mut c) = (row as isize, col as isize);
        // At most one lap of the longer side.
        for _ in 0..self.rows.max(self.columns) {
            r += dr;
            c += dc;
            let (rows, cols) = (self.rows as isize, self.columns as isize);
            if !(0..rows).contains(&r) || !(0..cols).contains(&c) {
                if !wrap {
                    return None;
                }
                r = r.rem_euclid(rows);
                c = c.rem_euclid(cols);
            }
            if self.present(r as usize, c as usize) {
                let to = r as usize * self.columns + c as usize;
                return (to != from).then_some(to);
            }
        }
        None
    }
}

/// The 1-based desktop one step from the focused output's active one, if
/// there is one and the mode is `de`.
pub fn target(state: &AbyssState, dir: Direction) -> Option<usize> {
    if !de::stacking(state) {
        return None;
    }
    let active = state.outputs.focused()?.active;
    Grid::of(state)
        .step(active, dir, state.config.general.desktop_wrap)
        .map(|i| i + 1)
}

/// Switch the focused output to the desktop next to its active one. Does
/// nothing outside `de`, at an unwrapped edge or on a one-cell grid. True when
/// a switch was made.
pub fn switch(state: &mut AbyssState, dir: Direction) -> bool {
    let Some(to) = target(state, dir) else {
        return false;
    };
    super::switch_workspace(state, to);
    true
}

/// Move the focused window one desktop toward `dir` and go there with it,
/// whatever `general.follow-window-to-workspace` says (the chord names both).
pub fn move_with(state: &mut AbyssState, dir: Direction) -> bool {
    let Some(to) = target(state, dir) else {
        return false;
    };
    let Some(window) = state.focus.clone() else {
        return false;
    };
    // The target came from the focused output's grid and `switch_workspace`
    // switches that output: a window living on another output is not ours to
    // carry there.
    let focused = state.outputs.focused().map(|e| e.id);
    if super::output_of_window(state, &window).is_some_and(|o| Some(o) != focused) {
        return false;
    }
    super::move_window_to_workspace(state, &window, to);
    if state.outputs.focused().is_some_and(|e| e.active + 1 != to) {
        super::switch_workspace(state, to);
    }
    super::focus_window(state, &window);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(g: Grid, from: usize, dir: Direction, wrap: bool) -> Option<usize> {
        g.step(from, dir, wrap)
    }

    #[test]
    fn the_shape_follows_the_row_count_and_is_always_valid() {
        assert_eq!(Grid::new(2), Grid { rows: 2, columns: 5 });
        assert_eq!(Grid::new(1), Grid { rows: 1, columns: 10 });
        assert_eq!(Grid::new(3), Grid { rows: 3, columns: 4 });
        assert_eq!(Grid::new(10), Grid { rows: 10, columns: 1 });
        // A value that slipped past validation cannot make a bad grid.
        assert_eq!(Grid::new(0), Grid::new(1));
        assert_eq!(Grid::new(99), Grid::new(10));
    }

    #[test]
    fn two_rows_of_five_step_and_wrap() {
        let g = Grid::new(2);
        // Desktop 1 is index 0, top-left; desktop 6 is the bottom-left.
        assert_eq!(walk(g, 0, Direction::Right, true), Some(1));
        assert_eq!(walk(g, 0, Direction::Down, true), Some(5));
        assert_eq!(walk(g, 5, Direction::Up, true), Some(0));
        assert_eq!(
            walk(g, 4, Direction::Right, true),
            Some(0),
            "wraps within the row"
        );
        assert_eq!(walk(g, 0, Direction::Left, true), Some(4));
        assert_eq!(
            walk(g, 0, Direction::Up, true),
            Some(5),
            "wraps within the column"
        );
        assert_eq!(walk(g, 9, Direction::Down, true), Some(4));
    }

    #[test]
    fn without_wrap_an_edge_is_a_dead_end() {
        let g = Grid::new(2);
        for (from, dir) in [
            (0, Direction::Left),
            (0, Direction::Up),
            (4, Direction::Right),
            (9, Direction::Right),
            (5, Direction::Down),
        ] {
            assert_eq!(walk(g, from, dir, false), None, "{from} {dir:?}");
        }
        assert_eq!(walk(g, 0, Direction::Right, false), Some(1));
    }

    #[test]
    fn a_short_last_row_has_no_cells_to_land_on() {
        // 3 rows of 4: the last row holds desktops 9 and 10 only.
        let g = Grid::new(3);
        assert_eq!(
            walk(g, 9, Direction::Right, true),
            Some(8),
            "wraps past the missing cells"
        );
        assert_eq!(walk(g, 8, Direction::Left, true), Some(9));
        assert_eq!(walk(g, 1, Direction::Down, true), Some(5));
        assert_eq!(walk(g, 5, Direction::Down, true), Some(9));
        // Column 2 has only two cells: down from the second wraps to the top.
        assert_eq!(walk(g, 6, Direction::Down, true), Some(2));
        assert_eq!(walk(g, 6, Direction::Down, false), None);
        assert_eq!(walk(g, 9, Direction::Up, false), Some(5));
    }

    #[test]
    fn a_line_of_one_cell_goes_nowhere() {
        let single_row = Grid::new(1);
        assert_eq!(walk(single_row, 3, Direction::Up, true), None);
        assert_eq!(walk(single_row, 3, Direction::Down, true), None);
        assert_eq!(walk(single_row, 9, Direction::Right, true), Some(0));
        let single_col = Grid::new(10);
        assert_eq!(walk(single_col, 3, Direction::Left, true), None);
        assert_eq!(walk(single_col, 9, Direction::Down, true), Some(0));
    }

    #[test]
    fn every_cell_reaches_a_distinct_neighbour_or_none() {
        for rows in 1..=workspace::COUNT as i32 {
            let g = Grid::new(rows);
            for from in 0..workspace::COUNT {
                for dir in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
                    for wrap in [true, false] {
                        if let Some(to) = g.step(from, dir, wrap) {
                            assert!(to < workspace::COUNT && to != from, "{rows} {from} {dir:?}");
                        }
                    }
                }
            }
        }
    }
}
