// SPDX-License-Identifier: AGPL-3.0-only
//! Rasterisation (T-06 §7). Grayscale coverage masks and bitmap emoji come from the `swash`
//! scaler (skrifa outlines and hinting, `zeno` coverage); COLRv0/v1 is our painter
//! ([`crate::colr`]). Grid glyphs use integer origins; glass-interface text uses four
//! horizontal sub-pixel bins. There is no LCD anti-aliasing.

use crate::colr;
use crate::config::Hinting;
use crate::face::FaceInstance;
use std::collections::HashMap;
use std::sync::Arc;
use swash::scale::{Render, ScaleContext, Source, StrikeWith, image::Content};
use swash::zeno::{Angle, Format, Transform, Vector};

/// Horizontal sub-pixel positions used for glass-interface text.
pub const SUBPIXEL_BINS: u8 = 4;

/// An 8-bit coverage mask positioned relative to the glyph origin (baseline, pen position).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphBitmap {
    /// Pixels from the origin to the left edge (may be negative).
    pub left: i32,
    /// Pixels from the baseline up to the top edge.
    pub top: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height` coverage bytes, rows top to bottom.
    pub data: Vec<u8>,
}

impl GlyphBitmap {
    /// True if no pixel is covered.
    pub fn is_blank(&self) -> bool {
        self.width == 0 || self.height == 0 || self.data.iter().all(|&b| b == 0)
    }

    /// The columns `[x0, x1)` measured from the glyph origin, as a bitmap whose origin is the
    /// left edge of the slice at `x0`. A ligature spanning *n* cells is rasterised once and
    /// cut into *n* of these, so each cell keeps its own colours. Out-of-range columns are
    /// transparent.
    pub fn slice(&self, x0: i32, x1: i32) -> GlyphBitmap {
        let w = (x1 - x0).max(0) as u32;
        let mut data = vec![0u8; (w * self.height) as usize];
        for row in 0..self.height as usize {
            for cx in 0..w as usize {
                let src_x = x0 + cx as i32 - self.left;
                if src_x >= 0 && (src_x as u32) < self.width {
                    data[row * w as usize + cx] =
                        self.data[row * self.width as usize + src_x as usize];
                }
            }
        }
        GlyphBitmap {
            left: 0,
            top: self.top,
            width: w,
            height: self.height,
            data,
        }
    }
}

/// A premultiplied RGBA8 colour glyph positioned relative to the glyph origin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorBitmap {
    /// Pixels from the origin to the left edge.
    pub left: i32,
    /// Pixels from the baseline up to the top edge.
    pub top: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` premultiplied RGBA bytes.
    pub data: Vec<u8>,
}

/// Parameters of a grayscale rasterisation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterParams {
    /// Pixels per em.
    pub px_size: f32,
    /// Hinting mode. swash exposes one hinted mode (skrifa smooth hinting), so `Light` and
    /// `Full` both enable it and `None` disables it.
    pub hinting: Hinting,
    /// Horizontal sub-pixel bin, `0..SUBPIXEL_BINS`. Always 0 for grid glyphs.
    pub subpixel_bin: u8,
    /// `font.thicken`: extra stem weight as a percent of the pixel size.
    pub thicken_pct: f32,
}

impl RasterParams {
    /// Grid glyph parameters: integer origin.
    pub fn grid(px_size: f32, hinting: Hinting, thicken_pct: f32) -> Self {
        Self {
            px_size,
            hinting,
            subpixel_bin: 0,
            thicken_pct,
        }
    }
}

/// Embolden strength of synthetic bold, as a fraction of the pixel size.
const SYNTHETIC_BOLD: f32 = 0.035;
/// Skew of synthetic italic.
const SYNTHETIC_ITALIC_DEG: f32 = 14.0;
/// Largest glyph image we will produce, per side.
const MAX_SIDE: u32 = 4096;

/// Rasteriser: owns the swash scale context. One per render thread.
pub struct Rasterizer {
    ctx: ScaleContext,
}

