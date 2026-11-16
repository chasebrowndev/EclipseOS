// SPDX-License-Identifier: AGPL-3.0-only
//! T-19 §4 / T-18 §2: XTGETTCAP replies keep the closed grammar for any payload.
#![no_main]
use libfuzzer_sys::fuzz_target;

fn hex_pairs(b: &[u8]) -> bool {
    b.len() % 2 == 0 && b.iter().all(|c| matches!(c, b'0'..=b'9' | b'A'..=b'F'))
}

fuzz_target!(|data: &[u8]| {
    let (tn, payload) = match data.split_first() {
        Some((&n, rest)) if rest.len() >= n as usize % 32 => rest.split_at(n as usize % 32),
        _ => (&[][..], data),
    };
    let term = String::from_utf8_lossy(tn);
    let replies = apoth_terminfo::xtgettcap(payload, &term);
    let items = payload.iter().filter(|&&c| c == b';').count() + 1;
    assert!(replies.len() <= items);
    for r in &replies {
        let body = r
            .strip_prefix(b"\x1bP")
            .and_then(|r| r.strip_suffix(b"\x1b\\"))
            .expect("framing");
        assert!(body.starts_with(b"0+r") || body.starts_with(b"1+r"));
        let rest = &body[3..];
        let (name, val) = match rest.iter().position(|&c| c == b'=') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        assert!(hex_pairs(name));
        if let Some(v) = val {
            assert!(hex_pairs(v) && body[0] == b'1');
        }
    }
    // Chunk-free and deterministic.
    assert_eq!(replies, apoth_terminfo::xtgettcap(payload, &term));
});
