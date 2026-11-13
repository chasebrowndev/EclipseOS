// SPDX-License-Identifier: Apache-2.0
//! The `[state][byte] -> (action, next state)` table, built by const evaluation.
//!
//! This is the single source of transitions (T-01 §2.1). The parser looks every
//! sequence byte, ground control and string terminator up here; the bulk scans only
//! find *where* the next such byte is (their stop sets are checked against the table
//! by `string_stops` and a unit test). A snapshot test renders the table.

use crate::params::StrKind;

/// Parser states (Williams DEC-compatible machine).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum State {
    Ground = 0,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    ApcString,
    PmString,
    SosString,
}

pub(crate) const STATE_COUNT: usize = 16;

pub(crate) const ALL_STATES: [State; STATE_COUNT] = [
    State::Ground,
    State::Escape,
    State::EscapeIntermediate,
    State::CsiEntry,
    State::CsiParam,
    State::CsiIntermediate,
    State::CsiIgnore,
    State::DcsEntry,
    State::DcsParam,
    State::DcsIntermediate,
    State::DcsPassthrough,
    State::DcsIgnore,
    State::OscString,
    State::ApcString,
    State::PmString,
    State::SosString,
];

impl State {
    /// DCS states before the final byte (an abort here reports a partial head).
    pub(crate) const fn is_dcs_header(self) -> bool {
        matches!(
            self,
            State::DcsEntry | State::DcsParam | State::DcsIntermediate
        )
    }

    /// States whose bytes are consumed by the string scanner.
    pub(crate) const fn is_string(self) -> bool {
        matches!(
            self,
            State::DcsPassthrough
                | State::DcsIgnore
                | State::OscString
                | State::ApcString
                | State::PmString
                | State::SosString
        )
    }
}

impl State {
    /// The string kind a string state carries.
    pub(crate) const fn str_kind(self) -> Option<StrKind> {
        match self {
            State::DcsPassthrough | State::DcsIgnore => Some(StrKind::Dcs),
            State::OscString => Some(StrKind::Osc),
            State::ApcString => Some(StrKind::Apc),
            State::PmString => Some(StrKind::Pm),
            State::SosString => Some(StrKind::Sos),
            _ => None,
        }
    }

    /// Where `Act::Collect` goes when the intermediate buffer is full: a CSI or DCS
    /// sequence with more than 4 intermediates is ignored to its final byte.
    pub(crate) const fn on_inter_overflow(self, normal: State) -> State {
        match self {
            State::CsiEntry | State::CsiParam | State::CsiIntermediate => State::CsiIgnore,
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => State::DcsIgnore,
            _ => normal,
        }
    }
}

/// The final byte that makes `ESC <final>` the string terminator (ST). Pinned to the
/// Escape row by a unit test.
pub(crate) const ST_FINAL: u8 = b'\\';

/// Bytes a string scan must stop at: every byte whose table entry in `s` is not
/// plain payload. The parser's vectorised scan must search for exactly this set.
#[cfg(test)]
pub(crate) fn string_stops(s: State) -> ([u8; 4], usize) {
    let mut out = [0u8; 4];
    let mut n = 0;
    let mut b = 0usize;
    while b < 256 {
        if TABLE[s as usize][b].act != Act::StrByte {
            out[n] = b as u8;
            n += 1;
        }
        b += 1;
    }
    (out, n)
}

/// What to do with the byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// Drop the byte.
    Ignore,
    /// Text byte (Ground only; handled by the bulk path).
    Print,
    /// Run a C0 control.
    Execute,
    /// ESC: reset collected state and enter Escape.
    Clear,
    /// Store an intermediate byte.
    Collect,
    /// Store the private marker.
    Private,
    /// Feed a parameter byte.
    Param,
    EscDispatch,
    CsiDispatch,
    /// DCS final byte: begin the string.
    DcsHook,
    /// ESC inside a DCS header: report the partial head, enter Escape.
    DcsAbort,
    /// CAN/SUB: abort (partial DCS head reported), execute, Ground.
    Cancel,
    /// Byte >= 0x80 inside a sequence: abort, reprocess the byte in Ground.
    Abort,
    /// OSC/APC/PM/SOS introducer: begin the string for the next state.
    StrStart,
    /// String payload byte.
    StrByte,
    /// ESC inside a string: ST or restart.
    StrEsc,
    /// BEL inside OSC: terminate.
    StrBel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) act: Act,
    pub(crate) next: State,
}

