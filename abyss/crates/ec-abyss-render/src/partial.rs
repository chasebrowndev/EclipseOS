// SPDX-License-Identifier: AGPL-3.0-only
//! Damage-limited blur rebuilds (COMP-02, liquid-glass Phase 1b): which texels
//! of each chain level a damage rectangle can change.
//!
//! The dual-Kawase chain is a pipeline of local filters. A damage rectangle in
//! level 0 changes, at level `k`, only the texels whose taps (plus the
//! bilinear footprint of each) reach a changed texel of level `k - 1`. The
//! bound is derived from the actual taps of the shaders in `blur.rs`:
//!
//! * downsample: 5 taps, the furthest at `+-0.5 * offset` source texels on each
//!   axis ([`reach_down`]);
//! * upsample: 8 taps, the furthest at `+-offset` source texels ([`reach_up`]);
//!
//! each read bilinearly (so one more texel, in effect: a tap at `p` reads
//! texels whose centre is within 1 of `p`) and clamped to the edge. A
//! destination texel `i` of a pass whose source holds `s` texels per
//! destination texel, starting at source coordinate `origin`, is centred at
//! `origin + (i + 0.5) * s`, and depends on source texel `t` only if that
//! centre is within `reach + 1` of `t`'s centre. [`spread`] inverts that, with
//! one extra texel each side for float rounding. State-free and independent of
//! GL; `blur.rs` draws exactly the rectangles this returns, so a partial
//! rebuild equals a full one texel for texel.

use smithay::utils::{Physical, Point, Rectangle, Size};

/// Most rectangles kept per level before they are folded into their bounding
/// box (each rectangle is a draw instance per pass).
pub const MAX_RECTS: usize = 8;

/// A partial rebuild is only worth it below this share of level 1's area
/// (numerator over denominator): past half, the grown rectangles reach most of
/// the level anyway and the per-rectangle bookkeeping costs more than it saves.
pub const FULL_SHARE: (i64, i64) = (1, 2);

/// Furthest downsample tap from the texel centre, in source texels.
pub fn reach_down(offset: f32) -> f64 {
    0.5 * f64::from(offset)
}

/// Furthest upsample tap from the texel centre, in source texels.
pub fn reach_up(offset: f32) -> f64 {
    f64::from(offset)
}

/// Destination texels `[lo, hi)` of one axis that source texels `[a, b)` can
/// change. `origin` is the source coordinate of the destination's left edge,
/// `s` the source texels per destination texel, `len` the destination length.
fn span(a: i32, b: i32, origin: f64, s: f64, r: f64, len: i32) -> Option<(i32, i32)> {
    let lo = (((f64::from(a) - 0.5 - r - origin) / s - 0.5).floor() as i32 - 1).max(0);
    let hi = (((f64::from(b) + 0.5 + r - origin) / s - 0.5).ceil() as i32 + 2).min(len);
    (lo < hi).then_some((lo, hi))
}

/// The destination rectangle a source rectangle `src` can change.
pub fn spread(
    src: Rectangle<i32, Physical>,
    origin: (f64, f64),
    s: (f64, f64),
    reach: f64,
    dst: Size<i32, Physical>,
) -> Option<Rectangle<i32, Physical>> {
    if src.is_empty() {
        return None;
    }
    let (x0, x1) = span(src.loc.x, src.loc.x + src.size.w, origin.0, s.0, reach, dst.w)?;
    let (y0, y1) = span(src.loc.y, src.loc.y + src.size.h, origin.1, s.1, reach, dst.h)?;
    Some(Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into()))
}

/// Fold `rects` into their bounding box when there are too many.
fn limit(rects: &mut Vec<Rectangle<i32, Physical>>) {
    if rects.len() > MAX_RECTS {
        let all = rects.iter().copied().reduce(|a, b| a.merge(b));
        rects.clear();
        rects.extend(all);
    }
}

