// SPDX-License-Identifier: Apache-2.0
//! VTCORE-55 chunk invariance, fast/slow UTF-8 parity (UNICODE-01), hostile input.

mod common;
use apoth_vt::Parser;
use apoth_vt::bench::{CountSink, corpus_ascii, corpus_csi, corpus_dense};
use common::*;

fn check_all_splits(bytes: &[u8], rec: &Rec) {
    let (whole, pw) = run_with(rec.clone(), &[bytes]);
    for k in 0..=bytes.len() {
        let (a, pa) = run_with(rec.clone(), &[&bytes[..k], &bytes[k..]]);
        assert_eq!(whole, a, "split at {k} of {bytes:?}");
        assert_eq!(
            pw.is_idle(),
            pa.is_idle(),
            "idle differs, split {k} of {bytes:?}"
        );
    }
    let singles: Vec<&[u8]> = bytes.chunks(1).collect();
    let (s, _) = run_with(rec.clone(), &singles);
    assert_eq!(whole, s, "byte-at-a-time of {bytes:?}");
}

#[test]
fn every_split_point_corpus() {
    let corpus: &[&[u8]] = &[
        b"plain text\r\nsecond line\ttab",
        b"\x1b[1;31mred\x1b[0m \x1b[38:2::1:2:3m",
        b"\x1b[?1049h\x1b[2J\x1b[H",
        b"\x1b]0;window title\x07\x1b]8;;http://example\x1b\\link\x1b]8;;\x1b\\",
        b"\x1b]1337;File=name=x:AAAA\x07",
        b"\x1bP1$qm\x1b\\\x1bP>|abc\x1b\\",
        b"\x1b_Gi=1,a=T;AAAA\x1b\\\x1b^pm\x1b\\\x1bXsos\x1b\\",
        "h\u{e9}llo \u{4e16}\u{754c} \u{1f600}".as_bytes(),
        b"\xc3\x28\xe2\x82\x28\xf0\x9f\x98\x28\xed\xa0\x80\xff\xc0\x80",
        b"\x1b[1\x18x\x1b[1\x1b[2m\x1b]0;a\x18b\x1b]2;t\x1b[1m",
        b"\x1bP1\x18x\x1bP1<q abc\x1b\\y",
        b"\x1b[ !\"#$mx\x1b[?1?2hy",
    ];
    let caps = [
        Rec::default(),
        Rec::with_caps(4, 3, 2),
        Rec::with_caps(1, 1, 1),
    ];
    for c in corpus {
        for r in &caps {
            check_all_splits(c, r);
        }
    }
}

#[test]
fn every_split_point_random() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let caps = [Rec::default(), Rec::with_caps(6, 5, 4)];
    for _ in 0..400 {
        let s = gen_stream(&mut rng, 24);
        for r in &caps {
            check_all_splits(&s, r);
        }
    }
}

#[test]
fn random_chunkings() {
    let mut rng = Rng(42);
    for _ in 0..300 {
        let s = gen_stream(&mut rng, 40);
        let (whole, _) = run_with(Rec::default(), &[&s]);
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut at = 0;
        while at < s.len() {
            let n = 1 + rng.below(9);
            let end = (at + n).min(s.len());
            chunks.push(&s[at..end]);
            at = end;
        }
        let (got, _) = run_with(Rec::default(), &chunks);
        assert_eq!(whole, got, "{s:?}");
    }
}

#[test]
fn arbitrary_bytes_never_panic_and_stay_invariant() {
    let mut rng = Rng(7);
    for _ in 0..300 {
        let n = rng.below(200);
        let s: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        check_all_splits(&s, &Rec::with_caps(8, 8, 8));
    }
}

#[test]
fn utf8_fast_path_matches_lossy_and_scalar() {
    let mut rng = Rng(99);
    for _ in 0..2000 {
        let n = rng.below(64);
        let mut body: Vec<u8> = Vec::new();
        for _ in 0..n {
            // printable ASCII, any high byte; no C0/DEL
            let b = if rng.below(2) == 0 {
                0x20 + rng.below(0x5F) as u8
            } else {
                0x80 + rng.below(0x80) as u8
            };
            body.push(b);
        }
        let mut input = body.clone();
        input.push(b'\n'); // flushes any carried partial sequence
        let ev = run(&input);
        let mut text = String::new();
        for e in &ev {
            if let Ev::Print(s) = e {
                text.push_str(s);
            }
        }
        assert_eq!(text, String::from_utf8_lossy(&body), "{body:?}");
        // scalar (byte at a time) path agrees
        let singles: Vec<&[u8]> = input.chunks(1).collect();
        assert_eq!(ev, run_with(Rec::default(), &singles).0);
    }
}

#[test]
fn bench_corpora_are_invariant_and_parse() {
    for c in [corpus_ascii(4096), corpus_dense(4096), corpus_csi(4096)] {
        let mut whole = CountSink::default();
        Parser::new().advance(&c, &mut whole);
        let mut parts = CountSink::default();
        let mut p = Parser::new();
        for ch in c.chunks(37) {
            p.advance(ch, &mut parts);
        }
        assert_eq!(whole.text_bytes, parts.text_bytes);
        assert_eq!(whole.csis, parts.csis);
        assert_eq!(whole.controls, parts.controls);
    }
    let mut s = CountSink::default();
    Parser::new().advance(&corpus_csi(4096), &mut s);
    assert!(s.csis > 100);
}

/// The whole-sequence CSI path (one chunk holds ESC, parameters and final) must match the
/// table path (any split point, or byte at a time) for every parameter shape, including
/// overflowing lists, huge numbers, private markers, intermediates and embedded controls.
#[test]
fn csi_whole_sequence_matches_table_path() {
    let mut rng = Rng(0xC51_C51);
    let params = b"0123456789;:";
    let tails = b"@AHJKmhlrsu?<>=!$ \"'";
    for _ in 0..600 {
        let mut s = Vec::new();
        for _ in 0..1 + rng.below(4) {
            s.extend_from_slice(b"\x1b[");
            for _ in 0..rng.below(80) {
                s.push(params[rng.below(params.len())]);
            }
            match rng.below(6) {
                0 => s.push(0x07),
                1 => s.push(tails[rng.below(tails.len())]),
                _ => {}
            }
            s.push(0x40 + rng.below(0x3F) as u8);
            if rng.below(3) == 0 {
                s.extend_from_slice(b"text");
            }
        }
        check_all_splits(&s, &Rec::default());
        check_all_splits(&s, &Rec::with_caps(6, 5, 4));
    }
}
