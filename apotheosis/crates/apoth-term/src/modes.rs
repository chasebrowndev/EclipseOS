// SPDX-License-Identifier: AGPL-3.0-only
//! The mode table (T-03 §2): every ANSI and DEC private mode in one place.
//!
//! DECRQM answers, DECSET/DECRST handling and the generated docs derive from [`MODES`].
//! A mode that feeds the input path carries its mirror bit in `input_bit`.

use apoth_input::modes::bit;

/// What a mode accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// `h` and `l` change it.
    Settable,
    /// Always set; DECRQM answers 3.
    ReadOnlySet,
    /// Always reset; DECRQM answers 4.
    ReadOnlyReset,
    /// Not a mode of this terminal; DECRQM answers 0.
    Unrecognised,
}

/// Which part of the system acts on the mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// The terminal state machine.
    Term,
    /// The input encoders, through the lock-free mirror.
    Input,
    /// The renderer or shell integration reads it from the term.
    Render,
    /// Sixel (T-15).
    Sixel,
}

/// One mode.
#[derive(Clone, Copy, Debug)]
pub struct ModeDef {
    /// Mode number.
    pub num: u16,
    /// ANSI mode (`CSI Pm h`) rather than DEC private (`CSI ? Pm h`).
    pub ansi: bool,
    /// Short name.
    pub name: &'static str,
    /// Power-on state.
    pub default: bool,
    /// What `h`/`l` do.
    pub access: Access,
    /// Acting component.
    pub owner: Owner,
    /// Bit in the `InputModes` mirror (0 = not an input-path mode).
    pub input_bit: u64,
    /// Config key that changes the DECRQM answer (T-04 §2), if any.
    pub config_key: Option<&'static str>,
}

const fn m(
    num: u16,
    ansi: bool,
    name: &'static str,
    default: bool,
    access: Access,
    owner: Owner,
    input_bit: u64,
) -> ModeDef {
    ModeDef {
        num,
        ansi,
        name,
        default,
        access,
        owner,
        input_bit,
        config_key: None,
    }
}

use Access::{ReadOnlyReset, ReadOnlySet, Settable, Unrecognised};
use Owner::{Input, Render, Sixel, Term};

/// The table. Indices are stable within a build and used as bit positions of the term's
/// mode word (at most 128 entries).
pub const MODES: &[ModeDef] = &[
    // ANSI modes.
    m(2, true, "KAM", false, ReadOnlyReset, Term, 0),
    m(4, true, "IRM", false, Settable, Term, 0),
    m(12, true, "SRM", true, ReadOnlySet, Term, 0),
    m(20, true, "LNM", false, Settable, Term, 0),
    // DEC private modes.
    m(1, false, "DECCKM", false, Settable, Input, bit::DECCKM),
    m(2, false, "DECANM", true, ReadOnlySet, Term, 0),
    m(3, false, "DECCOLM", false, ReadOnlyReset, Term, 0),
    m(4, false, "DECSCLM", false, ReadOnlyReset, Term, 0),
    m(5, false, "DECSCNM", false, Settable, Render, 0),
    m(6, false, "DECOM", false, Settable, Term, 0),
    m(7, false, "DECAWM", true, Settable, Term, 0),
    m(8, false, "DECARM", true, Settable, Input, 0),
    m(9, false, "X10_MOUSE", false, Settable, Input, bit::MOUSE_9),
    m(12, false, "ATT610_BLINK", false, Settable, Render, 0),
    m(25, false, "DECTCEM", true, Settable, Render, 0),
    m(40, false, "ALLOW_DECCOLM", false, ReadOnlyReset, Term, 0),
    m(45, false, "REVERSE_WRAP", false, Settable, Term, 0),
    m(47, false, "ALT_SCREEN_47", false, Settable, Term, 0),
    m(66, false, "DECNKM", false, Settable, Input, bit::DECKPAM),
    m(67, false, "DECBKM", false, Settable, Input, bit::DECBKM),
    m(69, false, "DECLRMM", false, Settable, Term, 0),
    m(80, false, "DECSDM", false, Settable, Sixel, 0),
    m(
        1000,
        false,
        "MOUSE_1000",
        false,
        Settable,
        Input,
        bit::MOUSE_1000,
    ),
    m(1001, false, "MOUSE_HILITE", false, ReadOnlyReset, Term, 0),
    m(
        1002,
        false,
        "MOUSE_1002",
        false,
        Settable,
        Input,
        bit::MOUSE_1002,
    ),
    m(
        1003,
        false,
        "MOUSE_1003",
        false,
        Settable,
        Input,
        bit::MOUSE_1003,
    ),
    m(
        1004,
        false,
        "FOCUS",
        false,
        Settable,
        Input,
        bit::FOCUS_1004,
    ),
    m(
        1005,
        false,
        "MOUSE_UTF8",
        false,
        Settable,
        Input,
        bit::MOUSE_1005,
    ),
    m(
        1006,
        false,
        "MOUSE_SGR",
        false,
        Settable,
        Input,
        bit::MOUSE_SGR_1006,
    ),
    m(
        1007,
        false,
        "ALT_SCROLL",
        true,
        Settable,
        Input,
        bit::ALT_SCROLL_1007,
    ),
    m(1010, false, "SCROLL_ON_OUTPUT", false, Settable, Render, 0),
    m(1011, false, "SCROLL_ON_KEY", false, Settable, Render, 0),
    m(
        1015,
        false,
        "MOUSE_URXVT",
        false,
        Settable,
        Input,
        bit::MOUSE_1015,
    ),
    m(
        1016,
        false,
        "MOUSE_PIXELS",
        false,
        Settable,
        Input,
        bit::MOUSE_PIXELS_1016,
    ),
    m(1036, false, "ALT_ESC", true, Settable, Input, bit::ALT_ESC),
    m(1047, false, "ALT_SCREEN_1047", false, Settable, Term, 0),
    m(1048, false, "SAVE_CURSOR", false, Settable, Term, 0),
    m(1049, false, "ALT_SCREEN_1049", false, Settable, Term, 0),
    m(
        1070,
        false,
        "SIXEL_PRIVATE_COLORS",
        true,
        Settable,
        Sixel,
        0,
    ),
    m(
        2004,
        false,
        "BRACKETED_PASTE",
        false,
        Settable,
        Input,
        bit::BRACKETED_PASTE,
    ),
    m(2026, false, "SYNC_OUTPUT", false, Settable, Render, 0),
    ModeDef {
        config_key: Some("unicode.width-method"),
        ..m(2027, false, "GRAPHEME_WIDTH", false, Settable, Term, 0)
    },
    m(
        2031,
        false,
        "COLOR_SCHEME_REPORT",
        false,
        Settable,
        Render,
        0,
    ),
    m(2048, false, "SIZE_REPORT", false, Settable, Term, 0),
    m(8452, false, "SIXEL_CURSOR_RIGHT", false, Settable, Sixel, 0),
    // Modes the table knows are not ours (DECRQM answers 0 rather than guessing).
    m(1034, false, "META_8BIT", false, Unrecognised, Term, 0),
];