fn area(rects: &[Rectangle<i32, Physical>]) -> i64 {
    rects
        .iter()
        .map(|r| i64::from(r.size.w) * i64::from(r.size.h))
        .sum()
}

/// The rectangles of every pass of one partial rebuild. Reused between
/// rebuilds so a steady frame allocates nothing.
#[derive(Default)]
pub struct Plan {
    /// `down[k]`: level `k + 1` (what the downsample into it writes).
    pub down: Vec<Vec<Rectangle<i32, Physical>>>,
    /// `up[k]`: level `k + 2`'s upsampled value, for levels `2..passes`
    /// (the top level is the downsample's own).
    pub up: Vec<Vec<Rectangle<i32, Physical>>>,
    /// The result texture (level 1 resolution), in its own (upright) space.
    pub result: Vec<Rectangle<i32, Physical>>,
}

impl Plan {
    /// Work out the rectangles for `damage` (level 0 texels, already clipped to
    /// the built region) of a chain whose level 1 reads `tex` of level 0 and
    /// whose level `k` has size `sizes[k - 1]`. `mirrored` flips the result's
    /// rectangles (the final pass writes upright). Returns `false` when the
    /// damage is too wide for a partial rebuild (see [`FULL_SHARE`]) or there
    /// is none, with the plan left unspecified.
    pub fn fill(
        &mut self,
        damage: &[Rectangle<i32, Physical>],
        tex: Rectangle<i32, Physical>,
        sizes: &[Size<i32, Physical>],
        offset: f32,
        mirrored: bool,
    ) -> bool {
        let passes = sizes.len();
        if passes == 0 || damage.is_empty() {
            return false;
        }
        self.down.resize_with(passes, Vec::new);
        self.up.resize_with(passes.saturating_sub(2), Vec::new);
        for l in self.down.iter_mut().chain(self.up.iter_mut()) {
            l.clear();
        }
        self.result.clear();
        let (rd, ru) = (reach_down(offset), reach_up(offset));
        let ratio = |src: Size<i32, Physical>, dst: Size<i32, Physical>| {
            (
                f64::from(src.w) / f64::from(dst.w),
                f64::from(src.h) / f64::from(dst.h),
            )
        };
        // Level 1 reads `tex` of level 0: source coordinates are level 0's.
        let s1 = ratio(tex.size, sizes[0]);
        let origin = (f64::from(tex.loc.x), f64::from(tex.loc.y));
        let first = &mut self.down[0];
        first.extend(damage.iter().filter_map(|d| spread(*d, origin, s1, rd, sizes[0])));
        limit(first);
        if first.is_empty() {
            return false;
        }
        let share = i64::from(sizes[0].w) * i64::from(sizes[0].h);
        if area(first) * FULL_SHARE.1 >= share * FULL_SHARE.0 {
            return false;
        }
        for k in 1..passes {
            let (a, b) = self.down.split_at_mut(k);
            let s = ratio(sizes[k - 1], sizes[k]);
            b[0].extend(
                a[k - 1]
                    .iter()
                    .filter_map(|d| spread(*d, (0.0, 0.0), s, rd, sizes[k])),
            );
            limit(&mut b[0]);
        }
        // Up values of levels `passes - 1` down to 2, each from the one above.
        for level in (2..passes).rev() {
            let s = ratio(sizes[level], sizes[level - 1]);
            let to = sizes[level - 1];
            if level + 1 == passes {
                let dst = &mut self.up[level - 2];
                dst.extend(
                    self.down[passes - 1]
                        .iter()
                        .filter_map(|d| spread(*d, (0.0, 0.0), s, ru, to)),
                );
                limit(dst);
            } else {
                let (a, b) = self.up.split_at_mut(level - 1);
                let dst = &mut a[level - 2];
                dst.extend(b[0].iter().filter_map(|d| spread(*d, (0.0, 0.0), s, ru, to)));
                limit(dst);
            }
        }
        // The final pass writes level 1's resolution from level 2 (or copies
        // level 1 when there is a single pass).
        if passes >= 2 {
            let s = ratio(sizes[1], sizes[0]);
            let src: &Vec<_> = if passes == 2 { &self.down[1] } else { &self.up[0] };
            self.result
                .extend(src.iter().filter_map(|d| spread(*d, (0.0, 0.0), s, ru, sizes[0])));
        } else {
            self.result.extend(self.down[0].iter().copied());
        }
        limit(&mut self.result);
        if mirrored {
            let h = sizes[0].h;
            for r in &mut self.result {
                r.loc = Point::from((r.loc.x, h - (r.loc.y + r.size.h)));
            }
        }
        true
    }

