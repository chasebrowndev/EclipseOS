// SPDX-License-Identifier: AGPL-3.0-only
//! Behaviour tests that need code rather than a fixture: mode-table agreement (T-03 §2),
//! the input mirror, reply bounds, determinism and no-panic streams (T-03 §6).

mod support;

use std::time::{Duration, Instant};

use apoth_input::modes::bit;
use apoth_term::modes::{Access, MODES};
use apoth_term::{ModeState, Term, TermConfig};
use support::{Chunking, digest};

fn term(rows: usize, cols: usize) -> Term {
    Term::new(TermConfig {
        rows,
        cols,
        ..TermConfig::default()
    })
}

fn feed(t: &mut Term, s: &str) {
    t.feed(s.as_bytes(), Instant::now());
}

#[test]
fn decrqm_agrees_with_the_mode_table_for_every_mode() {
    for d in MODES {
        let mut t = term(3, 10);
        let q = if d.ansi {
            format!("\x1b[{}$p", d.num)
        } else {
            format!("\x1b[?{}$p", d.num)
        };
        feed(&mut t, &q);
        let got = String::from_utf8(t.replies().take_all()).unwrap();
        let want_pm = match d.access {
            Access::Unrecognised => 0,
            Access::ReadOnlySet => 3,
            Access::ReadOnlyReset => 4,
            Access::Settable => {
                if !d.ansi && d.num == 2027 {
                    3 // width-method `unicode` (default): permanently set (T-04 §2)
                } else if d.default {
                    1
                } else {
                    2
                }
            }
        };
        let pfx = if d.ansi { "" } else { "?" };
        assert_eq!(
            got,
            format!("\x1b[{pfx}{};{want_pm}$y", d.num),
            "{}",
            d.name
        );
        assert_eq!(
            t.mode_state(d.ansi, d.num) as u8,
            want_pm,
            "mode_state {}",
            d.name
        );
    }
}

#[test]
fn decrqm_answers_unknown_numbers_with_zero() {
    let mut t = term(3, 10);
    feed(&mut t, "\x1b[?9999$p\x1b[9999$p\x1b[?70000$p");
    let got = String::from_utf8(t.replies().take_all()).unwrap();
    assert_eq!(got, "\x1b[?9999;0$y\x1b[9999;0$y\x1b[?70000;0$y");
}

#[test]
fn settable_modes_round_trip_through_decrqm() {
    for d in MODES
        .iter()
        .filter(|d| d.access == Access::Settable && !d.ansi)
    {
        if matches!(d.num, 47 | 1047 | 1048 | 1049 | 2027 | 6 | 69) {
            continue; // covered by fixtures (screen switch, margins, config-dependent)
        }
        let mut t = term(3, 10);
        feed(&mut t, &format!("\x1b[?{}h", d.num));
        assert_eq!(t.mode_state(false, d.num), ModeState::Set, "{}", d.name);
        feed(&mut t, &format!("\x1b[?{}l", d.num));
        assert_eq!(t.mode_state(false, d.num), ModeState::Reset, "{}", d.name);
    }
}

#[test]
fn every_input_path_mode_moves_its_mirror_bit() {
    for d in MODES.iter().filter(|d| d.input_bit != 0) {
        let mut t = term(3, 10);
        let m = t.input_mirror();
        feed(&mut t, &format!("\x1b[?{}h", d.num));
        assert!(m.load().has(d.input_bit), "set ?{} ({})", d.num, d.name);
        feed(&mut t, &format!("\x1b[?{}l", d.num));
        assert!(!m.load().has(d.input_bit), "reset ?{} ({})", d.num, d.name);
    }
}

#[test]
fn mirror_is_stored_before_the_next_byte_is_parsed() {
    // Feed byte by byte: after each byte the mirror equals what the state needs.
    let mut t = term(3, 10);
    let m = t.input_mirror();
    for (i, b) in b"\x1b[?1h".iter().enumerate() {
        t.feed(&[*b], Instant::now());
        let on = m.load().has(bit::DECCKM);
        assert_eq!(on, i == 4, "after byte {i}");
    }
}

#[test]
fn kitty_flags_publish_and_screens_are_separate() {
    let mut t = term(3, 10);
    let m = t.input_mirror();
    feed(&mut t, "\x1b[>3u");
    assert_eq!(m.load().kitty_flags(), 3);
    feed(&mut t, "\x1b[?1049h");
    assert_eq!(m.load().kitty_flags(), 0);
    feed(&mut t, "\x1b[>5u");
    assert_eq!(m.load().kitty_flags(), 5);
    feed(&mut t, "\x1b[?1049l");
    assert_eq!(m.load().kitty_flags(), 3);
    feed(&mut t, "\x1b[<u");
    assert_eq!(m.load().kitty_flags(), 0);
    feed(&mut t, "\x1bc");
    assert_eq!(m.load().kitty_flags(), 0);
}

