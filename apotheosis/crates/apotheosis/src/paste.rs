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
    /// (e): clipboard content last set by OSC 52 from a pane, into a shell prompt.
    Program,
}

/// What main knows about the target pane and the content's provenance (T-18 §4 (a), (e)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Target {
    /// The pane is at an OSC 133 prompt.
    pub(crate) at_prompt: bool,
    /// Mode 2004 is set and a prompt mark followed it ("in force").
    pub(crate) bracketed_ok: bool,
    /// The content is what an OSC 52 write last put on the clipboard.
    pub(crate) osc52: bool,
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

/// The guard with no prompt or provenance facts (tests of rules (b)-(d)).
#[cfg(test)]
pub(crate) fn needs_confirm(
    src: &[u8],
    origin: Origin,
    modes: InputModes,
    s: &PasteSettings,
) -> Option<Reason> {
    needs_confirm_at(src, origin, modes, s, Target::default())
}

/// The guard (T-18 §4): `None` pastes at once, `Some(reason)` asks first.
///
/// (a) a newline into a pane at an OSC 133 prompt confirms unless bracketed paste is in
/// force (2004 set *and* a prompt mark seen since); outside a prompt the older
/// `paste.confirm-multiline` rule (newline without 2004) still applies. (e) content an
/// OSC 52 write put on the clipboard confirms at a prompt even under `never`.
pub(crate) fn needs_confirm_at(
    src: &[u8],
    origin: Origin,
    modes: InputModes,
    s: &PasteSettings,
    t: Target,
) -> Option<Reason> {
    let program = t.osc52 && t.at_prompt;
    match s.guard {
        Guard::Never => return program.then_some(Reason::Program),
        Guard::Always => return Some(Reason::Always),
        Guard::Auto => {}
    }
    let bracketed = modes.has(bit::BRACKETED_PASTE);
    if origin == Origin::DropFiles {
        return Some(Reason::Files);
    }
    let multiline = src.iter().any(|&b| b == b'\n' || b == b'\r');
    if multiline && t.at_prompt && !(bracketed && t.bracketed_ok) {
        return Some(Reason::Newline);
    }
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
    if program {
        return Some(Reason::Program);
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

    /// T-18 §12: the paste-guard table, cases (a)-(e) and the 2004 rule.
    #[test]
    fn guard_table_a_to_e_and_the_2004_rule() {
        let auto = settings(Guard::Auto);
        let never = settings(Guard::Never);
        let prompt = Target {
            at_prompt: true,
            ..Target::default()
        };
        let in_force = Target {
            at_prompt: true,
            bracketed_ok: true,
            osc52: false,
        };
        let osc52 = Target {
            at_prompt: true,
            bracketed_ok: true,
            osc52: true,
        };
        let sel = Origin::Selection;
        type Row<'a> = (
            &'a [u8],
            Origin,
            InputModes,
            &'a PasteSettings,
            Target,
            Option<Reason>,
        );
        #[rustfmt::skip]
        let table: &[Row] = &[
            // (a) newline at a prompt, 2004 off.
            (b"a\nb", sel, plain(), &auto, prompt, Some(Reason::Newline)),
            // (a) 2004 set but no prompt mark since: not in force.
            (b"a\nb", sel, bracketed(), &auto, prompt, Some(Reason::Newline)),
            // (a) 2004 in force: no question.
            (b"a\nb", sel, bracketed(), &auto, in_force, None),
            // (a) not at a prompt with 2004 set (an editor): no question.
            (b"a\nb", sel, bracketed(), &auto, Target::default(), None),
            // (b) controls before sanitising.
            (b"a\x1bb", sel, bracketed(), &auto, in_force, Some(Reason::Controls)),
            // (c) over confirm-bytes.
            (&[b'x'; 17], sel, bracketed(), &auto, in_force, Some(Reason::Size)),
            // (d) a uri-list drop.
            (b"/tmp/x", Origin::DropFiles, bracketed(), &auto, in_force, Some(Reason::Files)),
            // (e) OSC 52 content into a prompt, even under `never`.
            (b"ls", sel, bracketed(), &auto, osc52, Some(Reason::Program)),
            (b"ls", sel, bracketed(), &never, osc52, Some(Reason::Program)),
            // (e) OSC 52 content not at a prompt.
            (b"ls", sel, bracketed(), &never, Target { at_prompt: false, ..osc52 }, None),
            // `never` asks nothing else.
            (b"a\nb", sel, plain(), &never, prompt, None),
        ];
        for (i, (src, origin, modes, s, t, want)) in table.iter().enumerate() {
            assert_eq!(
                needs_confirm_at(src, *origin, *modes, s, *t),
                *want,
                "row {i}"
            );
        }
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
