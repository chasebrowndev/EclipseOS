// SPDX-License-Identifier: AGPL-3.0-only
//! Depth tiers (COMP-02, liquid-glass Phase 1a): which blurred windows share
//! one blur pyramid.
//!
//! A window's *tier* is 0 if no window below it in z-order overlaps its own
//! region, and otherwise one more than the highest tier among the lower
//! windows that do overlap it. Regions are the windows' rectangles (not their
//! blur crops), and every window counts, glass or not.
//!
//! One pyramid is kept per tier. Tier 0 is built from what lies below every
//! window (the wallpaper and the bottom layers). Tier `d` is built from that
//! plus the content of the windows of lower tiers, composited in z-order, so a
//! window sees exactly the lower windows that overlap it and never its own
//! content or anything above it. Tiled windows, which overlap nothing, all
//! share tier 0 whatever their blur margins reach. State-free and
//! allocation-free: the tiers go into a caller-owned scratch vector.

use smithay::utils::{Physical, Rectangle};

/// Stacked layers that get a pyramid each (tiers `0..MAX_TIERS`). A window
/// deeper than that samples the last pyramid: stacks beyond four are not
/// meaningful visually, and the clamp keeps every window on the shared path.
pub const MAX_TIERS: usize = 4;

/// Tier of each of `n` windows listed front to back (`rect_of(k)` is the
/// `k`th window's region). The walk is back to front, so a window's lower
/// neighbours already have theirs.
pub fn assign_tiers(n: usize, rect_of: impl Fn(usize) -> Rectangle<i32, Physical>, tiers: &mut Vec<u8>) {
    tiers.clear();
    tiers.resize(n, 0);
    for k in (0..n).rev() {
        let mine = rect_of(k);
        let mut tier = 0u8;
        for (j, above) in tiers.iter().enumerate().skip(k + 1) {
            if mine.overlaps(rect_of(j)) {
                tier = tier.max(above.saturating_add(1));
            }
        }
        tiers[k] = tier;
    }
}

/// The pyramid a window of `tier` samples: its own tier, clamped to the last.
///
/// The last pyramid is built exactly like the others: the windows of a lower
/// *true* tier (below [`MAX_TIERS`]) composited over what lies below every
/// window. A clamped window therefore does not see a lower window that is
/// itself clamped, which is the whole cost of the cap.
pub fn pyramid_of(tier: u8) -> u8 {
    tier.min(MAX_TIERS as u8 - 1)
}

/// One piece of what pyramid `d` is built from, front to back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Window `k`'s own elements (surfaces, border, shadow).
    Window(usize),
    /// Window `k`'s glass backdrop element, drawn just below its content.
    Glass(usize),
    /// Everything below every window: bottom layers and the wallpaper.
    Base,
}

