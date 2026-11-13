// SPDX-License-Identifier: AGPL-3.0-only
//! Built-in glyphs (T-06 §6): box drawing, blocks, shades, quadrants, sextants, octants, braille,
//! Powerline separators and the Symbols for Legacy Computing ranges, plus the procedural tofu
//! with hex. Original work from the Unicode charts; the data tables are in `builtin_tables`.
//!
//! Every glyph is drawn at the exact cell size and never from a font. Straight lines and
//! rectangles sit on integer pixel edges, so they meet the cell edges exactly at every scale;
//! diagonals and arcs are antialiased from the pixel-centre distance to the shape (analytic, no
//! supersampling). Output is an exact-cell coverage bitmap, cached per (cell size, thickness,
//! code point) by [`SpriteCache`].

use crate::builtin_tables::{BOX, DIAGONALS, MOSAICS, OCTANTS, QUARTER_RECTS, SEXTANTS};
use std::collections::HashMap;
use std::sync::Arc;

/// How far Powerline tips and half circles overshoot the cell, in pixels, so neighbouring cells
/// do not leave a seam.
const OVERSHOOT_PX: f32 = 0.5;

/// True if `cp` is drawn by this module. A single leading range check rejects almost all text.
pub fn is_sprite(cp: u32) -> bool {
    if cp < 0x2500 {
        return false;
    }
    matches!(cp,
        0x2500..=0x259F
        | 0x2800..=0x28FF
        | 0xE0B0..=0xE0BF
        | 0x1CD00..=0x1CDE5
        | 0x1CEA0..=0x1CEAF
        | 0x1FB00..=0x1FB92
        | 0x1FB94..=0x1FB96
        | 0x1FB9A..=0x1FBAD
        | 0x1FBCE..=0x1FBDF)
}

/// Parameters of a sprite: the exact cell and the light stroke thickness in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpriteParams {
    /// Cell width in pixels.
    pub cell_w: u32,
    /// Cell height in pixels.
    pub cell_h: u32,
    /// Light line thickness in pixels (at least 1). Heavy lines are twice this.
    pub light: u32,
}

impl SpriteParams {
    /// Default thickness: one sixteenth of the cell height, at least one pixel.
    pub fn new(cell_w: u32, cell_h: u32) -> Self {
        Self {
            cell_w,
            cell_h,
            light: ((cell_h + 8) / 16).max(1),
        }
    }

    /// Scales the default thickness by `font.box-thickness`.
    pub fn with_thickness(mut self, factor: f32) -> Self {
        let f = if factor.is_finite() {
            factor.max(0.0)
        } else {
            1.0
        };
        // Clamped to the cell height so heavy lines (`light * 2`) cannot overflow.
        let cap = self.cell_h.max(1);
        self.light = ((self.light as f32 * f).round().min(cap as f32) as u32).clamp(1, cap);
        self
    }
}

/// An exact-cell 8-bit coverage bitmap, rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpriteBitmap {
    /// Width in pixels (the cell width).
    pub width: u32,
    /// Height in pixels (the cell height).
    pub height: u32,
    /// `width * height` coverage bytes.
    pub data: Vec<u8>,
}

impl SpriteBitmap {
    /// Row `y` of the bitmap.
    pub fn row(&self, y: u32) -> &[u8] {
        let w = self.width as usize;
        &self.data[y as usize * w..(y as usize + 1) * w]
    }
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

fn smooth(d: f32) -> f32 {
    (d + 0.5).clamp(0.0, 1.0)
}

fn seg_dist(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + dx * t, a.1 + dy * t);
    ((px - cx) * (px - cx) + (py - cy) * (py - cy)).sqrt()
}

/// Coverage of a convex polygon at a pixel centre.
fn poly_cov(pts: &[(f32, f32)], x: f32, y: f32) -> f32 {
    let n = pts.len();
    let mut area2 = 0.0;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        area2 += a.0 * b.1 - b.0 * a.1;
    }
    let sign = if area2 >= 0.0 { 1.0 } else { -1.0 };
    let mut c = 1.0f32;
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let len = (ex * ex + ey * ey).sqrt();
        if len == 0.0 {
            continue;
        }
        let d = sign * (ex * (y - a.1) - ey * (x - a.0)) / len;
        c = c.min(smooth(d));
    }
    c
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Self {
            w: w as usize,
            h: h as usize,
            px: vec![0.0; w as usize * h as usize],
        }
    }

    fn put(&mut self, x: usize, y: usize, v: f32) {
        let p = &mut self.px[y * self.w + x];
        if v > *p {
            *p = v;
        }
    }

    /// Exact-area coverage of a float rectangle, scaled by `v`.
    fn rect_v(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, v: f32) {
        let xa = x0.max(0.0).floor() as usize;
        let ya = y0.max(0.0).floor() as usize;
        let xb = x1.min(self.w as f32).ceil().max(0.0) as usize;
        let yb = y1.min(self.h as f32).ceil().max(0.0) as usize;
        for y in ya..yb {
            let oy = ((y1.min(y as f32 + 1.0)) - y0.max(y as f32)).max(0.0);
            for x in xa..xb {
                let ox = ((x1.min(x as f32 + 1.0)) - x0.max(x as f32)).max(0.0);
                self.put(x, y, ox * oy * v);
            }
        }
    }

    fn rect(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        self.rect_v(x0 as f32, y0 as f32, x1 as f32, y1 as f32, 1.0);
    }

    fn rect_a(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, v: f32) {
        self.rect_v(x0 as f32, y0 as f32, x1 as f32, y1 as f32, v);
    }

    /// Per-pixel function of the pixel centre.
    fn each(&mut self, f: impl Fn(f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let v = f(x as f32 + 0.5, y as f32 + 0.5);
                if v > 0.0 {
                    self.put(x, y, v);
                }
            }
        }
    }

    fn poly(&mut self, pts: &[(f32, f32)], v: f32) {
        self.each(|x, y| poly_cov(pts, x, y) * v);
    }

    fn stroke(&mut self, pts: &[(f32, f32)], thick: f32) {
        let hw = thick / 2.0;
        self.each(|x, y| {
            let mut d = f32::MAX;
            for s in pts.windows(2) {
                d = d.min(seg_dist(x, y, s[0], s[1]));
            }
            smooth(hw - d)
        });
    }

    fn finish(self) -> SpriteBitmap {
        SpriteBitmap {
            width: self.w as u32,
            height: self.h as u32,
            data: self
                .px
                .iter()
                .map(|&v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
                .collect(),
        }
    }
}

