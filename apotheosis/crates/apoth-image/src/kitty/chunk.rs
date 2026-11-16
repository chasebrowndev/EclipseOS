// SPDX-License-Identifier: AGPL-3.0-only
//! Chunked-transfer assembly (GRAPHICS-03, -42, -43).
//!
//! One APC at a time goes in; completed transfers come out with the payload already
//! base64-decoded incrementally (alphabet and padding validated per chunk). Half-received
//! transfers are keyed by image id (or number) and dropped silently after the stale
//! timeout (spec gap: the fate on timeout is unspecified; silent drop never leaks).

use super::{Command, Header, Ids, parse_apc};
use crate::store::ProcessBudget;
use crate::{ErrCode, Limits};
use base64::Engine as _;
use base64::alphabet::STANDARD as ALPHABET;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

pub(crate) static ENGINE: GeneralPurpose = GeneralPurpose::new(
    &ALPHABET,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// A fully received transmission-type command.
#[derive(Debug)]
pub struct Transfer {
    /// The first chunk's header (the only one carrying keys).
    pub header: Header,
    /// Decoded payload: pixel bytes for `t=d`, a path / shm name for other media.
    pub payload: Vec<u8>,
}

/// A request that failed; reply with `format_reply(ids, Err(code), quiet)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    pub ids: Ids,
    pub quiet: u8,
    pub code: ErrCode,
}

#[derive(Debug)]
pub enum Event {
    /// A command with no payload to assemble (place, delete, animate control ...).
    Command(Header),
    /// A transmission is complete.
    Transfer(Transfer),
    /// Chunk accepted, more expected.
    Pending,
    Error(Failure),
}

#[derive(Debug)]
struct Pending {
    header: Header,
    data: Vec<u8>,
    b64_total: usize,
    touched_ms: u64,
}

/// Per-pane chunk accumulator.
#[derive(Debug)]
pub struct Assembler {
    limits: Limits,
    pending: Vec<Pending>,
    stale_dropped: u64,
    budget: ProcessBudget,
    /// Decoded bytes of all pending transfers, as charged to `budget`.
    charged: u64,
}

fn is_transmission(c: &Command) -> bool {
    matches!(
        c,
        Command::Transmit(_) | Command::TransmitPlace(..) | Command::Query(_) | Command::Frame(_)
    )
}

impl Assembler {
    pub fn new(limits: Limits) -> Self {
        Self::with_budget(limits, ProcessBudget::new())
    }

    /// Pending decoded bytes count against `limits.quota` (summed over transfers) and
    /// against the shared process budget.
    pub fn with_budget(limits: Limits, budget: ProcessBudget) -> Self {
        Self {
            limits,
            pending: Vec::new(),
            stale_dropped: 0,
            budget,
            charged: 0,
        }
    }

    /// Decoded bytes currently held by half-received transfers.
    pub fn pending_bytes(&self) -> u64 {
        self.pending.iter().map(|p| p.data.len() as u64).sum()
    }

    fn sync_budget(&mut self) {
        let now = self.pending_bytes();
        if now >= self.charged {
            self.budget.add(now - self.charged);
        } else {
            self.budget.sub(self.charged - now);
        }
        self.charged = now;
    }

    /// Would holding `p` on top of the other pending transfers exceed pane/process limits?
    fn over_budget(&self, p: &Pending) -> bool {
        let others = self.pending_bytes();
        let mine = p.data.len() as u64;
        let delta = (others + mine).saturating_sub(self.charged);
        others + mine > self.limits.quota
            || self.budget.used().saturating_add(delta) > self.limits.quota_total
    }

