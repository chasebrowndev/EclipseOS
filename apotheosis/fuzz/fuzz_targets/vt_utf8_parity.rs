// SPDX-License-Identifier: AGPL-3.0-only
//! UNICODE-01: the bulk UTF-8 path against a byte-at-a-time reference. Control bytes are
//! removed from the input so the only action is text; the reference is the W3C
//! maximal-subpart decoder (`String::from_utf8_lossy`). A final LF flushes a pending tail.
#![no_main]
use apoth_vt::{Params, Parser, Sink, StrEnd, StrHead, StrKind, StrRoute};
use libfuzzer_sys::fuzz_target;

#[derive(Default)]
struct Text(String);

impl Sink for Text {
    fn print_run(&mut self, text: &str) {
        assert!(!text.is_empty());
        self.0.push_str(text);
    }
    fn execute(&mut self, _c0: u8) {}
    fn esc(&mut self, _i: &[u8], _f: u8) {}
    fn csi(&mut self, _p: &Params, _i: &[u8], _pr: u8, _f: u8) {}
    fn string_start(&mut self, _k: StrKind, _h: &StrHead) -> StrRoute {
        StrRoute::SKIP
    }
    fn string_put(&mut self, _c: &[u8]) {}
    fn string_end(&mut self, _e: StrEnd) {}
}

fuzz_target!(|data: &[u8]| {
    let Some((&cut, rest)) = data.split_first() else {
        return;
    };
    let mut bytes: Vec<u8> = rest.iter().copied().filter(|&b| b >= 0x20 && b != 0x7f).collect();
    let expect = String::from_utf8_lossy(&bytes).into_owned();
    bytes.push(b'\n');
    let k = (cut as usize).min(bytes.len());

    // Bulk: one call.
    let mut whole = Text::default();
    Parser::new().advance(&bytes, &mut whole);
    assert_eq!(whole.0, expect, "bulk path");

    // Split once (exercises the carry decoder across the cut).
    let mut split = Text::default();
    let mut p = Parser::new();
    p.advance(&bytes[..k], &mut split);
    p.advance(&bytes[k..], &mut split);
    assert_eq!(split.0, expect, "split path");

    // Reference: one byte at a time (scalar carry decoder only).
    let mut slow = Text::default();
    let mut p = Parser::new();
    for b in &bytes {
        p.advance(core::slice::from_ref(b), &mut slow);
    }
    assert_eq!(slow.0, expect, "byte-at-a-time path");
});
