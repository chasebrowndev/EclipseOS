// SPDX-License-Identifier: AGPL-3.0-only
//! One unit of decode work: read the medium (file, temp file, shm) if any, then decode to
//! RGBA. Built on the io thread, run on the worker pool, answered by token (T-15 §4,
//! PERFORMANCE-53: decode never runs on the io thread).

use super::{MediaPolicy, MediaResult, Medium, TransmitSpec, decode_transfer, load_media};
use crate::{ErrCode, Image, Limits};

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
}

/// What a job produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Image(Image),
    /// Reply with this error.
    Failed(ErrCode),
    /// Policy denied the medium: no effect, no reply.
    Denied,
}

#[derive(Debug)]
pub struct DecodeDone {
    pub token: u64,
    pub outcome: Outcome,
}

impl DecodeJob {
    /// Blocking: file reads and pixel decoding.
    pub fn run(self) -> DecodeDone {
        let token = self.token;
        let outcome = self.outcome();
        DecodeDone { token, outcome }
    }

    fn outcome(self) -> Outcome {
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
        match decode_transfer(&self.spec, data, &self.limits) {
            Ok(img) => Outcome::Image(img),
            Err(e) => Outcome::Failed(e),
        }
    }
}