/// `round(n * k / of)`, half up.
fn frac(n: u32, k: u32, of: u32) -> u32 {
    (n * k * 2 + of) / (2 * of)
}

// ------------------------------------------------------------------ box drawing

type Band = (u32, u32);

/// Bands across an axis of length `n` for a line of `weight` (1 light, 2 heavy, 3 double).
fn bands(weight: u8, n: u32, light: u32) -> Vec<Band> {
    match weight {
        0 => Vec::new(),
        3 => {
            let l = light.min((n / 3).max(1));
            let s = n.saturating_sub(3 * l) / 2;
            vec![(s, s + l), (s + 2 * l, s + 3 * l)]
        }
        w => {
            let t = if w == 2 {
                light.saturating_mul(2)
            } else {
                light
            }
            .min(n)
            .max(1);
            let s = n.saturating_sub(t) / 2;
            vec![(s, s + t)]
        }
    }
}

/// Extent along the arm axis of band `i` of an arm. `own` are the arm's bands across, `neg` and
/// `pos` the bands (along the arm axis) of the perpendicular arms on the negative and positive
/// side. Corners stop at the outer stem for the outer line and at the inner stem for the inner
/// line of a double; straddled doubles stop at the stem on the arm's side.
fn arm_span(
    len: u32,
    pos_dir: bool,
    own: &[Band],
    i: usize,
    neg: &[Band],
    pos: &[Band],
) -> (u32, u32) {
    let t = own[i].1 - own[i].0;
    let lo = neg.iter().chain(pos).map(|b| b.0).min();
    let Some(p_lo) = lo else {
        let c0 = (len - t.min(len)) / 2;
        return if pos_dir { (c0, len) } else { (0, c0 + t) };
    };
    let p_hi = neg.iter().chain(pos).map(|b| b.1).max().unwrap_or(p_lo);
    let both = !neg.is_empty() && !pos.is_empty();
    let outer = if own.len() == 1 {
        !both
    } else if both {
        false
    } else if !neg.is_empty() {
        i == 1
    } else {
        i == 0
    };
    let perp_double = neg.len() == 2 || pos.len() == 2;
    if pos_dir {
        let start = if outer || !perp_double {
            p_lo
        } else {
            neg.iter().chain(pos).map(|b| b.0).max().unwrap_or(p_lo)
        };
        (start, len)
    } else {
        let end = if outer || !perp_double {
            p_hi
        } else {
            neg.iter().chain(pos).map(|b| b.1).min().unwrap_or(p_hi)
        };
        (0, end)
    }
}

fn draw_lines(c: &mut Canvas, p: &SpriteParams, left: u8, right: u8, up: u8, down: u8) {
    let (cw, ch) = (p.cell_w, p.cell_h);
    let hl = bands(left, ch, p.light);
    let hr = bands(right, ch, p.light);
    let vu = bands(up, cw, p.light);
    let vd = bands(down, cw, p.light);
    for i in 0..hl.len() {
        let (a, b) = arm_span(cw, false, &hl, i, &vu, &vd);
        c.rect(a, hl[i].0, b, hl[i].1);
    }
    for i in 0..hr.len() {
        let (a, b) = arm_span(cw, true, &hr, i, &vu, &vd);
        c.rect(a, hr[i].0, b, hr[i].1);
    }
    for i in 0..vu.len() {
        let (a, b) = arm_span(ch, false, &vu, i, &hl, &hr);
        c.rect(vu[i].0, a, vu[i].1, b);
    }
    for i in 0..vd.len() {
        let (a, b) = arm_span(ch, true, &vd, i, &hl, &hr);
        c.rect(vd[i].0, a, vd[i].1, b);
    }
}

fn draw_dashes(c: &mut Canvas, p: &SpriteParams, weight: u8, count: u8, vertical: bool) {
    let (len, across) = if vertical {
        (p.cell_h, p.cell_w)
    } else {
        (p.cell_w, p.cell_h)
    };
    let band = bands(weight, across, p.light)[0];
    let n = f32::from(count);
    let seg = len as f32 / n;
    let gap = (seg / 4.0).round().max(1.0);
    for k in 0..count {
        // Each dash starts on its slot and leaves `gap` pixels before the next slot, so the
        // trailing gap of one cell and the leading edge of the next never merge two dashes.
        let a = (f32::from(k) * seg).round() as u32;
        let b = (((f32::from(k) + 1.0) * seg).round() - gap).max(a as f32 + 1.0) as u32;
        let b = b.min(len);
        if vertical {
            c.rect(band.0, a, band.1, b);
        } else {
            c.rect(a, band.0, b, band.1);
        }
    }
}

/// Quarter ellipse joining the middle of two cell edges. Directions: 0 left, 1 right, 2 up, 3 down.
fn draw_arc(c: &mut Canvas, p: &SpriteParams, a: u8, b: u8) {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let l = p.light.min(p.cell_w.min(p.cell_h)).max(1);
    let sx = bands(1, p.cell_w, l)[0];
    let sy = bands(1, p.cell_h, l)[0];
    let cx = (sx.0 + sx.1) as f32 / 2.0;
    let cy = (sy.0 + sy.1) as f32 / 2.0;
    let (h_dir, v_dir) = if a < 2 { (a, b) } else { (b, a) };
    let x_h = if h_dir == 0 { 0.0 } else { cw };
    let y_v = if v_dir == 2 { 0.0 } else { ch };
    let steps = (((cw + ch) * 2.0) as usize).clamp(16, 256);
    let pts: Vec<(f32, f32)> = (0..=steps)
        .map(|i| {
            let th = std::f32::consts::FRAC_PI_2 * i as f32 / steps as f32;
            (x_h + (cx - x_h) * th.cos(), y_v + (cy - y_v) * th.sin())
        })
        .collect();
    c.stroke(&pts, l as f32);
}

