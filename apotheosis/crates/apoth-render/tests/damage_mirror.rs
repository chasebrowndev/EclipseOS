// SPDX-License-Identifier: AGPL-3.0-only
//! Damage mirror test (T-07 §6, PERFORMANCE-30): randomised editor-shaped traffic with
//! full-screen scrolls, region scrolls (DECSTBM), column-restricted scrolls (DECSLRM) and
//! scroll-then-edit within one frame. After every frame the retained row-ring texture model,
//! driven only by `Planner` output, must equal a from-scratch render, and the per-row
//! payloads moved by `move_rows` must equal freshly built ones.
//!
//! The producer below follows the grid's damage contract (T-02 §5): scrolls of one region
//! accumulate; a scroll of another region, or one of at least the region height, makes the
//! frame `Full`; dirty flags move with their rows; rows vacated by a scroll are dirty.

#![allow(clippy::needless_range_loop)]

use apoth_grid::Damage;
use apoth_render::damage::{Op, Planner, RowRing, move_rows};

const C: usize = 7;
const R: usize = 9;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

struct Term {
    content: Vec<Vec<u32>>,
    dirty: Vec<bool>,
    scroll: Option<(usize, usize, usize, usize, i32)>,
    full: bool,
}

impl Term {
    fn new() -> Self {
        Self {
            content: vec![vec![0; C]; R],
            dirty: vec![false; R],
            scroll: None,
            full: false,
        }
    }

    fn edit(&mut self, y: usize, x: usize, v: u32) {
        self.content[y][x] = v;
        self.dirty[y] = true;
    }

    /// Vertical scroll of columns `left..=right` over rows `top..=bottom`.
    fn do_scroll(&mut self, top: usize, bottom: usize, left: usize, right: usize, delta: i32) {
        let height = bottom - top + 1;
        let n = delta.unsigned_abs() as usize;
        let full_width = left == 0 && right == C - 1;
        if n >= height {
            for y in top..=bottom {
                for x in left..=right {
                    self.content[y][x] = 0;
                }
                self.dirty[y] = true;
            }
            self.full = true;
            return;
        }
        if delta > 0 {
            for y in top..=bottom - n {
                for x in left..=right {
                    self.content[y][x] = self.content[y + n][x];
                }
                if full_width {
                    self.dirty[y] = self.dirty[y + n];
                } else {
                    self.dirty[y] = true;
                }
            }
            for y in bottom + 1 - n..=bottom {
                for x in left..=right {
                    self.content[y][x] = 0;
                }
                self.dirty[y] = true;
            }
        } else {
            for y in (top + n..=bottom).rev() {
                for x in left..=right {
                    self.content[y][x] = self.content[y - n][x];
                }
                if full_width {
                    self.dirty[y] = self.dirty[y - n];
                } else {
                    self.dirty[y] = true;
                }
            }
            for y in top..top + n {
                for x in left..=right {
                    self.content[y][x] = 0;
                }
                self.dirty[y] = true;
            }
        }
        if !self.full {
            match self.scroll {
                None => self.scroll = Some((top, bottom, left, right, delta)),
                Some((t, b, l, r, d)) if (t, b, l, r) == (top, bottom, left, right) => {
                    let nd = d + delta;
                    self.scroll = Some((t, b, l, r, nd));
                    if nd.unsigned_abs() as usize >= height {
                        self.full = true;
                    }
                }
                Some(_) => self.full = true,
            }
        }
        // Partial-width rows were all marked dirty above; the dirty flags of rows that moved
        // under a full-width scroll moved with them.
    }

    fn end_frame(&mut self) -> (Damage, Vec<u32>) {
        let changed: Vec<u32> = (0..R)
            .filter(|&y| self.dirty[y])
            .map(|y| y as u32)
            .collect();
        let damage = if self.full {
            Damage::Full
        } else if let Some((top, bottom, left, right, delta)) = self.scroll {
            Damage::Scroll {
                top,
                bottom,
                left,
                right,
                delta,
            }
        } else if !changed.is_empty() {
            Damage::Rows
        } else {
            Damage::None
        };
        self.dirty.iter_mut().for_each(|d| *d = false);
        self.scroll = None;
        self.full = false;
        (damage, changed)
    }
}

