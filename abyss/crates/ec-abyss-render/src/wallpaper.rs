// SPDX-License-Identifier: AGPL-3.0-only
//! The compositor's wallpaper (ADR 0104, stage 2): the bottom-most element of
//! an output, under the background layer and so under everything blur samples.
//!
//! State-free in the sense of COMP-02: nothing here reads `AbyssState`. The
//! picture is decoded and laid out off the main thread ([`compose`] is the
//! pure part), arrives as one output-sized BGRA buffer, and is drawn 1:1. Until
//! then, and when a decode fails, the output shows its colour alone.
//!
//! Fit modes are those `ec-wallpaper` had: `fill` covers and crops, `fit`
//! letterboxes in the colour, `center` is one image pixel per logical pixel.
//! All three are one rule: scale the image by `s` physical pixels per image
//! pixel (`max`, `min` or the output scale), centre it, clip it to the output
//! ([`place`]).

use std::collections::HashMap;
use std::path::PathBuf;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                solid::{SolidColorBuffer, SolidColorRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    output::Output,
    utils::{Buffer, Point, Rectangle, Scale, Size, Transform},
};

use crate::AbyssRenderElement;

/// How the picture meets the output (`wallpaper.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Fill,
    Fit,
    Center,
}

impl Mode {
    /// An unknown name is `fill`; the config parser has already refused it.
    pub fn parse(s: &str) -> Mode {
        match s {
            "fit" => Mode::Fit,
            "center" => Mode::Center,
            _ => Mode::Fill,
        }
    }
}

/// Where the picture lands: `dst` is the visible rectangle in output pixels
/// (x, y, w, h), `src` the part of the image it shows, in image pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub dst: (i32, i32, u32, u32),
    pub src: (f64, f64, f64, f64),
}

/// Lay an `img`-sized picture on an `out`-sized output (both physical pixels).
/// `scale` is the output scale, which `center` uses. `None` for an empty
/// image or output.
pub fn place(mode: Mode, img: (u32, u32), out: (u32, u32), scale: f64) -> Option<Placement> {
    let (iw, ih) = (img.0 as f64, img.1 as f64);
    let (ow, oh) = (out.0 as f64, out.1 as f64);
    if img.0 == 0 || img.1 == 0 || out.0 == 0 || out.1 == 0 || scale <= 0.0 {
        return None;
    }
    let s = match mode {
        Mode::Fill => (ow / iw).max(oh / ih),
        Mode::Fit => (ow / iw).min(oh / ih),
        Mode::Center => scale,
    };
    let (fw, fh) = (iw * s, ih * s);
    let (x0, y0) = ((ow - fw) / 2.0, (oh - fh) / 2.0);
    let span = |origin: f64, len: f64, bound: f64| {
        let lo = origin.max(0.0).round();
        let hi = (origin + len).min(bound).round();
        (lo as i32, (hi - lo).max(0.0) as u32)
    };
    let (dx, dw) = span(x0, fw, ow);
    let (dy, dh) = span(y0, fh, oh);
    if dw == 0 || dh == 0 {
        return None;
    }
    let src = (
        ((dx as f64 - x0) / s).clamp(0.0, iw),
        ((dy as f64 - y0) / s).clamp(0.0, ih),
        (dw as f64 / s).min(iw),
        (dh as f64 / s).min(ih),
    );
    Some(Placement {
        dst: (dx, dy, dw, dh),
        src,
    })
}

/// A decoded picture: straight-alpha RGBA8, row-major.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Taps for one axis: for each destination pixel the first source index and
/// its weights (a triangle filter as wide as the reduction, so a downscale
/// averages instead of aliasing).
fn taps(src0: f64, src_len: f64, dst_len: u32, bound: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = src_len / dst_len as f64;
    let reach = ratio.max(1.0);
    (0..dst_len)
        .map(|i| {
            let centre = src0 + (i as f64 + 0.5) * ratio;
            let lo = ((centre - reach).floor().max(0.0) as usize).min(bound - 1);
            let hi = ((centre + reach).ceil() as usize).clamp(lo + 1, bound);
            let mut w: Vec<f32> = (lo..hi)
                .map(|k| (1.0 - ((k as f64 + 0.5 - centre).abs() / reach)).max(0.0) as f32)
                .collect();
            let sum: f32 = w.iter().sum();
            if sum > 0.0 {
                w.iter_mut().for_each(|v| *v /= sum);
            } else {
                // Centre between two samples with no overlap (extreme upscale
                // edge): take the nearest one.
                w.iter_mut().for_each(|v| *v = 0.0);
                w[((centre as usize).clamp(lo, hi - 1)) - lo] = 1.0;
            }
            (lo, w)
        })
        .collect()
}

