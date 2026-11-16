// SPDX-License-Identifier: AGPL-3.0-only
//! Container decoders for iTerm2 images and `apoth icat` (T-15 §3, §5; GRAPHICS-21, -33):
//! PNG and APNG (`png`), GIF (`gif`), JPEG (`zune-jpeg`), WebP and animated WebP
//! (`image-webp`). Every decoder runs under the [`Limits`]: dimensions are checked before
//! the pixel buffer exists, and an animation stops being read once its frames would pass
//! the animation quota or [`MAX_FRAMES`]. Animated formats are composed to full frames here
//! (disposal and blending applied), so the result feeds [`crate::anim`] unchanged.
//!
//! Blocking CPU work: the worker pool or the CLI, never the io thread (PERFORMANCE-53).

use crate::anim::MAX_FRAMES;
use crate::kitty::decode_png;
use crate::{ErrCode, Image, Limits, checked_rgba_len};
use std::io::Cursor;

/// A container format, by magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Gif,
    Jpeg,
    Webp,
}

/// Identify the format from its first bytes.
pub fn sniff(b: &[u8]) -> Option<Kind> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(Kind::Gif)
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Kind::Jpeg)
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(Kind::Webp)
    } else {
        None
    }
}

/// Decoded frames, composed to full size. A still image has one frame (gap 0).
#[derive(Debug)]
pub struct Decoded {
    /// Frame pixels and the time each shows (ms).
    pub frames: Vec<(Image, i32)>,
}

impl Decoded {
    fn still(img: Image) -> Self {
        Self {
            frames: vec![(img, 0)],
        }
    }
}

/// Decode any supported container. `anim_quota` bounds the bytes of frames after the first.
pub fn decode_any(bytes: &[u8], limits: &Limits, anim_quota: u64) -> Result<Decoded, ErrCode> {
    match sniff(bytes).ok_or(ErrCode::Einval)? {
        Kind::Png => decode_apng(bytes, limits, anim_quota),
        Kind::Gif => decode_gif(bytes, limits, anim_quota),
        Kind::Jpeg => decode_jpeg(bytes, limits).map(Decoded::still),
        Kind::Webp => decode_webp(bytes, limits, anim_quota),
    }
}

/// Collects frames under the frame count and animation quota.
struct Frames {
    out: Vec<(Image, i32)>,
    bytes: u64,
    quota: u64,
}

impl Frames {
    fn new(quota: u64) -> Self {
        Self {
            out: Vec::new(),
            bytes: 0,
            quota,
        }
    }

    /// False once no more frames fit (the rest of the animation is dropped).
    fn push(&mut self, img: Image, gap: i32) -> bool {
        if self.out.len() >= MAX_FRAMES {
            return false;
        }
        if !self.out.is_empty() {
            if self.bytes + img.bytes() > self.quota {
                return false;
            }
            self.bytes += img.bytes();
        }
        self.out.push((img, gap));
        true
    }

    fn done(self) -> Result<Decoded, ErrCode> {
        if self.out.is_empty() {
            return Err(ErrCode::Enodata);
        }
        let mut frames = self.out;
        if frames.len() == 1 {
            frames[0].1 = 0;
        }
        Ok(Decoded { frames })
    }
}

fn blank(w: u32, h: u32, limits: &Limits) -> Result<Image, ErrCode> {
    let len = checked_rgba_len(w, h, limits)?;
    if len as u64 > limits.image_cap() {
        return Err(ErrCode::Efbig);
    }
    Ok(Image {
        width: w,
        height: h,
        rgba: vec![0; len],
    })
}

/// Clear `x, y, w, h` (clipped) of `img` to transparent.
fn clear_rect(img: &mut Image, x: u32, y: u32, w: u32, h: u32) {
    let x1 = (x + w).min(img.width);
    let y1 = (y + h).min(img.height);
    for yy in y..y1 {
        let a = (yy as usize * img.width as usize + x as usize) * 4;
        let b = (yy as usize * img.width as usize + x1 as usize) * 4;
        if a < b {
            img.rgba[a..b].fill(0);
        }
    }
}

/// Paste an RGBA sub-image (clipped) at `x, y`; `over` blends, else replaces.
fn paste(canvas: &mut Image, sub: &Image, x: u32, y: u32, over: bool) {
    if x >= canvas.width || y >= canvas.height {
        return;
    }
    let w = sub.width.min(canvas.width - x);
    let h = sub.height.min(canvas.height - y);
    crate::anim::composite(canvas, sub, 0, 0, x, y, w, h, !over);
}