type Row = [Entry; 256];

const fn fill(row: &mut Row, lo: u8, hi: u8, act: Act, next: State) {
    let mut i = lo as usize;
    while i <= hi as usize {
        row[i] = Entry { act, next };
        i += 1;
    }
}

const fn new_row(s: State) -> Row {
    [Entry {
        act: Act::Ignore,
        next: s,
    }; 256]
}

/// CAN, SUB, ESC and bytes >= 0x80 behave the same "anywhere" in sequence states.
const fn anywhere(row: &mut Row, esc_act: Act, high: bool) {
    fill(row, 0x18, 0x18, Act::Cancel, State::Ground);
    fill(row, 0x1A, 0x1A, Act::Cancel, State::Ground);
    fill(row, 0x1B, 0x1B, esc_act, State::Escape);
    if high {
        fill(row, 0x80, 0xFF, Act::Abort, State::Ground);
    }
}

const fn ground_row() -> Row {
    let mut r = new_row(State::Ground);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::Ground);
    fill(&mut r, 0x1B, 0x1B, Act::Clear, State::Escape);
    fill(&mut r, 0x20, 0x7E, Act::Print, State::Ground);
    fill(&mut r, 0x7F, 0x7F, Act::Ignore, State::Ground);
    fill(&mut r, 0x80, 0xFF, Act::Print, State::Ground);
    r
}

const fn escape_row() -> Row {
    let mut r = new_row(State::Escape);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::Escape);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::EscapeIntermediate);
    fill(&mut r, 0x30, 0x7E, Act::EscDispatch, State::Ground);
    fill(&mut r, 0x50, 0x50, Act::Ignore, State::DcsEntry);
    fill(&mut r, 0x58, 0x58, Act::StrStart, State::SosString);
    fill(&mut r, 0x5B, 0x5B, Act::Ignore, State::CsiEntry);
    fill(&mut r, 0x5D, 0x5D, Act::StrStart, State::OscString);
    fill(&mut r, 0x5E, 0x5E, Act::StrStart, State::PmString);
    fill(&mut r, 0x5F, 0x5F, Act::StrStart, State::ApcString);
    anywhere(&mut r, Act::Clear, true);
    r
}

const fn esc_inter_row() -> Row {
    let mut r = new_row(State::EscapeIntermediate);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::EscapeIntermediate);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::EscapeIntermediate);
    fill(&mut r, 0x30, 0x7E, Act::EscDispatch, State::Ground);
    anywhere(&mut r, Act::Clear, true);
    r
}

const fn csi_entry_row() -> Row {
    let mut r = new_row(State::CsiEntry);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::CsiEntry);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::CsiIntermediate);
    fill(&mut r, 0x30, 0x3B, Act::Param, State::CsiParam);
    fill(&mut r, 0x3C, 0x3F, Act::Private, State::CsiParam);
    fill(&mut r, 0x40, 0x7E, Act::CsiDispatch, State::Ground);
    anywhere(&mut r, Act::Clear, true);
    r
}

const fn csi_param_row() -> Row {
    let mut r = new_row(State::CsiParam);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::CsiParam);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::CsiIntermediate);
    fill(&mut r, 0x30, 0x3B, Act::Param, State::CsiParam);
    fill(&mut r, 0x3C, 0x3F, Act::Ignore, State::CsiIgnore);
    fill(&mut r, 0x40, 0x7E, Act::CsiDispatch, State::Ground);
    anywhere(&mut r, Act::Clear, true);
    r
}

