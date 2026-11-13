// SPDX-License-Identifier: Apache-2.0
//! The streaming parser. `advance` is a pure function of (state, bytes): any split
//! of the input yields the same action stream, up to adjacent `print_run`s and
//! adjacent `string_put`s being cut at the split.

use crate::limits::{HEAD_LEN, MAX_INTERMEDIATES, UTF8_CARRY};
use crate::params::{Params, StrEnd, StrHead, StrKind, StrRoute};
use crate::sink::Sink;
use crate::table::{Act, ST_FINAL, State, TABLE};
use crate::utf8::{is_ctl, lead_info, scan_ctl};

const REPLACEMENT: &str = "\u{FFFD}";

/// Where a string is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Collecting the routing head (OSC, APC).
    Head,
    /// Streaming payload to the sink.
    Body,
    /// Skipping to the terminator without calling the sink.
    Skip,
}

/// The VT parser. Fixed size, no heap; see the crate docs.
#[derive(Clone, Debug)]
pub struct Parser {
    state: State,
    carry: [u8; 4],
    carry_len: usize,
    params: Params,
    inter: [u8; MAX_INTERMEDIATES],
    inter_len: usize,
    inter_over: bool,
    private: u8,
    // string machinery
    kind: StrKind,
    phase: Phase,
    esc_pending: bool,
    osc_sel: bool,
    head: StrHead,
    cap: u32,
    sent: u32,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// A parser in the ground state.
    pub const fn new() -> Self {
        Self {
            state: State::Ground,
            carry: [0; 4],
            carry_len: 0,
            params: Params::new(),
            inter: [0; MAX_INTERMEDIATES],
            inter_len: 0,
            inter_over: false,
            private: 0,
            kind: StrKind::Osc,
            phase: Phase::Skip,
            esc_pending: false,
            osc_sel: false,
            head: StrHead::new(),
            cap: 0,
            sent: 0,
        }
    }

    /// True when no sequence, string or partial code point is in flight.
    pub fn is_idle(&self) -> bool {
        self.state == State::Ground && self.carry_len == 0
    }

    /// Feed bytes. The only entry point.
    pub fn advance<S: Sink>(&mut self, bytes: &[u8], sink: &mut S) {
        let mut i = 0;
        while i < bytes.len() {
            i = if self.state == State::Ground {
                self.ground(bytes, i, sink)
            } else if self.state.is_string() {
                self.string(bytes, i, sink)
            } else {
                self.step(bytes[i], i, sink)
            };
        }
    }

    fn clear(&mut self) {
        self.params.clear();
        self.inter_len = 0;
        self.inter_over = false;
        self.private = 0;
    }

    // ---------------------------------------------------------------- ground

