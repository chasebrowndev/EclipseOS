// SPDX-License-Identifier: Apache-2.0
//! Value types handed to a [`Sink`](crate::Sink): parameters, string heads, routes.

use crate::limits::{HEAD_LEN, MAX_PARAMS};

/// CSI/DCS numeric parameters (T-01 §2.3).
///
/// Empty parameters (`CSI ;m`) are distinguishable from `0` via [`Params::get`].
/// Values saturate at `u32::MAX`. At most [`MAX_PARAMS`] are stored; the rest set
/// [`Params::overflow`].
#[derive(Clone, Debug)]
pub struct Params {
    vals: [u32; MAX_PARAMS],
    len: usize,
    present: u64,
    colon: u64,
    overflow: bool,
    cur: u32,
    cur_present: bool,
    started: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

impl Params {
    /// An empty parameter list.
    pub const fn new() -> Self {
        Self {
            vals: [0; MAX_PARAMS],
            len: 0,
            present: 0,
            colon: 0,
            overflow: false,
            cur: 0,
            cur_present: false,
            started: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.len = 0;
        self.present = 0;
        self.colon = 0;
        self.overflow = false;
        self.cur = 0;
        self.cur_present = false;
        self.started = false;
    }

    /// Feed one parameter-area byte (`0-9`, `;`, `:`).
    pub(crate) fn feed(&mut self, b: u8) {
        self.started = true;
        match b {
            b'0'..=b'9' => {
                self.cur = self
                    .cur
                    .saturating_mul(10)
                    .saturating_add(u32::from(b - b'0'));
                self.cur_present = true;
            }
            b':' => self.separate(true),
            _ => self.separate(false),
        }
    }

    fn separate(&mut self, colon: bool) {
        if self.len < MAX_PARAMS {
            self.vals[self.len] = self.cur;
            if self.cur_present {
                self.present |= 1 << self.len;
            }
            if colon {
                self.colon |= 1 << self.len;
            }
            self.len += 1;
        } else {
            self.overflow = true;
        }
        self.cur = 0;
        self.cur_present = false;
    }

    /// Close the trailing parameter at dispatch.
    pub(crate) fn finish(&mut self) {
        if self.started {
            self.separate(false);
            self.started = false;
        }
    }

    /// Number of stored parameters (at most 64).
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the sequence carried no parameter bytes at all.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Parameter `i`; `None` if out of range or empty (`CSI ;m`).
    pub fn get(&self, i: usize) -> Option<u32> {
        if i < self.len && self.present >> i & 1 == 1 {
            Some(self.vals[i])
        } else {
            None
        }
    }

    /// Parameter `i`, with empty and out-of-range reading as `default`.
    pub fn get_or(&self, i: usize, default: u32) -> u32 {
        self.get(i).unwrap_or(default)
    }

    /// Stored values; empty parameters read `0` here, use [`Params::get`] to tell.
    pub fn values(&self) -> &[u32] {
        &self.vals[..self.len]
    }

    /// Iterate parameters, `None` for empty ones.
    pub fn iter(&self) -> impl Iterator<Item = Option<u32>> + '_ {
        (0..self.len).map(|i| self.get(i))
    }

    /// True if the separator after parameter `i` was `:`.
    pub fn colon_after(&self, i: usize) -> bool {
        i < self.len && self.colon >> i & 1 == 1
    }

    /// Bit `i` set when the separator after parameter `i` was `:`.
    pub fn colon_mask(&self) -> u64 {
        self.colon
    }

    /// Bit `i` set when parameter `i` was non-empty.
    pub fn present_mask(&self) -> u64 {
        self.present
    }

    /// More than 64 parameters were sent; the first 64 are still delivered.
    pub fn overflow(&self) -> bool {
        self.overflow
    }
}

/// Which string protocol a [`StrHead`] belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrKind {
    /// `ESC ]`
    Osc,
    /// `ESC P`
    Dcs,
    /// `ESC _`
    Apc,
    /// `ESC ^`
    Pm,
    /// `ESC X`
    Sos,
}

