// SPDX-License-Identifier: AGPL-3.0-only
//! The paste path (T-05 §6, T-18 §4): the guard decision and the encoded unit. Pure. The
//! sanitising, bracketing and newline rules live in `apoth_input::encode_paste`; this module
//! decides whether the user must confirm first and builds the one write unit.
//!
//! Paste content is user content: nothing here logs it, only sizes.

use apoth_input::modes::bit;
use apoth_input::{InputModes, PasteError, PasteNewline, PasteOptions, encode_paste};

use crate::settings::{Guard, PasteSettings};

/// Where the bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The clipboard or primary selection, or a keyboard binding.
    Selection,
    /// A text drop.
    DropText,
    /// A `text/uri-list` drop: always confirmed under `auto` (T-18 §4 (d)).
    DropFiles,
}

/// Why a confirmation is wanted (never content, only the reason).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Always,
    Newline,
    Controls,
    Size,
    Files,
    Newline2,
}

/// Bytes and lines of a paste, for the confirm bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stats {
    pub(crate) bytes: usize,
    pub(crate) lines: usize,
}

pub(crate) fn stats(src: &[u8]) -> Stats {
    let nl = src.iter().filter(|&&b| b == b'\n').count();
    // A paste that does not end in a newline still has its last line.
    let tail = usize::from(!src.is_empty() && src.last() != Some(&b'\n'));
    Stats {
        bytes: src.len(),
        lines: nl + tail,
    }
}

/// Did the text carry control characters the sanitiser will replace (before sanitising)?
fn has_controls(src: &[u8]) -> bool {
    src.iter()
        .any(|&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r')) || b == 0x7f)
        || core::str::from_utf8(src)
            .map(|s| s.chars().any(|c| ('\u{80}'..='\u{9f}').contains(&c)))
            .unwrap_or(false)
}

/// The guard (T-18 §4): `None` pastes at once, `Some(reason)` asks first.
///
/// Rule (a) uses "bracketed paste is set" as the in-force test: the binary does not see OSC 133
/// prompt marks on the main thread, so 2004 counts as in force whenever it is set (a spec gap,
/// reported). Rule (e) (OSC 52 provenance) needs the mux's tagging and is not reachable here.
pub(crate) fn needs_confirm(
    src: &[u8],
    origin: Origin,
    modes: InputModes,
    s: &PasteSettings,
) -> Option<Reason> {
    match s.guard {
        Guard::Never => return None,
        Guard::Always => return Some(Reason::Always),
        Guard::Auto => {}
    }
    let bracketed = modes.has(bit::BRACKETED_PASTE);
    if origin == Origin::DropFiles {
        return Some(Reason::Files);
    }
    let multiline = src.iter().any(|&b| b == b'\n' || b == b'\r');
    if multiline && !bracketed && s.confirm_multiline {
        return Some(Reason::Newline);
    }
    // `paste.newline confirm` asks for every newline paste outside bracketed mode.
    if multiline && !bracketed && s.opts.newline == PasteNewline::Confirm {
        return Some(Reason::Newline2);
    }
    if has_controls(src) {
        return Some(Reason::Controls);
    }
    if src.len() > s.confirm_bytes {
        return Some(Reason::Size);
    }
    None
}

/// Why nothing was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Empty,
    TooLarge,
}