#[test]
fn kitty_stack_is_eight_deep() {
    let mut t = term(3, 10);
    for i in 1..=9 {
        feed(&mut t, &format!("\x1b[>{i}u"));
    }
    feed(&mut t, "\x1b[<7u");
    assert_eq!(t.kitty_flags(), 2);
}

#[test]
fn golden_da2_versions() {
    for (v, want) in [
        ((0, 1, 0), "\x1b[>1;100;0c"),
        ((1, 4, 2), "\x1b[>1;10402;0c"),
        ((12, 34, 56), "\x1b[>1;123456;0c"),
    ] {
        let mut t = Term::new(TermConfig {
            version: v,
            ..TermConfig::default()
        });
        feed(&mut t, "\x1b[>c");
        assert_eq!(String::from_utf8(t.replies().take_all()).unwrap(), want);
    }
}

#[test]
fn da1_both_forms() {
    let mut t = term(2, 4);
    feed(&mut t, "\x1b[c");
    assert_eq!(t.replies().take_all(), b"\x1b[?62;22c");
    let mut t = Term::new(TermConfig {
        sixel_advertised: true,
        ..TermConfig::default()
    });
    feed(&mut t, "\x1b[c");
    assert_eq!(t.replies().take_all(), b"\x1b[?62;4;22c");
}

#[test]
fn reply_queue_is_bounded_and_counts_drops() {
    let mut t = term(2, 4);
    let n = 70_000;
    let bytes = "\x1b[5n".repeat(n);
    t.feed(bytes.as_bytes(), Instant::now());
    // each reply is 4 bytes: 64 KiB fits 16384 of them.
    assert_eq!(t.replies().len_bytes(), 64 * 1024);
    assert_eq!(t.replies().dropped(), (n - 16384) as u64);
    // Replies are whole units, in order.
    assert_eq!(t.replies().pop().unwrap(), b"\x1b[0n");
}

#[test]
fn no_reply_echoes_attacker_bytes() {
    let mut t = term(3, 20);
    feed(&mut t, "\x1bP$qEVIL\x1b\\\x1bP+q4556494c\x1b\\\x1b[?9999$p");
    let r = String::from_utf8(t.replies().take_all()).unwrap();
    assert!(!r.contains("EVIL"));
}

#[test]
fn sync_hold_ends_on_the_deadline_and_next_deadline_reports_it() {
    let mut t = term(3, 10);
    let t0 = Instant::now();
    t.feed(b"\x1b[?2026h", t0);
    assert!(t.presentation_hold());
    assert_eq!(t.next_deadline(), Some(t0 + Duration::from_millis(1000)));
    t.tick(t0 + Duration::from_millis(999));
    assert!(t.presentation_hold());
    t.tick(t0 + Duration::from_millis(1000));
    assert!(!t.presentation_hold());
    assert_eq!(t.next_deadline(), None);
}

#[test]
fn feed_wakes_report_what_happened() {
    let mut t = term(3, 10);
    let w = t.feed(b"a", Instant::now());
    assert!(w.dirty && !w.replies && !w.events && !w.hold);
    let w = t.feed(b"\x1b[5n\x07", Instant::now());
    assert!(w.replies && w.events);
}

// ---- no-panic, bounded memory, determinism -----------------------------------------------

struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const FRAGMENTS: &[&str] = &[
    "\x1b[",
    "\x1b]",
    "\x1bP",
    "\x1b_",
    "\x1b",
    "\x1b\\",
    "\x07",
    "\x18",
    ";",
    ":",
    "?",
    ">",
    "<",
    "=",
    "!",
    "$",
    " ",
    "\"",
    "'",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "38",
    "48",
    "58",
    "m",
    "H",
    "J",
    "K",
    "r",
    "s",
    "h",
    "l",
    "p",
    "q",
    "n",
    "t",
    "u",
    "@",
    "P",
    "L",
    "M",
    "S",
    "T",
    "X",
    "b",
    "I",
    "Z",
    "g",
    "c",
    "\n",
    "\r",
    "\t",
    "\x08",
    "\x0e",
    "\x0f",
    "世",
    "é",
    "\u{301}",
    "\u{200d}",
    "👨",
    "\u{fe0f}",
    "abc",
    "52;c;aGk=",
    "133;A",
    "8;;http://x",
    "4;1;?",
    "9;4;1;50",
    "?1049",
    "?2026",
    "?69",
    "?1000",
    "?2004",
    "(0",
    "#8",
    "7",
    "8",
    "D",
    "E",
    "M",
];

