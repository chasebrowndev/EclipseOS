// SPDX-License-Identifier: AGPL-3.0-only
//! Shared helpers for the `apoth-term` code tests: chunked feeding and a state digest.
//! The golden corpus itself lives in `apoth-testkit/fixtures` (T-25 §1); this module only
//! carries what the Rust-side tests need.
#![allow(dead_code, unreachable_pub)]

use std::time::Instant;

use apoth_grid::SnapRow;
use apoth_term::{Term, TermConfig, row_text};

#[derive(Clone, Copy, Debug)]
pub enum Chunking {
    Whole,
    Size(usize),
    Random(u64),
}

pub const CHUNKINGS: &[Chunking] = &[
    Chunking::Whole,
    Chunking::Size(1),
    Chunking::Size(7),
    Chunking::Size(4096),
    Chunking::Random(1),
    Chunking::Random(2),
    Chunking::Random(3),
];

fn hash(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    h
}

struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

pub fn split(bytes: &[u8], ch: Chunking, id: &str) -> Vec<Vec<u8>> {
    match ch {
        Chunking::Whole => vec![bytes.to_vec()],
        Chunking::Size(n) => bytes.chunks(n.max(1)).map(<[u8]>::to_vec).collect(),
        Chunking::Random(seed) => {
            let mut r = Rng(hash(id) ^ seed.wrapping_mul(0x9e3779b97f4a7c15) | 1);
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                let n = 1 + (r.step() % 16) as usize;
                let e = (i + n).min(bytes.len());
                out.push(bytes[i..e].to_vec());
                i = e;
            }
            out
        }
    }
}

pub fn show(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        match c {
            0x1b => s.push_str("\\e"),
            0x07 => s.push_str("\\a"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s
}

/// Digest of everything observable after feeding `bytes` in the given chunking, for the
/// chunking-invariance tests and the random-stream property test.
pub fn digest(cfg: TermConfig, bytes: &[u8], ch: Chunking) -> String {
    let mut term = Term::new(cfg);
    let now = Instant::now();
    for c in split(bytes, ch, "digest") {
        term.feed(&c, now);
    }
    let mut s = String::new();
    for y in 0..term.rows() {
        s.push_str(&row_text(term.grid(), y));
        s.push('|');
    }
    let c = term.cursor();
    s.push_str(&format!(
        "{:?} hist={} alt={} hold={} ",
        c,
        term.primary().history_rows(),
        term.on_alternate(),
        term.presentation_hold()
    ));
    s.push_str(&show(&term.replies().take_all()));
    s.push_str(&format!("{:?}", term.events().render()));
    s.push_str(&format!("{:?}", term.input_mirror().load()));
    let mut row = SnapRow::default();
    for y in 0..term.rows() {
        term.grid().copy_row(y, &mut row);
        for c in &row.cells {
            s.push_str(&format!("{:?}", c.style));
        }
    }
    s
}
