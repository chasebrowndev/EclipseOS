// SPDX-License-Identifier: AGPL-3.0-only
//! The arithmetic behind the arrangement canvas: pure, no iced.
//!
//! Three jobs. A [`Viewport`] maps between logical space (what the compositor
//! calls a position) and canvas pixels. [`hit_test`] says which rectangle is
//! under a point. [`snap`] decides where a dragged rectangle lands: always
//! flush against a neighbour — no gap, no overlap — and aligned to that
//! neighbour's edge when it passes within the threshold. It lives apart from
//! the widget so it can be tested without a window.

/// An integer rectangle in logical space. `w` and `h` are non-negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    pub const fn new(x: i64, y: i64, w: i64, h: i64) -> Self {
        Rect { x, y, w, h }
    }

    pub const fn right(self) -> i64 {
        self.x + self.w
    }

    pub const fn bottom(self) -> i64 {
        self.y + self.h
    }

    /// Strict overlap: rectangles that only share an edge do not overlap.
    pub const fn overlaps(self, o: Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub const fn at(self, x: i64, y: i64) -> Rect {
        Rect { x, y, ..self }
    }

    pub fn union(self, o: Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect {
            x,
            y,
            w: self.right().max(o.right()) - x,
            h: self.bottom().max(o.bottom()) - y,
        }
    }

    /// Grown by `d` on every side.
    pub fn grown(self, d: i64) -> Rect {
        Rect {
            x: self.x - d,
            y: self.y - d,
            w: self.w + 2 * d,
            h: self.h + 2 * d,
        }
    }
}

/// The bounding box of all of them, or `None` for none.
pub fn bounds(rects: &[Rect]) -> Option<Rect> {
    rects.iter().copied().reduce(Rect::union)
}

/// Logical space to canvas pixels: `screen = (logical - origin) * scale + offset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub scale: f64,
    origin: (f64, f64),
    offset: (f64, f64),
}

impl Viewport {
    /// The largest uniform scale that fits `content` into a `width` x `height`
    /// canvas with `pad` pixels spare on every side, centred.
    pub fn fit(content: Rect, width: f64, height: f64, pad: f64) -> Viewport {
        let cw = (content.w as f64).max(1.0);
        let ch = (content.h as f64).max(1.0);
        let scale = ((width - 2.0 * pad) / cw)
            .min((height - 2.0 * pad) / ch)
            .max(f64::MIN_POSITIVE);
        Viewport {
            scale,
            origin: (content.x as f64, content.y as f64),
            offset: ((width - cw * scale) / 2.0, (height - ch * scale) / 2.0),
        }
    }

    pub fn to_screen(self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.origin.0) * self.scale + self.offset.0,
            (y - self.origin.1) * self.scale + self.offset.1,
        )
    }

    pub fn to_logical(self, sx: f64, sy: f64) -> (f64, f64) {
        (
            (sx - self.offset.0) / self.scale + self.origin.0,
            (sy - self.offset.1) / self.scale + self.origin.1,
        )
    }

    /// `(x, y, w, h)` in canvas pixels.
    pub fn rect_to_screen(self, r: Rect) -> (f64, f64, f64, f64) {
        let (x, y) = self.to_screen(r.x as f64, r.y as f64);
        (x, y, r.w as f64 * self.scale, r.h as f64 * self.scale)
    }
}

/// The viewport an arrangement of `rects` is drawn through: every rectangle
/// plus `slack` (a fraction of the largest side) of empty canvas around them,
/// so a monitor can be dropped beside the others.
pub fn fit_layout(rects: &[Rect], width: f64, height: f64, pad: f64, slack: f64) -> Viewport {
    let Some(b) = bounds(rects) else {
        return Viewport::fit(Rect::new(0, 0, 1920, 1080), width, height, pad);
    };
    let biggest = rects.iter().map(|r| r.w.max(r.h)).max().unwrap_or(0);
    Viewport::fit(b.grown((biggest as f64 * slack) as i64), width, height, pad)
}

