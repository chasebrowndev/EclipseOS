// SPDX-License-Identifier: Apache-2.0
//! Bench corpora and harness hooks (T-23 `THR-ASCII`, `THR-DENSE`, `THR-CSI`).
//! Deterministic generators; the measuring harness lives in the bench crate (M9/M13).
#![allow(missing_docs)]

use crate::{Params, Sink, StrEnd, StrHead, StrKind, StrRoute};

/// A sink that only counts, for parser-only throughput lines.
#[derive(Debug, Default, Clone)]
pub struct CountSink {
    pub text_bytes: u64,
    pub controls: u64,
    pub escapes: u64,
    pub csis: u64,
    pub strings: u64,
    pub payload_bytes: u64,
}

impl Sink for CountSink {
    fn print_run(&mut self, text: &str) {
        self.text_bytes += text.len() as u64;
    }
    fn execute(&mut self, _c0: u8) {
        self.controls += 1;
    }
    fn esc(&mut self, _inter: &[u8], _fin: u8) {
        self.escapes += 1;
    }
    fn csi(&mut self, _p: &Params, _inter: &[u8], _private: u8, _fin: u8) {
        self.csis += 1;
    }
    fn string_start(&mut self, _kind: StrKind, _head: &StrHead) -> StrRoute {
        self.strings += 1;
        StrRoute { cap: u32::MAX }
    }
    fn string_put(&mut self, chunk: &[u8]) {
        self.payload_bytes += chunk.len() as u64;
    }
    fn string_end(&mut self, _end: StrEnd) {}
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

/// `THR-ASCII`: long lines of printable ASCII with LF/CR, about `len` bytes.
pub fn corpus_ascii(len: usize) -> Vec<u8> {
    let mut rng = Lcg(1);
    let mut out = Vec::with_capacity(len + 128);
    while out.len() < len {
        let n = 20 + rng.next() as usize % 100;
        for _ in 0..n {
            out.push(b' ' + (rng.next() % 95) as u8);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `THR-DENSE`: SGR colour changes every few characters, mixed UTF-8.
pub fn corpus_dense(len: usize) -> Vec<u8> {
    let mut rng = Lcg(2);
    let words = ["fn", "let", "héllo", "世界", "x", "42", "==", "→"];
    let mut out = Vec::with_capacity(len + 128);
    while out.len() < len {
        let c = 30 + rng.next() % 8;
        out.extend_from_slice(format!("\x1b[1;{c}m").as_bytes());
        out.extend_from_slice(words[rng.next() as usize % words.len()].as_bytes());
        out.extend_from_slice(b"\x1b[0m ");
        if rng.next().is_multiple_of(16) {
            out.extend_from_slice(b"\r\n");
        }
    }
    out
}

/// `THR-CSI`: cursor movement, erase and 24-bit colour sequences back to back.
pub fn corpus_csi(len: usize) -> Vec<u8> {
    let mut rng = Lcg(3);
    let mut out = Vec::with_capacity(len + 128);
    while out.len() < len {
        let s = match rng.next() % 4 {
            0 => format!("\x1b[{};{}H", 1 + rng.next() % 50, 1 + rng.next() % 200),
            1 => format!("\x1b[{}K", rng.next() % 3),
            2 => format!(
                "\x1b[38;2;{};{};{}m",
                rng.next() % 256,
                rng.next() % 256,
                rng.next() % 256
            ),
            _ => format!(
                "\x1b[48:2::{}:{}:{}m",
                rng.next() % 256,
                rng.next() % 256,
                rng.next() % 256
            ),
        };
        out.extend_from_slice(s.as_bytes());
    }
    out
}