impl std::fmt::Debug for Rasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rasterizer").finish_non_exhaustive()
    }
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer {
    /// A fresh rasteriser.
    pub fn new() -> Self {
        Self {
            ctx: ScaleContext::new(),
        }
    }

    /// Grayscale outline mask of `glyph`. `None` if the face has no outline for it or the glyph
    /// id does not fit swash's 16 bits.
    pub fn gray(
        &mut self,
        inst: &FaceInstance,
        glyph: u32,
        p: &RasterParams,
    ) -> Option<GlyphBitmap> {
        let gid = u16::try_from(glyph).ok()?;
        let face = inst.face();
        let font = face.swash_font()?;
        let mut scaler = self
            .ctx
            .builder_with_id(font, [face.uid(), u64::from(face.ttc_index())])
            .size(p.px_size)
            .hint(p.hinting != Hinting::None)
            .normalized_coords(inst.swash_coords())
            .build();
        let bin = p.subpixel_bin.min(SUBPIXEL_BINS - 1);
        let mut render = Render::new(&[Source::Outline]);
        render
            .format(Format::Alpha)
            .offset(Vector::new(f32::from(bin) / f32::from(SUBPIXEL_BINS), 0.0));
        let strength = if inst.synthetic_bold() {
            SYNTHETIC_BOLD * p.px_size
        } else {
            0.0
        } + p.thicken_pct * 0.01 * p.px_size;
        if strength > 0.0 {
            render.embolden(strength);
        }
        if inst.synthetic_italic() {
            render.transform(Some(Transform::skew(
                Angle::from_degrees(SYNTHETIC_ITALIC_DEG),
                Angle::from_degrees(0.0),
            )));
        }
        let img = render.render(&mut scaler, gid)?;
        if img.content != Content::Mask
            || img.placement.width > MAX_SIDE
            || img.placement.height > MAX_SIDE
        {
            return None;
        }
        Some(GlyphBitmap {
            left: img.placement.left,
            top: img.placement.top,
            width: img.placement.width,
            height: img.placement.height,
            data: img.data,
        })
    }

    /// Colour image of `glyph`: COLRv0/v1 through our painter, else a CBDT/sbix bitmap through
    /// swash. `foreground` fills palette entry 0xFFFF. OT-SVG is not drawn.
    pub fn color(
        &mut self,
        inst: &FaceInstance,
        glyph: u32,
        px_size: f32,
        foreground: [u8; 4],
    ) -> Option<ColorBitmap> {
        let src = inst.face().color();
        if src.colr
            && let Some(bm) = colr::paint_glyph(inst, glyph, px_size, foreground)
        {
            return Some(bm);
        }
        if !(src.cbdt || src.sbix) {
            return None;
        }
        let gid = u16::try_from(glyph).ok()?;
        let face = inst.face();
        let font = face.swash_font()?;
        let mut scaler = self
            .ctx
            .builder_with_id(font, [face.uid(), u64::from(face.ttc_index())])
            .size(px_size)
            .normalized_coords(inst.swash_coords())
            .build();
        let img =
            Render::new(&[Source::ColorBitmap(StrikeWith::BestFit)]).render(&mut scaler, gid)?;
        if img.content != Content::Color
            || img.placement.width > MAX_SIDE
            || img.placement.height > MAX_SIDE
            || img.data.len() != (img.placement.width * img.placement.height * 4) as usize
        {
            return None;
        }
        let mut data = img.data;
        for px in data.chunks_exact_mut(4) {
            let a = u32::from(px[3]);
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * a + 127) / 255) as u8;
            }
        }
        Some(ColorBitmap {
            left: img.placement.left,
            top: img.placement.top,
            width: img.placement.width,
            height: img.placement.height,
            data,
        })
    }
}

