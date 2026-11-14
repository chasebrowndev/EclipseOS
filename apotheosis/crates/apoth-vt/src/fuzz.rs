// SPDX-License-Identifier: Apache-2.0
//! Stable entry point for the `vt_parse` fuzz target (T-01 §5, T-18 §11). Arbitrary bytes
//! go through the parser twice, whole and in a data-derived chunking; it panics only when
//! an invariant breaks: the two action streams differ (after coalescing the only
//! chunk-dependent edges, adjacent text runs and adjacent payload slices), a text run
//! carries a control, a string delivers more than its cap, or a parameter/intermediate
//! list exceeds its fixed bound.

use crate::limits::{MAX_INTERMEDIATES, MAX_PARAMS};
use crate::{Params, Parser, Sink, StrEnd, StrHead, StrKind, StrRoute};

/// Per-kind payload caps used by the target. Small, so the cap path is exercised.
const CAPS: [u32; 3] = [64, 48, 32];

/// A canonical transcript of the action stream.
#[derive(Default)]
struct Transcript {
    out: Vec<u8>,
    /// Tag of the last record (`P` text, `U` payload), for coalescing.
    last: u8,
    /// Cap of the open string and bytes delivered into it.
    cap: u32,
    put: u64,
    open: bool,
}

impl Transcript {
    fn tag(&mut self, t: u8) {
        self.out.push(0);
        self.out.push(t);
        self.last = t;
    }
}

impl Sink for Transcript {
    fn print_run(&mut self, text: &str) {
        assert!(!text.is_empty(), "empty text run");
        assert!(
            text.chars().all(|c| c >= ' ' && c != '\u{7f}'),
            "text run carries a control"
        );
        if self.last != b'P' {
            self.tag(b'P');
        }
        self.out.extend_from_slice(text.as_bytes());
    }

    fn execute(&mut self, c0: u8) {
        assert!(c0 < 0x20, "execute of a non-C0 byte");
        self.tag(b'X');
        self.out.push(c0);
    }

    fn esc(&mut self, inter: &[u8], fin: u8) {
        assert!(inter.len() <= MAX_INTERMEDIATES);
        self.tag(b'E');
        self.out.extend_from_slice(inter);
        self.out.push(fin);
    }

    fn csi(&mut self, p: &Params, inter: &[u8], private: u8, fin: u8) {
        assert!(p.len() <= MAX_PARAMS, "params over their bound");
        assert!(inter.len() <= MAX_INTERMEDIATES);
        self.tag(b'C');
        self.out.push(private);
        for v in p.iter() {
            self.out
                .extend_from_slice(&v.map_or(u32::MAX, |v| v).to_le_bytes());
        }
        self.out.extend_from_slice(&p.colon_mask().to_le_bytes());
        self.out.push(u8::from(p.overflow()));
        self.out.extend_from_slice(inter);
        self.out.push(fin);
    }

    fn string_start(&mut self, kind: StrKind, head: &StrHead) -> StrRoute {
        assert!(!self.open, "string started inside a string");
        self.tag(b'S');
        self.out.push(kind as u8);
        self.out.extend_from_slice(head.bytes());
        self.out.push(u8::from(head.complete()));
        self.cap = match kind {
            StrKind::Osc => CAPS[0],
            StrKind::Dcs => CAPS[1],
            StrKind::Apc => CAPS[2],
            StrKind::Pm | StrKind::Sos => 0,
        };
        self.put = 0;
        self.open = true;
        StrRoute { cap: self.cap }
    }

    fn string_put(&mut self, chunk: &[u8]) {
        assert!(self.open, "payload outside a string");
        assert!(!chunk.is_empty(), "empty payload slice");
        self.put += chunk.len() as u64;
        assert!(self.put <= u64::from(self.cap), "payload over its cap");
        if self.last != b'U' {
            self.tag(b'U');
        }
        self.out.extend_from_slice(chunk);
    }

    fn string_end(&mut self, end: StrEnd) {
        self.open = false;
        self.tag(b'Z');
        self.out.push(end as u8);
    }
}

/// Run the parser over `data` whole and chunked; panics only on a broken invariant.
pub fn fuzz_parse(data: &[u8]) {
    let mut whole = Transcript::default();
    let mut p = Parser::new();
    p.advance(data, &mut whole);

    let mut split = Transcript::default();
    let mut q = Parser::new();
    let mut rest = data;
    while let Some(&b) = rest.first() {
        let take = (usize::from(b % 13) + 1).min(rest.len());
        let (head, tail) = rest.split_at(take);
        q.advance(head, &mut split);
        rest = tail;
    }
    assert!(whole.out == split.out, "chunking changed the action stream");
    assert_eq!(p.is_idle(), q.is_idle(), "chunking changed the final state");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advance_until_stops_after_the_paused_string_end_and_resumes_losslessly() {
        let data = b"a\x1b_Gx\x1b\\b\x1b]0;t\x07c\x1bPq#\x1b\\d";
        let mut whole = Transcript::default();
        Parser::new().advance(data, &mut whole);
        let mut paused = Transcript::default();
        let mut p = Parser::new();
        let mut rest: &[u8] = data;
        let mut stops = Vec::new();
        while !rest.is_empty() {
            let n = p.advance_until(rest, &mut paused, |t| t.last == b'Z');
            stops.push(rest[..n].to_vec());
            rest = &rest[n..];
            paused.last = 0; // resume: the next pause needs a new end
        }
        assert_eq!(
            stops[0], b"a\x1b_Gx\x1b\\",
            "stops right after the first ST"
        );
        assert!(
            stops.len() >= 3,
            "every string end can stop: {}",
            stops.len()
        );
        assert!(whole.out == paused.out, "pausing changed the action stream");
    }

    #[test]
    fn corpus_seeds_never_panic() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/vt_parse");
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            fuzz_parse(&std::fs::read(entry.unwrap().path()).unwrap());
            n += 1;
        }
        assert!(n >= 6);
    }

    #[test]
    fn pseudo_random_streams_hold_the_invariants() {
        let mut x = 0x9e37_79b9_7f4a_7c15_u64;
        for len in 0..300 {
            let mut v = Vec::with_capacity(len);
            for _ in 0..len {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                // Bias towards the interesting bytes.
                let b = x as u8;
                v.push(match b % 5 {
                    0 => 0x1b,
                    1 => b"[]P_^X\\;:?07"[usize::from(b >> 4) % 12],
                    _ => b,
                });
            }
            fuzz_parse(&v);
        }
    }
}
