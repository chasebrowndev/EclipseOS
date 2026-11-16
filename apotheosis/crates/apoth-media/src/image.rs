// SPDX-License-Identifier: AGPL-3.0-only
//! Image decoding in the sandboxed helper (T-15 §5, T-16 §4): a [`PixelDecoder`] that
//! sends the container bytes to an image-mode helper and checks what comes back against
//! the same limits the in-process decoder would have applied. One helper serves one owner
//! (a pane), so nothing a hostile file does to a helper reaches another pane's images. It
//! fails closed: without a working sandbox nothing is decoded.

use std::path::PathBuf;

use apoth_image::decoder::PixelDecoder;
use apoth_image::formats::Decoded;
use apoth_image::{ErrCode, Image, Limits};

use crate::client::{Helper, Mode};
use crate::helper::ERR_CODES;
use crate::wire::{self, FromHelper, ImageKind, ToHelper};

/// How long one decode may take before the helper is killed.
const DECODE_MS: i32 = 30_000;

#[derive(Debug, Default)]
pub struct Sandboxed {
    helper: Option<Helper>,
    /// The helper binary (tests); `None` finds it next to the executable.
    path: Option<PathBuf>,
    test_args: Vec<String>,
}

impl Sandboxed {
    pub fn new() -> Sandboxed {
        Sandboxed::default()
    }

    /// Tests: an explicit helper binary and sandbox knobs.
    pub fn with_helper(path: PathBuf, test_args: Vec<String>) -> Sandboxed {
        Sandboxed {
            helper: None,
            path: Some(path),
            test_args,
        }
    }

    fn helper(&mut self) -> Option<&mut Helper> {
        if self.helper.is_none() {
            let args: Vec<&str> = self.test_args.iter().map(String::as_str).collect();
            let h = match &self.path {
                Some(p) => Helper::start_at(p, Mode::Image, &args, wire::MAX_MESSAGE),
                None => Helper::start(Mode::Image, &args, wire::MAX_MESSAGE),
            };
            self.helper = h.ok();
        }
        self.helper.as_mut()
    }

    fn call(
        &mut self,
        kind: ImageKind,
        bytes: &[u8],
        limits: &Limits,
        anim_quota: u64,
    ) -> Result<Vec<(Image, i32)>, ErrCode> {
        if bytes.len() > wire::MAX_MESSAGE - 64 {
            return Err(ErrCode::Efbig);
        }
        let req = ToHelper::Image {
            kind,
            max_axis: limits.max_axis,
            max_decoded: limits.image_cap(),
            anim_quota,
            data: bytes.to_vec(),
        };
        let r = self.exchange(&req);
        match r {
            Some(FromHelper::ImageOk(frames)) => {
                let ok = check_frames(&frames, kind, limits, anim_quota);
                if !ok {
                    self.helper = None;
                    return Err(ErrCode::Einval);
                }
                Ok(frames
                    .into_iter()
                    .map(|f| {
                        (
                            Image {
                                width: f.width,
                                height: f.height,
                                rgba: f.rgba,
                            },
                            f.gap_ms,
                        )
                    })
                    .collect())
            }
            Some(FromHelper::ImageErr(i)) => Err(ERR_CODES
                .get(usize::from(i))
                .copied()
                .unwrap_or(ErrCode::Einval)),
            _ => {
                // Unavailable, gone, slow or off-protocol: drop it (a fresh one next time).
                self.helper = None;
                Err(ErrCode::Einval)
            }
        }
    }

    fn exchange(&mut self, req: &ToHelper) -> Option<FromHelper> {
        let h = self.helper()?;
        h.tx.send(&req.encode(), None).ok()?;
        if !h.rx.readable(DECODE_MS) {
            return None;
        }
        let (tag, body) = h.rx.recv().ok()??;
        FromHelper::decode(tag, &body).ok()
    }
}

/// What the helper returned obeys the limits the request named.
fn check_frames(
    frames: &[wire::ImageFrame],
    kind: ImageKind,
    limits: &Limits,
    anim_quota: u64,
) -> bool {
    if frames.is_empty() || (kind == ImageKind::Png && frames.len() != 1) {
        return false;
    }
    let mut rest = 0u64;
    for (i, f) in frames.iter().enumerate() {
        let bytes = u64::from(f.width) * u64::from(f.height) * 4;
        if f.width == 0
            || f.height == 0
            || f.width > limits.max_axis
            || f.height > limits.max_axis
            || bytes != f.rgba.len() as u64
            || bytes > limits.image_cap()
            || f.gap_ms < 0
        {
            return false;
        }
        if i > 0 {
            rest += bytes;
        }
    }
    rest <= anim_quota
}

impl PixelDecoder for Sandboxed {
    fn png(&mut self, bytes: &[u8], limits: &Limits) -> Result<Image, ErrCode> {
        let mut f = self.call(ImageKind::Png, bytes, limits, 0)?;
        Ok(f.swap_remove(0).0)
    }

    fn any(&mut self, bytes: &[u8], limits: &Limits, anim_quota: u64) -> Result<Decoded, ErrCode> {
        let frames = self.call(ImageKind::Any, bytes, limits, anim_quota)?;
        Ok(Decoded { frames })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32) -> wire::ImageFrame {
        wire::ImageFrame {
            width: w,
            height: h,
            gap_ms: 0,
            rgba: vec![0; (w * h * 4) as usize],
        }
    }

    #[test]
    fn results_past_the_limits_are_refused() {
        let l = Limits {
            max_axis: 8,
            ..Limits::default()
        };
        assert!(check_frames(&[frame(8, 8)], ImageKind::Png, &l, 0));
        assert!(!check_frames(&[frame(9, 1)], ImageKind::Png, &l, 0));
        assert!(!check_frames(&[], ImageKind::Any, &l, 0));
        assert!(!check_frames(
            &[frame(1, 1), frame(1, 1)],
            ImageKind::Png,
            &l,
            1 << 20
        ));
        // Animation frames past the quota.
        assert!(!check_frames(
            &[frame(4, 4), frame(4, 4)],
            ImageKind::Any,
            &l,
            63
        ));
        assert!(check_frames(
            &[frame(4, 4), frame(4, 4)],
            ImageKind::Any,
            &l,
            64
        ));
        let mut lying = frame(2, 2);
        lying.rgba.pop();
        assert!(!check_frames(&[lying], ImageKind::Any, &l, 0));
    }
}