fn draw_diagonals(c: &mut Canvas, p: &SpriteParams, which: u8) {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let len = (cw * cw + ch * ch).sqrt();
    let (ux, uy) = (cw / len, ch / len);
    let e = 1.0;
    if which & 1 != 0 {
        c.stroke(
            &[(cw + ux * e, -uy * e), (-ux * e, ch + uy * e)],
            p.light as f32,
        );
    }
    if which & 2 != 0 {
        c.stroke(
            &[(-ux * e, -uy * e), (cw + ux * e, ch + uy * e)],
            p.light as f32,
        );
    }
}

// ------------------------------------------------------------------ blocks and mosaics

fn draw_grid(c: &mut Canvas, p: &SpriteParams, cols: u32, rows: u32, mask: u32) {
    for r in 0..rows {
        for col in 0..cols {
            if mask & (1 << (r * cols + col)) != 0 {
                let (x0, x1) = (frac(p.cell_w, col, cols), frac(p.cell_w, col + 1, cols));
                let (y0, y1) = (frac(p.cell_h, r, rows), frac(p.cell_h, r + 1, rows));
                c.rect(x0, y0, x1, y1);
            }
        }
    }
}

fn draw_braille(c: &mut Canvas, p: &SpriteParams, bits: u32) {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let d = ((cw / 4.0).min(ch / 8.0).round() as u32)
        .max(1)
        .min(p.cell_w)
        .min(p.cell_h);
    // (column, row) per dot 1..8.
    const POS: [(u32, u32); 8] = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (0, 3),
        (1, 3),
    ];
    for (i, &(col, row)) in POS.iter().enumerate() {
        if bits & (1 << i) == 0 {
            continue;
        }
        let cx = cw * (0.25 + 0.5 * col as f32);
        let cy = ch * (0.125 + 0.25 * row as f32);
        let x0 = ((cx - d as f32 / 2.0).round().max(0.0) as u32).min(p.cell_w - d);
        let y0 = ((cy - d as f32 / 2.0).round().max(0.0) as u32).min(p.cell_h - d);
        c.rect(x0, y0, x0 + d, y0 + d);
    }
}

fn draw_mosaic(c: &mut Canvas, p: &SpriteParams, m: [u8; 6]) {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let pt = |x: u8, y: u8| (f32::from(x) * cw / 2.0, f32::from(y) * ch / 3.0);
    let (a, b, k) = (pt(m[0], m[1]), pt(m[2], m[3]), pt(m[4], m[5]));
    let (ex, ey) = (b.0 - a.0, b.1 - a.1);
    let len = (ex * ex + ey * ey).sqrt().max(f32::MIN_POSITIVE);
    let side = |q: (f32, f32)| (ex * (q.1 - a.1) - ey * (q.0 - a.0)) / len;
    let s = if side(k) >= 0.0 { 1.0 } else { -1.0 };
    c.each(|x, y| smooth(s * side((x, y))));
}

/// One of the four triangles of the cell cut by both diagonals: 0 left, 1 upper, 2 right, 3 lower.
fn tri_quarter(p: &SpriteParams, which: u8) -> [(f32, f32); 3] {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let m = (cw / 2.0, ch / 2.0);
    match which {
        0 => [(0.0, 0.0), (0.0, ch), m],
        1 => [(0.0, 0.0), (cw, 0.0), m],
        2 => [(cw, 0.0), (cw, ch), m],
        _ => [(0.0, ch), (cw, ch), m],
    }
}

fn draw_tri_blocks(c: &mut Canvas, p: &SpriteParams, cp: u32) {
    let one = |c: &mut Canvas, w: u8| {
        let t = tri_quarter(p, w);
        c.poly(&t, 1.0);
    };
    match cp {
        // Three quarters: everything but one triangle.
        0x1FB68..=0x1FB6B => {
            let missing = match cp {
                0x1FB68 => 0,
                0x1FB69 => 1,
                0x1FB6A => 2,
                _ => 3,
            };
            let t = tri_quarter(p, missing);
            c.each(|x, y| 1.0 - poly_cov(&t, x, y));
        }
        0x1FB6C => one(c, 0),
        0x1FB6D => one(c, 1),
        0x1FB6E => one(c, 2),
        0x1FB6F => one(c, 3),
        0x1FB9A => {
            one(c, 1);
            one(c, 3);
        }
        _ => {
            one(c, 0);
            one(c, 2);
        }
    }
}

fn draw_eighths(c: &mut Canvas, p: &SpriteParams, cp: u32) {
    let (cw, ch) = (p.cell_w, p.cell_h);
    let fx = |k: u32| frac(cw, k, 8);
    let fy = |k: u32| frac(ch, k, 8);
    match cp {
        0x1FB70..=0x1FB75 => {
            let n = cp - 0x1FB70 + 2;
            c.rect(fx(n - 1), 0, fx(n), ch);
        }
        0x1FB76..=0x1FB7B => {
            let n = cp - 0x1FB76 + 2;
            c.rect(0, fy(n - 1), cw, fy(n));
        }
        0x1FB7C => {
            c.rect(0, 0, fx(1), ch);
            c.rect(0, ch - fy(1), cw, ch);
        }
        0x1FB7D => {
            c.rect(0, 0, fx(1), ch);
            c.rect(0, 0, cw, fy(1));
        }
        0x1FB7E => {
            c.rect(cw - fx(1), 0, cw, ch);
            c.rect(0, 0, cw, fy(1));
        }
        0x1FB7F => {
            c.rect(cw - fx(1), 0, cw, ch);
            c.rect(0, ch - fy(1), cw, ch);
        }
        0x1FB80 => {
            c.rect(0, 0, cw, fy(1));
            c.rect(0, ch - fy(1), cw, ch);
        }
        0x1FB81 => {
            for k in [1, 3, 5, 8] {
                c.rect(0, fy(k - 1), cw, fy(k));
            }
        }
        0x1FB82..=0x1FB86 => {
            let n = [2, 3, 5, 6, 7][(cp - 0x1FB82) as usize];
            c.rect(0, 0, cw, fy(n));
        }
        _ => {
            let n = [2, 3, 5, 6, 7][(cp - 0x1FB87) as usize];
            c.rect(cw - fx(n), 0, cw, ch);
        }
    }
}