/// Key of a cached grid glyph. `instance` is [`FaceInstance::key`] (face, variation location
/// and synthetic flags), so a bold-synthesised and a true-bold glyph never collide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// [`FaceInstance::key`].
    pub instance: u64,
    /// Glyph id.
    pub glyph: u32,
    /// Pixel size times 64, rounded.
    pub size_q: u32,
    /// Hinting mode.
    pub hinting: Hinting,
    /// Sub-pixel bin (0 for grid glyphs).
    pub subpixel_bin: u8,
    /// Thicken as hundredths of a percent.
    pub thicken_q: u32,
}

impl GlyphKey {
    /// Builds the key for a rasterisation request.
    pub fn new(inst: &FaceInstance, glyph: u32, p: &RasterParams) -> Self {
        Self {
            instance: inst.key(),
            glyph,
            size_q: (p.px_size * 64.0).round() as u32,
            hinting: p.hinting,
            subpixel_bin: p.subpixel_bin,
            thicken_q: (p.thicken_pct * 100.0).round().max(0.0) as u32,
        }
    }
}

/// Grayscale glyph cache plus a separate colour cache keyed per (glyph, size) (FONTS-18). Both
/// are emptied together when the font generation changes.
#[derive(Debug, Default)]
pub struct GlyphCache {
    gray: HashMap<GlyphKey, Option<Arc<GlyphBitmap>>>,
    color: HashMap<(u64, u32, u32), Option<Arc<ColorBitmap>>>,
    generation: u64,
    /// Rasterisations performed (cache misses).
    pub rasterized: u64,
}

impl GlyphCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops everything if `generation` differs from the one the cache was filled under.
    pub fn sync_generation(&mut self, generation: u64) {
        if self.generation != generation {
            self.gray.clear();
            self.color.clear();
            self.generation = generation;
        }
    }

    /// Cached grayscale glyph; a glyph that fails to rasterise is cached as missing once.
    pub fn gray(
        &mut self,
        r: &mut Rasterizer,
        inst: &FaceInstance,
        glyph: u32,
        p: &RasterParams,
    ) -> Option<Arc<GlyphBitmap>> {
        let key = GlyphKey::new(inst, glyph, p);
        if let Some(v) = self.gray.get(&key) {
            return v.clone();
        }
        self.rasterized += 1;
        let v = r.gray(inst, glyph, p).map(Arc::new);
        self.gray.insert(key, v.clone());
        v
    }

    /// Cached colour glyph at zero offset, per (glyph, size).
    pub fn color(
        &mut self,
        r: &mut Rasterizer,
        inst: &FaceInstance,
        glyph: u32,
        px_size: f32,
        foreground: [u8; 4],
    ) -> Option<Arc<ColorBitmap>> {
        let key = (inst.key(), glyph, (px_size * 64.0).round() as u32);
        if let Some(v) = self.color.get(&key) {
            return v.clone();
        }
        self.rasterized += 1;
        let v = r.color(inst, glyph, px_size, foreground).map(Arc::new);
        self.color.insert(key, v.clone());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_cuts_columns_and_keeps_rows() {
        let g = GlyphBitmap {
            left: -2,
            top: 5,
            width: 6,
            height: 2,
            data: (0..12).collect(),
        };
        // Columns are measured from the glyph origin; the bitmap starts at x = -2.
        let s = g.slice(0, 3);
        assert_eq!((s.left, s.top, s.width, s.height), (0, 5, 3, 2));
        assert_eq!(s.data, vec![2, 3, 4, 8, 9, 10]);
        // Beyond the bitmap: transparent.
        let s = g.slice(2, 6);
        assert_eq!(s.data, vec![4, 5, 0, 0, 10, 11, 0, 0]);
        // Slices of a partition reassemble the original columns.
        let a = g.slice(-2, 1);
        let b = g.slice(1, 4);
        assert_eq!(a.data.len() + b.data.len(), 12);
    }

    #[test]
    fn blank_detection() {
        assert!(GlyphBitmap::default().is_blank());
        let g = GlyphBitmap {
            left: 0,
            top: 0,
            width: 1,
            height: 1,
            data: vec![0],
        };
        assert!(g.is_blank());
        let g = GlyphBitmap { data: vec![1], ..g };
        assert!(!g.is_blank());
    }
}
