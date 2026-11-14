// SPDX-License-Identifier: AGPL-3.0-only
//! iTerm2 inline images, OSC 1337 `File=` (T-15 §3, GRAPHICS-21). Parsing only; the result
//! feeds the shared store under the image quota.
//!
//! `inline=1` only: a missing or `inline=0` argument is refused (silently; no reply
//! channel exists for OSC 1337). PNG/APNG, GIF, JPEG and WebP decode through
//! [`crate::formats`]; any other payload is `Unsupported`. Multipart transfers
//! (`MultipartFile`/`FilePart`/`FileEnd`, GRAPHICS-22, a "could") are not implemented.

use crate::formats::{Decoded, decode_any, sniff};
use crate::kitty::ENGINE;
use crate::{ErrCode, Image, Limits};
use base64::Engine as _;

/// A `width=` / `height=` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    Auto,
    Cells(u32),
    Px(u32),
    Percent(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ITermFile {
    /// Decoded `name=` (display only; control characters stripped, at most 255 bytes).
    pub name: Option<String>,
    /// Declared byte size, if any; verified against the payload.
    pub size: Option<u64>,
    pub width: Dim,
    pub height: Dim,
    /// `preserveAspectRatio` (default true).
    pub preserve_aspect: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ITermError {
    /// Not a `File=` sequence.
    NotFile,
    /// `inline=1` absent (download request) -- refused.
    InlineRefused,
    BadArgs,
    TooLarge,
    BadData,
    Unsupported,
}

fn dim(v: &str) -> Option<Dim> {
    if v.is_empty() || v == "auto" {
        return Some(Dim::Auto);
    }
    let (digits, ctor): (&str, fn(u32) -> Dim) = if let Some(d) = v.strip_suffix("px") {
        (d, Dim::Px)
    } else if let Some(d) = v.strip_suffix('%') {
        (d, Dim::Percent)
    } else {
        (v, Dim::Cells)
    };
    if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(ctor(digits.parse().ok()?))
}

/// Parse the OSC 1337 body (the bytes after `1337;`, starting with `File=`).
/// Returns the arguments and the base64 payload slice.
pub fn parse_file(body: &[u8]) -> Result<(ITermFile, &[u8]), ITermError> {
    let rest = body.strip_prefix(b"File=").ok_or(ITermError::NotFile)?;
    let colon = rest
        .iter()
        .position(|&b| b == b':')
        .ok_or(ITermError::BadArgs)?;
    let args = std::str::from_utf8(&rest[..colon]).map_err(|_| ITermError::BadArgs)?;
    let data = &rest[colon + 1..];
    let mut f = ITermFile {
        name: None,
        size: None,
        width: Dim::Auto,
        height: Dim::Auto,
        preserve_aspect: true,
    };
    let mut inline = false;
    for kv in args.split(';').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').ok_or(ITermError::BadArgs)?;
        match k {
            "inline" => inline = v == "1",
            "size" => f.size = Some(v.parse().map_err(|_| ITermError::BadArgs)?),
            "width" => f.width = dim(v).ok_or(ITermError::BadArgs)?,
            "height" => f.height = dim(v).ok_or(ITermError::BadArgs)?,
            "preserveAspectRatio" => f.preserve_aspect = v != "0",
            "name" => {
                if let Ok(raw) = ENGINE.decode(v.as_bytes()) {
                    let s: String = String::from_utf8_lossy(&raw)
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect();
                    let mut s = s;
                    while s.len() > 255 {
                        s.pop();
                    }
                    f.name = Some(s);
                }
            }
            _ => {}
        }
    }
    if !inline {
        return Err(ITermError::InlineRefused);
    }
    Ok((f, data))
}

/// Decode the payload of a parsed file under the limits: PNG/APNG, GIF, JPEG or WebP
/// (animated containers give every frame). Declared and estimated sizes are checked before
/// the base64 decode allocates.
pub fn decode_payload(
    file: &ITermFile,
    b64: &[u8],
    limits: &Limits,
) -> Result<Decoded, ITermError> {
    let est = (b64.len() / 4 + 1) as u64 * 3;
    if b64.len() > limits.max_transfer_b64
        || est > limits.image_cap().saturating_add(3)
        || file.size.is_some_and(|s| s > limits.image_cap())
    {
        return Err(ITermError::TooLarge);
    }
    let raw = ENGINE.decode(b64).map_err(|_| ITermError::BadData)?;
    if file.size.is_some_and(|s| s != raw.len() as u64) {
        return Err(ITermError::BadData);
    }
    if sniff(&raw).is_none() {
        return Err(ITermError::Unsupported);
    }
    decode_any(&raw, limits, limits.animation_quota).map_err(|e| match e {
        ErrCode::Efbig => ITermError::TooLarge,
        _ => ITermError::BadData,
    })
}

/// [`decode_payload`], first frame only.
pub fn decode_file(file: &ITermFile, b64: &[u8], limits: &Limits) -> Result<Image, ITermError> {
    let d = decode_payload(file, b64, limits)?;
    d.frames
        .into_iter()
        .next()
        .map(|(img, _)| img)
        .ok_or(ITermError::BadData)
}

/// The cell box of a file image `iw` x `ih` px on a `screen` (cols, rows) of `cell` px:
/// `width`/`height` in cells, pixels or percent of the screen; `auto` derives from the
/// image (and from the other side when `preserveAspectRatio`). Returns (cols, rows), both
/// at least 1 and at most the screen's size for percent values.
pub fn cell_box(
    f: &ITermFile,
    iw: u32,
    ih: u32,
    cell: (u32, u32),
    screen: (u32, u32),
) -> (u32, u32) {
    let (cw, ch) = (u64::from(cell.0.max(1)), u64::from(cell.1.max(1)));
    let px = |d: Dim, scr: u32, c: u64| -> Option<u64> {
        match d {
            Dim::Auto => None,
            Dim::Cells(n) => Some(u64::from(n) * c),
            Dim::Px(n) => Some(u64::from(n)),
            Dim::Percent(p) => Some(u64::from(scr) * c * u64::from(p.min(100)) / 100),
        }
    };
    let (iw64, ih64) = (u64::from(iw.max(1)), u64::from(ih.max(1)));
    let (w, h) = match (px(f.width, screen.0, cw), px(f.height, screen.1, ch)) {
        (None, None) => (iw64, ih64),
        (Some(w), None) if f.preserve_aspect => (w, (w * ih64 / iw64).max(1)),
        (Some(w), None) => (w, ih64),
        (None, Some(h)) if f.preserve_aspect => ((h * iw64 / ih64).max(1), h),
        (None, Some(h)) => (iw64, h),
        (Some(w), Some(h)) => (w, h),
    };
    let cols = w.div_ceil(cw).clamp(1, 10_000) as u32;
    let rows = h.div_ceil(ch).clamp(1, 10_000) as u32;
    (cols, rows)
}
