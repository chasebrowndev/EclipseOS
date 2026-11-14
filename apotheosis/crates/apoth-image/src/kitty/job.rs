// SPDX-License-Identifier: AGPL-3.0-only
//! One unit of decode work: read the medium (file, temp file, shm) if any, then decode to
//! RGBA; for an animation frame (`a=f`) also compose it onto its canvas; for an iTerm2
//! inline image decode the container. Built on the io thread, run on the worker pool,
//! answered by token (T-15 §4, §5; PERFORMANCE-53: decode never runs on the io thread).
//! The content hash the store shares blobs by is computed here too.

use std::sync::Arc;

use super::{MediaPolicy, MediaResult, Medium, TransmitSpec, decode_transfer_with, load_media};
use crate::anim::{composite, diff_bbox, rect_inside};
use crate::decoder::{InProcess, PixelDecoder};
use crate::formats::Decoded;
use crate::iterm::{ITermError, ITermFile, decode_payload_with};
use crate::{ErrCode, Image, Limits};

/// The canvas an `a=f` frame is drawn on.
#[derive(Debug, Clone)]
pub enum Canvas {
    /// An existing frame (`c=` base, or the frame `r=` edits).
    Frame(Arc<Image>),
    /// A fresh frame of the root's size filled with `Y=` (RGBA; transparent when absent).
    Fill {
        width: u32,
        height: u32,
        rgba: [u8; 4],
    },
}

/// Composition half of an `a=f` job.
#[derive(Debug, Clone)]
pub struct FrameJob {
    pub canvas: Canvas,
    /// Where the transmitted rectangle lands.
    pub x: u32,
    pub y: u32,
    /// `X=1`: overwrite instead of alpha blending.
    pub overwrite: bool,
    /// The frame shown before this one when appending (the change rect is diffed against
    /// it); `None` when editing a frame in place (the change rect is the sent rect).
    pub prev: Option<Arc<Image>>,
}

/// What the worker does with the bytes.
#[derive(Debug, Clone, Default)]
pub enum Work {
    /// A kitty transmission (`a=t`, `a=T`, `a=q`).
    #[default]
    Kitty,
    /// A kitty frame load (`a=f`).
    Frame(FrameJob),
    /// An iTerm2 `File=` payload (base64 in `payload`).
    ITerm(ITermFile),
}

/// A completed transfer waiting for a worker.
#[derive(Debug)]
pub struct DecodeJob {
    /// Matches the answer to the request.
    pub token: u64,
    pub spec: TransmitSpec,
    /// Pixel bytes for `t=d`; the path or shm name for the other media.
    pub payload: Vec<u8>,
    /// Required for file-type media: the already-resolved policy decision and quota.
    pub policy: Option<MediaPolicy>,
    pub limits: Limits,
    pub work: Work,
}

/// What a job produced.
#[derive(Debug)]
pub enum Outcome {
    /// A still image and its content hash.
    Image(Image, u64),
    /// A composed animation frame and the rect that differs from the frame it follows
    /// (or the edited rect); `None` = all of it.
    Frame(Image, Option<[u32; 4]>),
    /// An iTerm2 container: its frames (a still image has one).
    Decoded(Decoded),
    /// Reply with this error.
    Failed(ErrCode),
    /// Policy denied the medium (or iTerm2 refused the file): no effect, no reply.
    Denied,
}

#[derive(Debug)]
pub struct DecodeDone {
    pub token: u64,
    pub outcome: Outcome,
}

impl DecodeJob {
    /// Blocking: file reads, pixel decoding, frame composition.
    pub fn run(self) -> DecodeDone {
        self.run_with(&mut InProcess)
    }

    /// [`DecodeJob::run`] with container decoding done by `dec` (the sandboxed helper).
    pub fn run_with(self, dec: &mut dyn PixelDecoder) -> DecodeDone {
        let token = self.token;
        let outcome = self.outcome(dec);
        DecodeDone { token, outcome }
    }

    fn outcome(self, dec: &mut dyn PixelDecoder) -> Outcome {
        if let Work::ITerm(file) = &self.work {
            return match decode_payload_with(file, &self.payload, &self.limits, dec) {
                Ok(d) => Outcome::Decoded(d),
                Err(ITermError::TooLarge) => Outcome::Failed(ErrCode::Efbig),
                Err(_) => Outcome::Failed(ErrCode::Einval),
            };
        }
        let data = if self.spec.medium == Medium::Direct {
            self.payload
        } else {
            let Some(policy) = self.policy.as_ref() else {
                return Outcome::Denied;
            };
            match load_media(
                self.spec.medium,
                &self.payload,
                self.spec.size,
                self.spec.offset,
                policy,
                &self.limits,
            ) {
                MediaResult::Data(d) => d,
                MediaResult::Denied => return Outcome::Denied,
                MediaResult::Failed => return Outcome::Failed(ErrCode::EnoentFile),
            }
        };
        let img = match decode_transfer_with(&self.spec, data, &self.limits, dec) {
            Ok(img) => img,
            Err(e) => return Outcome::Failed(e),
        };
        match self.work {
            Work::Frame(f) => match compose(f, &img) {
                Ok((out, delta)) => Outcome::Frame(out, delta),
                Err(e) => Outcome::Failed(e),
            },
            _ => {
                let h = img.content_hash();
                Outcome::Image(img, h)
            }
        }
    }
}

/// Draw `piece` onto the frame's canvas.
fn compose(f: FrameJob, piece: &Image) -> Result<(Image, Option<[u32; 4]>), ErrCode> {
    let mut canvas = match f.canvas {
        Canvas::Frame(img) => Arc::unwrap_or_clone(img),
        Canvas::Fill {
            width,
            height,
            rgba,
        } => {
            let n = width as usize * height as usize;
            Image {
                width,
                height,
                rgba: rgba.repeat(n),
            }
        }
    };
    if !rect_inside(
        f.x,
        f.y,
        piece.width,
        piece.height,
        canvas.width,
        canvas.height,
    ) {
        return Err(ErrCode::Einval);
    }
    composite(
        &mut canvas,
        piece,
        0,
        0,
        f.x,
        f.y,
        piece.width,
        piece.height,
        f.overwrite,
    );
    let delta = match &f.prev {
        Some(prev) => diff_bbox(prev, &canvas),
        None => Some([f.x, f.y, piece.width, piece.height]),
    };
    Ok((canvas, delta))
}
