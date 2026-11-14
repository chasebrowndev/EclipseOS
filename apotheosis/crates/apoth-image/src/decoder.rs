// SPDX-License-Identifier: AGPL-3.0-only
//! Where untrusted pixel bytes are decoded (T-15 §5). [`InProcess`] decodes on the calling
//! worker; the terminal substitutes the sandboxed media helper (`apoth-media`) when
//! `graphics.sandbox-decode` is on. Raw RGB/RGBA transfers and inflate stay in process:
//! they are copies under size caps, not parsers of a container.

use crate::formats::{Decoded, decode_any};
use crate::kitty::decode_png;
use crate::{ErrCode, Image, Limits};

pub trait PixelDecoder {
    /// A still PNG (kitty `f=100`).
    fn png(&mut self, bytes: &[u8], limits: &Limits) -> Result<Image, ErrCode>;
    /// Any supported container, every frame (iTerm2, `apoth icat`).
    fn any(&mut self, bytes: &[u8], limits: &Limits, anim_quota: u64) -> Result<Decoded, ErrCode>;
}

/// Decode on the calling thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct InProcess;

impl PixelDecoder for InProcess {
    fn png(&mut self, bytes: &[u8], limits: &Limits) -> Result<Image, ErrCode> {
        decode_png(bytes, limits)
    }
    fn any(&mut self, bytes: &[u8], limits: &Limits, anim_quota: u64) -> Result<Decoded, ErrCode> {
        decode_any(bytes, limits, anim_quota)
    }
}