fn draw_shades(c: &mut Canvas, p: &SpriteParams, cp: u32) {
    let (cw, ch) = (p.cell_w, p.cell_h);
    let (hx, hy) = (frac(cw, 1, 2), frac(ch, 1, 2));
    match cp {
        0x1FB8C => c.rect_a(0, 0, hx, ch, 0.5),
        0x1FB8D => c.rect_a(hx, 0, cw, ch, 0.5),
        0x1FB8E => c.rect_a(0, 0, cw, hy, 0.5),
        0x1FB8F => c.rect_a(0, hy, cw, ch, 0.5),
        0x1FB90 => c.rect_a(0, 0, cw, ch, 0.5),
        0x1FB91 => {
            c.rect(0, 0, cw, hy);
            c.rect_a(0, hy, cw, ch, 0.5);
        }
        0x1FB92 => {
            c.rect_a(0, 0, cw, hy, 0.5);
            c.rect(0, hy, cw, ch);
        }
        0x1FB94 => {
            c.rect_a(0, 0, hx, ch, 0.5);
            c.rect(hx, 0, cw, ch);
        }
        0x1FB95 | 0x1FB96 => {
            let inverse = cp == 0x1FB96;
            for r in 0..8 {
                for col in 0..4 {
                    if ((r + col) % 2 == 0) != inverse {
                        c.rect(
                            frac(cw, col, 4),
                            frac(ch, r, 8),
                            frac(cw, col + 1, 4),
                            frac(ch, r + 1, 8),
                        );
                    }
                }
            }
        }
        _ => {
            // 1FB9C..=1FB9F, clockwise from upper left: medium-shade half-cell triangles.
            let (cwf, chf) = (cw as f32, ch as f32);
            let pts: [(f32, f32); 3] = match cp {
                0x1FB9C => [(0.0, 0.0), (cwf, 0.0), (0.0, chf)],
                0x1FB9D => [(0.0, 0.0), (cwf, 0.0), (cwf, chf)],
                0x1FB9E => [(cwf, 0.0), (cwf, chf), (0.0, chf)],
                _ => [(0.0, 0.0), (0.0, chf), (cwf, chf)],
            };
            c.poly(&pts, 0.5);
        }
    }
}

fn draw_quarter_rect(c: &mut Canvas, p: &SpriteParams, r: [u8; 4]) {
    let fx = |k: u8| frac(p.cell_w, u32::from(k), 8);
    let fy = |k: u8| frac(p.cell_h, u32::from(k), 8);
    c.rect(fx(r[0]), fy(r[1]), fx(r[2]), fy(r[3]));
}

fn draw_polylines(c: &mut Canvas, p: &SpriteParams, entry: &[[u8; 2]; 12]) {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let mut cur: Vec<(f32, f32)> = Vec::new();
    for pt in entry {
        match *pt {
            [255, 255] => break,
            [254, 254] => {
                if cur.len() > 1 {
                    c.stroke(&cur, p.light as f32);
                }
                cur.clear();
            }
            [x, y] => cur.push((f32::from(x) * cw / 2.0, f32::from(y) * ch / 2.0)),
        }
    }
    if cur.len() > 1 {
        c.stroke(&cur, p.light as f32);
    }
}

// ------------------------------------------------------------------ Powerline

fn draw_powerline(c: &mut Canvas, p: &SpriteParams, cp: u32) {
    let (cw, ch) = (p.cell_w as f32, p.cell_h as f32);
    let l = p.light as f32;
    let o = OVERSHOOT_PX;
    let mid = ch / 2.0;
    let slope = ch / (2.0 * cw);
    match cp {
        0xE0B0 => c.poly(&[(0.0, 0.0), (cw + o, mid), (0.0, ch)], 1.0),
        0xE0B2 => c.poly(&[(cw, 0.0), (-o, mid), (cw, ch)], 1.0),
        0xE0B1 => c.stroke(&[(-1.0, -slope), (cw, mid), (-1.0, ch + slope)], l),
        0xE0B3 => c.stroke(&[(cw + 1.0, -slope), (0.0, mid), (cw + 1.0, ch + slope)], l),
        0xE0B4 | 0xE0B6 => {
            let left = cp == 0xE0B6;
            let (rx, ry) = (cw + o, mid);
            c.each(|x, y| {
                let u = if left { cw - x } else { x };
                let v = y - mid;
                let f = (u / rx) * (u / rx) + (v / ry) * (v / ry);
                let gx = 2.0 * u / (rx * rx);
                let gy = 2.0 * v / (ry * ry);
                let g = (gx * gx + gy * gy).sqrt().max(1e-6);
                smooth((1.0 - f) / g)
            });
        }
        0xE0B5 | 0xE0B7 => {
            let left = cp == 0xE0B7;
            let (rx, ry) = ((cw - l / 2.0).max(1.0), (mid - l / 2.0).max(1.0));
            let steps = (((cw + ch) * 2.0) as usize).clamp(16, 256);
            let pts: Vec<(f32, f32)> = (0..=steps)
                .map(|i| {
                    let th = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::PI * i as f32 / steps as f32;
                    let x = rx * th.cos();
                    (if left { cw - x } else { x }, mid + ry * th.sin())
                })
                .collect();
            c.stroke(&pts, l);
        }
        0xE0B8 => c.poly(&[(0.0, 0.0), (0.0, ch), (cw, ch)], 1.0),
        0xE0BA => c.poly(&[(cw, 0.0), (cw, ch), (0.0, ch)], 1.0),
        0xE0BC => c.poly(&[(0.0, 0.0), (cw, 0.0), (0.0, ch)], 1.0),
        0xE0BE => c.poly(&[(0.0, 0.0), (cw, 0.0), (cw, ch)], 1.0),
        0xE0B9 | 0xE0BF => draw_diagonals(c, p, 2),
        _ => draw_diagonals(c, p, 1),
    }
}

