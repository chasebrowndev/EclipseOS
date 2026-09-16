// SPDX-License-Identifier: AGPL-3.0-only

//! The one pixel type every stage agrees on.

/// A rectangle in compositor-logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Tightly packed RGBA8, `stride` bytes per row, top row first.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixels: Vec<u8>,
    /// Where on screen these pixels came from, so OCR boxes can be mapped
    /// back to an anchor the compositor understands.
    pub origin: Region,
}
