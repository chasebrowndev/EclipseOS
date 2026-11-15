// SPDX-License-Identifier: AGPL-3.0-only
//! T-02 §6 `vt_advance`: no panic, and any split of the input into chunks yields the same
//! action stream (text and string payloads coalesced; run edges are not part of the contract).
#![no_main]
use apoth_vt::{Params, Parser, Sink, StrEnd, StrHead, StrKind, StrRoute};
use libfuzzer_sys::fuzz_target;

#[derive(Default, PartialEq, Debug)]
struct Rec {
    ev: Vec<String>,
    text: String,
    payload: Vec<u8>,
}

impl Rec {
    fn flush(&mut self) {
        if !self.text.is_empty() {
            self.ev.push(format!("T{:?}", core::mem::take(&mut self.text)));
        }
        if !self.payload.is_empty() {
            self.ev.push(format!("P{:?}", core::mem::take(&mut self.payload)));
        }
    }
}

impl Sink for Rec {
    fn print_run(&mut self, text: &str) {
        self.text.push_str(text);
    }
    fn execute(&mut self, c0: u8) {
        self.flush();
        self.ev.push(format!("X{c0}"));
    }
    fn esc(&mut self, inter: &[u8], fin: u8) {
        self.flush();
        self.ev.push(format!("E{inter:?}{fin}"));
    }
    fn csi(&mut self, p: &Params, inter: &[u8], private: u8, fin: u8) {
        self.flush();
        self.ev.push(format!(
            "C{:?}/{}/{}/{inter:?}{private}{fin}",
            p.values(),
            p.colon_mask(),
            p.present_mask()
        ));
    }
    fn string_start(&mut self, kind: StrKind, head: &StrHead) -> StrRoute {
        self.flush();
        self.ev.push(format!("S{kind:?}{:?}", head.bytes()));
        // A small cap forces the over-cap path too.
        StrRoute { cap: 64 }
    }
    fn string_put(&mut self, chunk: &[u8]) {
        self.payload.extend_from_slice(chunk);
    }
    fn string_end(&mut self, end: StrEnd) {
        self.flush();
        self.ev.push(format!("Z{end:?}"));
    }
}

fn run(chunks: &[&[u8]]) -> Vec<String> {
    let mut rec = Rec::default();
    let mut p = Parser::new();
    for c in chunks {
        p.advance(c, &mut rec);
    }
    rec.flush();
    rec.ev
}

fuzz_target!(|data: &[u8]| {
    let Some((hdr, bytes)) = data.split_at_checked(2) else {
        return;
    };
    let a = (hdr[0] as usize).min(bytes.len());
    let b = a + (hdr[1] as usize).min(bytes.len() - a);
    let whole = run(&[bytes]);
    assert_eq!(whole, run(&[&bytes[..a], &bytes[a..]]));
    assert_eq!(whole, run(&[&bytes[..a], &bytes[a..b], &bytes[b..]]));
    // Byte at a time.
    if bytes.len() <= 4096 {
        let singles: Vec<&[u8]> = bytes.chunks(1).collect();
        assert_eq!(whole, run(&singles));
    }
});