    fn ground<S: Sink>(&mut self, bytes: &[u8], mut i: usize, sink: &mut S) -> usize {
        while i < bytes.len() {
            if self.carry_len > 0 {
                i = self.carry_step(bytes[i], i, sink);
                continue;
            }
            let b = bytes[i];
            if is_ctl(b) {
                let e = TABLE[State::Ground as usize][b as usize];
                match e.act {
                    Act::Execute => sink.execute(b),
                    Act::Clear => {
                        self.clear();
                        self.state = State::Escape;
                        return i + 1;
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            let rest = &bytes[i..];
            let n = scan_ctl(rest);
            self.text(&rest[..n], n == rest.len(), sink);
            i += n;
        }
        i
    }

    /// Emit a run that contains no C0/DEL. `at_end`: the run ends the input, so an
    /// incomplete trailing sequence is carried instead of replaced.
    fn text<S: Sink>(&mut self, mut run: &[u8], at_end: bool, sink: &mut S) {
        while !run.is_empty() {
            match simdutf8::compat::from_utf8(run) {
                Ok(s) => {
                    sink.print_run(s);
                    return;
                }
                Err(e) => {
                    let v = e.valid_up_to();
                    if v > 0
                        && let Ok(s) = core::str::from_utf8(&run[..v])
                    {
                        sink.print_run(s);
                    }
                    match e.error_len() {
                        Some(n) => {
                            sink.print_run(REPLACEMENT);
                            run = &run[(v + n).min(run.len())..];
                        }
                        None => {
                            let tail = &run[v..];
                            if at_end && tail.len() <= UTF8_CARRY {
                                self.carry[..tail.len()].copy_from_slice(tail);
                                self.carry_len = tail.len();
                            } else {
                                sink.print_run(REPLACEMENT);
                            }
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Byte-at-a-time completion of a carried code point. Consumes `b` (returns
    /// `i + 1`) or flushes the carry as U+FFFD and leaves `b` for reprocessing.
    fn carry_step<S: Sink>(&mut self, b: u8, i: usize, sink: &mut S) -> usize {
        let n = self.carry_len;
        let Some((need, lo, hi)) = lead_info(self.carry[0]) else {
            self.carry_len = 0;
            sink.print_run(REPLACEMENT);
            return i;
        };
        let ok = if n == 1 {
            (lo..=hi).contains(&b)
        } else {
            (0x80..=0xBF).contains(&b)
        };
        if !ok {
            self.carry_len = 0;
            sink.print_run(REPLACEMENT);
            return i;
        }
        self.carry[n] = b;
        self.carry_len = n + 1;
        if self.carry_len == need {
            self.carry_len = 0;
            match core::str::from_utf8(&self.carry[..need]) {
                Ok(s) => sink.print_run(s),
                Err(_) => sink.print_run(REPLACEMENT),
            }
        }
        i + 1
    }

    // ------------------------------------------------- table-driven sequences

    /// One byte in a non-ground, non-string state. Returns the next index
    /// (`i` itself when the byte is to be reprocessed).
    fn step<S: Sink>(&mut self, b: u8, i: usize, sink: &mut S) -> usize {
        let e = TABLE[self.state as usize][b as usize];
        let mut next = e.next;
        let mut consumed = true;
        match e.act {
            Act::Ignore | Act::Print | Act::StrByte | Act::StrEsc | Act::StrBel => {}
            Act::Execute => sink.execute(b),
            Act::Clear => self.clear(),
            Act::Collect => {
                if self.inter_len < MAX_INTERMEDIATES {
                    self.inter[self.inter_len] = b;
                    self.inter_len += 1;
                } else {
                    let o = self.state.on_inter_overflow(next);
                    if o == next {
                        self.inter_over = true;
                    }
                    next = o;
                }
            }
            Act::Private => self.private = b,
            Act::Param => self.params.feed(b),
            Act::EscDispatch => {
                if !self.inter_over {
                    sink.esc(&self.inter[..self.inter_len], b);
                }
            }
            Act::CsiDispatch => {
                self.params.finish();
                sink.csi(&self.params, &self.inter[..self.inter_len], self.private, b);
            }
            Act::DcsHook => {
                self.build_dcs_head(Some(b));
                self.kind = StrKind::Dcs;
                self.esc_pending = false;
                self.complete_head(true, sink);
            }
            Act::DcsAbort => {
                self.abort_dcs_header(sink);
                self.clear();
            }
            Act::Cancel => {
                self.abort_dcs_header(sink);
                sink.execute(b);
            }
            Act::Abort => {
                self.abort_dcs_header(sink);
                consumed = false;
            }
            Act::StrStart => {
                self.esc_pending = false;
                self.osc_sel = false;
                self.head.reset();
                self.phase = Phase::Head;
                self.kind = next.str_kind().unwrap_or(StrKind::Sos);
                if matches!(self.kind, StrKind::Pm | StrKind::Sos) {
                    self.complete_head(true, sink);
                }
            }
        }
        if next == State::DcsIgnore && self.state != State::DcsIgnore {
            self.kind = next.str_kind().unwrap_or(StrKind::Dcs);
            self.phase = Phase::Skip;
            self.esc_pending = false;
        }
        self.state = next;
        if consumed { i + 1 } else { i }
    }

    fn build_dcs_head(&mut self, fin: Option<u8>) {
        self.params.finish();
        let h = &mut self.head;
        h.reset();
        h.params_mut().clone_from(&self.params);
        let buf = h.buf_mut();
        buf[0] = self.private;
        buf[1..=self.inter_len].copy_from_slice(&self.inter[..self.inter_len]);
        let split = 1 + self.inter_len;
        let mut len = split;
        if let Some(f) = fin {
            buf[split] = f;
            len += 1;
        }
        h.set_split(split);
        h.set_len(len);
        h.set_complete(fin.is_some());
    }

    /// Leaving a DCS header without a final byte: partial head, then end.
    fn abort_dcs_header<S: Sink>(&mut self, sink: &mut S) {
        if self.state.is_dcs_header() {
            self.build_dcs_head(None);
            sink.string_start(StrKind::Dcs, &self.head);
            sink.string_end(StrEnd::Aborted);
        }
    }

    // ---------------------------------------------------------------- strings

    /// Call `string_start` with the current head, then open the payload (or skip).
    fn complete_head<S: Sink>(&mut self, complete: bool, sink: &mut S) {
        self.head.set_complete(complete);
        let StrRoute { cap } = sink.string_start(self.kind, &self.head);
        if cap == 0 {
            sink.string_end(StrEnd::Rejected);
            self.phase = Phase::Skip;
            return;
        }
        self.cap = cap;
        self.sent = 0;
        self.phase = Phase::Body;
        // Selector / first bytes buffered in the head are payload too.
        let from = if self.kind == StrKind::Dcs {
            self.head.len()
        } else {
            self.head.split()
        };
        let to = self.head.len();
        if from < to {
            let mut tmp = [0u8; HEAD_LEN];
            tmp[..to - from].copy_from_slice(&self.head.raw()[from..to]);
            self.deliver(&tmp[..to - from], sink);
        }
    }

    /// Hand payload to the sink, enforcing the cap.
    fn deliver<S: Sink>(&mut self, data: &[u8], sink: &mut S) {
        let room = (self.cap - self.sent) as usize;
        if data.len() <= room {
            sink.string_put(data);
            self.sent += data.len() as u32;
        } else {
            if room > 0 {
                sink.string_put(&data[..room]);
            }
            self.sent = self.cap;
            sink.string_end(StrEnd::OverCap);
            self.phase = Phase::Skip;
        }
    }

    /// Finish the current string and return to Ground.
    fn end_string<S: Sink>(&mut self, end: StrEnd, sink: &mut S) {
        if self.phase == Phase::Head {
            self.complete_head(false, sink);
        }
        if self.phase == Phase::Body {
            sink.string_end(end);
        }
        self.phase = Phase::Skip;
        self.state = State::Ground;
    }

    fn restart_escape(&mut self) {
        self.clear();
        self.state = State::Escape;
    }

    /// One unit of work in a string state: a pending ESC, one head byte, or one
    /// payload scan. Returns the next index.
    fn string<S: Sink>(&mut self, bytes: &[u8], i: usize, sink: &mut S) -> usize {
        if self.esc_pending {
            self.esc_pending = false;
            if self.act(State::Escape, bytes[i]) == Act::EscDispatch && bytes[i] == ST_FINAL {
                self.end_string(StrEnd::St, sink);
                return i + 1;
            }
            self.end_string(StrEnd::Aborted, sink);
            self.restart_escape();
            return i;
        }
        if self.phase == Phase::Head {
            return self.head_byte(bytes[i], i, sink);
        }
        let hay = &bytes[i..];
        let mut t = memchr::memchr3(0x1B, 0x18, 0x1A, hay);
        if self.kind == StrKind::Osc {
            let upto = t.unwrap_or(hay.len());
            if let Some(p) = memchr::memchr(0x07, &hay[..upto]) {
                t = Some(p);
            }
        }
        let data = &hay[..t.unwrap_or(hay.len())];
        if self.phase == Phase::Body && !data.is_empty() {
            self.deliver(data, sink);
        }
        let Some(p) = t else { return bytes.len() };
        let at = i + p;
        self.terminator(bytes[at], sink);
        at + 1
    }

    /// The table's action for `b` in state `s`.
    fn act(&self, s: State, b: u8) -> Act {
        TABLE[s as usize][b as usize].act
    }

    /// A byte the string scan stopped at, dispatched through the table.
    fn terminator<S: Sink>(&mut self, b: u8, sink: &mut S) {
        match self.act(self.state, b) {
            Act::StrEsc => self.esc_pending = true,
            Act::StrBel => self.end_string(StrEnd::Bel, sink),
            _ => {
                self.end_string(StrEnd::Aborted, sink);
                sink.execute(b);
            }
        }
    }

    /// One byte while collecting the head of an OSC or APC string.
    fn head_byte<S: Sink>(&mut self, b: u8, i: usize, sink: &mut S) -> usize {
        if matches!(
            self.act(self.state, b),
            Act::StrEsc | Act::StrBel | Act::Cancel
        ) {
            self.terminator(b, sink);
            return i + 1;
        }
        if self.kind == StrKind::Apc {
            let len = self.head.len();
            self.head.buf_mut()[len] = b;
            self.head.set_len(len + 1);
            self.complete_head(true, sink);
            return i + 1;
        }
        // OSC
        let len = self.head.len();
        if !self.osc_sel {
            if b.is_ascii_digit() && len < HEAD_LEN {
                self.head.buf_mut()[len] = b;
                self.head.set_len(len + 1);
                self.head.set_split(len + 1);
                return i + 1;
            }
            if b == b';' {
                self.osc_sel = true;
                return i + 1;
            }
            if b.is_ascii_digit() {
                self.head.set_truncated();
            }
            // Not a command digit: the head is done, this byte is payload.
            self.complete_head(true, sink);
            return i;
        }
        if matches!(b, b';' | b'=' | b':') {
            self.complete_head(true, sink);
            return i;
        }
        if len < HEAD_LEN {
            self.head.buf_mut()[len] = b;
            self.head.set_len(len + 1);
            return i + 1;
        }
        self.head.set_truncated();
        self.complete_head(true, sink);
        i
    }
}
