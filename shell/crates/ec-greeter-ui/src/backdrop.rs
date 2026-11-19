// SPDX-License-Identifier: AGPL-3.0-only
//! The wallpaper behind the panes, and the blurred copy of it that the
//! fallback panes show through.
//!
//! With glass the compositor draws the ground (blur included) under this
//! window, so the screen draws no wallpaper of its own: an opaque image would
//! hide the glass. Without glass the screen draws the wallpaper full-bleed and
//! each pane shows a blurred, tinted slice of it: the same look, painted here.
//!
//! The blur is taken once, at load, on a small copy: downscale, soften, and let
//! the renderer's linear filter scale it back up. It costs nothing per frame.

use std::fs;
use std::path::Path;

use iced::widget::image::Handle;
use iced::Rectangle;

use crate::tokens::backdrop as tk;

/// A decoded wallpaper: the sharp image and its blurred copy.
#[derive(Clone)]
pub struct Backdrop {
    pub sharp: Handle,
    pub frost: Handle,
    w: u32,
    h: u32,
}

impl std::fmt::Debug for Backdrop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backdrop")
            .field("w", &self.w)
            .field("h", &self.h)
            .finish()
    }
}

impl Backdrop {
    /// Read and decode a PNG. `None` for anything that is not a plain file, is
    /// too large, or does not decode: the screen then has no wallpaper, and
    /// that is all it has lost.
    pub fn load(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > tk::MAX_BYTES {
            return None;
        }
        Self::decode(&fs::read(path).ok()?)
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
        let (w, h) = (img.width(), img.height());
        if w == 0 || h == 0 || w > tk::MAX_SIDE || h > tk::MAX_SIDE {
            return None;
        }
        let small_w = (w / tk::FROST_DIV).max(tk::FROST_MIN);
        let small_h = ((u64::from(h) * u64::from(small_w)) / u64::from(w)).max(1) as u32;
        let small = image::imageops::resize(
            &img.to_rgba8(),
            small_w,
            small_h,
            image::imageops::FilterType::Triangle,
        );
        let soft = image::imageops::blur(&small, tk::FROST_SIGMA);
        let frost = Handle::from_rgba(soft.width(), soft.height(), soft.into_raw());
        let sharp = Handle::from_rgba(w, h, img.to_rgba8().into_raw());
        Some(Self { sharp, frost, w, h })
    }

    /// Where the image sits in a window of `view`: scaled to cover it, centred.
    /// The same box the sharp image fills, so a pane's slice lines up with it.
    pub fn cover(&self, view: Rectangle) -> Rectangle {
        let (iw, ih) = (self.w.max(1) as f32, self.h.max(1) as f32);
        let k = (view.width / iw).max(view.height / ih);
        let (w, h) = (iw * k, ih * k);
        Rectangle {
            x: view.x + (view.width - w) / 2.0,
            y: view.y + (view.height - h) / 2.0,
            width: w,
            height: h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bd(w: u32, h: u32) -> Backdrop {
        Backdrop {
            sharp: Handle::from_rgba(1, 1, vec![0; 4]),
            frost: Handle::from_rgba(1, 1, vec![0; 4]),
            w,
            h,
        }
    }

    #[test]
    fn cover_fills_the_window_and_centres() {
        let view = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 1280.0,
            height: 720.0,
        };
        let c = bd(2000, 1000).cover(view);
        assert!(c.width >= 1280.0 && c.height >= 720.0);
        assert!((c.x + c.width / 2.0 - 640.0).abs() < 0.01);
        assert!((c.y + c.height / 2.0 - 360.0).abs() < 0.01);
    }

    #[test]
    fn an_empty_window_does_not_panic() {
        let _ = bd(0, 0).cover(Rectangle {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
    }

    #[test]
    fn garbage_does_not_decode() {
        assert!(Backdrop::decode(b"not a png").is_none());
        assert!(Backdrop::load(Path::new("/nonexistent/wallpaper.png")).is_none());
    }
}