    /// Transfers dropped by the stale timeout so far.
    pub fn stale_dropped(&self) -> u64 {
        self.stale_dropped
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Drop transfers idle for longer than the stale timeout. `push` calls this, but an
    /// idle pane never pushes: the owner must also call it from a timer.
    pub fn expire(&mut self, now_ms: u64) {
        let ttl = self.limits.chunk_stale_ms;
        let before = self.pending.len();
        self.pending
            .retain(|p| now_ms.saturating_sub(p.touched_ms) <= ttl);
        self.stale_dropped += (before - self.pending.len()) as u64;
        self.sync_budget();
    }

    /// Drop every half-received transfer (any delete command aborts a chunked upload).
    pub fn abort(&mut self) {
        self.pending.clear();
        self.sync_budget();
    }

    /// When the oldest half-received transfer goes stale, in the caller's milliseconds.
    pub fn next_expiry(&self) -> Option<u64> {
        self.pending
            .iter()
            .map(|p| p.touched_ms.saturating_add(self.limits.chunk_stale_ms + 1))
            .min()
    }

    /// Feed one APC body (starting at `G`).
    pub fn push(&mut self, apc: &[u8], now_ms: u64) -> Event {
        let ev = self.push_inner(apc, now_ms);
        self.sync_budget();
        ev
    }

    fn push_inner(&mut self, apc: &[u8], now_ms: u64) -> Event {
        self.expire(now_ms);
        let (header, payload) = match parse_apc(apc) {
            Ok(v) => v,
            Err(e) => {
                return Event::Error(Failure {
                    ids: e.ids,
                    quiet: e.quiet,
                    code: e.code,
                });
            }
        };
        // A bare chunk continues a pending transfer: by id/number when given, else the
        // most recently started one.
        if header.chunk_keys_only && !self.pending.is_empty() {
            let pos = if header.ids == Ids::default() {
                Some(self.pending.len() - 1)
            } else {
                self.pending.iter().position(|p| {
                    p.header.ids.id == header.ids.id && p.header.ids.number == header.ids.number
                })
            };
            if let Some(pos) = pos {
                return self.continue_at(pos, &header, payload, now_ms);
            }
        }
        if !is_transmission(&header.command) {
            return Event::Command(header);
        }
        let (ids, quiet) = (header.ids, header.quiet);
        let fail = |code| Event::Error(Failure { ids, quiet, code });
        if self.pending.len() >= self.limits.max_pending_transfers && header.more {
            self.pending.remove(0); // oldest goes
            self.stale_dropped += 1;
        }
        // A new transfer for an id replaces a half-received one with the same id.
        if header.ids.id != 0 || header.ids.number != 0 {
            self.pending.retain(|p| {
                p.header.ids.id != header.ids.id || p.header.ids.number != header.ids.number
            });
        }
        let mut p = Pending {
            header,
            data: Vec::new(),
            b64_total: 0,
            touched_ms: now_ms,
        };
        let more = p.header.more;
        if let Err(code) = self.absorb(&mut p, payload, more) {
            return fail(code);
        }
        if more && self.over_budget(&p) {
            return fail(ErrCode::Enospc);
        }
        if more {
            self.pending.push(p);
            Event::Pending
        } else {
            Event::Transfer(Transfer {
                header: p.header,
                payload: p.data,
            })
        }
    }

    fn continue_at(&mut self, pos: usize, h: &Header, payload: &[u8], now_ms: u64) -> Event {
        let mut p = self.pending.remove(pos);
        p.touched_ms = now_ms;
        let mut res = self.absorb(&mut p, payload, h.more);
        if res.is_ok() && h.more && self.over_budget(&p) {
            res = Err(ErrCode::Enospc);
        }
        match res {
            Err(code) => Event::Error(Failure {
                ids: p.header.ids,
                quiet: p.header.quiet,
                code,
            }),
            Ok(()) if h.more => {
                self.pending.push(p);
                Event::Pending
            }
            Ok(()) => Event::Transfer(Transfer {
                header: p.header,
                payload: p.data,
            }),
        }
    }

    /// Validate and decode one chunk's base64 into the transfer.
    fn absorb(&self, p: &mut Pending, b64: &[u8], more: bool) -> Result<(), ErrCode> {
        if b64.len() > self.limits.max_chunk_b64 {
            return Err(ErrCode::Efbig);
        }
        p.b64_total = p.b64_total.saturating_add(b64.len());
        if p.b64_total > self.limits.max_transfer_b64 {
            return Err(ErrCode::Efbig);
        }
        if more && (!b64.len().is_multiple_of(4) || b64.contains(&b'=')) {
            return Err(ErrCode::Einval);
        }
        ENGINE
            .decode_vec(b64, &mut p.data)
            .map_err(|_| ErrCode::Enodata)?;
        if p.data.len() as u64 > self.limits.max_decoded_bytes {
            return Err(ErrCode::Efbig);
        }
        Ok(())
    }
}

impl Drop for Assembler {
    fn drop(&mut self) {
        self.budget.sub(self.charged);
    }
}