const fn csi_inter_row() -> Row {
    let mut r = new_row(State::CsiIntermediate);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::CsiIntermediate);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::CsiIntermediate);
    fill(&mut r, 0x30, 0x3F, Act::Ignore, State::CsiIgnore);
    fill(&mut r, 0x40, 0x7E, Act::CsiDispatch, State::Ground);
    anywhere(&mut r, Act::Clear, true);
    r
}

const fn csi_ignore_row() -> Row {
    let mut r = new_row(State::CsiIgnore);
    fill(&mut r, 0x00, 0x1F, Act::Execute, State::CsiIgnore);
    fill(&mut r, 0x40, 0x7E, Act::Ignore, State::Ground);
    anywhere(&mut r, Act::Clear, true);
    r
}

const fn dcs_entry_row() -> Row {
    let mut r = new_row(State::DcsEntry);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::DcsIntermediate);
    fill(&mut r, 0x30, 0x3B, Act::Param, State::DcsParam);
    fill(&mut r, 0x3C, 0x3F, Act::Private, State::DcsParam);
    fill(&mut r, 0x40, 0x7E, Act::DcsHook, State::DcsPassthrough);
    anywhere(&mut r, Act::DcsAbort, true);
    r
}

const fn dcs_param_row() -> Row {
    let mut r = new_row(State::DcsParam);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::DcsIntermediate);
    fill(&mut r, 0x30, 0x3B, Act::Param, State::DcsParam);
    fill(&mut r, 0x3C, 0x3F, Act::Ignore, State::DcsIgnore);
    fill(&mut r, 0x40, 0x7E, Act::DcsHook, State::DcsPassthrough);
    anywhere(&mut r, Act::DcsAbort, true);
    r
}

const fn dcs_inter_row() -> Row {
    let mut r = new_row(State::DcsIntermediate);
    fill(&mut r, 0x20, 0x2F, Act::Collect, State::DcsIntermediate);
    fill(&mut r, 0x30, 0x3F, Act::Ignore, State::DcsIgnore);
    fill(&mut r, 0x40, 0x7E, Act::DcsHook, State::DcsPassthrough);
    anywhere(&mut r, Act::DcsAbort, true);
    r
}

/// String-body rows: every byte is payload except the terminators.
const fn string_row(s: State, bel: bool) -> Row {
    let mut r = new_row(s);
    fill(&mut r, 0x00, 0xFF, Act::StrByte, s);
    if bel {
        fill(&mut r, 0x07, 0x07, Act::StrBel, State::Ground);
    }
    anywhere(&mut r, Act::StrEsc, false);
    // ESC stays in the string until the next byte says ST or restart.
    fill(&mut r, 0x1B, 0x1B, Act::StrEsc, s);
    r
}

const fn build() -> [Row; STATE_COUNT] {
    [
        ground_row(),
        escape_row(),
        esc_inter_row(),
        csi_entry_row(),
        csi_param_row(),
        csi_inter_row(),
        csi_ignore_row(),
        dcs_entry_row(),
        dcs_param_row(),
        dcs_inter_row(),
        string_row(State::DcsPassthrough, false),
        string_row(State::DcsIgnore, false),
        string_row(State::OscString, true),
        string_row(State::ApcString, false),
        string_row(State::PmString, false),
        string_row(State::SosString, false),
    ]
}

pub(crate) static TABLE: [Row; STATE_COUNT] = build();

/// Render the table as text, one line per run of identical entries.
pub(crate) fn render() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for (si, s) in ALL_STATES.iter().enumerate() {
        let _ = writeln!(out, "{s:?}");
        let row = &TABLE[si];
        let mut lo = 0usize;
        while lo < 256 {
            let mut hi = lo;
            while hi + 1 < 256 && row[hi + 1] == row[lo] {
                hi += 1;
            }
            let e = row[lo];
            let _ = writeln!(out, "  {lo:02X}-{hi:02X} {:?} -> {:?}", e.act, e.next);
            lo = hi + 1;
        }
    }
    out
}
