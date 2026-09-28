// SPDX-License-Identifier: AGPL-3.0-only

//! Key chords and the built-in action names (FOG §UI and navigation).

use std::fmt;

/// Every built-in action, by the name config binds it with. The UI maps
/// these to behaviour; the palette lists them by [`Action::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Down,
    Up,
    Open,
    Parent,
    Top,
    Bottom,
    Palette,
    QuickLook,
    Visual,
    SelectAll,
    PathEdit,
    Copy,
    Cut,
    Paste,
    Rename,
    Trash,
    Delete,
    Undo,
    NewFolder,
    ToggleHidden,
    NewTab,
    CloseTab,
    SplitToggle,
}

impl Action {
    pub const ALL: [Action; 23] = [
        Action::Down,
        Action::Up,
        Action::Open,
        Action::Parent,
        Action::Top,
        Action::Bottom,
        Action::Palette,
        Action::QuickLook,
        Action::Visual,
        Action::SelectAll,
        Action::PathEdit,
        Action::Copy,
        Action::Cut,
        Action::Paste,
        Action::Rename,
        Action::Trash,
        Action::Delete,
        Action::Undo,
        Action::NewFolder,
        Action::ToggleHidden,
        Action::NewTab,
        Action::CloseTab,
        Action::SplitToggle,
    ];

    /// The config and palette name.
    pub fn name(self) -> &'static str {
        match self {
            Action::Down => "down",
            Action::Up => "up",
            Action::Open => "open",
            Action::Parent => "parent",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::Palette => "palette",
            Action::QuickLook => "quick-look",
            Action::Visual => "visual",
            Action::SelectAll => "select-all",
            Action::PathEdit => "path-edit",
            Action::Copy => "copy",
            Action::Cut => "cut",
            Action::Paste => "paste",
            Action::Rename => "rename",
            Action::Trash => "trash",
            Action::Delete => "delete",
            Action::Undo => "undo",
            Action::NewFolder => "new-folder",
            Action::ToggleHidden => "toggle-hidden",
            Action::NewTab => "new-tab",
            Action::CloseTab => "close-tab",
            Action::SplitToggle => "split-toggle",
        }
    }

    pub fn from_name(name: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == name)
    }
}

/// What a chord runs: a built-in action or a custom `action` by name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Action(Action),
    Custom(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Space,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F(u8),
}

/// A normalized key chord. Letters carry Shift as a modifier (`G` is
/// `shift+g`); other characters have it baked in (`:` never has Shift), so
/// the chord a layout produces and the chord config names compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

impl Chord {
    pub fn new(mut mods: Mods, key: Key) -> Chord {
        let key = match key {
            Key::Char(' ') => Key::Space,
            Key::Char(c) if c.is_uppercase() => {
                let mut low = c.to_lowercase();
                match (low.next(), low.next()) {
                    (Some(l), None) => {
                        mods.shift = true;
                        Key::Char(l)
                    }
                    _ => Key::Char(c),
                }
            }
            Key::Char(c) => {
                if !c.is_lowercase() {
                    mods.shift = false;
                }
                Key::Char(c)
            }
            k => k,
        };
        Chord { mods, key }
    }

    /// Parse `ctrl+shift+n`, `G`, `:`, `shift+delete`, `f2`.
    pub fn parse(s: &str) -> Result<Chord, String> {
        let mut parts: Vec<&str> = s.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        let mut mods = Mods::default();
        for m in parts {
            let flag = match m.to_ascii_lowercase().as_str() {
                "ctrl" => &mut mods.ctrl,
                "alt" => &mut mods.alt,
                "shift" => &mut mods.shift,
                "super" => &mut mods.logo,
                "" => return Err(format!("empty key in chord `{s}` (write `plus` for +)")),
                _ => return Err(format!("unknown modifier `{m}` in chord `{s}`")),
            };
            if *flag {
                return Err(format!("modifier `{m}` repeated in chord `{s}`"));
            }
            *flag = true;
        }
        let mut chars = key.chars();
        let key = match (chars.next(), chars.next()) {
            (None, _) => return Err(format!("empty key in chord `{s}` (write `plus` for +)")),
            (Some(c), None) => Key::Char(c),
            _ => match key.to_ascii_lowercase().as_str() {
                "enter" => Key::Enter,
                "backspace" => Key::Backspace,
                "tab" => Key::Tab,
                "space" => Key::Space,
                "delete" => Key::Delete,
                "insert" => Key::Insert,
                "home" => Key::Home,
                "end" => Key::End,
                "pageup" => Key::PageUp,
                "pagedown" => Key::PageDown,
                "up" => Key::Up,
                "down" => Key::Down,
                "left" => Key::Left,
                "right" => Key::Right,
                "backslash" => Key::Char('\\'),
                "plus" => Key::Char('+'),
                "esc" | "escape" => return Err("Esc is hard-coded and cannot be bound".into()),
                k => match k.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    Some(n @ 1..=12) => Key::F(n),
                    _ => return Err(format!("unknown key `{key}` in chord `{s}`")),
                },
            },
        };
        Ok(Chord::new(mods, key))
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.mods.ctrl, "ctrl+"),
            (self.mods.alt, "alt+"),
            (self.mods.shift, "shift+"),
            (self.mods.logo, "super+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        match self.key {
            Key::Char('\\') => f.write_str("backslash"),
            Key::Char('+') => f.write_str("plus"),
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "f{n}"),
            k => f.write_str(&format!("{k:?}").to_ascii_lowercase()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_normalize() {
        let g = Chord::parse("G").unwrap();
        assert_eq!(g, Chord::parse("shift+g").unwrap());
        assert_eq!(g.to_string(), "shift+g");
        let colon = Chord::parse(":").unwrap();
        let shifted = Chord::new(
            Mods {
                shift: true,
                ..Mods::default()
            },
            Key::Char(':'),
        );
        assert_eq!(colon, shifted);
        assert_eq!(Chord::parse("space").unwrap().key, Key::Space);
        assert_eq!(Chord::parse("ctrl+backslash").unwrap().key, Key::Char('\\'));
        assert_eq!(Chord::parse("F2").unwrap().key, Key::F(2));
        let n = Chord::parse("ctrl+shift+n").unwrap();
        assert!(n.mods.ctrl && n.mods.shift && n.key == Key::Char('n'));
        assert_eq!(n.to_string(), "ctrl+shift+n");
    }

    #[test]
    fn bad_chords() {
        for s in [
            "",
            "ctrl+",
            "ctrl++",
            "hyper+a",
            "ctrl+ctrl+a",
            "esc",
            "f13",
            "nope",
        ] {
            assert!(Chord::parse(s).is_err(), "{s}");
        }
    }

    #[test]
    fn action_names_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.name()), Some(a));
        }
        assert_eq!(Action::from_name("launch-missiles"), None);
    }
}