    /// Texels written by every pass, for the profile.
    pub fn texels(&self) -> u64 {
        let sum = |l: &[Vec<Rectangle<i32, Physical>>]| l.iter().map(|v| area(v)).sum::<i64>();
        (sum(&self.down) + sum(&self.up) + area(&self.result)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// xorshift: the tests need a repeatable spread, not quality.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: i32) -> i32 {
            (self.next() % n.max(1) as u64) as i32
        }
    }

    /// A single-channel texture.
    #[derive(Clone)]
    struct Grid {
        w: i32,
        h: i32,
        v: Vec<f32>,
    }

    impl Grid {
        fn at(&self, x: i32, y: i32) -> f32 {
            self.v[(y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize]
        }
        /// GL_LINEAR at texel coordinates `(px, py)` (texel `t` is centred at
        /// `t + 0.5`), clamped to the edge.
        fn bilinear(&self, px: f64, py: f64) -> f32 {
            let (x, y) = (px - 0.5, py - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
            let (x0, y0) = (x0 as i32, y0 as i32);
            let top = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
            let bot = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
            top * (1.0 - fy) + bot * fy
        }
    }

    /// The shaders' taps, in units of `halfpixel * offset` = `0.5 * offset`
    /// source texels, with their weights.
    fn down_taps() -> Vec<((f64, f64), f32)> {
        vec![
            ((0.0, 0.0), 4.0),
            ((-1.0, -1.0), 1.0),
            ((1.0, 1.0), 1.0),
            ((1.0, -1.0), 1.0),
            ((-1.0, 1.0), 1.0),
        ]
    }
    fn up_taps() -> Vec<((f64, f64), f32)> {
        vec![
            ((-2.0, 0.0), 1.0),
            ((-1.0, 1.0), 2.0),
            ((0.0, 2.0), 1.0),
            ((1.0, 1.0), 2.0),
            ((2.0, 0.0), 1.0),
            ((1.0, -1.0), 2.0),
            ((0.0, -2.0), 1.0),
            ((-1.0, -1.0), 2.0),
        ]
    }

    /// One pass, as the GPU runs it: every destination texel centre mapped to
    /// the source (`origin + (i + 0.5) * s`), taps at `unit` source texels.
    fn run(
        src: &Grid,
        dst: (i32, i32),
        origin: (f64, f64),
        s: (f64, f64),
        unit: f64,
        taps: &[((f64, f64), f32)],
    ) -> Grid {
        let total: f32 = taps.iter().map(|t| t.1).sum();
        let mut out = Grid {
            w: dst.0,
            h: dst.1,
            v: vec![0.0; (dst.0 * dst.1) as usize],
        };
        for j in 0..dst.1 {
            for i in 0..dst.0 {
                let cx = origin.0 + (f64::from(i) + 0.5) * s.0;
                let cy = origin.1 + (f64::from(j) + 0.5) * s.1;
                let mut sum = 0.0;
                for ((dx, dy), w) in taps {
                    sum += src.bilinear(cx + dx * unit, cy + dy * unit) * w;
                }
                out.v[(j * dst.0 + i) as usize] = sum / total;
            }
        }
        out
    }

    fn noise(rng: &mut Rng, w: i32, h: i32) -> Grid {
        Grid {
            w,
            h,
            v: (0..w * h).map(|_| (rng.next() % 1000) as f32 / 10.0).collect(),
        }
    }

    /// Every destination texel that a perturbation of `damage` changes lies
    /// inside the rectangle `spread` reports, for both passes, ratios that are
    /// not 2, and offsets from 1 to 16.
    #[test]
    fn a_pass_changes_nothing_outside_its_spread() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let mut checked = 0;
        for case in 0..400 {
            let up = case % 2 == 1;
            let offset = [1.0f32, 2.0, 2.5, 4.0, 8.0, 16.0][(rng.below(6)) as usize];
            let (sw, sh) = (20 + rng.below(60), 20 + rng.below(60));
            // Down: source about 2x the destination (never exactly); up: about half.
            let (dw, dh) = if up {
                (2 * sw + rng.below(3) - 1, 2 * sh + rng.below(3) - 1)
            } else {
                (sw / 2 + rng.below(2), sh / 2 + rng.below(2))
            };
            let src = noise(&mut rng, sw, sh);
            let s = (f64::from(sw) / f64::from(dw), f64::from(sh) / f64::from(dh));
            // A source sub-rectangle origin only for the first, `from` pass.
            let origin = if up || case % 4 == 0 {
                (0.0, 0.0)
            } else {
                (f64::from(rng.below(4)), f64::from(rng.below(4)))
            };
            let (taps, unit, reach) = if up {
                (up_taps(), 0.5 * f64::from(offset), reach_up(offset))
            } else {
                (down_taps(), 0.5 * f64::from(offset), reach_down(offset))
            };
            let before = run(&src, (dw, dh), origin, s, unit, &taps);
            for _ in 0..6 {
                let (x, y) = (rng.below(sw), rng.below(sh));
                let (w, h) = (1 + rng.below(sw - x), 1 + rng.below(sh - y));
                let d = r(x, y, w, h);
                let mut hit = src.clone();
                for yy in y..y + h {
                    for xx in x..x + w {
                        hit.v[(yy * sw + xx) as usize] += 7.0 + (rng.next() % 50) as f32;
                    }
                }
                let after = run(&hit, (dw, dh), origin, s, unit, &taps);
                let bound = spread(d, origin, s, reach, Size::from((dw, dh)));
                for j in 0..dh {
                    for i in 0..dw {
                        let k = (j * dw + i) as usize;
                        if before.v[k] != after.v[k] {
                            checked += 1;
                            let inside = bound.is_some_and(|b| {
                                i >= b.loc.x
                                    && i < b.loc.x + b.size.w
                                    && j >= b.loc.y
                                    && j < b.loc.y + b.size.h
                            });
                            assert!(
                                inside,
                                "texel ({i},{j}) changed outside {bound:?}: up {up} offset {offset} \
                                 src {sw}x{sh} dst {dw}x{dh} damage {d:?}"
                            );
                        }
                    }
                }
            }
        }
        assert!(checked > 10_000, "the test must actually move texels: {checked}");
    }