/// Resample the `src` rectangle of `img` to `dw` x `dh`: premultiplied RGBA8.
fn resample(img: &Image, src: (f64, f64, f64, f64), dw: u32, dh: u32) -> Vec<u8> {
    let (iw, ih) = (img.width as usize, img.height as usize);
    let cols = taps(src.0, src.2, dw, iw);
    let rows = taps(src.1, src.3, dh, ih);
    // Only the source rows some output row reads.
    let first = rows.iter().map(|(s, _)| *s).min().unwrap_or(0);
    let last = rows.iter().map(|(s, w)| s + w.len()).max().unwrap_or(0);
    let span = last - first;
    // Horizontal pass, premultiplying as it goes.
    let mut mid = vec![0u8; span * dw as usize * 4];
    for y in 0..span {
        let row = &img.rgba[(first + y) * iw * 4..(first + y + 1) * iw * 4];
        for (x, (start, w)) in cols.iter().enumerate() {
            let mut acc = [0f32; 4];
            for (k, wk) in w.iter().enumerate() {
                let p = &row[(start + k) * 4..(start + k) * 4 + 4];
                let a = p[3] as f32 / 255.0;
                acc[0] += wk * p[0] as f32 * a;
                acc[1] += wk * p[1] as f32 * a;
                acc[2] += wk * p[2] as f32 * a;
                acc[3] += wk * p[3] as f32;
            }
            let o = (y * dw as usize + x) * 4;
            for c in 0..4 {
                mid[o + c] = acc[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    // Vertical pass.
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for (y, (start, w)) in rows.iter().enumerate() {
        for x in 0..dw as usize {
            let mut acc = [0f32; 4];
            for (k, wk) in w.iter().enumerate() {
                let o = ((start - first + k) * dw as usize + x) * 4;
                for c in 0..4 {
                    acc[c] += wk * mid[o + c] as f32;
                }
            }
            let o = (y * dw as usize + x) * 4;
            for c in 0..4 {
                out[o + c] = acc[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// The whole output as `Argb8888` bytes (B, G, R, A in memory, A always 255):
/// `color` everywhere, the picture placed on it per `mode`. The picture's own
/// alpha blends over `color`.
pub fn compose(img: &Image, out: (u32, u32), mode: Mode, scale: f64, color: [u8; 3]) -> Vec<u8> {
    let (ow, oh) = (out.0 as usize, out.1 as usize);
    let mut buf = vec![255u8; ow * oh * 4];
    for px in buf.chunks_exact_mut(4) {
        px[0] = color[2];
        px[1] = color[1];
        px[2] = color[0];
    }
    let Some(p) = place(mode, (img.width, img.height), out, scale) else {
        return buf;
    };
    if img.rgba.len() < img.width as usize * img.height as usize * 4 {
        return buf;
    }
    let pm = resample(img, p.src, p.dst.2, p.dst.3);
    for y in 0..p.dst.3 as usize {
        for x in 0..p.dst.2 as usize {
            let s = &pm[(y * p.dst.2 as usize + x) * 4..][..4];
            let o = ((p.dst.1 as usize + y) * ow + p.dst.0 as usize + x) * 4;
            let behind = (255 - s[3]) as u32;
            let mix =
                |over: u8, under: u8| (over as u32 + (under as u32 * behind + 127) / 255).min(255) as u8;
            buf[o] = mix(s[2], color[2]);
            buf[o + 1] = mix(s[1], color[1]);
            buf[o + 2] = mix(s[0], color[0]);
        }
    }
    buf
}

/// What one output's wallpaper was built from. A new key is a new build.
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    /// `None` is the colour alone.
    pub path: Option<PathBuf>,
    pub mode: Mode,
    pub color: [u8; 3],
    /// The output in physical pixels.
    pub size: (u32, u32),
    pub scale_milli: u32,
    /// Bumped by [`Store::invalidate`] (a config reload): re-read the file.
    pub epoch: u64,
}

struct Shown {
    hash: [u8; 32],
    buffer: MemoryRenderBuffer,
}

struct Slot {
    solid: SolidColorBuffer,
    size: Size<i32, smithay::utils::Logical>,
    shown: Option<Shown>,
    /// The last key a build was attempted for, whatever came of it, so a
    /// picture that will not decode is not retried every frame.
    tried: Option<Key>,
}

/// Every output's wallpaper. Owned by `BorderStore`.
#[derive(Default)]
pub struct Store {
    slots: HashMap<Output, Slot>,
    epoch: u64,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Ask every output to re-read its picture (the picture still shows until
    /// the new one lands).
    pub fn invalidate(&mut self) {
        self.epoch += 1;
    }

    /// Record the colour and logical size `output` is drawn with now.
    pub fn want(&mut self, output: &Output, size: Size<i32, smithay::utils::Logical>, color: [f32; 4]) {
        let slot = self.slots.entry(output.clone()).or_insert_with(|| Slot {
            solid: SolidColorBuffer::new(size, color),
            size,
            shown: None,
            tried: None,
        });
        slot.size = size;
        slot.solid.update(size, color);
    }

    /// True when a build for `key` has already been tried (or landed).
    pub fn satisfied(&self, output: &Output, key: &Key) -> bool {
        self.slots
            .get(output)
            .is_some_and(|s| s.tried.as_ref() == Some(key))
    }

    /// A build finished: `pixels` is the composed buffer and its hash, or
    /// `None` when the picture could not be read (or `key` has none), which
    /// leaves the colour alone. Returns whether anything visible changed.
    pub fn install(&mut self, output: &Output, key: Key, pixels: Option<(Vec<u8>, [u8; 32])>) -> bool {
        let Some(slot) = self.slots.get_mut(output) else {
            return false;
        };
        slot.tried = Some(key.clone());
        match pixels {
            None => slot.shown.take().is_some(),
            Some((_, hash)) if slot.shown.as_ref().is_some_and(|s| s.hash == hash) => false,
            Some((bytes, hash)) => {
                let (w, h) = (key.size.0 as i32, key.size.1 as i32);
                let opaque = Rectangle::<i32, Buffer>::from_size(Size::from((w, h)));
                let buffer = MemoryRenderBuffer::from_slice(
                    &bytes,
                    Fourcc::Argb8888,
                    (w, h),
                    1,
                    Transform::Normal,
                    Some(vec![opaque]),
                );
                slot.shown = Some(Shown { hash, buffer });
                true
            }
        }
    }

    /// Forget outputs that are gone.
    pub fn retain(&mut self, live: &[Output]) {
        self.slots.retain(|o, _| live.contains(o));
    }

    /// The bottom-most element of `output`: the picture, else its colour.
    /// `scale` is the output's fractional scale.
    pub fn element(
        &self,
        renderer: &mut GlesRenderer,
        output: &Output,
        scale: Scale<f64>,
    ) -> Option<AbyssRenderElement> {
        let slot = self.slots.get(output)?;
        if let Some(shown) = &slot.shown {
            let size = slot.size;
            // `src` is the whole raster at scale 1 (see `titlebar::raster_src`),
            // `size` the logical box it fills; the raster is output-sized in
            // physical pixels so it lands 1:1.
            let raster = Size::<i32, Buffer>::from((
                (size.w as f64 * scale.x).round() as i32,
                (size.h as f64 * scale.y).round() as i32,
            ));
            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                Point::<f64, smithay::utils::Physical>::from((0.0, 0.0)),
                &shown.buffer,
                None,
                Some(crate::titlebar::raster_src((raster.w, raster.h))),
                Some(size),
                Kind::Unspecified,
            ) {
                return Some(AbyssRenderElement::Memory(e));
            }
        }
        Some(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
            &slot.solid,
            (0, 0),
            scale,
            1.0,
            Kind::Unspecified,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, px: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            rgba: px.iter().copied().cycle().take((w * h * 4) as usize).collect(),
        }
    }

    #[test]
    fn fill_covers_and_crops_the_overhang() {
        // 4:3 picture on a 16:9 output: full width, top and bottom cropped.
        let p = place(Mode::Fill, (4000, 3000), (1920, 1080), 1.0).unwrap();
        assert_eq!(p.dst, (0, 0, 1920, 1080));
        assert!((p.src.0).abs() < 1e-6 && (p.src.2 - 4000.0).abs() < 1e-6);
        assert!((p.src.1 - 3000.0 * (1.0 - 1080.0 / 1440.0) / 2.0).abs() < 1.0);
        assert!((p.src.3 - 2250.0).abs() < 1.0);
    }

    #[test]
    fn fit_letterboxes_all_of_the_picture() {
        let p = place(Mode::Fit, (4000, 3000), (1920, 1080), 1.0).unwrap();
        assert_eq!(p.dst, (240, 0, 1440, 1080));
        assert_eq!(p.src, (0.0, 0.0, 4000.0, 3000.0));
        // Wider than the output: bars above and below.
        let p = place(Mode::Fit, (3000, 1000), (1500, 1000), 1.0).unwrap();
        assert_eq!(p.dst, (0, 250, 1500, 500));
    }

    #[test]
    fn center_is_one_image_pixel_per_logical_pixel() {
        // Smaller than the output: bordered, at native size times the scale.
        let p = place(Mode::Center, (400, 200), (1000, 800), 1.0).unwrap();
        assert_eq!(p.dst, (300, 300, 400, 200));
        let p = place(Mode::Center, (400, 200), (1000, 800), 2.0).unwrap();
        assert_eq!(p.dst, (100, 200, 800, 400));
        assert_eq!(p.src, (0.0, 0.0, 400.0, 200.0));
        // Larger: cropped evenly, the middle shows.
        let p = place(Mode::Center, (3000, 2000), (1000, 800), 1.0).unwrap();
        assert_eq!(p.dst, (0, 0, 1000, 800));
        assert_eq!(p.src, (1000.0, 600.0, 1000.0, 800.0));
    }

    #[test]
    fn empty_inputs_place_nothing() {
        assert!(place(Mode::Fill, (0, 10), (10, 10), 1.0).is_none());
        assert!(place(Mode::Fill, (10, 10), (0, 10), 1.0).is_none());
    }

    #[test]
    fn fill_leaves_no_colour_showing() {
        let out = compose(
            &img(8, 4, [200, 100, 50, 255]),
            (16, 16),
            Mode::Fill,
            1.0,
            [1, 2, 3],
        );
        for px in out.chunks_exact(4) {
            assert_eq!(px, [50, 100, 200, 255]);
        }
    }

    #[test]
    fn fit_paints_the_bars_in_the_colour() {
        let out = compose(
            &img(8, 4, [200, 100, 50, 255]),
            (16, 16),
            Mode::Fit,
            1.0,
            [1, 2, 3],
        );
        let at = |x: usize, y: usize| &out[(y * 16 + x) * 4..][..4];
        assert_eq!(at(8, 0), [3, 2, 1, 255]);
        assert_eq!(at(8, 15), [3, 2, 1, 255]);
        assert_eq!(at(8, 8), [50, 100, 200, 255]);
    }

    #[test]
    fn a_translucent_picture_blends_over_the_colour() {
        let out = compose(
            &img(4, 4, [255, 255, 255, 128]),
            (4, 4),
            Mode::Fill,
            1.0,
            [0, 0, 0],
        );
        assert!(out
            .chunks_exact(4)
            .all(|p| (126..=130).contains(&p[0]) && p[3] == 255));
    }

    #[test]
    fn downscale_averages() {
        // A 2x1 black/white checker reduced to one pixel is mid-grey.
        let image = Image {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255],
        };
        let out = compose(&image, (1, 1), Mode::Fill, 1.0, [9, 9, 9]);
        assert!((out[0] as i32 - 128).abs() <= 2, "{out:?}");
    }
}