// The mode word is a u128.
const _: () = assert!(MODES.len() <= 128);

/// Table index of a mode.
pub fn index(ansi: bool, num: u16) -> Option<usize> {
    MODES.iter().position(|d| d.ansi == ansi && d.num == num)
}

/// The mode word with every mode at its default.
pub(crate) fn default_word() -> u128 {
    let mut w = 0u128;
    for (i, d) in MODES.iter().enumerate() {
        if d.default {
            w |= 1 << i;
        }
    }
    w
}

/// A DECRQM state value (`Pm`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeState {
    /// 0: not recognised.
    Unknown = 0,
    /// 1: set.
    Set = 1,
    /// 2: reset.
    Reset = 2,
    /// 3: permanently set.
    PermSet = 3,
    /// 4: permanently reset.
    PermReset = 4,
}

/// A name for docs and fixtures.
impl ModeState {
    /// `unknown`, `set`, `reset`, `perm-set` or `perm-reset`.
    pub fn name(self) -> &'static str {
        match self {
            ModeState::Unknown => "unknown",
            ModeState::Set => "set",
            ModeState::Reset => "reset",
            ModeState::PermSet => "perm-set",
            ModeState::PermReset => "perm-reset",
        }
    }
}

/// The documentation table, generated from [`MODES`] (T-03 §2 "generated docs").
pub fn render_docs() -> String {
    let mut s = String::new();
    for d in MODES {
        s.push_str(&format!(
            "{}{} {} default={} access={:?} owner={:?} input_bit={:#x}\n",
            if d.ansi { "" } else { "?" },
            d.num,
            d.name,
            if d.default { "set" } else { "reset" },
            d.access,
            d.owner,
            d.input_bit
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_unique_per_kind() {
        for (i, a) in MODES.iter().enumerate() {
            for b in &MODES[i + 1..] {
                assert!(!(a.ansi == b.ansi && a.num == b.num), "{} dup", a.name);
            }
        }
    }

    #[test]
    fn every_input_mode_has_a_unique_mirror_bit() {
        let mut seen = 0u64;
        for d in MODES.iter().filter(|d| d.owner == Owner::Input) {
            if d.num == 8 && !d.ansi {
                // DECARM is key repeat, an input-class mode with no encoder bit.
                continue;
            }
            assert_ne!(d.input_bit, 0, "{} has no mirror bit", d.name);
            assert_eq!(d.input_bit.count_ones(), 1, "{}", d.name);
            assert_eq!(seen & d.input_bit, 0, "{} shares a bit", d.name);
            seen |= d.input_bit;
        }
        // The spec's list: DECCKM, DECKPAM, 1036, 2004, 9, 1000-1006, 1015, 1016, 1004.
        for (num, want) in [
            (1u16, bit::DECCKM),
            (66, bit::DECKPAM),
            (1036, bit::ALT_ESC),
            (2004, bit::BRACKETED_PASTE),
            (9, bit::MOUSE_9),
            (1000, bit::MOUSE_1000),
            (1002, bit::MOUSE_1002),
            (1003, bit::MOUSE_1003),
            (1004, bit::FOCUS_1004),
            (1005, bit::MOUSE_1005),
            (1006, bit::MOUSE_SGR_1006),
            (1015, bit::MOUSE_1015),
            (1016, bit::MOUSE_PIXELS_1016),
        ] {
            let i = index(false, num).expect("listed");
            assert_eq!(MODES[i].input_bit, want, "?{num}");
        }
    }

    #[test]
    fn sixel_modes_are_in_the_table_with_spec_defaults() {
        let d = |n| MODES[index(false, n).unwrap()];
        assert!(!d(80).default);
        assert!(d(1070).default);
        assert!(!d(8452).default);
    }

    #[test]
    fn defaults_agree_with_the_input_mirror_default() {
        let w = default_word();
        let mut bits = 0u64;
        for (i, d) in MODES.iter().enumerate() {
            if w >> i & 1 == 1 {
                bits |= d.input_bit;
            }
        }
        assert_eq!(bits, apoth_input::InputModes::default().raw().0);
    }

    #[test]
    fn docs_list_every_mode() {
        assert_eq!(render_docs().lines().count(), MODES.len());
    }
}
