// SPDX-License-Identifier: AGPL-3.0-only
//! Retained-grid damage planning (T-07 §4 pass 3; RENDERING-05, -09, PERFORMANCE-14).
//!
//! The retained grid and background textures are row rings: logical row `y` lives at
//! physical row `(offset + y) % rows`. [`Planner::plan`] turns the grid's [`Damage`] plus its
//! `changed` rows into ring/copy operations and the set of rows to redraw. The GPU executor
//! (and the CPU model in the mirror test) apply the operations in order, then redraw.
//!
//! Choices where T-07 is silent:
//! * a region scroll that does not span every column (DECSLRM) is not copied: every row of
//!   the region is redrawn, because per-row instance ranges cannot be split by column;
//! * newly exposed rows of any scroll are always added to the redraw set, whether or not
//!   the grid listed them in `changed`.

use apoth_grid::Damage;

/// The row ring's offset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowRing {
    rows: usize,
    offset: usize,
}

impl RowRing {
    /// A ring of `rows` rows at offset 0.
    #[must_use]
    pub fn new(rows: usize) -> Self {
        Self { rows, offset: 0 }
    }

    /// Row count.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Current offset (the row-offset uniform).
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Physical row of logical row `y`.
    #[must_use]
    pub fn phys(&self, y: usize) -> usize {
        if self.rows == 0 {
            0
        } else {
            (self.offset + y) % self.rows
        }
    }

    /// Advances so that logical row `y + delta` becomes `y`.
    pub fn advance(&mut self, delta: i32) {
        if self.rows > 0 {
            self.offset =
                (self.offset as i64 + i64::from(delta)).rem_euclid(self.rows as i64) as usize;
        }
    }
}

/// One texture operation, in logical rows, applied in order before the redraws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Full-grid scroll: the ring offset advanced by `delta` (already applied to the ring).
    Advance {
        /// Net rows up (positive) or down (negative).
        delta: i32,
    },
    /// Region scroll over full-width rows `top..=bottom`, copied through a scratch texture
    /// (never overlapping in place). For `delta > 0` rows `top+delta..=bottom` move to
    /// `top..=bottom-delta`; for `delta < 0` rows `top..=bottom+delta` move to
    /// `top-delta..=bottom`.
    RegionCopy {
        /// First row.
        top: usize,
        /// Last row.
        bottom: usize,
        /// Net rows up (positive) or down (negative).
        delta: i32,
    },
}

/// The result of planning one frame.
#[derive(Debug, Default)]
pub struct Plan {
    /// Texture operations, in order.
    pub ops: Vec<Op>,
    /// Logical rows to redraw, ascending and unique.
    pub redraw: Vec<u32>,
    /// A frame must be presented (cursor movement alone needs one).
    pub present: bool,
}

/// Reusable planner; allocation-free once its buffers have capacity.
#[derive(Debug, Default)]
pub struct Planner {
    plan: Plan,
    mark: Vec<bool>,
}

impl Planner {
    /// An empty planner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The plan produced by the last [`Planner::plan`].
    #[must_use]
    pub fn last(&self) -> &Plan {
        &self.plan
    }

    /// Plans `damage` over a `rows` x `cols` grid, advancing `ring` for full-grid scrolls.
    /// `extra` rows (forced invalidation, pending glyphs) are merged into the redraw set.
    pub fn plan(
        &mut self,
        damage: Damage,
        changed: &[u32],
        extra: &[u32],
        rows: usize,
        cols: usize,
        ring: &mut RowRing,
    ) -> &Plan {
        self.plan.ops.clear();
        self.plan.redraw.clear();
        self.plan.present = !matches!(damage, Damage::None);
        self.mark.clear();
        self.mark.resize(rows, false);

        let mut all = false;
        match damage {
            Damage::None | Damage::CursorOnly => {}
            Damage::Rows => {}
            Damage::Full => all = true,
            Damage::Scroll {
                top,
                bottom,
                left,
                right,
                delta,
            } => {
                let height = bottom.saturating_sub(top) + 1;
                if bottom >= rows
                    || top > bottom
                    || right >= cols
                    || delta == 0
                    || delta.unsigned_abs() as usize >= height
                {
                    all = true;
                } else {
                    let full_width = left == 0 && right + 1 == cols;
                    let full_height = top == 0 && bottom + 1 == rows;
                    let n = delta.unsigned_abs() as usize;
                    if full_width && full_height {
                        ring.advance(delta);
                        self.plan.ops.push(Op::Advance { delta });
                    } else if full_width {
                        self.plan.ops.push(Op::RegionCopy { top, bottom, delta });
                    } else {
                        for y in top..=bottom {
                            self.mark[y] = true;
                        }
                    }
                    if full_width {
                        if delta > 0 {
                            for y in bottom + 1 - n..=bottom {
                                self.mark[y] = true;
                            }
                        } else {
                            for y in top..top + n {
                                self.mark[y] = true;
                            }
                        }
                    }
                }
            }
        }
        if all {
            self.mark.iter_mut().for_each(|m| *m = true);
        }
        for &y in changed.iter().chain(extra) {
            if let Some(m) = self.mark.get_mut(y as usize) {
                *m = true;
            }
        }
        for (y, m) in self.mark.iter().enumerate() {
            if *m {
                self.plan.redraw.push(y as u32);
            }
        }
        if !self.plan.redraw.is_empty() {
            self.plan.present = true;
        }
        &self.plan
    }
}

