// SPDX-License-Identifier: AGPL-3.0-only
//! Keys to [`Nav`]. Only the key's identity is looked at, and nothing about
//! one is logged: a text field in a pane receives human typing.

use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};

use crate::model::Nav;

/// `None` for a key no pane uses. Letter shortcuts need no modifier but
/// shift, so a Ctrl+R or Alt+A the human meant for something else is left
/// alone.
pub fn nav_from(key: &Key, mods: Modifiers) -> Option<Nav> {
    match key {
        Key::Named(Named::ArrowLeft) => Some(Nav::Left),
        Key::Named(Named::ArrowRight) => Some(Nav::Right),
        Key::Named(Named::ArrowUp) => Some(Nav::Up),
        Key::Named(Named::ArrowDown) => Some(Nav::Down),
        Key::Named(Named::Tab) if mods.shift() => Some(Nav::Prev),
        Key::Named(Named::Tab) => Some(Nav::Next),
        Key::Named(Named::Space) => Some(Nav::Space),
        Key::Named(Named::Enter) => Some(Nav::Enter),
        Key::Named(Named::Escape) => Some(Nav::Escape),
        Key::Character(c) if !(mods.control() || mods.alt() || mods.logo()) => {
            match c.to_lowercase().as_str() {
                "a" => Some(Nav::Audio),
                "r" => Some(Nav::Remember),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(n: Named) -> Key {
        Key::Named(n)
    }

    #[test]
    fn arrows_enter_escape_space() {
        let none = Modifiers::empty();
        assert_eq!(nav_from(&named(Named::ArrowLeft), none), Some(Nav::Left));
        assert_eq!(nav_from(&named(Named::ArrowDown), none), Some(Nav::Down));
        assert_eq!(nav_from(&named(Named::Enter), none), Some(Nav::Enter));
        assert_eq!(nav_from(&named(Named::Escape), none), Some(Nav::Escape));
        assert_eq!(nav_from(&named(Named::Space), none), Some(Nav::Space));
        assert_eq!(nav_from(&named(Named::F5), none), None);
    }

    #[test]
    fn shift_tab_goes_back() {
        assert_eq!(
            nav_from(&named(Named::Tab), Modifiers::empty()),
            Some(Nav::Next)
        );
        assert_eq!(
            nav_from(&named(Named::Tab), Modifiers::SHIFT),
            Some(Nav::Prev)
        );
    }

    #[test]
    fn letters_are_shortcuts_only_without_ctrl_alt_logo() {
        let a = Key::Character("a".into());
        assert_eq!(nav_from(&a, Modifiers::empty()), Some(Nav::Audio));
        assert_eq!(
            nav_from(&Key::Character("R".into()), Modifiers::SHIFT),
            Some(Nav::Remember)
        );
        assert_eq!(nav_from(&a, Modifiers::CTRL), None);
        assert_eq!(nav_from(&a, Modifiers::ALT), None);
        assert_eq!(
            nav_from(&Key::Character("x".into()), Modifiers::empty()),
            None
        );
    }
}