fn stream(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng(seed | 1);
    let mut out = Vec::new();
    for _ in 0..n {
        if r.step().is_multiple_of(11) {
            out.push((r.step() & 0xff) as u8);
        } else {
            out.extend_from_slice(FRAGMENTS[(r.step() as usize) % FRAGMENTS.len()].as_bytes());
        }
    }
    out
}

#[test]
fn random_streams_never_panic_and_stay_bounded() {
    for seed in 1..=60u64 {
        let bytes = stream(seed, 3000);
        let mut t = term(6 + (seed % 7) as usize, 10 + (seed % 23) as usize);
        let now = Instant::now();
        for (i, c) in bytes.chunks(1 + (seed as usize % 37)).enumerate() {
            t.feed(c, now);
            if i % 50 == 0 {
                let cur = t.cursor();
                assert!(cur.y < t.rows() && cur.x < t.cols(), "cursor out of bounds");
            }
        }
        assert!(t.replies().len_bytes() <= 64 * 1024);
        assert!(t.events_ref().marks.len() <= 256);
        assert!(t.events_ref().clipboard.len() <= 4);
        assert!(t.title().len() <= 4096);
        let cur = t.cursor();
        assert!(cur.y < t.rows() && cur.x < t.cols());
    }
}

#[test]
fn random_streams_are_chunking_invariant() {
    for seed in 100..130u64 {
        let bytes = stream(seed, 800);
        let cfg = || TermConfig {
            rows: 8,
            cols: 20,
            ..TermConfig::default()
        };
        let whole = digest(cfg(), &bytes, Chunking::Whole);
        for ch in [
            Chunking::Size(1),
            Chunking::Size(3),
            Chunking::Size(7),
            Chunking::Size(4096),
            Chunking::Random(seed),
        ] {
            assert_eq!(digest(cfg(), &bytes, ch), whole, "seed {seed} {ch:?}");
        }
    }
}

#[test]
fn random_resizes_keep_state_consistent() {
    let mut r = Rng(0xdead_beef);
    let mut t = term(10, 30);
    let now = Instant::now();
    for round in 0..200u64 {
        let bytes = stream(round + 1000, 60);
        t.feed(&bytes, now);
        let rows = 1 + (r.step() % 20) as usize;
        let cols = 1 + (r.step() % 50) as usize;
        let (nr, nc) = t.resize(rows, cols);
        assert_eq!((t.rows(), t.cols()), (nr, nc));
        let c = t.cursor();
        assert!(c.y < nr && c.x < nc, "cursor {c:?} outside {nr}x{nc}");
        assert_eq!(t.primary().rows(), nr);
        assert_eq!(t.alternate().cols(), nc);
    }
}

#[test]
fn hostile_resize_is_clamped() {
    let mut t = term(5, 5);
    let (r, c) = t.resize(100_000, 100_000);
    assert!(r >= 1 && c >= 1 && r * c <= 1 << 22);
    let (r, c) = t.resize(0, 0);
    assert_eq!((r, c), (1, 1));
}

#[test]
fn strings_over_their_cap_are_dropped_not_truncated() {
    let mut t = term(3, 10);
    let mut s = String::from("\x1b]2;");
    s.push_str(&"a".repeat(5000));
    s.push('\x07');
    feed(&mut t, &s);
    assert!(t.events_ref().title.is_none());
    assert_eq!(t.title(), "");
}

#[test]
fn osc_52_is_tagged_program_origin() {
    let mut t = term(3, 10);
    feed(&mut t, "\x1b]52;c;aGk=\x07");
    let c = t.events().clipboard.pop_front().unwrap();
    assert!(c.program_origin);
    assert_eq!(c.data, b"hi");
}

#[test]
fn cpr_never_underflows_after_restoring_origin_cursor_outside_margins() {
    let mut t = term(24, 80);
    feed(&mut t, "\x1b[?6h\x1b7\x1b[5;10r\x1b8\x1b[6n");
    let got = String::from_utf8(t.replies().take_all()).unwrap();
    assert!(got.starts_with("\x1b[") && got.len() < 16, "{got:?}");
    let mut t = term(24, 80);
    feed(
        &mut t,
        "\x1b[?6h\x1b[?69h\x1b[?69h\x1b7\x1b[5;20s\x1b8\x1b[6n",
    );
    let got = String::from_utf8(t.replies().take_all()).unwrap();
    assert!(got.len() < 16, "{got:?}");
}
