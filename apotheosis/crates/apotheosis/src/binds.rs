// SPDX-License-Identifier: AGPL-3.0-only
//! The built-in key bindings for copy, paste, selection and scrolling (T-05 §3). Bind lookup
//! applies to real seat key events before encoding; a consumed key emits nothing to the PTY,
//! release included. User binds (`binds { }`, T-11) arrive with the full dispatcher; these
//! defaults are what an empty configuration gets.

use apoth_input::{Key, Mods, Named};

use crate::view::ScrollBy;

/// What a built-in bind does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    /// `selection.copy` to the clipboard.
    Copy,
    /// `input.paste` from the clipboard.
    Paste,
    /// `input.paste-selection` from the primary selection.
    PasteSelection,
    /// `selection.select-all`.
    SelectAll,
    /// `scroll.*`. Not consumed on the alternate screen (it has no scrollback).
    Scroll(ScrollBy),
}

const CS: Mods = Mods(Mods::CTRL.0 | Mods::SHIFT.0);

/// The table: key, exact modifiers (locks ignored), action.
const TABLE: &[(Key, Mods, Act)] = &[
    (Key::Char('c'), CS, Act::Copy),
    (Key::Named(Named::Insert), Mods::CTRL, Act::Copy),
    (Key::Char('v'), CS, Act::Paste),
    (Key::Named(Named::Insert), Mods::SHIFT, Act::PasteSelection),
    (Key::Char('a'), CS, Act::SelectAll),
    (
        Key::Named(Named::PageUp),
        Mods::SHIFT,
        Act::Scroll(ScrollBy::Pages(1)),
    ),
    (
        Key::Named(Named::PageDown),
        Mods::SHIFT,
        Act::Scroll(ScrollBy::Pages(-1)),
    ),
    (Key::Named(Named::Up), CS, Act::Scroll(ScrollBy::Lines(1))),
    (
        Key::Named(Named::Down),
        CS,
        Act::Scroll(ScrollBy::Lines(-1)),
    ),
    (
        Key::Named(Named::Home),
        Mods::SHIFT,
        Act::Scroll(ScrollBy::Top),
    ),
    (
        Key::Named(Named::End),
        Mods::SHIFT,
        Act::Scroll(ScrollBy::Bottom),
    ),
];

/// The action bound to `key` with `mods` held, if any.
pub(crate) fn lookup(key: Key, mods: Mods) -> Option<Act> {
    let m = mods.without_locks();
    TABLE
        .iter()
        .find(|(k, km, _)| *k == key && *km == m)
        .map(|(_, _, a)| *a)
}

/// Keys of the table that scroll, for the repeat rule: only scrolling repeats.
pub(crate) fn repeats(a: Act) -> bool {
    matches!(a, Act::Scroll(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve() {
        assert_eq!(lookup(Key::Char('c'), CS), Some(Act::Copy));
        assert_eq!(lookup(Key::Char('v'), CS), Some(Act::Paste));
        assert_eq!(
            lookup(Key::Named(Named::Insert), Mods::SHIFT),
            Some(Act::PasteSelection)
        );
        assert_eq!(
            lookup(Key::Named(Named::PageUp), Mods::SHIFT),
            Some(Act::Scroll(ScrollBy::Pages(1)))
        );
        assert_eq!(
            lookup(Key::Named(Named::End), Mods::SHIFT),
            Some(Act::Scroll(ScrollBy::Bottom))
        );
    }

    #[test]
    fn modifiers_must_match_exactly() {
        // Plain Ctrl+C is the interrupt byte, never a copy.
        assert_eq!(lookup(Key::Char('c'), Mods::CTRL), None);
        assert_eq!(lookup(Key::Char('c'), Mods::NONE), None);
        assert_eq!(lookup(Key::Char('c'), CS | Mods::ALT), None);
        assert_eq!(lookup(Key::Named(Named::PageUp), Mods::NONE), None);
    }

    #[test]
    fn lock_keys_do_not_matter() {
        assert_eq!(
            lookup(Key::Char('v'), CS | Mods::NUM | Mods::CAPS),
            Some(Act::Paste)
        );
    }

    #[test]
    fn only_scrolling_repeats() {
        assert!(repeats(Act::Scroll(ScrollBy::Lines(1))));
        assert!(!repeats(Act::Paste));
    }
}