/// The single write unit for `src` under the current modes (bracket wrapper included, so a
/// paste is never split across other writes). `Confirm` newline mode is sent as `cr` once the
/// user has confirmed.
pub(crate) fn encode(src: &[u8], modes: InputModes, s: &PasteSettings) -> Result<Vec<u8>, Refused> {
    if src.is_empty() {
        return Err(Refused::Empty);
    }
    let mut opts: PasteOptions = s.opts;
    if opts.newline == PasteNewline::Confirm {
        opts.newline = PasteNewline::Cr;
    }
    let mut out = Vec::new();
    encode_paste(src, modes, &opts, &mut out).map_err(|PasteError::TooLarge| Refused::TooLarge)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(guard: Guard) -> PasteSettings {
        PasteSettings {
            guard,
            confirm_bytes: 16,
            confirm_multiline: true,
            opts: PasteOptions::default(),
        }
    }

    fn plain() -> InputModes {
        InputModes::from_raw(0, 0)
    }

    fn bracketed() -> InputModes {
        InputModes::from_raw(bit::BRACKETED_PASTE, 0)
    }

    #[test]
    fn counts_lines_with_and_without_trailing_newline() {
        assert_eq!(stats(b"a\nb\n"), Stats { bytes: 4, lines: 2 });
        assert_eq!(stats(b"a\nb"), Stats { bytes: 3, lines: 2 });
        assert_eq!(stats(b""), Stats { bytes: 0, lines: 0 });
        assert_eq!(stats(b"one"), Stats { bytes: 3, lines: 1 });
    }

    #[test]
    fn single_short_line_pastes_at_once() {
        let s = settings(Guard::Auto);
        assert_eq!(
            needs_confirm(b"ls -l", Origin::Selection, plain(), &s),
            None
        );
    }

    #[test]
    fn newline_confirms_only_without_bracketed_paste() {
        let s = settings(Guard::Auto);
        assert_eq!(
            needs_confirm(b"a\nb", Origin::Selection, plain(), &s),
            Some(Reason::Newline)
        );
        assert_eq!(
            needs_confirm(b"a\nb", Origin::Selection, bracketed(), &s),
            None
        );
        let mut off = s;
        off.confirm_multiline = false;
        assert_eq!(
            needs_confirm(b"a\nb", Origin::Selection, plain(), &off),
            None
        );
        off.opts.newline = PasteNewline::Confirm;
        assert_eq!(
            needs_confirm(b"a\nb", Origin::Selection, plain(), &off),
            Some(Reason::Newline2)
        );
    }

    #[test]
    fn controls_and_size_confirm_even_when_bracketed() {
        let s = settings(Guard::Auto);
        assert_eq!(
            needs_confirm(b"a\x1b[2Jb", Origin::Selection, bracketed(), &s),
            Some(Reason::Controls)
        );
        assert_eq!(
            needs_confirm("a\u{85}b".as_bytes(), Origin::Selection, bracketed(), &s),
            Some(Reason::Controls)
        );
        assert_eq!(
            needs_confirm(&[b'x'; 17], Origin::Selection, bracketed(), &s),
            Some(Reason::Size)
        );
        // TAB, LF and CR are not "control characters" for rule (b).
        assert_eq!(needs_confirm(b"a\tb", Origin::Selection, plain(), &s), None);
    }

    #[test]
    fn file_drops_always_confirm_under_auto() {
        let s = settings(Guard::Auto);
        assert_eq!(
            needs_confirm(b"'/a'", Origin::DropFiles, bracketed(), &s),
            Some(Reason::Files)
        );
        assert_eq!(needs_confirm(b"x", Origin::DropText, plain(), &s), None);
    }

    #[test]
    fn always_and_never() {
        assert_eq!(
            needs_confirm(b"x", Origin::Selection, plain(), &settings(Guard::Always)),
            Some(Reason::Always)
        );
        let never = settings(Guard::Never);
        assert_eq!(
            needs_confirm(b"a\nb\x1b", Origin::DropFiles, plain(), &never),
            None
        );
    }

    #[test]
    fn encode_brackets_and_sanitises() {
        let s = settings(Guard::Auto);
        let out = encode(b"a\x1b[201~b\nc", bracketed(), &s).unwrap();
        assert!(out.starts_with(b"\x1b[200~") && out.ends_with(b"\x1b[201~"));
        // The embedded escape became its picture: exactly one 201~ terminator remains.
        let n = out.windows(6).filter(|w| *w == b"\x1b[201~").count();
        assert_eq!(n, 1);
        assert!(out.contains(&b'\n'));
    }

    #[test]
    fn encode_without_bracketed_translates_newlines_to_cr() {
        let s = settings(Guard::Auto);
        assert_eq!(encode(b"a\r\nb\nc", plain(), &s).unwrap(), b"a\rb\rc");
        let mut c = s;
        c.opts.newline = PasteNewline::Confirm;
        assert_eq!(encode(b"a\nb", plain(), &c).unwrap(), b"a\rb");
        let mut asis = s;
        asis.opts.newline = PasteNewline::AsIs;
        assert_eq!(encode(b"a\nb", plain(), &asis).unwrap(), b"a\nb");
    }

    #[test]
    fn encode_refuses_empty_and_oversize() {
        let mut s = settings(Guard::Auto);
        assert_eq!(encode(b"", plain(), &s), Err(Refused::Empty));
        s.opts.max_bytes = 3;
        assert_eq!(encode(b"abcd", plain(), &s), Err(Refused::TooLarge));
    }
}
