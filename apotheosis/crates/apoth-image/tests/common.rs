// SPDX-License-Identifier: AGPL-3.0-only
#![allow(dead_code)]
use base64::Engine as _;

pub(crate) fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Encode an RGBA image as PNG with the png crate's encoder.
pub(crate) fn png_rgba(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let mut wr = e.write_header().unwrap();
    wr.write_image_data(rgba).unwrap();
    wr.finish().unwrap();
    out
}

pub(crate) fn apc(keys: &str, payload: &str) -> Vec<u8> {
    format!("G{keys};{payload}").into_bytes()
}