fn to_rgba(px: &[u8], ct: png::ColorType, n: usize) -> Result<Vec<u8>, ErrCode> {
    let mut o = Vec::with_capacity(n * 4);
    match ct {
        png::ColorType::Rgba => o.extend_from_slice(px.get(..n * 4).ok_or(ErrCode::Ebadpng)?),
        png::ColorType::Rgb => {
            for p in px.chunks_exact(3).take(n) {
                o.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in px.iter().take(n) {
                o.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for p in px.chunks_exact(2).take(n) {
                o.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        png::ColorType::Indexed => return Err(ErrCode::Ebadpng),
    }
    if o.len() != n * 4 {
        return Err(ErrCode::Ebadpng);
    }
    Ok(o)
}

/// PNG, with APNG frames when the file carries an animation control chunk.
pub fn decode_apng(bytes: &[u8], limits: &Limits, quota: u64) -> Result<Decoded, ErrCode> {
    let cap = usize::try_from(limits.image_cap()).unwrap_or(usize::MAX);
    let mut dec = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: cap });
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut r = dec.read_info().map_err(|_| ErrCode::Ebadpng)?;
    let (w, h) = r.info().size();
    let Some(actl) = r.info().animation_control else {
        return decode_png(bytes, limits).map(Decoded::still);
    };
    let mut canvas = blank(w, h, limits)?;
    let full = r.output_buffer_size().ok_or(ErrCode::Efbig)?;
    if full as u64 > limits.image_cap() {
        return Err(ErrCode::Efbig);
    }
    let mut buf = vec![0u8; full];
    let mut frames = Frames::new(quota);
    // The default image may sit outside the animation (no frame control before IDAT).
    let total = actl.num_frames as usize + 1;
    for _ in 0..total.min(MAX_FRAMES + 1) {
        let Ok(oi) = r.next_frame(&mut buf) else {
            break;
        };
        let Some(fc) = r.info().frame_control else {
            continue; // the hidden default image
        };
        let n = oi.width as usize * oi.height as usize;
        let sub = Image {
            width: oi.width,
            height: oi.height,
            rgba: to_rgba(&buf, oi.color_type, n)?,
        };
        let before = (fc.dispose_op == png::DisposeOp::Previous).then(|| canvas.clone());
        paste(
            &mut canvas,
            &sub,
            fc.x_offset,
            fc.y_offset,
            fc.blend_op == png::BlendOp::Over,
        );
        let den = if fc.delay_den == 0 { 100 } else { fc.delay_den };
        let gap = (u32::from(fc.delay_num) * 1000 / u32::from(den)) as i32;
        if !frames.push(canvas.clone(), gap.max(1)) {
            break;
        }
        match fc.dispose_op {
            png::DisposeOp::Background => {
                clear_rect(&mut canvas, fc.x_offset, fc.y_offset, oi.width, oi.height);
            }
            png::DisposeOp::Previous => {
                if let Some(b) = before {
                    canvas = b;
                }
            }
            png::DisposeOp::None => {}
        }
    }
    frames.done()
}

/// GIF: frames composed with their disposal methods; a delay under 20 ms shows 100 ms (the
/// common reading of 0 and 1 centisecond delays).
pub fn decode_gif(bytes: &[u8], limits: &Limits, quota: u64) -> Result<Decoded, ErrCode> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let cap = std::num::NonZeroU64::new(limits.image_cap()).ok_or(ErrCode::Efbig)?;
    opts.set_memory_limit(gif::MemoryLimit::Bytes(cap));
    let mut d = opts
        .read_info(Cursor::new(bytes))
        .map_err(|_| ErrCode::Enodata)?;
    let (w, h) = (u32::from(d.width()), u32::from(d.height()));
    let mut canvas = blank(w, h, limits)?;
    let mut frames = Frames::new(quota);
    loop {
        let f = match d.read_next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) if !frames.out.is_empty() => break,
            Err(_) => return Err(ErrCode::Enodata),
        };
        let (fx, fy, fw, fh) = (
            u32::from(f.left),
            u32::from(f.top),
            u32::from(f.width),
            u32::from(f.height),
        );
        if f.buffer.len() != fw as usize * fh as usize * 4 {
            break;
        }
        let sub = Image {
            width: fw,
            height: fh,
            rgba: f.buffer.to_vec(),
        };
        let dispose = f.dispose;
        let delay = u32::from(f.delay) * 10;
        let before = (dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
        paste(&mut canvas, &sub, fx, fy, true);
        let gap = if delay < 20 { 100 } else { delay as i32 };
        if !frames.push(canvas.clone(), gap) {
            break;
        }
        match dispose {
            gif::DisposalMethod::Background => clear_rect(&mut canvas, fx, fy, fw, fh),
            gif::DisposalMethod::Previous => {
                if let Some(b) = before {
                    canvas = b;
                }
            }
            _ => {}
        }
    }
    frames.done()
}