// ------------------------------------------------------------------ entry points

/// Renders the sprite for `cp` at the given cell, or `None` if `cp` is not a sprite.
pub fn render(cp: u32, p: &SpriteParams) -> Option<SpriteBitmap> {
    if !is_sprite(cp) || p.cell_w == 0 || p.cell_h == 0 {
        return None;
    }
    let mut c = Canvas::new(p.cell_w, p.cell_h);
    let (cw, ch) = (p.cell_w, p.cell_h);
    match cp {
        0x2500..=0x257F => {
            let [kind, a, b, cc, d] = BOX[(cp - 0x2500) as usize];
            match kind {
                0 => draw_lines(&mut c, p, a, b, cc, d),
                1 => draw_dashes(&mut c, p, a, b, cc == 1),
                2 => draw_arc(&mut c, p, a, b),
                _ => draw_diagonals(&mut c, p, a),
            }
        }
        0x2580..=0x259F => {
            let (hx, hy) = (frac(cw, 1, 2), frac(ch, 1, 2));
            match cp {
                0x2580 => c.rect(0, 0, cw, hy),
                0x2581..=0x2588 => c.rect(0, ch - frac(ch, cp - 0x2580, 8), cw, ch),
                0x2589..=0x258F => c.rect(0, 0, frac(cw, 0x2590 - cp, 8), ch),
                0x2590 => c.rect(hx, 0, cw, ch),
                0x2591..=0x2593 => c.rect_a(0, 0, cw, ch, (cp - 0x2590) as f32 * 0.25),
                0x2594 => c.rect(0, 0, cw, frac(ch, 1, 8)),
                0x2595 => c.rect(cw - frac(cw, 1, 8), 0, cw, ch),
                _ => {
                    // Quadrants 2596..=259F: bit 0 upper-left, 1 upper-right, 2 lower-left, 3 lower-right.
                    let m: u32 = match cp {
                        0x2596 => 0b0100,
                        0x2597 => 0b1000,
                        0x2598 => 0b0001,
                        0x2599 => 0b1101,
                        0x259A => 0b1001,
                        0x259B => 0b0111,
                        0x259C => 0b1011,
                        0x259D => 0b0010,
                        0x259E => 0b0110,
                        _ => 0b1110,
                    };
                    draw_grid(&mut c, p, 2, 2, m);
                }
            }
        }
        0x2800..=0x28FF => draw_braille(&mut c, p, cp - 0x2800),
        0xE0B0..=0xE0BF => draw_powerline(&mut c, p, cp),
        0x1CD00..=0x1CDE5 => {
            draw_grid(&mut c, p, 2, 4, u32::from(OCTANTS[(cp - 0x1CD00) as usize]))
        }
        0x1CEA0..=0x1CEAF => draw_quarter_rect(&mut c, p, QUARTER_RECTS[(cp - 0x1CEA0) as usize]),
        0x1FB00..=0x1FB3B => draw_grid(
            &mut c,
            p,
            2,
            3,
            u32::from(SEXTANTS[(cp - 0x1FB00) as usize]),
        ),
        0x1FB3C..=0x1FB67 => draw_mosaic(&mut c, p, MOSAICS[(cp - 0x1FB3C) as usize]),
        0x1FB68..=0x1FB6F | 0x1FB9A | 0x1FB9B => draw_tri_blocks(&mut c, p, cp),
        0x1FB70..=0x1FB8B => draw_eighths(&mut c, p, cp),
        0x1FB8C..=0x1FB96 | 0x1FB9C..=0x1FB9F => draw_shades(&mut c, p, cp),
        0x1FBA0..=0x1FBAD => draw_polylines(&mut c, p, &DIAGONALS[(cp - 0x1FBA0) as usize]),
        0x1FBCE => c.rect(0, 0, frac(cw, 2, 3), ch),
        0x1FBCF => c.rect(0, 0, frac(cw, 1, 3), ch),
        0x1FBD0..=0x1FBDF => draw_polylines(&mut c, p, &DIAGONALS[(cp - 0x1FBD0) as usize + 14]),
        _ => return None,
    }
    Some(c.finish())
}

/// 3x5 hex digit font, one row per `u8` (bit 2 = left pixel).
const HEX: [[u8; 5]; 16] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 2, 2],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
    [2, 5, 7, 5, 5],
    [6, 5, 6, 5, 6],
    [3, 4, 4, 4, 3],
    [6, 5, 5, 5, 6],
    [7, 4, 6, 4, 7],
    [7, 4, 6, 4, 4],
];

/// The "tofu with hex" glyph for a code point nothing covers: a one pixel box around the code
/// point in hexadecimal digits (two rows), over a `width` x `height` pixel area (a wide
/// cluster passes two cells of width). Digits are dropped when they would not fit.
pub fn tofu(cp: u32, width: u32, height: u32) -> SpriteBitmap {
    let mut c = Canvas::new(width, height);
    if width == 0 || height == 0 {
        return c.finish();
    }
    c.rect(0, 0, width, 1);
    c.rect(0, height - 1, width, height);
    c.rect(0, 0, 1, height);
    c.rect(width - 1, 0, width, height);
    let hex = format!("{cp:04X}");
    let digits: Vec<usize> = hex
        .bytes()
        .map(|b| (if b >= b'A' { b - b'A' + 10 } else { b - b'0' }) as usize)
        .collect();
    let per_row = digits.len().div_ceil(2);
    let m = if width.min(height) >= 10 { 2 } else { 1 };
    let (iw, ih) = (
        width.saturating_sub(2 * (1 + m)),
        height.saturating_sub(2 * (1 + m)),
    );
    let units_w = (per_row * 4 - 1) as u32;
    let k = (iw / units_w).min(ih / 11);
    if k == 0 {
        return c.finish();
    }
    let (tw, th) = (k * units_w, k * 11);
    let (x0, y0) = ((width - tw) / 2, (height - th) / 2);
    for (i, &dgt) in digits.iter().enumerate() {
        let (row, col) = ((i / per_row) as u32, (i % per_row) as u32);
        for (ry, bits) in HEX[dgt].iter().enumerate() {
            for rx in 0..3u32 {
                if bits & (4 >> rx) != 0 {
                    let x = x0 + (col * 4 + rx) * k;
                    let y = y0 + (row * 6 + ry as u32) * k;
                    c.rect(x, y, x + k, y + k);
                }
            }
        }
    }
    c.finish()
}

