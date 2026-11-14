// SPDX-License-Identifier: AGPL-3.0-only
//! Stable entry points for the `osc_all` and `kitty_keyboard_decode` fuzz targets (T-18
//! §11). They panic only when a security invariant breaks:
//!
//! * a reply that is not in the closed reply grammar (T-18 §2): every reply is a run of
//!   `CSI <printable> <final>`, `OSC|DCS|APC <printable> (BEL | ESC \)`; no payload byte
//!   from the request can smuggle a control into it;
//! * a denied class with an observable effect or a reply (T-18 §3, §11);
//! * a title carrying a control character;
//! * kitty keyboard flags outside their five bits, or the published mirror disagreeing
//!   with the term.

use std::time::Instant;

use crate::{Term, TermConfig, deny};

/// OSC numbers the target addresses (byte 0 of the input picks one), and the deny bit
/// that gates each (0: not policy-gated).
const OSCS: &[(u32, u32)] = &[
    (0, deny::TITLE_SET),
    (1, deny::TITLE_SET),
    (2, deny::TITLE_SET),
    (4, deny::COLOUR),
    (7, 0),
    (8, deny::HYPERLINK),
    (9, deny::NOTIFICATION),
    (10, deny::COLOUR),
    (11, deny::COLOUR),
    (12, deny::COLOUR),
    (17, deny::COLOUR),
    (19, deny::COLOUR),
    (22, 0),
    (50, 0),
    (
        52,
        deny::CLIPBOARD_WRITE | deny::CLIPBOARD_READ | deny::PRIMARY,
    ),
    (99, deny::NOTIFICATION),
    (104, deny::COLOUR),
    (110, deny::COLOUR),
    (111, deny::COLOUR),
    (112, deny::COLOUR),
    (133, 0),
    (633, 0),
    (666, 0),
    (777, deny::NOTIFICATION),
    (1337, deny::USER_VAR),
    (3008, 0),
    (5522, 0),
];

/// Every deny bit.
const DENY_ALL: u32 = deny::TITLE_SET
    | deny::TITLE_REPORT
    | deny::WINDOW_REPORT
    | deny::COLOUR
    | deny::NOTIFICATION
    | deny::HYPERLINK
    | deny::CLIPBOARD_WRITE
    | deny::CLIPBOARD_READ
    | deny::PRIMARY
    | deny::USER_VAR;

fn term(deny_bits: u32) -> Term {
    Term::new(TermConfig {
        rows: 3,
        cols: 16,
        deny: deny_bits,
        ..TermConfig::default()
    })
}

type Observed = (Vec<String>, String, Vec<String>, Vec<u8>);

/// Everything a sequence can change that a user or program can observe.
fn observe(t: &mut Term) -> Observed {
    let rows = (0..t.rows()).map(|y| t.row_text(y)).collect();
    let mut ev = t.events_ref().render();
    for i in 0..=255u8 {
        ev.push(format!("{:?}", t.palette(i)));
    }
    ev.push(format!("{:?}", t.colors()));
    let link = t.grid().link_at(0, 0).map(|(_, u)| u).unwrap_or_default();
    ev.push(format!("link {link}"));
    let replies = t.replies().take_all();
    (rows, t.title().to_string(), ev, replies)
}

