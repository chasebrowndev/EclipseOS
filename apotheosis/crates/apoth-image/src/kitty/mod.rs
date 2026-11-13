// SPDX-License-Identifier: AGPL-3.0-only
//! Kitty graphics protocol (T-15 §1): command model, chunk assembly, replies, media
//! loading and pixel decoding. Pure; the grid wiring is M25 proper.

mod chunk;
mod cmd;
mod decode;
mod media;
mod reply;

pub(crate) use chunk::ENGINE;
pub use chunk::{Assembler, Event, Failure, Transfer};
pub use cmd::*;
pub use decode::{decode_png, decode_transfer};
pub use media::{Decision, MediaPolicy, MediaResult, load_media};
pub use reply::{ReplyQueue, format_reply};