/// Sprite cache keyed by (cell size, thickness, code point). A tofu is cached by (cell size,
/// code point) in the same table. Emptied by the owner when the font generation changes.
#[derive(Debug, Default)]
pub struct SpriteCache {
    sprites: HashMap<(SpriteParams, u32), Option<Arc<SpriteBitmap>>>,
    tofus: HashMap<(u32, u32, u32), Arc<SpriteBitmap>>,
    /// Bitmaps drawn (cache misses).
    pub drawn: u64,
}

impl SpriteCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops every cached bitmap.
    pub fn clear(&mut self) {
        self.sprites.clear();
        self.tofus.clear();
    }

    /// The sprite for `cp`, or `None` when `cp` is not a sprite.
    pub fn get(&mut self, cp: u32, p: &SpriteParams) -> Option<Arc<SpriteBitmap>> {
        if !is_sprite(cp) {
            return None;
        }
        if let Some(v) = self.sprites.get(&(*p, cp)) {
            return v.clone();
        }
        self.drawn += 1;
        let v = render(cp, p).map(Arc::new);
        self.sprites.insert((*p, cp), v.clone());
        v
    }

    /// The tofu for `cp` over `width` x `height` pixels.
    pub fn tofu(&mut self, cp: u32, width: u32, height: u32) -> Arc<SpriteBitmap> {
        if let Some(v) = self.tofus.get(&(cp, width, height)) {
            return v.clone();
        }
        self.drawn += 1;
        let v = Arc::new(tofu(cp, width, height));
        self.tofus.insert((cp, width, height), v.clone());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Six cell sizes: the scale 1, 1.25, 1.5, 2 cells of a 9x18 base plus two small and odd ones.
    const SIZES: [(u32, u32); 6] = [(6, 12), (7, 15), (9, 18), (11, 23), (14, 28), (17, 35)];

    #[test]
    fn degenerate_params_do_not_panic() {
        assert!(bands(1, 0, 3).len() == 1 && bands(2, 0, 3).len() == 1);
        let p = SpriteParams::new(0, 0);
        assert!(render(0x2500, &p).is_none());
        let p = SpriteParams::new(9, 18);
        for f in [f32::INFINITY, f32::NAN, 1e30, -5.0] {
            let q = p.with_thickness(f);
            assert!(q.light >= 1 && q.light <= 18);
            assert!(render(0x2501, &q).is_some());
            assert!(render(0x2550, &q).is_some());
        }
    }

    fn art(b: &SpriteBitmap) -> String {
        let mut s = String::new();
        for y in 0..b.height {
            for &v in b.row(y) {
                s.push(match v {
                    0 => '.',
                    255 => '#',
                    _ => '+',
                });
            }
            s.push('\n');
        }
        s
    }

    fn r(cp: u32, w: u32, h: u32) -> SpriteBitmap {
        render(cp, &SpriteParams::new(w, h)).expect("sprite")
    }

    fn at(b: &SpriteBitmap, x: u32, y: u32) -> u8 {
        b.data[(y * b.width + x) as usize]
    }

    #[test]
    fn range_check_matches_renderer_everywhere() {
        let p = SpriteParams::new(8, 16);
        for cp in (0..0x1_0000)
            .chain(0x1CC00..0x1CF00)
            .chain(0x1FA00..0x1FC00)
        {
            assert_eq!(is_sprite(cp), render(cp, &p).is_some(), "U+{cp:04X}");
        }
        assert!(!is_sprite(u32::from('a')));
        assert!(!is_sprite(0x1F600));
        assert!(
            render(
                0x2588,
                &SpriteParams {
                    cell_w: 0,
                    cell_h: 4,
                    light: 1
                }
            )
            .is_none()
        );
    }

    #[test]
    fn every_sprite_fills_exactly_the_cell() {
        for (w, h) in SIZES {
            for cp in (0x2500..0x2600)
                .chain(0x2800..0x2900)
                .chain(0xE0B0..0xE0C0)
                .chain(0x1FB00..0x1FBE0)
            {
                if let Some(b) = render(cp, &SpriteParams::new(w, h)) {
                    assert_eq!((b.width, b.height), (w, h), "U+{cp:04X}");
                    assert_eq!(b.data.len(), (w * h) as usize);
                }
            }
            for cp in (0x1CD00..=0x1CDE5).chain(0x1CEA0..=0x1CEAF) {
                let b = render(cp, &SpriteParams::new(w, h)).unwrap();
                assert_eq!(b.data.len(), (w * h) as usize);
                assert!(b.data.iter().any(|&v| v > 0), "U+{cp:04X} blank");
            }
        }
    }

    #[test]
    fn rendering_is_deterministic_and_cached() {
        let p = SpriteParams::new(9, 18);
        let mut cache = SpriteCache::new();
        let a = cache.get(0x256D, &p).unwrap();
        let b = cache.get(0x256D, &p).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(cache.drawn, 1);
        assert_eq!(*a, r(0x256D, 9, 18));
        assert!(cache.get(u32::from('x'), &p).is_none());
        let t1 = cache.tofu(0xE9, 9, 18);
        let t2 = cache.tofu(0xE9, 9, 18);
        assert!(Arc::ptr_eq(&t1, &t2));
    }

    #[test]
    fn blocks_are_exact() {
        for (w, h) in SIZES {
            assert!(r(0x2588, w, h).data.iter().all(|&v| v == 255));
            let up = r(0x2580, w, h);
            let hy = frac(h, 1, 2);
            for y in 0..h {
                assert!(up.row(y).iter().all(|&v| v == if y < hy { 255 } else { 0 }));
            }
            let lo = r(0x2584, w, h);
            for y in 0..h {
                assert!(
                    lo.row(y)
                        .iter()
                        .all(|&v| v == if y >= h - hy { 255 } else { 0 })
                );
            }
            let right = r(0x2590, w, h);
            assert_eq!(at(&right, w - 1, 0), 255);
            assert_eq!(at(&right, 0, 0), 0);
            // Eighths grow monotonically.
            let mut prev = 0;
            for cp in 0x2581..=0x2588 {
                let n = r(cp, w, h).data.iter().filter(|&&v| v == 255).count();
                assert!(n > prev);
                prev = n;
            }
            assert!(r(0x2591, w, h).data.iter().all(|&v| v == 64));
            assert!(r(0x2592, w, h).data.iter().all(|&v| v == 128));
            assert!(r(0x2593, w, h).data.iter().all(|&v| v == 191));
        }
    }

    #[test]
    fn quadrants_tile_the_cell() {
        for (w, h) in SIZES {
            let ul = r(0x2598, w, h);
            let ur = r(0x259D, w, h);
            let ll = r(0x2596, w, h);
            let lr = r(0x2597, w, h);
            for i in 0..(w * h) as usize {
                let sum = [&ul, &ur, &ll, &lr]
                    .iter()
                    .map(|b| u32::from(b.data[i]))
                    .sum::<u32>();
                assert_eq!(sum, 255, "pixel {i} at {w}x{h}");
            }
            let diag = r(0x259A, w, h);
            for i in 0..(w * h) as usize {
                assert_eq!(diag.data[i], ul.data[i].max(lr.data[i]));
            }
        }
    }

    #[test]
    fn sextants_and_octants_tile() {
        for (w, h) in SIZES {
            // 1FB00 upper-left sextant, 1FB3B everything but the top-left... check partition.
            let first = r(0x1FB00, w, h);
            assert_eq!(at(&first, 0, 0), 255);
            assert_eq!(at(&first, w - 1, h - 1), 0);
            let last = r(0x1FB3B, w, h);
            assert_eq!(at(&last, 0, 0), 0);
            assert_eq!(at(&last, w - 1, h - 1), 255);
            // 1CD00 is octant 3 (left column, second row).
            let o = r(0x1CD00, w, h);
            let y = frac(h, 1, 4);
            assert_eq!(at(&o, 0, y), 255);
            assert_eq!(at(&o, 0, 0), 0);
            assert_eq!(at(&o, w - 1, y), 0);
        }
    }

    #[test]
    fn braille_dots() {
        let b = r(0x2800, 8, 16);
        assert!(b.data.iter().all(|&v| v == 0));
        let all = r(0x28FF, 8, 16);
        let blank = "........\n";
        let dots = ".##..##.\n.##..##.\n";
        let expect = format!(
            "{blank}{dots}{blank}{blank}{dots}{blank}{blank}{dots}{blank}{blank}{dots}{blank}"
        );
        assert_eq!(art(&all), expect);
        // Dot 1 only (U+2801): upper-left.
        let one = r(0x2801, 8, 16);
        assert_eq!(at(&one, 1, 1), 255);
        assert_eq!(at(&one, 5, 1), 0);
        // Dot 8 only (U+2880): lower-right.
        let eight = r(0x2880, 8, 16);
        assert_eq!(at(&eight, 5, 13), 255);
        assert_eq!(at(&eight, 1, 13), 0);
    }

    #[test]
    fn light_cross_and_corners_at_odd_size() {
        // 5x9 cell: light = 1, stems centred at column 2 and row 4.
        let cross = r(0x253C, 5, 9);
        assert_eq!(
            art(&cross),
            "..#..\n..#..\n..#..\n..#..\n#####\n..#..\n..#..\n..#..\n..#..\n"
        );
        let corner = r(0x250C, 5, 9);
        assert_eq!(
            art(&corner),
            ".....\n.....\n.....\n.....\n..###\n..#..\n..#..\n..#..\n..#..\n"
        );
    }

    #[test]
    fn double_line_rows() {
        // 8x16: light 1; double = bands of 1 px with a 1 px gap, centred: rows 6 and 8.
        let h = r(0x2550, 8, 16);
        for y in 0..16 {
            let want = if y == 6 || y == 8 { 255 } else { 0 };
            assert!(h.row(y).iter().all(|&v| v == want), "row {y}");
        }
        // Double corner: outer line reaches the outer stem, inner line the inner stem.
        let c = r(0x2554, 8, 16);
        let rows: Vec<String> = art(&c).lines().map(str::to_string).collect();
        assert_eq!(rows[5], "........");
        assert_eq!(rows[6], "..######");
        assert_eq!(rows[7], "..#.....");
        assert_eq!(rows[8], "..#.####");
        assert_eq!(rows[9], "..#.#...");
        assert_eq!(rows[15], "..#.#...");
    }

    #[test]
    fn box_arms_reach_cell_edges_at_every_scale() {
        for (w, h) in SIZES {
            for (idx, &[kind, a, b, c, d]) in BOX.iter().enumerate().take(128) {
                if kind != 0 {
                    continue;
                }
                let bm = r(0x2500 + idx as u32, w, h);
                let col = |x: u32| (0..h).any(|y| at(&bm, x, y) == 255);
                let row = |y: u32| (0..w).any(|x| at(&bm, x, y) == 255);
                assert!(a == 0 || col(0), "U+{:04X} left at {w}x{h}", 0x2500 + idx);
                assert!(
                    b == 0 || col(w - 1),
                    "U+{:04X} right at {w}x{h}",
                    0x2500 + idx
                );
                assert!(c == 0 || row(0), "U+{:04X} up at {w}x{h}", 0x2500 + idx);
                assert!(
                    d == 0 || row(h - 1),
                    "U+{:04X} down at {w}x{h}",
                    0x2500 + idx
                );
            }
        }
    }

    #[test]
    fn horizontal_lines_tile_across_cells() {
        for (w, h) in SIZES {
            for cp in [0x2500, 0x2501, 0x2550] {
                let b = r(cp, w, h);
                for y in 0..h {
                    let row = b.row(y);
                    assert!(
                        row.iter().all(|&v| v == row[0]),
                        "U+{cp:04X} row {y} not uniform"
                    );
                }
            }
        }
    }

    #[test]
    fn dashes_continue_across_cells() {
        for (w, h) in SIZES {
            for (cp, n) in [(0x2504, 3), (0x2508, 4), (0x254C, 2)] {
                // n dashes with a gap each need at least 2n pixels.
                if w < 2 * n {
                    continue;
                }
                let b = r(cp, w, h);
                let y = (0..h).find(|&y| b.row(y).iter().any(|&v| v > 0)).unwrap();
                let row = b.row(y);
                // The last pixel is a gap, so abutting cells do not merge their end dashes.
                assert!(row.contains(&0));
                assert!(row[0] > 0, "U+{cp:04X} at {w}x{h}");
                assert_eq!(row[w as usize - 1], 0, "U+{cp:04X} at {w}x{h}");
            }
        }
    }

    #[test]
    fn arcs_join_the_edge_midpoints() {
        for (w, h) in SIZES {
            // U+256D: arc from the right edge to the bottom edge.
            let b = r(0x256D, w, h);
            let l = SpriteParams::new(w, h).light;
            let sx = bands(1, w, l)[0];
            let sy = bands(1, h, l)[0];
            assert!(at(&b, w - 1, sy.0) >= 200, "right edge at {w}x{h}");
            assert!(at(&b, sx.0, h - 1) >= 200, "bottom edge at {w}x{h}");
            assert_eq!(at(&b, 0, 0), 0);
            assert_eq!(at(&b, w - 1, 0), 0);
        }
    }

    #[test]
    fn diagonals_cross_corners() {
        for (w, h) in SIZES {
            let both = r(0x2573, w, h);
            assert!(at(&both, 0, 0) > 100 && at(&both, w - 1, h - 1) > 100);
            assert!(at(&both, w - 1, 0) > 100 && at(&both, 0, h - 1) > 100);
            let slash = r(0x2571, w, h);
            assert!(at(&slash, w - 1, 0) > 100);
            assert_eq!(at(&slash, 0, 0), 0);
        }
    }

    #[test]
    fn powerline_triangles_fill_the_base_edge() {
        for (w, h) in SIZES {
            let t = r(0xE0B0, w, h);
            // The corner rows hold only half the corner pixel; every row between is solid.
            for y in 1..h - 1 {
                assert_eq!(at(&t, 0, y), 255, "base column at {w}x{h} row {y}");
            }
            assert!(at(&t, w - 1, h / 2) > 100);
            assert_eq!(at(&t, w - 1, 0), 0);
            let l = r(0xE0B2, w, h);
            for y in 1..h - 1 {
                assert_eq!(at(&l, w - 1, y), 255);
            }
            let half = r(0xE0B4, w, h);
            assert!(at(&half, 0, h / 2) == 255 && at(&half, 0, 0) > 0);
            let lower_left = r(0xE0B8, w, h);
            assert_eq!(at(&lower_left, 0, h - 1), 255);
            assert_eq!(at(&lower_left, w - 1, 0), 0);
        }
    }

    #[test]
    fn mosaic_and_triangle_blocks() {
        for (w, h) in SIZES {
            // 1FB3C: lower-left corner wedge.
            let m = r(0x1FB3C, w, h);
            assert_eq!(at(&m, 0, h - 1), 255);
            assert_eq!(at(&m, w - 1, 0), 0);
            // 1FB6C left triangle, 1FB68 all but the left triangle: complementary.
            let left = r(0x1FB6C, w, h);
            let rest = r(0x1FB68, w, h);
            for i in 0..(w * h) as usize {
                let s = u32::from(left.data[i]) + u32::from(rest.data[i]);
                assert!((254..=256).contains(&s) || s == 255, "pixel {i}: {s}");
            }
        }
    }

    #[test]
    fn legacy_eighths_and_quarter_rects() {
        for (w, h) in SIZES {
            let v2 = r(0x1FB70, w, h);
            assert_eq!(
                at(&v2, frac(w, 1, 8), 0),
                if frac(w, 1, 8) < frac(w, 2, 8) {
                    255
                } else {
                    0
                }
            );
            let up_quarter = r(0x1FB82, w, h);
            assert_eq!(at(&up_quarter, 0, 0), 255);
            assert_eq!(at(&up_quarter, 0, h - 1), 0);
            let q = r(0x1CEA0, w, h);
            assert_eq!(at(&q, w - 1, h - 1), 255);
            assert_eq!(at(&q, 0, 0), 0);
            let lines = r(0x1FBA0, w, h);
            assert!(lines.data.iter().any(|&v| v > 0));
        }
    }

    #[test]
    fn thickness_scales_lines() {
        let thin = SpriteParams::new(10, 20);
        let thick = thin.with_thickness(3.0);
        let a = render(0x2500, &thin)
            .unwrap()
            .data
            .iter()
            .filter(|&&v| v == 255)
            .count();
        let b = render(0x2500, &thick)
            .unwrap()
            .data
            .iter()
            .filter(|&&v| v == 255)
            .count();
        assert_eq!(a, 10 * thin.light as usize);
        assert_eq!(b, 10 * thick.light as usize);
        assert!(b > a);
    }

    #[test]
    fn tofu_draws_a_box_and_hex() {
        let t = tofu(0x1F600, 18, 36);
        assert_eq!((t.width, t.height), (18, 36));
        assert_eq!(at(&t, 0, 0), 255);
        assert_eq!(at(&t, 17, 35), 255);
        assert!(t.data.iter().filter(|&&v| v == 255).count() > 2 * (18 + 36));
        // Different code points draw different digits.
        assert_ne!(tofu(0x1F600, 18, 36), tofu(0x1F601, 18, 36));
        // Tiny cells: box only, no panic, still exact size.
        for (w, h) in [(1, 1), (2, 3), (3, 5), (4, 8)] {
            let s = tofu(0xE9, w, h);
            assert_eq!(s.data.len(), (w * h) as usize);
        }
        assert!(tofu(1, 0, 0).data.is_empty());
    }
}
