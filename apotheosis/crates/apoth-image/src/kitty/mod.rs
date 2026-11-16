// SPDX-License-Identifier: AGPL-3.0-only
//! Kitty graphics protocol (T-15 §1): command model, chunk assembly, replies, media
//! loading, pixel decoding and the decode job the worker pool runs. Pure: no grid, no GPU.

mod chunk;
mod cmd;
mod decode;
mod job;
mod media;
mod reply;

pub(crate) use chunk::ENGINE;
pub use chunk::{Assembler, Event, Failure, Transfer};
pub use cmd::*;
pub use decode::{decode_png, decode_transfer, decode_transfer_with};
pub use job::{Canvas, DecodeDone, DecodeJob, FrameJob, Outcome, Work};
pub use media::{Decision, MediaPolicy, MediaResult, load_media};
pub use reply::{ReplyQueue, format_reply};
