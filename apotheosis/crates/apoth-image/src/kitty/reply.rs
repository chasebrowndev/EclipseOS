// SPDX-License-Identifier: AGPL-3.0-only
//! Reply formatting and the per-pane reply queue (GRAPHICS-06, T-18 §5).
//!
//! Replies are built from ids (numbers) and the closed [`ErrCode`] alphabet only; no
//! attacker bytes are echoed.

use super::Ids;
use crate::{ErrCode, Limits};
use std::fmt::Write as _;

/// Format `ESC _ G i=..,I=..,p=..;OK|<ERR> ESC \`. `None` when the request gave no id or
/// number (the protocol sends no reply then), or when `quiet` mutes it
/// (`q=1` mutes OK, `q=2` mutes everything).
pub fn format_reply(ids: Ids, result: Result<(), ErrCode>, quiet: u8) -> Option<Vec<u8>> {
    if ids.id == 0 && ids.number == 0 {
        return None;
    }
    match (&result, quiet) {
        (_, 2) | (Ok(()), 1) => return None,
        _ => {}
    }
    let mut s = String::from("\x1b_G");
    let mut sep = "";
    for (key, v) in [("i", ids.id), ("I", ids.number), ("p", ids.placement)] {
        if v != 0 {
            let _ = write!(s, "{sep}{key}={v}");
            sep = ",";
        }
    }
    s.push(';');
    s.push_str(match result {
        Ok(()) => "OK",
        Err(e) => e.reply_text(),
    });
    s.push_str("\x1b\\");
    Some(s.into_bytes())
}

/// Bounded, rate-limited queue of replies awaiting the PTY write. Time is passed in so
/// the type stays deterministic.
#[derive(Debug)]
pub struct ReplyQueue {
    buf: Vec<u8>,
    cap: usize,
    tokens: u32,
    burst: u32,
    per_sec: u32,
    last_ms: u64,
    carry_ms: u64,
    dropped: u64,
}

impl ReplyQueue {
    pub fn new(limits: &Limits) -> Self {
        Self {
            buf: Vec::new(),
            cap: limits.reply_queue_bytes,
            tokens: limits.reply_burst,
            burst: limits.reply_burst,
            per_sec: limits.reply_per_sec,
            last_ms: 0,
            carry_ms: 0,
            dropped: 0,
        }
    }

    /// Queue a reply; returns false (and counts a drop) on rate limit or overflow.
    pub fn push(&mut self, reply: &[u8], now_ms: u64) -> bool {
        let dt = now_ms.saturating_sub(self.last_ms);
        self.last_ms = now_ms.max(self.last_ms);
        let add = (dt.saturating_add(self.carry_ms)).saturating_mul(u64::from(self.per_sec));
        self.carry_ms = add % 1000 / u64::from(self.per_sec.max(1));
        let add = u32::try_from(add / 1000).unwrap_or(u32::MAX);
        self.tokens = self.tokens.saturating_add(add).min(self.burst);
        if self.tokens == 0 || self.buf.len() + reply.len() > self.cap {
            self.dropped += 1;
            return false;
        }
        self.tokens -= 1;
        self.buf.extend_from_slice(reply);
        true
    }

    /// Take everything queued for the PTY.
    pub fn drain(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }

    /// Replies dropped to the rate limit or a full queue.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}