/// The routing head of a string, buffered in a fixed array (T-01 §3.1).
///
/// - OSC: `osc_command_digits()` (the ASCII digits before `;`) and `osc_selector()`
///   (bytes after the `;` up to the next `;`, `=` or `:`).
/// - DCS: `dcs_private()`, `params()`, `dcs_intermediates()`, `dcs_final()`.
/// - APC: `bytes()` is the first byte. PM/SOS: empty.
#[derive(Clone, Debug)]
pub struct StrHead {
    buf: [u8; HEAD_LEN],
    len: usize,
    split: usize,
    complete: bool,
    truncated: bool,
    params: Params,
}

impl Default for StrHead {
    fn default() -> Self {
        Self::new()
    }
}

impl StrHead {
    /// An empty head.
    pub const fn new() -> Self {
        Self {
            buf: [0; HEAD_LEN],
            len: 0,
            split: 0,
            complete: true,
            truncated: false,
            params: Params::new(),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.len = 0;
        self.split = 0;
        self.complete = true;
        self.truncated = false;
        self.params.clear();
    }

    pub(crate) fn buf_mut(&mut self) -> &mut [u8; HEAD_LEN] {
        &mut self.buf
    }

    pub(crate) fn set_len(&mut self, len: usize) {
        self.len = len;
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn set_split(&mut self, split: usize) {
        self.split = split;
    }

    pub(crate) fn split(&self) -> usize {
        self.split
    }

    pub(crate) fn set_complete(&mut self, complete: bool) {
        self.complete = complete;
    }

    pub(crate) fn set_truncated(&mut self) {
        self.truncated = true;
    }

    pub(crate) fn params_mut(&mut self) -> &mut Params {
        &mut self.params
    }

    pub(crate) fn raw(&self) -> &[u8; HEAD_LEN] {
        &self.buf
    }

    /// All head bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    /// False when the string ended or aborted before its head was complete.
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// True when the head hit the 32-byte limit and was cut.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// OSC: the command digits as ASCII.
    pub fn osc_command_digits(&self) -> &[u8] {
        &self.buf[..self.split.min(self.len)]
    }

    /// OSC: the numeric command, `None` if empty or beyond `u32`.
    pub fn osc_command(&self) -> Option<u32> {
        let d = self.osc_command_digits();
        if d.is_empty() {
            return None;
        }
        let mut v: u32 = 0;
        for &c in d {
            v = v.checked_mul(10)?.checked_add(u32::from(c - b'0'))?;
        }
        Some(v)
    }

    /// OSC: selector bytes after the command's `;`.
    pub fn osc_selector(&self) -> &[u8] {
        &self.buf[self.split.min(self.len)..self.len]
    }

    /// DCS: private marker byte, `0` if none.
    pub fn dcs_private(&self) -> u8 {
        if self.len > 0 { self.buf[0] } else { 0 }
    }

    /// DCS: intermediate bytes.
    pub fn dcs_intermediates(&self) -> &[u8] {
        if self.split >= 1 && self.split <= self.len {
            &self.buf[1..self.split]
        } else {
            &[]
        }
    }

    /// DCS: final byte, `None` if the header was aborted first.
    pub fn dcs_final(&self) -> Option<u8> {
        if self.complete && self.split < self.len {
            Some(self.buf[self.split])
        } else {
            None
        }
    }

    /// DCS: header parameters.
    pub fn params(&self) -> &Params {
        &self.params
    }
}

/// The sink's answer to `string_start`: the most payload bytes it will accept.
/// `cap == 0` skips the string with no memory (unknown protocol, PM, SOS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrRoute {
    /// Maximum payload bytes, counted from the first byte delivered by `string_put`.
    pub cap: u32,
}

impl StrRoute {
    /// Skip the string.
    pub const SKIP: Self = Self { cap: 0 };
}

/// How a string ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrEnd {
    /// Terminated by `ESC \`.
    St,
    /// Terminated by BEL (OSC only).
    Bel,
    /// Aborted by CAN, SUB or a new ESC sequence.
    Aborted,
    /// Payload exceeded the route's cap; the remainder was skipped.
    OverCap,
    /// The route had `cap == 0`; reported immediately, the string is skipped silently.
    Rejected,
}

impl StrEnd {
    /// True for a clean termination (ST or BEL).
    pub fn is_ok(self) -> bool {
        matches!(self, Self::St | Self::Bel)
    }
}
