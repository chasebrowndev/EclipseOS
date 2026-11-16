// SPDX-License-Identifier: AGPL-3.0-only
//! Points and rectangles in logical pixels.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Half-open: the right and bottom edges are outside.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// Shrink by `d` on every side (grow when negative). Never negative size.
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(
            self.x + d,
            self.y + d,
            (self.w - 2.0 * d).max(0.0),
            (self.h - 2.0 * d).max(0.0),
        )
    }

    /// Grow by `d` on every side.
    pub fn outset(&self, d: f32) -> Rect {
        self.inset(-d)
    }

    /// `(top h, rest)`.
    pub fn split_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.clamp(0.0, self.h);
        (
            Rect::new(self.x, self.y, self.w, h),
            Rect::new(self.x, self.y + h, self.w, self.h - h),
        )
    }

    /// `(rest, bottom h)`.
    pub fn split_bottom(&self, h: f32) -> (Rect, Rect) {
        let h = h.clamp(0.0, self.h);
        (
            Rect::new(self.x, self.y, self.w, self.h - h),
            Rect::new(self.x, self.bottom() - h, self.w, h),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }

    /// A `w` x `h` rect centred vertically in `self`, left edge at `x`.
    pub fn centred_y(&self, x: f32, w: f32, h: f32) -> Rect {
        Rect::new(x, self.y + (self.h - h) / 2.0, w, h)
    }
}