/// The topmost rectangle (the last, drawn on top) under a canvas point.
pub fn hit_test(rects: &[Rect], vp: Viewport, sx: f64, sy: f64) -> Option<usize> {
    let (lx, ly) = vp.to_logical(sx, sy);
    rects.iter().rposition(|r| {
        lx >= r.x as f64 && lx < r.right() as f64 && ly >= r.y as f64 && ly < r.bottom() as f64
    })
}

/// Where `moving` lands when dragged to `(moving.x, moving.y)` among `others`.
///
/// Every candidate is flush against one neighbour's edge — left of, right of,
/// above or below — and slides freely along that edge while it still touches
/// it. Within `threshold` of the neighbour's own edge it aligns to it (top to
/// top, bottom to bottom, left to left, right to right). Candidates that
/// overlap anything are dropped; the survivor nearest the pointer wins. So
/// the result never overlaps and never floats free of the layout.
///
/// With no neighbours there is nothing to be flush with: it stays where it is.
pub fn snap(moving: Rect, others: &[Rect], threshold: i64) -> (i64, i64) {
    if others.is_empty() {
        return (moving.x, moving.y);
    }
    let (w, h) = (moving.w, moving.h);
    let mut cands: Vec<(i64, i64)> = Vec::new();

    // Slide along an axis. It must keep at least a quarter of the shorter side
    // in contact (a corner-grazing placement is not a neighbour), and within
    // the threshold of *any* output's edge — not only the one it is flush
    // against — it aligns to that edge, near edge to near edge or far to far.
    let slide = |want: i64, size: i64, o_lo: i64, o_hi: i64, edges: &[i64]| -> i64 {
        let need = (size.min(o_hi - o_lo) / 4).max(1);
        let (lo, hi) = (o_lo - size + need, o_hi - need);
        let want = want.clamp(lo, hi);
        let mut best = want;
        let mut gap = threshold + 1;
        for &e in edges {
            for v in [e, e - size] {
                let d = (v - want).abs();
                if d <= threshold && d < gap && (lo..=hi).contains(&v) {
                    best = v;
                    gap = d;
                }
            }
        }
        best
    };
    let xs: Vec<i64> = others.iter().flat_map(|o| [o.x, o.right()]).collect();
    let ys: Vec<i64> = others.iter().flat_map(|o| [o.y, o.bottom()]).collect();

    for o in others {
        let y = slide(moving.y, h, o.y, o.bottom(), &ys);
        cands.push((o.right(), y));
        cands.push((o.x - w, y));
        let x = slide(moving.x, w, o.x, o.right(), &xs);
        cands.push((x, o.bottom()));
        cands.push((x, o.y - h));
    }

    cands
        .into_iter()
        .filter(|&(x, y)| {
            let r = moving.at(x, y);
            !others.iter().any(|o| r.overlaps(*o))
        })
        .min_by_key(|&(x, y)| {
            let (dx, dy) = (x - moving.x, y - moving.y);
            dx * dx + dy * dy
        })
        .unwrap_or((moving.x, moving.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Rect = Rect::new(0, 0, 1920, 1080);

    #[test]
    fn a_drop_beside_a_neighbour_goes_flush() {
        // Dropped 40 px to the right of A's edge and 10 px low.
        let (x, y) = snap(Rect::new(1960, 10, 1280, 720), &[A], 24);
        assert_eq!(x, 1920, "no gap");
        assert_eq!(y, 0, "top edges align within the threshold");
    }

    #[test]
    fn a_drop_on_top_of_a_neighbour_is_pushed_out() {
        let (x, y) = snap(Rect::new(500, 300, 800, 600), &[A], 24);
        let r = Rect::new(x, y, 800, 600);
        assert!(!r.overlaps(A));
        // Nearest exit is below the neighbour... or beside it; either is flush.
        assert!(x == 1920 || x == -800 || y == 1080 || y == -600);
    }

    #[test]
    fn it_slides_along_the_edge_when_not_near_an_alignment() {
        let (x, y) = snap(Rect::new(1925, 400, 1280, 720), &[A], 24);
        assert_eq!((x, y), (1920, 400));
    }

    #[test]
    fn bottom_edges_align_within_the_threshold() {
        // 1080 + 0 - 720 = 360 puts the bottoms level; 370 is within 24.
        let (_, y) = snap(Rect::new(1930, 370, 1280, 720), &[A], 24);
        assert_eq!(y, 360);
    }

    #[test]
    fn a_far_drop_still_attaches() {
        let (x, y) = snap(Rect::new(9000, 9000, 800, 600), &[A], 24);
        let r = Rect::new(x, y, 800, 600);
        assert!(!r.overlaps(A));
        let touching = x == A.right() || y == A.bottom();
        assert!(touching, "attached at {x},{y}");
    }

    #[test]
    fn it_never_overlaps_any_of_several_neighbours() {
        let b = Rect::new(1920, 0, 1280, 1024);
        // Dropped straddling the A/B seam.
        let (x, y) = snap(Rect::new(1500, 1000, 1000, 700), &[A, b], 24);
        let r = Rect::new(x, y, 1000, 700);
        assert!(!r.overlaps(A) && !r.overlaps(b), "{r:?}");
    }

    #[test]
    fn it_aligns_to_a_neighbour_it_is_not_flush_against() {
        // Flush against A's right edge, but A is tall: a second output sitting
        // above B lines its top up with B's even though B is not what it touches.
        let b = Rect::new(2000, -1080, 1280, 1080);
        let (x, y) = snap(Rect::new(3440, -1070, 800, 600), &[A, b], 24);
        assert_eq!((x, y), (3280, -1080));
    }

    #[test]
    fn a_corner_graze_is_not_a_neighbour() {
        // A 1 px contact with A's corner would be "touching"; it is refused.
        let (x, y) = snap(Rect::new(1920, -599, 800, 600), &[A], 0);
        let r = Rect::new(x, y, 800, 600);
        let contact = r.bottom().min(A.bottom()) - r.y.max(A.y);
        assert!(contact >= 150, "{r:?}");
    }

    #[test]
    fn a_lone_output_stays_put() {
        assert_eq!(snap(Rect::new(5, 7, 100, 100), &[], 24), (5, 7));
    }

    #[test]
    fn edge_sharing_is_not_overlap() {
        assert!(!A.overlaps(Rect::new(1920, 0, 10, 10)));
        assert!(A.overlaps(Rect::new(1919, 0, 10, 10)));
    }

    #[test]
    fn the_viewport_round_trips() {
        let vp = fit_layout(&[A, Rect::new(1920, 0, 1280, 1024)], 800.0, 232.0, 28.0, 0.45);
        let (sx, sy) = vp.to_screen(1234.0, 567.0);
        let (lx, ly) = vp.to_logical(sx, sy);
        assert!((lx - 1234.0).abs() < 1e-6 && (ly - 567.0).abs() < 1e-6);
    }

    #[test]
    fn the_layout_fits_the_canvas() {
        let rects = [A, Rect::new(1920, -200, 2560, 1440)];
        let vp = fit_layout(&rects, 800.0, 232.0, 28.0, 0.45);
        for r in rects {
            let (x, y, w, h) = vp.rect_to_screen(r);
            assert!(x >= 0.0 && y >= 0.0 && x + w <= 800.0 && y + h <= 232.0);
        }
    }

    #[test]
    fn hit_testing_takes_the_topmost() {
        let rects = [A, Rect::new(100, 100, 400, 400)];
        let vp = Viewport::fit(A, 960.0, 540.0, 0.0);
        let (sx, sy) = vp.to_screen(200.0, 200.0);
        assert_eq!(hit_test(&rects, vp, sx, sy), Some(1));
        let (sx, sy) = vp.to_screen(1000.0, 800.0);
        assert_eq!(hit_test(&rects, vp, sx, sy), Some(0));
        let (sx, sy) = vp.to_screen(2500.0, 800.0);
        assert_eq!(hit_test(&rects, vp, sx, sy), None);
    }

    #[test]
    fn bounds_cover_everything() {
        let b = bounds(&[A, Rect::new(-100, 50, 10, 10)]).unwrap();
        assert_eq!((b.x, b.y, b.right(), b.bottom()), (-100, 0, 1920, 1080));
        assert_eq!(bounds(&[]), None);
    }
}
