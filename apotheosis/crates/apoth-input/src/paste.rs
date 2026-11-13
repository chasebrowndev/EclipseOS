// SPDX-License-Identifier: Apache-2.0
//! Paste encoding and IME text (T-05 §4, §6; T-18 §4; INPUT-40, -58).
//!
//! The encoder defines no keys and no policy: the guard, confirm rules and `paste.*`
//! config belong to T-18 and the UI. A paste is one whole unit for the outbound queue,
//! bracket wrapper included.

use crate::kitty;
use crate::modes::{InputModes, bit};

/// `paste.newline` when bracketed paste is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PasteNewline {
    /// Leave newlines alone.
    AsIs,
    /// LF and CRLF become CR (default).
    #[default]
    Cr,
    /// The UI confirms first; once confirmed the paste is sent as `Cr`.
    Confirm,
}

/// Encoder inputs taken from `paste.*` config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasteOptions {
    /// `paste.newline`.
    pub newline: PasteNewline,
    /// `paste.trim-trailing-newline` (default false).
    pub trim_trailing_newline: bool,
    /// `paste.max-bytes` hard limit (default 5 MiB).
    pub max_bytes: usize,
}

impl Default for PasteOptions {
    fn default() -> Self {
        PasteOptions {
            newline: PasteNewline::Cr,
            trim_trailing_newline: false,
            max_bytes: 5 * 1024 * 1024,
        }
    }
}

/// Why a paste was not encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteError {
    /// Larger than `paste.max-bytes`; refused.
    TooLarge,
}

struct San {
    cr_mode: bool,
    prev_cr: bool,
}

impl San {
    fn push(&mut self, out: &mut Vec<u8>, c: char) {
        let was_cr = self.prev_cr;
        self.prev_cr = false;
        match c {
            '\t' => out.push(b'\t'),
            '\n' => {
                if !self.cr_mode {
                    out.push(b'\n');
                } else if !was_cr {
                    out.push(b'\r');
                }
            }
            '\r' => {
                out.push(b'\r');
                self.prev_cr = self.cr_mode;
            }
            // C0 to the U+2400 block.
            c if c < ' ' => push_char(out, char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}')),
            '\u{7f}' => push_char(out, '\u{2421}'),
            // C1 has no picture of its own: U+2426 SYMBOL FOR SUBSTITUTE FORM TWO.
            c if ('\u{80}'..='\u{9f}').contains(&c) => push_char(out, '\u{2426}'),
            c => push_char(out, c),
        }
    }
}

fn push_char(out: &mut Vec<u8>, c: char) {
    let mut b = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
}

/// Sanitise, newline-translate and (mode 2004) bracket `src`, appending to `out`.
///
/// Both modes: C0 other than TAB/LF/CR, DEL and C1 become visible pictures; invalid UTF-8
/// becomes U+FFFD; nothing is dropped. Mode 2004 on: wrapped in `ESC[200~ ... ESC[201~`,
/// newlines as-is (after sanitising no `ESC[201~` can remain, ESC is a picture). Mode 2004
/// off: newlines follow `opts.newline` (CRLF is one newline; every other LF or CR is its
/// own). `opts.max_bytes` is checked against the raw `src` length, before trimming,
/// sanitising and bracketing. On error `out` is untouched.
pub fn encode_paste(
    src: &[u8],
    modes: InputModes,
    opts: &PasteOptions,
    out: &mut Vec<u8>,
) -> Result<(), PasteError> {
    if src.len() > opts.max_bytes {
        return Err(PasteError::TooLarge);
    }
    let mut s = src;
    if opts.trim_trailing_newline {
        while let Some((&l, rest)) = s.split_last() {
            if l == b'\n' || l == b'\r' {
                s = rest;
            } else {
                break;
            }
        }
    }
    let bracket = modes.has(bit::BRACKETED_PASTE);
    out.reserve(s.len() + s.len() / 8 + 16);
    if bracket {
        out.extend_from_slice(b"\x1b[200~");
    }
    let mut st = San {
        cr_mode: !bracket && opts.newline != PasteNewline::AsIs,
        prev_cr: false,
    };
    let mut rest = s;
    loop {
        match core::str::from_utf8(rest) {
            Ok(t) => {
                t.chars().for_each(|c| st.push(out, c));
                break;
            }
            Err(e) => {
                let (ok, bad) = rest.split_at(e.valid_up_to());
                core::str::from_utf8(ok)
                    .unwrap_or("")
                    .chars()
                    .for_each(|c| st.push(out, c));
                st.push(out, '\u{fffd}');
                let skip = e.error_len().unwrap_or(bad.len()).max(1);
                rest = bad.get(skip..).unwrap_or(&[]);
            }
        }
    }
    if bracket {
        out.extend_from_slice(b"\x1b[201~");
    }
    Ok(())
}

