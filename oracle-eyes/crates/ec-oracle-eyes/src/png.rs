// SPDX-License-Identifier: AGPL-3.0-only

//! Image mode's encoder: a frame as a PNG, base64 for the model's stream-json
//! input (`oracle-eyes/spec.md`, image mode).
//!
//! Hand-written because the crate carries no image dependency and the one
//! format needed is small. The deflate stream uses stored (uncompressed)
//! blocks: larger than a real encoder's output, but correct and short enough
//! to audit, and a downscaled frame is a few megabytes at most.

use crate::frame::Frame;

/// A frame as PNG, box-averaged down so its width is at most `max_width`.
/// Pixels are read as RGB with alpha dropped, the same as the OCR path.
pub fn frame_png(f: &Frame, max_width: usize) -> Vec<u8> {
    let step = (f.width as usize).div_ceil(max_width.max(1)).max(1);
    let w = (f.width as usize / step).max(1);
    let h = (f.height as usize / step).max(1);
    let stride = f.stride as usize;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 3];
            for dy in 0..step {
                for dx in 0..step {
                    let p = (y * step + dy) * stride + (x * step + dx) * 4;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += u32::from(f.pixels.get(p + c).copied().unwrap_or(0));
                    }
                }
            }
            let n = (step * step) as u32;
            rgb.extend(acc.iter().map(|a| (a / n) as u8));
        }
    }
    rgb_png(w as u32, h as u32, &rgb)
}

/// `rgb` is `width * height * 3` bytes, rows top to bottom.
pub fn rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row = width as usize * 3;
    // Each scanline is prefixed with filter type 0 (none).
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for r in rgb.chunks(row.max(1)).take(height as usize) {
        raw.push(0);
        raw.extend_from_slice(r);
    }

    // zlib: CMF/FLG (0x78 0x01 is a valid header), stored blocks, Adler-32.
    let mut z = vec![0x78, 0x01];
    if raw.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        z.push(u8::from(last));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 8-bit depth, colour type 2 (RGB), default compression, filter, no interlace.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Standard base64 with padding. The model's stream-json input is JSON, so
/// the image travels as text.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Region;

    #[test]
    fn checksums_match_the_published_test_vectors() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn base64_pads_like_rfc_4648() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn a_png_has_its_signature_and_chunks() {
        let png = rgb_png(2, 1, &[255, 0, 0, 0, 255, 0]);
        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        assert!(png.windows(4).any(|w| w == b"IHDR"));
        assert!(png.windows(4).any(|w| w == b"IDAT"));
        assert!(png.ends_with(&[0xAE, 0x42, 0x60, 0x82]), "IEND crc");
    }

    #[test]
    fn a_frame_is_downscaled_to_the_width_cap() {
        let f = Frame {
            width: 40,
            height: 20,
            stride: 160,
            pixels: vec![200; 160 * 20],
            origin: Region {
                x: 0,
                y: 0,
                w: 40,
                h: 20,
            },
        };
        // Width 40 capped at 10 is a step of 4: a 10x5 image, IHDR at byte 16.
        let png = frame_png(&f, 10);
        assert_eq!(&png[16..20], &10u32.to_be_bytes());
        assert_eq!(&png[20..24], &5u32.to_be_bytes());
    }
}
