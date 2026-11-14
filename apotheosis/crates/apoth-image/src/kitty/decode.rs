// SPDX-License-Identifier: AGPL-3.0-only
//! Pixel decoding of a completed kitty transfer (GRAPHICS-04, -33, -44).
//!
//! Sizes are checked before any allocation; `o=z` inflates through a bounded pure-Rust
//! inflater. This is blocking CPU work: call it on the worker pool, never the io thread
//! (PERFORMANCE-53).

use super::{Format, TransmitSpec};
use crate::{ErrCode, Image, Limits, checked_rgba_len};
use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_zlib_with_limit};
use std::io::Cursor;

fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, ErrCode> {
    decompress_to_vec_zlib_with_limit(data, limit).map_err(|e| {
        if e.status == TINFLStatus::HasMoreOutput {
            ErrCode::Efbig
        } else {
            ErrCode::Enodata
        }
    })
}

/// Decode the bytes of a transfer (direct payload, or bytes read from file/shm) to RGBA.
pub fn decode_transfer(
    spec: &TransmitSpec,
    data: Vec<u8>,
    limits: &Limits,
) -> Result<Image, ErrCode> {
    let cap = usize::try_from(limits.image_cap()).unwrap_or(usize::MAX);
    match spec.format {
        Format::Png => {
            let raw = if spec.compressed {
                inflate(&data, cap)?
            } else {
                data
            };
            decode_png(&raw, limits)
        }
        Format::Rgb24 | Format::Rgba32 => {
            let bpp: usize = if spec.format == Format::Rgb24 { 3 } else { 4 };
            let rgba_len = checked_rgba_len(spec.width, spec.height, limits)?;
            if rgba_len as u64 > limits.image_cap() {
                return Err(ErrCode::Efbig);
            }
            let expected = spec.width as usize * spec.height as usize * bpp;
            let raw = if spec.compressed {
                inflate(&data, expected + 1)?
            } else {
                data
            };
            if raw.len() != expected {
                return Err(ErrCode::Enodata);
            }
            let rgba = if bpp == 4 {
                raw
            } else {
                let mut out = Vec::with_capacity(rgba_len);
                for px in raw.chunks_exact(3) {
                    out.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                out
            };
            Ok(Image {
                width: spec.width,
                height: spec.height,
                rgba,
            })
        }
    }
}

/// Decode a PNG to RGBA8 under the limits. Header dimensions are checked before the pixel
/// buffer is allocated.
pub fn decode_png(bytes: &[u8], limits: &Limits) -> Result<Image, ErrCode> {
    let cap = usize::try_from(limits.image_cap()).unwrap_or(usize::MAX);
    let mut dec = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: cap });
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|_| ErrCode::Ebadpng)?;
    let (w, h) = {
        let i = reader.info();
        (i.width, i.height)
    };
    let rgba_len = checked_rgba_len(w, h, limits)?;
    if rgba_len as u64 > limits.image_cap() {
        return Err(ErrCode::Efbig);
    }
    let out_len = reader.output_buffer_size().ok_or(ErrCode::Efbig)?;
    if out_len > rgba_len {
        return Err(ErrCode::Efbig);
    }
    let mut buf = vec![0u8; out_len];
    let info = reader.next_frame(&mut buf).map_err(|_| ErrCode::Ebadpng)?;
    let px = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => {
            let mut o = Vec::with_capacity(rgba_len);
            for p in px.chunks_exact(3) {
                o.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            o
        }
        png::ColorType::Grayscale => {
            let mut o = Vec::with_capacity(rgba_len);
            for &g in px {
                o.extend_from_slice(&[g, g, g, 255]);
            }
            o
        }
        png::ColorType::GrayscaleAlpha => {
            let mut o = Vec::with_capacity(rgba_len);
            for p in px.chunks_exact(2) {
                o.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            o
        }
        png::ColorType::Indexed => return Err(ErrCode::Ebadpng),
    };
    if rgba.len() != rgba_len {
        return Err(ErrCode::Ebadpng);
    }
    Ok(Image {
        width: w,
        height: h,
        rgba,
    })
}