/// CPU model of the retained texture: physical rows, a ring offset and a scratch.
struct Texture {
    phys: Vec<Vec<u32>>,
    payload: Vec<Vec<u32>>,
}

impl Texture {
    fn apply(&mut self, ops: &[Op], redraw: &[u32], ring: &RowRing, term: &Term) {
        for op in ops {
            match *op {
                Op::Advance { .. } => {}
                Op::RegionCopy { top, bottom, delta } => {
                    let n = delta.unsigned_abs() as usize;
                    let (src0, dst0, count) = if delta > 0 {
                        (top + n, top, bottom + 1 - top - n)
                    } else {
                        (top, top + n, bottom + 1 - top - n)
                    };
                    // Through a scratch, never in place.
                    let scratch: Vec<Vec<u32>> = (0..count)
                        .map(|k| self.phys[ring.phys(src0 + k)].clone())
                        .collect();
                    for (k, row) in scratch.into_iter().enumerate() {
                        self.phys[ring.phys(dst0 + k)] = row;
                    }
                }
            }
            move_rows(&mut self.payload, *op);
        }
        for &y in redraw {
            let y = y as usize;
            self.phys[ring.phys(y)] = term.content[y].clone();
            self.payload[y] = term.content[y].clone();
        }
    }
}

fn run(seed: u64, frames: usize) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut term = Term::new();
    let mut tex = Texture {
        phys: vec![vec![0; C]; R],
        payload: vec![vec![0; C]; R],
    };
    let mut ring = RowRing::new(R);
    let mut planner = Planner::new();
    let mut stamp = 1u32;

    for frame in 0..frames {
        for _ in 0..1 + rng.below(4) {
            match rng.below(10) {
                0..=3 => {
                    let (y, x) = (rng.below(R), rng.below(C));
                    stamp += 1;
                    term.edit(y, x, stamp);
                }
                4 | 5 => {
                    let d = 1 + rng.below(3) as i32;
                    let d = if rng.below(2) == 0 { d } else { -d };
                    term.do_scroll(0, R - 1, 0, C - 1, d);
                }
                6 | 7 => {
                    let top = rng.below(R - 1);
                    let bottom = top + 1 + rng.below(R - 1 - top);
                    let d = 1 + rng.below(3) as i32;
                    let d = if rng.below(2) == 0 { d } else { -d };
                    term.do_scroll(top, bottom, 0, C - 1, d);
                }
                8 => {
                    let top = rng.below(R - 1);
                    let bottom = top + 1 + rng.below(R - 1 - top);
                    let left = rng.below(C - 1);
                    let right = left + rng.below(C - left);
                    let d = 1 + rng.below(2) as i32;
                    let d = if rng.below(2) == 0 { d } else { -d };
                    term.do_scroll(top, bottom, left, right, d);
                }
                _ => {
                    // Scroll-then-edit in one frame.
                    term.do_scroll(0, R - 1, 0, C - 1, 1);
                    let (y, x) = (rng.below(R), rng.below(C));
                    stamp += 1;
                    term.edit(y, x, stamp);
                }
            }
        }
        let (damage, changed) = term.end_frame();
        let plan = planner.plan(damage, &changed, &[], R, C, &mut ring);
        if damage == Damage::None {
            assert!(
                !plan.present && plan.ops.is_empty() && plan.redraw.is_empty(),
                "idle must do no work"
            );
        }
        let (ops, redraw) = (plan.ops.clone(), plan.redraw.clone());
        tex.apply(&ops, &redraw, &ring, &term);

        for y in 0..R {
            assert_eq!(
                tex.phys[ring.phys(y)],
                term.content[y],
                "seed {seed} frame {frame} row {y} texture diverged after {damage:?}"
            );
            assert_eq!(
                tex.payload[y], term.content[y],
                "seed {seed} frame {frame} row {y} payload diverged"
            );
        }
    }
}

#[test]
fn texture_model_equals_from_scratch_after_every_frame() {
    for seed in 1..=24 {
        run(seed, 400);
    }
}