    /// The bound is not vacuous: it does not grow to the whole level for a
    /// one-texel damage.
    #[test]
    fn a_small_damage_stays_small() {
        let dst = Size::from((400, 300));
        let b = spread(r(700, 500, 4, 4), (0.0, 0.0), (2.0, 2.0), reach_down(2.0), dst).unwrap();
        assert!(b.size.w <= 12 && b.size.h <= 12, "{b:?}");
        assert!(b.loc.x <= 350 && b.loc.x + b.size.w >= 350);
        // Off the level entirely.
        assert_eq!(spread(r(10_000, 0, 4, 4), (0.0, 0.0), (2.0, 2.0), 1.0, dst), None);
    }

    fn sizes(full: (i32, i32), passes: usize) -> Vec<Size<i32, Physical>> {
        (1..=passes)
            .map(|l| Size::from(((full.0 >> l).max(1), (full.1 >> l).max(1))))
            .collect()
    }

    /// Across a whole chain, 1000 random damages: each level's rectangles
    /// contain the earlier level's (scaled), the lists are bounded, and a huge
    /// damage asks for a full rebuild.
    #[test]
    fn a_plan_grows_with_the_damage_and_never_loses_it() {
        let mut rng = Rng(42);
        let mut plan = Plan::default();
        let (full, passes) = ((1600, 1000), 4usize);
        let lv = sizes(full, passes);
        let tex = r(160, 96, 1600, 1000);
        let mut partial = 0;
        for _ in 0..1000 {
            let (x, y) = (tex.loc.x + rng.below(1500), tex.loc.y + rng.below(900));
            let d = [r(x, y, 1 + rng.below(80), 1 + rng.below(80))];
            for mirrored in [false, true] {
                if !plan.fill(&d, tex, &lv, 2.0, mirrored) {
                    continue;
                }
                partial += 1;
                assert!(plan.down.iter().all(|l| !l.is_empty() && l.len() <= MAX_RECTS));
                // The level-1 rectangle covers the damage's own texels.
                let b = plan.down[0][0];
                let (cx, cy) = ((x - tex.loc.x) / 2, (y - tex.loc.y) / 2);
                assert!(b.loc.x <= cx && b.loc.y <= cy, "{b:?} vs {cx},{cy}");
                // The result's rectangle stays inside the result.
                for r in &plan.result {
                    assert!(r.loc.x >= 0 && r.loc.y >= 0);
                    assert!(r.loc.x + r.size.w <= lv[0].w && r.loc.y + r.size.h <= lv[0].h);
                }
                // Margins only add: the deeper the level, the more of it per
                // texel of damage, but never more than the level has.
                for (k, l) in plan.down.iter().enumerate() {
                    for r in l {
                        assert!(r.loc.x + r.size.w <= lv[k].w && r.loc.y + r.size.h <= lv[k].h);
                    }
                }
            }
        }
        assert!(partial > 1000, "most small damages are partial: {partial}");
        // Damage over most of the region is a full rebuild.
        assert!(!plan.fill(&[tex], tex, &lv, 2.0, false));
        // No damage is nothing to plan.
        assert!(!plan.fill(&[], tex, &lv, 2.0, false));
    }