/// Moves per-row payloads (instance ranges) with their rows for a scroll plan. For
/// `Advance` and `RegionCopy` the moved rows are rotated in place; no allocation.
pub fn move_rows<T>(rows: &mut [T], op: Op) {
    match op {
        Op::Advance { delta } => rotate(rows, delta),
        Op::RegionCopy { top, bottom, delta } => {
            if bottom < rows.len() {
                rotate(&mut rows[top..=bottom], delta);
            }
        }
    }
}

fn rotate<T>(s: &mut [T], delta: i32) {
    if s.is_empty() {
        return;
    }
    let n = delta.unsigned_abs() as usize % s.len();
    if delta > 0 {
        s.rotate_left(n);
    } else {
        s.rotate_right(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scroll(top: usize, bottom: usize, left: usize, right: usize, delta: i32) -> Damage {
        Damage::Scroll {
            top,
            bottom,
            left,
            right,
            delta,
        }
    }

    #[test]
    fn none_is_zero_work() {
        let mut p = Planner::new();
        let mut r = RowRing::new(24);
        let plan = p.plan(Damage::None, &[], &[], 24, 80, &mut r);
        assert!(!plan.present && plan.ops.is_empty() && plan.redraw.is_empty());
    }

    #[test]
    fn cursor_only_presents_without_redraw() {
        let mut p = Planner::new();
        let mut r = RowRing::new(24);
        let plan = p.plan(Damage::CursorOnly, &[], &[], 24, 80, &mut r);
        assert!(plan.present && plan.redraw.is_empty() && plan.ops.is_empty());
    }

    #[test]
    fn rows_dedup_and_sort() {
        let mut p = Planner::new();
        let mut r = RowRing::new(10);
        let plan = p.plan(Damage::Rows, &[5, 2, 5], &[9, 2], 10, 80, &mut r);
        assert_eq!(plan.redraw, vec![2, 5, 9]);
    }

    #[test]
    fn full_grid_scroll_advances_ring_and_exposes_rows() {
        let mut p = Planner::new();
        let mut r = RowRing::new(10);
        let plan = p.plan(scroll(0, 9, 0, 79, 3), &[], &[], 10, 80, &mut r);
        assert_eq!(plan.ops, vec![Op::Advance { delta: 3 }]);
        assert_eq!(plan.redraw, vec![7, 8, 9]);
        assert_eq!(r.offset(), 3);
        assert_eq!(r.phys(0), 3);
        assert_eq!(r.phys(7), 0);
        let plan = p.plan(scroll(0, 9, 0, 79, -2), &[], &[], 10, 80, &mut r);
        assert_eq!(plan.redraw, vec![0, 1]);
        assert_eq!(r.offset(), 1);
    }

    #[test]
    fn region_scroll_copies_through_scratch() {
        let mut p = Planner::new();
        let mut r = RowRing::new(10);
        let plan = p.plan(scroll(2, 6, 0, 79, 1), &[], &[], 10, 80, &mut r);
        assert_eq!(
            plan.ops,
            vec![Op::RegionCopy {
                top: 2,
                bottom: 6,
                delta: 1
            }]
        );
        assert_eq!(plan.redraw, vec![6]);
        assert_eq!(r.offset(), 0);
    }

    #[test]
    fn partial_width_region_redraws_rows() {
        let mut p = Planner::new();
        let mut r = RowRing::new(10);
        let plan = p.plan(scroll(2, 4, 10, 20, 1), &[], &[], 10, 80, &mut r);
        assert!(plan.ops.is_empty());
        assert_eq!(plan.redraw, vec![2, 3, 4]);
    }

    #[test]
    fn delta_at_region_height_is_full() {
        let mut p = Planner::new();
        let mut r = RowRing::new(10);
        let plan = p.plan(scroll(2, 4, 0, 79, 3), &[], &[], 10, 80, &mut r);
        assert_eq!(plan.redraw.len(), 10);
        assert!(plan.ops.is_empty());
    }

    #[test]
    fn move_rows_follows_scroll() {
        let mut v: Vec<u32> = (0..6).collect();
        move_rows(
            &mut v,
            Op::RegionCopy {
                top: 1,
                bottom: 4,
                delta: 1,
            },
        );
        assert_eq!(v, vec![0, 2, 3, 4, 1, 5]);
        move_rows(&mut v, Op::Advance { delta: -1 });
        assert_eq!(v, vec![5, 0, 2, 3, 4, 1]);
    }
}
