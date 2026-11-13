// SPDX-License-Identifier: AGPL-3.0-only
//! CPU reference renderer for instance lists (T-07 §6).
//!
//! It draws exactly what the GPU passes are specified to draw (backgrounds, glyph quads,
//! decoration rects, cursor), in premultiplied RGBA8 with `One, OneMinusSrcAlpha`
//! compositing and the text-blending modes of [`crate::colour`]. It is the "from-scratch
//! render" the damage mirror test compares against, and the golden-image source when no GPU
//! adapter is present. It is not a fast path.

use crate::colour::{Rgb, TextBlending, blend_coverage};
use crate::cursor::CursorScene;
use crate::scene::{FLAG_COLOR, GlyphInstance, RectInstance, RowScene};
use apoth_font::metrics::CellMetrics;

/// A premultiplied RGBA8 image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width.
    pub w: usize,
    /// Height.
    pub h: usize,
    /// `w * h * 4` bytes, rows top to bottom.
    pub data: Vec<u8>,
}

impl Image {
    /// A transparent image (the premultiplied clear colour).
    #[must_use]
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            w,
            h,
            data: vec![0; w * h * 4],
        }
    }

    fn px(&mut self, x: i32, y: i32) -> Option<&mut [u8]> {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return None;
        }
        let i = (y as usize * self.w + x as usize) * 4;
        Some(&mut self.data[i..i + 4])
    }

    /// `One, OneMinusSrcAlpha` over a premultiplied source.
    fn over(&mut self, x: i32, y: i32, src: [u8; 4]) {
        if let Some(d) = self.px(x, y) {
            let inv = 255 - u32::from(src[3]);
            for (dc, sc) in d.iter_mut().zip(src) {
                let v = u32::from(sc) + (u32::from(*dc) * inv + 127) / 255;
                *dc = v.min(255) as u8;
            }
        }
    }

    /// Fills a rect (clipped) with `over`.
    pub fn fill(&mut self, r: &RectInstance) {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                self.over(x, y, r.color);
            }
        }
    }

    /// Straight-alpha copy for PNG output.
    #[must_use]
    pub fn to_straight(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for p in out.chunks_exact_mut(4) {
            let a = u32::from(p[3]);
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        out
    }
}

/// Pixel source for atlas rects: R8 coverage and RGBA colour (premultiplied).
pub trait AtlasPixels {
    /// Coverage at atlas page pixel `(x, y)`.
    fn coverage(&self, page: u16, x: u32, y: u32) -> u8;
    /// Premultiplied colour at atlas page pixel `(x, y)`.
    fn rgba(&self, page: u16, x: u32, y: u32) -> [u8; 4];
}

/// Draws one glyph instance at its cell's pixel origin plus its offset.
pub fn draw_glyph(
    img: &mut Image,
    g: &GlyphInstance,
    m: &CellMetrics,
    atlas: &dyn AtlasPixels,
    mode: TextBlending,
) {
    let ox = i32::from(g.cell[0]) * m.width + i32::from(g.offset[0]);
    let oy = i32::from(g.cell[1]) * m.height + i32::from(g.offset[1]);
    let fg = Rgb(g.color[0], g.color[1], g.color[2]);
    for j in 0..u32::from(g.size[1]) {
        for i in 0..u32::from(g.size[0]) {
            let (ax, ay) = (u32::from(g.atlas_xy[0]) + i, u32::from(g.atlas_xy[1]) + j);
            let (x, y) = (ox + i as i32, oy + j as i32);
            if g.flags & FLAG_COLOR != 0 {
                img.over(x, y, atlas.rgba(g.page, ax, ay));
                continue;
            }
            let cov = atlas.coverage(g.page, ax, ay);
            if cov == 0 {
                continue;
            }
            let Some(d) = img.px(x, y) else { continue };
            let a = f32::from(cov) / 255.0;
            if d[3] == 255 {
                let bg = Rgb(d[0], d[1], d[2]);
                let o = blend_coverage(mode, fg, bg, a);
                d.copy_from_slice(&[o.0, o.1, o.2, 255]);
            } else {
                // Translucent destination: plain premultiplied source-over.
                let src = [
                    (f32::from(fg.0) * a + 0.5) as u8,
                    (f32::from(fg.1) * a + 0.5) as u8,
                    (f32::from(fg.2) * a + 0.5) as u8,
                    cov,
                ];
                img.over(x, y, src);
            }
        }
    }
}

/// Renders the retained grid (rows in logical order), then the cursor.
#[must_use]
pub fn render(
    rows: &[RowScene],
    cursor: Option<&CursorScene>,
    m: &CellMetrics,
    atlas: &dyn AtlasPixels,
    mode: TextBlending,
) -> Image {
    let cols = rows.first().map_or(0, |r| r.bg.len());
    let mut img = Image::new(cols * m.width as usize, rows.len() * m.height as usize);
    for (y, row) in rows.iter().enumerate() {
        let y0 = y as i32 * m.height;
        for (x, bg) in row.bg.iter().enumerate() {
            img.fill(&RectInstance::new(
                x as i32 * m.width,
                y0,
                m.width,
                m.height,
                *bg,
            ));
        }
        for g in &row.glyphs {
            // Rows moved by a scroll keep the cell row they were built at; their position
            // is the row they now occupy.
            let mut g = *g;
            g.cell[1] = y as u16;
            draw_glyph(&mut img, &g, m, atlas, mode);
        }
        for r in &row.rects {
            img.fill(&RectInstance::new(r.x, r.y + y0, r.w, r.h, r.color));
        }
    }
    if let Some(c) = cursor {
        for r in &c.rects {
            img.fill(r);
        }
        if let Some(g) = &c.glyph {
            draw_glyph(&mut img, g, m, atlas, mode);
        }
    }
    img
}