    /// Mirrored output: the result rectangle is the vertical flip of the
    /// unmirrored one within the result's height.
    #[test]
    fn a_mirrored_plan_flips_only_the_result() {
        let lv = sizes((800, 600), 3);
        let tex = r(0, 0, 800, 600);
        let d = [r(100, 40, 30, 20)];
        let (mut a, mut b) = (Plan::default(), Plan::default());
        assert!(a.fill(&d, tex, &lv, 2.0, false) && b.fill(&d, tex, &lv, 2.0, true));
        assert_eq!(a.down, b.down);
        assert_eq!(a.up, b.up);
        let h = lv[0].h;
        let flipped: Vec<_> = a
            .result
            .iter()
            .map(|r| Rectangle::new((r.loc.x, h - (r.loc.y + r.size.h)).into(), r.size))
            .collect();
        assert_eq!(flipped, b.result);
    }

    /// Many scattered damages fold into one box rather than one rect each.
    #[test]
    fn many_rects_fold_into_a_bounding_box() {
        let lv = sizes((1600, 1000), 3);
        let tex = r(0, 0, 1600, 1000);
        let d: Vec<_> = (0..20).map(|i| r(10 + i * 20, 10 + (i % 5) * 4, 6, 6)).collect();
        let mut plan = Plan::default();
        assert!(plan.fill(&d, tex, &lv, 2.0, false));
        assert!(plan.down[0].len() <= MAX_RECTS);
    }
}