/// What the pyramid of tier `d` composites, in z-order (front to back), for a
/// topmost member at window index `k_top` (windows are listed front to back, so
/// the windows below it are `k_top + 1..`): each lower window of *true* tier
/// below `d` with its glass, then the base. A tier-0 pyramid is the base alone,
/// shared by every window that overlaps nothing below it. `has_glass(k)` says
/// whether window `k` has a glass element at that point.
pub fn pyramid_parts(
    d: u8,
    k_top: usize,
    tiers: &[u8],
    has_glass: impl Fn(usize) -> bool,
    mut put: impl FnMut(Part),
) {
    if d > 0 {
        for (k, _) in tiers.iter().enumerate().skip(k_top + 1).filter(|(_, t)| **t < d) {
            put(Part::Window(k));
            if has_glass(k) {
                put(Part::Glass(k));
            }
        }
    }
    put(Part::Base);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    /// Front to back.
    fn tiers(rects: &[Rectangle<i32, Physical>]) -> Vec<u8> {
        let mut t = Vec::new();
        assign_tiers(rects.len(), |k| rects[k], &mut t);
        t
    }

    #[test]
    fn windows_that_overlap_nothing_are_all_tier_zero() {
        let rects = [r(0, 0, 100, 100), r(200, 0, 100, 100), r(0, 200, 100, 100)];
        assert_eq!(tiers(&rects), [0, 0, 0]);
    }

    #[test]
    fn adjacent_windows_that_only_touch_stay_tier_zero() {
        // A tiled row: edges meet, nothing overlaps.
        let rects = [r(0, 0, 100, 100), r(100, 0, 100, 100), r(200, 0, 100, 100)];
        assert_eq!(tiers(&rects), [0, 0, 0]);
    }

    #[test]
    fn an_overlap_chain_climbs_one_tier_a_window() {
        // Front to back: c over b over a, each overlapping only the next below.
        let rects = [r(100, 0, 100, 100), r(50, 0, 100, 100), r(0, 0, 100, 100)];
        assert_eq!(tiers(&rects), [2, 1, 0]);
    }

    #[test]
    fn a_window_takes_the_highest_tier_it_overlaps() {
        // top overlaps both a (tier 0) and b (tier 1, over a): tier 2.
        let rects = [r(40, 0, 100, 100), r(60, 0, 100, 100), r(0, 0, 100, 100)];
        assert_eq!(tiers(&rects), [2, 1, 0]);
        // top overlaps only a: tier 1, whatever b is.
        let rects = [r(0, 0, 40, 40), r(500, 0, 100, 100), r(0, 0, 100, 100)];
        assert_eq!(tiers(&rects), [1, 0, 0]);
    }

    #[test]
    fn a_glass_window_over_an_opaque_one_is_tier_one() {
        // The opaque window has no glass of its own; it still counts.
        let opaque = r(0, 0, 1600, 1000);
        let glass = r(30, 30, 600, 400);
        assert_eq!(tiers(&[glass, opaque]), [1, 0]);
    }

    #[test]
    fn an_empty_region_overlaps_nothing() {
        let rects = [r(0, 0, 0, 0), r(0, 0, 100, 100)];
        assert_eq!(tiers(&rects), [0, 0]);
    }

    #[test]
    fn past_the_cap_windows_clamp_to_the_last_pyramid() {
        // A six-deep overlap chain, front to back: true tiers 5..0.
        let rects: Vec<_> = (0..6).rev().map(|i| r(i * 50, 0, 100, 100)).collect();
        let t = tiers(&rects);
        assert_eq!(t, [5, 4, 3, 2, 1, 0]);
        let got: Vec<_> = t.into_iter().map(pyramid_of).collect();
        assert_eq!(got, [3, 3, 3, 2, 1, 0]);
        assert_eq!(MAX_TIERS, 4);
    }

    fn parts(d: u8, k_top: usize, tiers: &[u8]) -> Vec<Part> {
        let mut v = Vec::new();
        pyramid_parts(d, k_top, tiers, |_| true, |p| v.push(p));
        v
    }

    /// Tiled windows overlap nothing: every one is tier 0, whose pyramid is the
    /// base alone, so N windows share one pyramid whatever their content does.
    #[test]
    fn tiled_windows_share_the_base_pyramid() {
        let rects = [
            r(0, 0, 100, 100),
            r(100, 0, 100, 100),
            r(0, 100, 100, 100),
            r(100, 100, 100, 100),
        ];
        let t = tiers(&rects);
        assert!(t.iter().all(|t| pyramid_of(*t) == 0));
        for k in 0..4 {
            assert_eq!(parts(0, k, &t), [Part::Base]);
        }
    }

    /// An overlapping pair, upper window 0 over lower window 1: the upper's
    /// pyramid holds the lower window (and its glass), then the base, and not
    /// the upper window's own content; the lower's holds only the base.
    #[test]
    fn the_upper_window_sees_the_lower_one_and_not_itself() {
        let t = tiers(&[r(50, 0, 100, 100), r(0, 0, 100, 100)]);
        assert_eq!(t, [1, 0]);
        let upper = parts(pyramid_of(t[0]), 0, &t);
        assert_eq!(upper, [Part::Window(1), Part::Glass(1), Part::Base]);
        assert!(!upper.contains(&Part::Window(0)) && !upper.contains(&Part::Glass(0)));
        assert_eq!(parts(pyramid_of(t[1]), 1, &t), [Part::Base]);
    }

    /// A window below the topmost member that is not lower in tier (a sibling
    /// of its own tier) is not composited: members never see each other.
    #[test]
    fn same_tier_siblings_do_not_see_each_other() {
        // Two tier-1 windows over one tier-0 window: 0 and 1 side by side, 2 under both.
        let t = tiers(&[r(0, 0, 100, 100), r(100, 0, 100, 100), r(0, 0, 200, 100)]);
        assert_eq!(t, [1, 1, 0]);
        assert_eq!(parts(1, 0, &t), [Part::Window(2), Part::Glass(2), Part::Base]);
    }

    /// The clamp: a six-deep chain has true tiers 5..0 and pyramids 3,3,3,2,1,0.
    /// P_3 is built from the windows of true tier below 3 under its topmost
    /// member, in z-order, over the base, whatever the clamped ones do.
    #[test]
    fn the_last_pyramid_is_built_from_what_lies_below_the_clamped_windows() {
        let rects: Vec<_> = (0..6).rev().map(|i| r(i * 50, 0, 100, 100)).collect();
        let t = tiers(&rects);
        assert_eq!(t, [5, 4, 3, 2, 1, 0]);
        let last = pyramid_of(t[0]);
        assert_eq!(last, 3);
        // Front-most clamped member is window 0.
        let got = parts(last, 0, &t);
        assert_eq!(
            got,
            [
                Part::Window(3),
                Part::Glass(3),
                Part::Window(4),
                Part::Glass(4),
                Part::Window(5),
                Part::Glass(5),
                Part::Base
            ]
        );
        // Lower pyramids stay strictly below their members.
        assert_eq!(
            parts(2, 3, &t),
            [
                Part::Window(4),
                Part::Glass(4),
                Part::Window(5),
                Part::Glass(5),
                Part::Base
            ]
        );
        assert_eq!(parts(1, 4, &t), [Part::Window(5), Part::Glass(5), Part::Base]);
        // A window with no glass of its own adds only its content.
        let mut v = Vec::new();
        pyramid_parts(1, 0, &t, |_| false, |p| v.push(p));
        assert_eq!(v, [Part::Window(5), Part::Base]);
    }
}
