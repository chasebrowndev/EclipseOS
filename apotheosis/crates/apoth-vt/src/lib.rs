// SPDX-License-Identifier: Apache-2.0
//! Streaming VT parser: ECMA-48/DEC state machine, string sinks with per-protocol byte budgets, bulk printable fast path.
//!
//! Governing spec: T-01 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! The parser knows syntax only. Input is UTF-8; bytes 0x80..=0x9F are never C1
//! controls. It never allocates, recurses or panics, whatever the input.
//!
//! Choices where T-01 is silent (pinned by `tests/golden.rs`):
//! - DEL is ignored in Ground and inside sequences; inside strings it is payload.
//! - In a string every byte is payload except ESC, CAN, SUB (and BEL for OSC).
//!   ESC followed by `\` is ST; ESC followed by anything else aborts the string
//!   (`StrEnd::Aborted`) and the byte is processed in the Escape state. CAN/SUB
//!   abort it and are then delivered through `execute`.
//! - A byte >= 0x80 inside ESC/CSI/DCS-header aborts the sequence silently and is
//!   reprocessed in Ground as the start of a code point.
//! - C0 controls inside a DCS header are ignored (as in the Williams table).
//! - An invalid DCS header (`DcsIgnore`) is skipped without any sink call.
#![forbid(unsafe_code)]

pub mod bench;
pub mod limits;
mod params;
mod parser;
mod sink;
mod table;
mod utf8;

pub use params::{Params, StrEnd, StrHead, StrKind, StrRoute};
pub use parser::Parser;
pub use sink::Sink;

/// The state table rendered as text, for the snapshot test.
#[doc(hidden)]
pub fn render_table() -> String {
    table::render()
}

#[cfg(test)]
mod tests {
    use crate::table::{ALL_STATES, Act, State, TABLE};

    #[test]
    fn every_row_stays_in_its_lane() {
        for (i, s) in ALL_STATES.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
        // Ground rows never enter a string or sequence except through ESC.
        for (b, _) in TABLE[State::Ground as usize].iter().enumerate() {
            let e = TABLE[State::Ground as usize][b];
            if b == 0x1B {
                assert_eq!((e.act, e.next), (Act::Clear, State::Escape));
            } else {
                assert_eq!(e.next, State::Ground);
            }
        }
    }

    #[test]
    fn can_sub_cancel_everywhere() {
        for s in ALL_STATES {
            if s == State::Ground {
                continue;
            }
            for b in [0x18usize, 0x1A] {
                let e = TABLE[s as usize][b];
                assert_eq!((e.act, e.next), (Act::Cancel, State::Ground), "{s:?}");
            }
        }
    }

    #[test]
    fn high_bytes_are_never_c1() {
        for s in ALL_STATES {
            #[allow(clippy::needless_range_loop)]
            for b in 0x80..=0xFFusize {
                let e = TABLE[s as usize][b];
                match s {
                    State::Ground => assert_eq!(e.act, Act::Print),
                    s if s.is_string() => assert_eq!(e.act, Act::StrByte),
                    _ => assert_eq!(e.act, Act::Abort, "{s:?} {b:02X}"),
                }
            }
        }
    }

    #[test]
    fn st_final_is_an_escape_dispatch() {
        use crate::table::ST_FINAL;
        let e = TABLE[State::Escape as usize][ST_FINAL as usize];
        assert_eq!((e.act, e.next), (Act::EscDispatch, State::Ground));
    }

    /// The vectorised string scan in the parser stops at 0x1B/0x18/0x1A (+BEL for OSC).
    /// If the table grows another non-payload byte, this fails until the scan follows.
    #[test]
    fn string_scan_stops_match_the_table() {
        use crate::table::string_stops;
        for s in ALL_STATES.into_iter().filter(|s| s.is_string()) {
            let (stops, n) = string_stops(s);
            let mut want = vec![0x18u8, 0x1A, 0x1B];
            if s == State::OscString {
                want.insert(0, 0x07);
            }
            assert_eq!(&stops[..n], &want[..], "{s:?}");
        }
    }

    #[test]
    fn csi_transitions() {
        let t = |s: State, b: u8| TABLE[s as usize][b as usize];
        assert_eq!(t(State::CsiEntry, b'?').act, Act::Private);
        assert_eq!(t(State::CsiParam, b'?').next, State::CsiIgnore);
        assert_eq!(t(State::CsiParam, b':').act, Act::Param);
        assert_eq!(t(State::CsiIntermediate, b'0').next, State::CsiIgnore);
        assert_eq!(t(State::CsiIgnore, b'm').next, State::Ground);
        assert_eq!(t(State::CsiIgnore, b'm').act, Act::Ignore);
        assert_eq!(t(State::Escape, b'[').next, State::CsiEntry);
        assert_eq!(t(State::Escape, b'P').next, State::DcsEntry);
        assert_eq!(t(State::Escape, b']').next, State::OscString);
        assert_eq!(t(State::Escape, b'_').next, State::ApcString);
        assert_eq!(t(State::Escape, b'^').next, State::PmString);
        assert_eq!(t(State::Escape, b'X').next, State::SosString);
        assert_eq!(t(State::Escape, b'Y').act, Act::EscDispatch);
        assert_eq!(t(State::DcsEntry, b'q').act, Act::DcsHook);
        assert_eq!(t(State::DcsParam, b'<').next, State::DcsIgnore);
        assert_eq!(t(State::OscString, 0x07).act, Act::StrBel);
        assert_eq!(t(State::ApcString, 0x07).act, Act::StrByte);
    }
}