/// Committed IME text (T-05 §4). Under kitty flags 8+16 it is `CSI 0;;cp:cp u`; otherwise
/// UTF-8. C0, DEL and C1 characters are dropped so an IME cannot inject control sequences.
pub fn encode_text(text: &str, modes: InputModes, out: &mut Vec<u8>) {
    fn keep(c: &char) -> bool {
        *c >= ' ' && *c != '\u{7f}' && !('\u{80}'..='\u{9f}').contains(c)
    }
    if !text.chars().any(|c| keep(&c)) {
        return;
    }
    if kitty::encode_text(text.chars().filter(keep), modes, out) {
        return;
    }
    for c in text.chars().filter(keep) {
        push_char(out, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paste(src: &[u8], bracket: bool, o: PasteOptions) -> Vec<u8> {
        let m = InputModes::default().with(bit::BRACKETED_PASTE, bracket);
        let mut out = Vec::new();
        encode_paste(src, m, &o, &mut out).unwrap();
        out
    }
    fn d() -> PasteOptions {
        PasteOptions::default()
    }

    #[test]
    fn bracketed_wraps_and_keeps_newlines() {
        assert_eq!(
            paste(b"a\nb\r\nc", true, d()),
            b"\x1b[200~a\nb\r\nc\x1b[201~"
        );
        assert_eq!(paste(b"", true, d()), b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn unbracketed_cr_newlines() {
        assert_eq!(paste(b"a\nb", false, d()), b"a\rb");
        assert_eq!(paste(b"a\r\nb", false, d()), b"a\rb");
        assert_eq!(paste(b"a\rb", false, d()), b"a\rb");
        assert_eq!(paste(b"a\n\nb", false, d()), b"a\r\rb");
        assert_eq!(paste(b"a\r\n\r\nb", false, d()), b"a\r\rb");
        assert_eq!(paste(b"a\n\rb", false, d()), b"a\r\rb");
        // Collapse is for CRLF pairs only: every other newline survives.
        assert_eq!(paste(b"a\r\n\nb", false, d()), b"a\r\rb");
        assert_eq!(paste(b"a\r\r\nb", false, d()), b"a\r\rb");
        assert_eq!(paste(b"a\r\n\n\nb", false, d()), b"a\r\r\rb");
        assert_eq!(paste(b"a\n\n\n", false, d()), b"a\r\r\r");
    }

    #[test]
    fn unbracketed_as_is_and_confirm() {
        let o = PasteOptions {
            newline: PasteNewline::AsIs,
            ..d()
        };
        assert_eq!(paste(b"a\nb\r\n", false, o), b"a\nb\r\n");
        let o = PasteOptions {
            newline: PasteNewline::Confirm,
            ..d()
        };
        assert_eq!(paste(b"a\nb", false, o), b"a\rb");
    }

    #[test]
    fn sanitises_in_both_modes() {
        for bracket in [false, true] {
            let out = paste(b"a\x1bb\x00c\x07d\x7fe\t", bracket, d());
            let body = if bracket {
                &out[6..out.len() - 6]
            } else {
                &out[..]
            };
            assert_eq!(
                core::str::from_utf8(body).unwrap(),
                "a\u{241b}b\u{2400}c\u{2407}d\u{2421}e\t"
            );
        }
        // C1 (U+0085 as UTF-8) is replaced.
        let out = paste("x\u{85}y\u{9f}z".as_bytes(), false, d());
        assert_eq!(core::str::from_utf8(&out).unwrap(), "x\u{2426}y\u{2426}z");
        // Non-control text survives.
        assert_eq!(
            paste("héllo 日本".as_bytes(), false, d()),
            "héllo 日本".as_bytes()
        );
    }

    #[test]
    fn end_marker_cannot_survive() {
        let out = paste(b"x\x1b[201~rm -rf ~\n", true, d());
        let s = core::str::from_utf8(&out).unwrap();
        assert!(s.starts_with("\x1b[200~") && s.ends_with("\x1b[201~"));
        assert_eq!(s.matches("\x1b[201~").count(), 1);
        assert!(s.contains("x\u{241b}[201~rm"));
    }

    #[test]
    fn invalid_utf8_is_replaced_not_dropped() {
        let out = paste(b"a\xffb\xc3", false, d());
        assert_eq!(core::str::from_utf8(&out).unwrap(), "a\u{fffd}b\u{fffd}");
    }

    #[test]
    fn trim_trailing_newline() {
        let o = PasteOptions {
            trim_trailing_newline: true,
            ..d()
        };
        assert_eq!(paste(b"ls\n", false, o), b"ls");
        assert_eq!(paste(b"ls\r\n\n", true, o), b"\x1b[200~ls\x1b[201~");
        assert_eq!(paste(b"a\nb", false, o), b"a\rb");
        assert_eq!(paste(b"ls\n", false, d()), b"ls\r");
    }

    #[test]
    fn too_large_is_refused_and_leaves_out_untouched() {
        let o = PasteOptions {
            max_bytes: 4,
            ..d()
        };
        let mut out = vec![1];
        assert_eq!(
            encode_paste(b"12345", InputModes::default(), &o, &mut out),
            Err(PasteError::TooLarge)
        );
        assert_eq!(out, [1]);
        assert!(encode_paste(b"1234", InputModes::default(), &o, &mut out).is_ok());
        assert_eq!(PasteOptions::default().max_bytes, 5 * 1024 * 1024);
    }

    #[test]
    fn appends_to_existing_buffer() {
        let mut out = b"X".to_vec();
        encode_paste(b"y", InputModes::default(), &d(), &mut out).unwrap();
        assert_eq!(out, b"Xy");
    }

    #[test]
    fn ime_text() {
        let mut out = Vec::new();
        encode_text("日本", InputModes::default(), &mut out);
        assert_eq!(out, "日本".as_bytes());
        out.clear();
        encode_text("a\x1bb\u{85}", InputModes::default(), &mut out);
        assert_eq!(out, b"ab");
        out.clear();
        encode_text("é", InputModes::default().with_kitty(24), &mut out);
        assert_eq!(out, b"\x1b[0;;233u");
        out.clear();
        encode_text("é", InputModes::default().with_kitty(8), &mut out);
        assert_eq!(out, "é".as_bytes());
        out.clear();
        encode_text("", InputModes::default().with_kitty(24), &mut out);
        assert!(out.is_empty());
    }
}