/// JPEG to RGBA (opaque).
pub fn decode_jpeg(bytes: &[u8], limits: &Limits) -> Result<Image, ErrCode> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let max = limits.max_axis as usize;
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(max)
        .set_max_height(max);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(bytes), opts);
    d.decode_headers().map_err(|_| ErrCode::Enodata)?;
    let info = d.info().ok_or(ErrCode::Enodata)?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    let len = checked_rgba_len(w, h, limits)?;
    if len as u64 > limits.image_cap() {
        return Err(ErrCode::Efbig);
    }
    let out_len = d.output_buffer_size().ok_or(ErrCode::Enodata)?;
    let n = w as usize * h as usize;
    if out_len != n * 4 && out_len != n * 3 {
        return Err(ErrCode::Enodata);
    }
    let mut px = vec![0u8; out_len];
    d.decode_into(&mut px).map_err(|_| ErrCode::Enodata)?;
    let rgba = if out_len == n * 4 {
        px
    } else {
        let mut o = Vec::with_capacity(len);
        for p in px.chunks_exact(3) {
            o.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        o
    };
    Ok(Image {
        width: w,
        height: h,
        rgba,
    })
}

/// WebP, still or animated (the decoder composes animation frames itself).
pub fn decode_webp(bytes: &[u8], limits: &Limits, quota: u64) -> Result<Decoded, ErrCode> {
    let mut d = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|_| ErrCode::Enodata)?;
    d.set_memory_limit(usize::try_from(limits.image_cap()).unwrap_or(usize::MAX));
    let (w, h) = d.dimensions();
    let len = checked_rgba_len(w, h, limits)?;
    if len as u64 > limits.image_cap() {
        return Err(ErrCode::Efbig);
    }
    let alpha = d.has_alpha();
    let out_len = d.output_buffer_size().ok_or(ErrCode::Efbig)?;
    let mut buf = vec![0u8; out_len];
    let expand = |b: &[u8]| -> Vec<u8> {
        if alpha {
            b.to_vec()
        } else {
            let mut o = Vec::with_capacity(len);
            for p in b.chunks_exact(3) {
                o.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            o
        }
    };
    let img = |rgba| Image {
        width: w,
        height: h,
        rgba,
    };
    if !d.is_animated() {
        d.read_image(&mut buf).map_err(|_| ErrCode::Enodata)?;
        return Ok(Decoded::still(img(expand(&buf))));
    }
    let mut frames = Frames::new(quota);
    for _ in 0..d.num_frames().min(MAX_FRAMES as u32) {
        let Ok(ms) = d.read_frame(&mut buf) else {
            break;
        };
        let gap = i32::try_from(ms).unwrap_or(i32::MAX).max(1);
        if !frames.push(img(expand(&buf)), gap) {
            break;
        }
    }
    frames.done()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_the_four_containers() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxxxx"), Some(Kind::Png));
        assert_eq!(sniff(b"GIF89a.."), Some(Kind::Gif));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Kind::Jpeg));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Kind::Webp));
        assert_eq!(sniff(b"BM"), None);
    }

    #[test]
    fn garbage_never_panics_and_is_an_error() {
        let l = Limits::default();
        for b in [
            &b"GIF89a\xff\xff\xff\xff"[..],
            &[0xFF, 0xD8, 0xFF, 0, 0, 0][..],
            &b"RIFF\x10\0\0\0WEBPVP8X"[..],
            &b"\x89PNG\r\n\x1a\n\0\0"[..],
        ] {
            assert!(decode_any(b, &l, 1 << 20).is_err());
        }
    }
}
