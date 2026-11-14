// SPDX-License-Identifier: AGPL-3.0-only
//! Kitty graphics protocol, sixel, iTerm2 inline images, the per-pane image store and quotas.
//!
//! Governing spec: T-15 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). This is the pure, GPU-free
//! part: parsers, decoders, the store and its quotas. Placement into the grid lives in
//! `apoth-term` (`graphics`), drawing in `apoth-render` (`image`). Nothing here logs payload
//! or path content.
//!
//! Fuzz targets: `apoth_term::fuzz::fuzz_kitty_apc` and `fuzz_sixel` drive these parsers
//! through the terminal (with seed corpora under `apoth-term/corpus/`); both must never panic
//! and never exceed the configured [`Limits`].
#![forbid(unsafe_code)]

pub mod anim;
pub mod decoder;
pub mod error;
pub mod formats;
pub mod iterm;
pub mod kitty;
pub mod limits;
pub mod placeholder;
pub mod sixel;
pub mod store;

pub use error::ErrCode;
pub use limits::Limits;

/// A decoded image: straight (non-premultiplied) RGBA8, row-major, no padding.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

impl Image {
    /// Byte size of the pixel data, charged against the quota.
    pub fn bytes(&self) -> u64 {
        self.rgba.len() as u64
    }

    /// Content hash for sharing identical images (PERFORMANCE-52); only a bucket key,
    /// equality is checked bytewise. Eight bytes per step so a video frame hashes in well
    /// under a millisecond on the worker that decoded it.
    pub fn content_hash(&self) -> u64 {
        const K: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut h = (u64::from(self.width) << 32 | u64::from(self.height)) ^ K;
        let mut chunks = self.rgba.chunks_exact(8);
        for c in &mut chunks {
            let v = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
            h = (h ^ v).wrapping_mul(K).rotate_left(29);
        }
        for &b in chunks.remainder() {
            h = (h ^ u64::from(b)).wrapping_mul(K);
        }
        h ^ (h >> 31)
    }

    /// Pixel-row ranges of the image cut into terminal cell rows (`cell_h` physical px).
    /// Sixel images are stored this way so scroll/erase/overwrite is slice bookkeeping.
    pub fn cell_row_slices(&self, cell_h: u32) -> Vec<std::ops::Range<u32>> {
        let cell_h = cell_h.max(1);
        let mut out = Vec::new();
        let mut y = 0;
        while y < self.height {
            let end = y.saturating_add(cell_h).min(self.height);
            out.push(y..end);
            y = end;
        }
        out
    }
}

/// Checked RGBA size of a `w` x `h` image against the per-axis and decoded-size caps.
/// Runs before any allocation (T-15 §4, T-18 §5).
pub fn checked_rgba_len(w: u32, h: u32, limits: &Limits) -> Result<usize, ErrCode> {
    if w == 0 || h == 0 || w > limits.max_axis || h > limits.max_axis {
        return Err(ErrCode::Einval);
    }
    let bytes = u64::from(w) * u64::from(h) * 4;
    if bytes > limits.max_decoded_bytes {
        return Err(ErrCode::Efbig);
    }
    usize::try_from(bytes).map_err(|_| ErrCode::Efbig)
}