/// Result of comparing two images.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffStats {
    /// Largest per-channel difference.
    pub max_channel: u8,
    /// Pixels with any channel above the tolerance.
    pub differing: usize,
    /// Total pixels.
    pub total: usize,
}

/// Compares two same-size images; panics if the sizes differ.
#[must_use]
pub fn diff(a: &Image, b: &Image, tolerance: u8) -> DiffStats {
    assert_eq!((a.w, a.h), (b.w, b.h), "image sizes differ");
    let mut s = DiffStats {
        max_channel: 0,
        differing: 0,
        total: a.w * a.h,
    };
    for (pa, pb) in a.data.chunks_exact(4).zip(b.data.chunks_exact(4)) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        s.max_channel = s.max_channel.max(d);
        if d > tolerance {
            s.differing += 1;
        }
    }
    s
}

/// A paged in-memory atlas for tests and CPU goldens.
#[derive(Clone, Debug)]
pub struct MemAtlas {
    size: u32,
    mask: Vec<Vec<u8>>,
    color: Vec<Vec<u8>>,
}

impl MemAtlas {
    /// `pages` square pages of `size` pixels.
    #[must_use]
    pub fn new(size: u32, pages: usize) -> Self {
        let n = (size * size) as usize;
        Self {
            size,
            mask: vec![vec![0; n]; pages],
            color: vec![vec![0; n * 4]; pages],
        }
    }

    /// Writes an R8 rect (`w * h` bytes) at `(x, y)` of `page`.
    pub fn write_mask(&mut self, page: u16, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        for j in 0..h {
            for i in 0..w {
                let at = ((y + j) * self.size + x + i) as usize;
                self.mask[usize::from(page)][at] = data[(j * w + i) as usize];
            }
        }
    }

    /// Writes a premultiplied RGBA rect (`w * h * 4` bytes).
    pub fn write_color(&mut self, page: u16, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        for j in 0..h {
            for i in 0..w {
                let at = (((y + j) * self.size + x + i) * 4) as usize;
                let s = ((j * w + i) * 4) as usize;
                self.color[usize::from(page)][at..at + 4].copy_from_slice(&data[s..s + 4]);
            }
        }
    }
}

impl MemAtlas {
    /// Page edge length in pixels.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Pages held.
    #[must_use]
    pub fn pages(&self) -> usize {
        self.mask.len()
    }

    /// The whole R8 page, `size * size` bytes (for uploading to a GPU atlas).
    #[must_use]
    pub fn mask_page(&self, page: usize) -> &[u8] {
        &self.mask[page]
    }

    /// The whole premultiplied RGBA page, `size * size * 4` bytes.
    #[must_use]
    pub fn color_page(&self, page: usize) -> &[u8] {
        &self.color[page]
    }
}

impl AtlasPixels for MemAtlas {
    fn coverage(&self, page: u16, x: u32, y: u32) -> u8 {
        self.mask[usize::from(page)][(y * self.size + x) as usize]
    }

    fn rgba(&self, page: u16, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * self.size + x) * 4) as usize;
        let c = &self.color[usize::from(page)];
        [c[at], c[at + 1], c[at + 2], c[at + 3]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decor::test_metrics;

    #[test]
    fn over_is_premultiplied_source_over() {
        let mut img = Image::new(1, 1);
        img.fill(&RectInstance::new(0, 0, 1, 1, [0, 0, 100, 255]));
        img.fill(&RectInstance::new(0, 0, 1, 1, [100, 0, 0, 100]));
        // dst * (1 - 100/255) + src
        assert_eq!(&img.data[..], &[100, 0, 61, 255]);
    }

    #[test]
    fn fill_is_clipped() {
        let mut img = Image::new(2, 2);
        img.fill(&RectInstance::new(-5, -5, 100, 100, [1, 1, 1, 255]));
        assert!(img.data.iter().all(|&b| b == 1 || b == 255));
        img.fill(&RectInstance::new(5, 5, 3, 3, [9, 9, 9, 255]));
    }

    #[test]
    fn glyph_uses_coverage_and_offset() {
        let m = test_metrics();
        let mut atlas = MemAtlas::new(16, 1);
        atlas.write_mask(0, 2, 3, 2, 1, &[255, 128]);
        let row = RowScene {
            bg: vec![[0, 0, 0, 255]; 2],
            glyphs: vec![GlyphInstance {
                cell: [1, 0],
                offset: [1, 2],
                atlas_xy: [2, 3],
                size: [2, 1],
                page: 0,
                flags: 0,
                color: [255, 255, 255, 255],
            }],
            ..RowScene::default()
        };
        let img = render(&[row], None, &m, &atlas, TextBlending::Native);
        let at = |x: usize, y: usize| img.data[(y * img.w + x) * 4];
        assert_eq!(at(8 + 1, 2), 255);
        assert_eq!(at(8 + 2, 2), 128);
        assert_eq!(at(8, 2), 0);
    }

    #[test]
    fn diff_counts() {
        let a = Image::new(2, 1);
        let mut b = a.clone();
        b.data[0] = 3;
        let s = diff(&a, &b, 2);
        assert_eq!((s.max_channel, s.differing, s.total), (3, 1, 2));
        assert_eq!(diff(&a, &b, 3).differing, 0);
    }

    #[test]
    fn straight_conversion() {
        let mut img = Image::new(1, 1);
        img.data.copy_from_slice(&[64, 0, 0, 128]);
        assert_eq!(img.to_straight(), vec![128, 0, 0, 128]);
    }
}
