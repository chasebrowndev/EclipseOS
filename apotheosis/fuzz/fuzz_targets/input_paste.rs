// SPDX-License-Identifier: AGPL-3.0-only
//! apoth-input paste sanitiser: no control byte survives, invalid UTF-8 becomes U+FFFD, the
//! bracketed frame cannot be closed from inside, and the size limit is exact.
#![no_main]
use apoth_input::modes::bit;
use apoth_input::{InputModes, PasteError, PasteNewline, PasteOptions, encode_paste, encode_text};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&f, src)) = data.split_first() else {
        return;
    };
    let bracket = f & 1 != 0;
    let modes = InputModes::default().with(bit::BRACKETED_PASTE, bracket);
    let opts = PasteOptions {
        newline: match (f >> 1) % 3 {
            0 => PasteNewline::AsIs,
            1 => PasteNewline::Cr,
            _ => PasteNewline::Confirm,
        },
        trim_trailing_newline: f & 8 != 0,
        max_bytes: if f & 16 != 0 { src.len().saturating_sub(1) } else { 1 << 20 },
    };
    let mut out = vec![0xAA];
    match encode_paste(src, modes, &opts, &mut out) {
        Err(PasteError::TooLarge) => {
            assert!(src.len() > opts.max_bytes);
            assert_eq!(out, [0xAA], "out untouched on error");
        }
        Ok(()) => {
            assert!(src.len() <= opts.max_bytes);
            let body = &out[1..];
            let inner = if bracket {
                let b = body.strip_prefix(b"\x1b[200~").expect("open frame");
                b.strip_suffix(b"\x1b[201~").expect("close frame")
            } else {
                body
            };
            let text = core::str::from_utf8(inner).expect("output is UTF-8");
            for c in text.chars() {
                assert!(
                    !(c < ' ' && !matches!(c, '\t' | '\n' | '\r'))
                        && c != '\u{7f}'
                        && !('\u{80}'..='\u{9f}').contains(&c),
                    "control survived"
                );
            }
            if !bracket && opts.newline != PasteNewline::AsIs {
                assert!(!text.contains('\n'), "LF not translated");
            }
        }
    }
    // IME text path: only valid UTF-8 reaches it; no control scalar may come out.
    if let Ok(t) = core::str::from_utf8(src) {
        let mut o = Vec::new();
        encode_text(t, InputModes::default(), &mut o);
        if let Ok(s) = core::str::from_utf8(&o) {
            assert!(!s.chars().any(|c| c < ' ' || c == '\u{7f}' || ('\u{80}'..='\u{9f}').contains(&c)));
        }
    }
});