/// True when `r` is a concatenation of closed-grammar replies.
pub fn reply_is_closed(r: &[u8]) -> bool {
    let printable = |b: u8| (0x20..0x7f).contains(&b);
    let mut i = 0;
    while i < r.len() {
        if r[i] != 0x1b || i + 1 >= r.len() {
            return false;
        }
        match r[i + 1] {
            b'[' => {
                i += 2;
                loop {
                    match r.get(i) {
                        Some(&b) if (0x40..0x7f).contains(&b) => break,
                        Some(&b) if (0x20..0x40).contains(&b) => i += 1,
                        _ => return false,
                    }
                }
                i += 1;
            }
            b']' | b'P' | b'_' => {
                i += 2;
                loop {
                    match r.get(i) {
                        Some(0x07) => {
                            i += 1;
                            break;
                        }
                        Some(0x1b) if r.get(i + 1) == Some(&b'\\') => {
                            i += 2;
                            break;
                        }
                        Some(&b) if printable(b) => i += 1,
                        _ => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

fn check_term(t: &Term) {
    assert!(
        !t.title().chars().any(char::is_control),
        "a control reached the title"
    );
}

/// Input: byte 0 picks the OSC number, byte 1 the terminator and a query form, the rest is
/// the payload with the bytes that end a string (ESC, BEL, CAN, SUB) removed so it stays
/// inside the sequence.
pub fn fuzz_osc(data: &[u8]) {
    let [n, form, payload @ ..] = data else {
        return;
    };
    let (num, gate) = OSCS[usize::from(*n) % OSCS.len()];
    let body: Vec<u8> = payload
        .iter()
        .copied()
        .filter(|b| !matches!(b, 0x1b | 0x07 | 0x18 | 0x1a))
        .collect();
    // `OSC 9 ; 4` is ConEmu progress, not a notification (T-18 §3 gates notifications).
    let gate = if num == 9 && (body.starts_with(b"4;") || body == b"4") {
        0
    } else {
        gate
    };
    let mut seq = format!("\x1b]{num}").into_bytes();
    if form & 4 == 0 || !body.is_empty() {
        seq.push(b';');
    }
    seq.extend_from_slice(&body);
    if form & 8 != 0 {
        seq.extend_from_slice(b";?");
    }
    seq.extend_from_slice(if form & 1 == 0 { b"\x07" } else { b"\x1b\\" });
    if form & 2 != 0 {
        // Text after the sequence: a hyperlink or title must not leak beyond it.
        seq.extend_from_slice(b"ab\x1b]8;;\x1b\\");
    }

    let now = Instant::now();
    for bits in [0, DENY_ALL] {
        let mut t = term(bits);
        t.feed(&seq, now);
        check_term(&t);
        let seen = observe(&mut t);
        assert!(reply_is_closed(&seen.3), "reply outside the closed grammar");
        if bits == DENY_ALL && gate != 0 {
            let mut fresh = term(bits);
            if form & 2 != 0 {
                fresh.feed(b"ab\x1b]8;;\x1b\\", now);
            }
            assert!(seen.3.is_empty(), "a denied OSC {num} replied");
            let base = observe(&mut fresh);
            assert!(
                seen == base,
                "a denied OSC {num} had an effect: {seen:?} vs {base:?}"
            );
        }
    }
}

/// Input: a list of operations, two bytes each. Byte 0 picks the form (`CSI > f u` push,
/// `CSI < n u` pop, `CSI = f ; m u` set, `CSI ? u` query, alt-screen switch, RIS), byte 1
/// is the parameter (values past 255 are made from the high bit).
pub fn fuzz_kitty_keyboard(data: &[u8]) {
    let mut t = term(0);
    let now = Instant::now();
    for op in data.chunks(2) {
        let [form, p] = *op else { break };
        let p = if form & 0x80 != 0 {
            u32::from(p) * 65_537
        } else {
            u32::from(p)
        };
        let seq = match form % 7 {
            0 => format!("\x1b[>{p}u"),
            1 => format!("\x1b[<{p}u"),
            2 => format!("\x1b[={p};{}u", (form >> 3) % 5),
            3 => "\x1b[?u".to_string(),
            4 => "\x1b[?1049h".to_string(),
            5 => "\x1b[?1049l".to_string(),
            _ => format!("\x1b[>{p};{p}u\x1b[<u\x1b[={p}u"),
        };
        t.feed(seq.as_bytes(), now);
        let f = t.kitty_flags();
        assert!(f < 32, "kitty flags outside their five bits");
        assert_eq!(
            t.input_mirror().load().kitty_flags(),
            f,
            "published mirror disagrees with the term"
        );
        let r = t.replies().take_all();
        if form % 7 == 3 {
            assert_eq!(r, format!("\x1b[?{f}u").into_bytes(), "query reply");
        } else {
            assert!(reply_is_closed(&r));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(name: &str, f: fn(&[u8])) -> usize {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("corpus")
            .join(name);
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            f(&std::fs::read(entry.unwrap().path()).unwrap());
            n += 1;
        }
        n
    }

    #[test]
    fn osc_corpus_seeds_hold_the_invariants() {
        assert!(corpus("osc_all", fuzz_osc) >= 8);
    }

    #[test]
    fn kitty_corpus_seeds_hold_the_invariants() {
        assert!(corpus("kitty_keyboard_decode", fuzz_kitty_keyboard) >= 4);
    }

    #[test]
    fn every_osc_with_hostile_payloads() {
        let payloads: &[&[u8]] = &[
            b"",
            b"?",
            b"c;?",
            b"p;aGk=",
            b"1;rgb:ff/00/00",
            b"1;?;2;?",
            b"notify;\xe2\x80\xaetitle;body\x9b31m",
            b"id=x;file:///etc/passwd",
            b"A;k=s",
            b"\x00\x01\x7f\x9c\x9d",
            b";;;;;;;;;;;;;;;;;;;;;;;;;;;;;;",
        ];
        for n in 0..OSCS.len() as u8 {
            for form in 0..16u8 {
                for p in payloads {
                    let mut d = vec![n, form];
                    d.extend_from_slice(p);
                    fuzz_osc(&d);
                }
            }
        }
    }

    #[test]
    fn closed_grammar_accepts_replies_and_rejects_injection() {
        assert!(reply_is_closed(b""));
        assert!(reply_is_closed(
            b"\x1b[?1;2c\x1b]11;rgb:0000/0000/0000\x1b\\"
        ));
        assert!(reply_is_closed(b"\x1bP1$r0m\x1b\\\x1b]l\x07"));
        assert!(!reply_is_closed(b"\x1b]2;a\x1b]0;b\x07"));
        assert!(!reply_is_closed(b"\x1b[1\x0d;2c"));
        assert!(!reply_is_closed(b"x"));
        assert!(!reply_is_closed(b"\x1b]52;c;\x9c"));
    }

    #[test]
    fn kitty_ops_cover_every_form() {
        let mut d = Vec::new();
        for form in 0..=255u8 {
            d.extend_from_slice(&[form, form.wrapping_mul(37)]);
        }
        fuzz_kitty_keyboard(&d);
    }
}
