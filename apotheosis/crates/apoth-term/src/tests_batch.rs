// SPDX-License-Identifier: AGPL-3.0-only
//! Differential check: the batched text path (`Machine::print_chars`) must leave exactly the
//! state that printing one character at a time leaves, for any mix of ASCII, wide, combining
//! and joiner characters, wraps, margins, autowrap on and off, and insert mode.

use std::time::Instant;

use apoth_grid::SnapRow;

use crate::{Term, TermConfig};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const TOKENS: &[&str] = &[
    "a",
    "hello world",
    "the quick brown fox jumps over",
    " ",
    "\u{e9}",
    "caf\u{e9} ",
    "\u{4e16}\u{754c}",
    "\u{1f600}",
    "\u{1f468}\u{200d}\u{1f469}",
    "e\u{301}",
    "\u{301}",
    "\u{200d}",
    "\u{fe0f}",
    "\u{2764}\u{fe0f}",
    "\u{2764}\u{fe0e}",
    "\u{1f1fa}\u{1f1f8}",
    "\u{7f}",
    "\r\n",
    "\n",
    "\r",
    "\x08",
    "\t",
    "\x1b[1m",
    "\x1b[31m",
    "\x1b[0m",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b[4h",
    "\x1b[4l",
    "\x1b[2;4H",
    "\x1b[1;1H",
    "\x1b[3C",
    "\x1b[K",
    "\x1b[2J",
    "\x1b[?69h\x1b[3;6s",
    "\x1b[?69l",
    "\x1b(0qx\x1b(B",
    "\x1b[2b",
    "\x1b[?2027h",
    "\x1b[?2027l",
];

fn dump(t: &Term) -> String {
    let mut out = format!("{:?}\n", t.cursor());
    let mut row = SnapRow::default();
    for y in 0..t.rows() {
        t.grid().copy_row(y, &mut row);
        out.push_str(&format!("{} w={}:", y, row.wrapped));
        for c in &row.cells {
            out.push_str(&format!(
                "[{:x} {:?} {} {:?} {}{}]",
                c.cp,
                c.wide,
                c.cluster_len,
                c.style,
                u8::from(c.link),
                u8::from(c.protected)
            ));
            for cp in row.cluster(c) {
                out.push_str(&format!("+{cp:x}"));
            }
        }
        out.push('\n');
    }
    out
}

fn run(cols: usize, rows: usize, input: &[u8], no_batch: bool) -> String {
    let mut t = Term::new(TermConfig {
        rows,
        cols,
        ..TermConfig::default()
    });
    t.m.no_batch = no_batch;
    let _ = t.feed(input, Instant::now());
    dump(&t)
}

#[test]
fn batched_text_matches_one_at_a_time() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for case in 0..3000 {
        let cols = [3usize, 5, 6, 7, 11, 20, 40][rng.below(7)];
        let rows = 2 + rng.below(6);
        let mut input = Vec::new();
        for _ in 0..40 {
            input.extend_from_slice(TOKENS[rng.below(TOKENS.len())].as_bytes());
        }
        let a = run(cols, rows, &input, false);
        let b = run(cols, rows, &input, true);
        assert_eq!(
            a,
            b,
            "case {case} cols {cols} rows {rows} input {:?}",
            String::from_utf8_lossy(&input)
        );
    }
}
