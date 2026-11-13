// SPDX-License-Identifier: Apache-2.0
//! `KeyboardBackend`: the seam between XKB keymap handling and the encoders (T-05 §1,
//! ADR 0125, INPUT-61..-76).
//!
//! Implementations live outside the pure encoders: `kbvm` (default, shipped) and
//! `libxkbcommon` behind the cargo feature `xkbcommon` (oracle and source-build escape
//! hatch, `APOTHEOSIS_XKB=system`). The trait is named `KeyboardBackend` per T-05 §1; the
//! matrix (INPUT-07, -61) and T-09 §1 still say `XkbBackend`.

use crate::key::{Key, KeyEvent, KeyKind, Mods};

/// `wl_keyboard.modifiers` payload (INPUT-63).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Components {
    /// Depressed modifier mask.
    pub depressed: u32,
    /// Latched modifier mask.
    pub latched: u32,
    /// Locked modifier mask.
    pub locked: u32,
    /// Effective group.
    pub group: u32,
}

/// Up to 16 bytes of UTF-8 inline, so a lookup never allocates (INPUT-62).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InlineText {
    buf: [u8; 16],
    len: u8,
}

impl InlineText {
    /// Copy `s`, truncating at a character boundary to 16 bytes.
    pub fn new(s: &str) -> Self {
        let mut t = InlineText::default();
        for c in s.chars() {
            let n = c.len_utf8();
            if t.len as usize + n > t.buf.len() {
                break;
            }
            c.encode_utf8(&mut t.buf[t.len as usize..]);
            t.len += n as u8;
        }
        t
    }
    /// The text.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }
}

/// Result of resolving one physical key against the current state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyLookup {
    /// Logical key (INPUT-65: unshifted, lower-cased codepoint for text keys).
    pub key: Key,
    /// Shift-only lookup symbol (INPUT-66).
    pub shifted: Option<char>,
    /// Base-layout key (INPUT-67).
    pub base: Option<char>,
    /// Text the key produces now (empty for dead keys, which are never emitted, INPUT-08).
    pub text: InlineText,
    /// Modifiers still to report: active modifiers minus those consumed (INPUT-06),
    /// resolved by virtual-modifier name (INPUT-68).
    pub mods: Mods,
    /// First-group keysym <= 127 for the Ctrl fallback (INPUT-70); `None` when off.
    pub ctrl_fallback: Option<char>,
    /// The key auto-repeats (INPUT-71); the repeat timer is `apoth-wl`'s.
    pub repeats: bool,
}

impl KeyLookup {
    /// Build the encoder event for this lookup.
    pub fn event(&self, kind: KeyKind) -> KeyEvent<'_> {
        KeyEvent {
            key: self.key,
            mods: self.mods,
            kind,
            text: self.text.as_str(),
            shifted: self.shifted,
            base: self.base,
            ctrl_fallback: self.ctrl_fallback,
        }
    }
}

/// Keymap load failure; carries a static reason, never keymap text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeymapError(pub &'static str);

/// XKB state behind one interface. `lookup` must not allocate.
pub trait KeyboardBackend {
    /// Parse the `wl_keyboard.keymap` text and build the lookup table (not a hot path).
    fn load_keymap(&mut self, keymap_text: &str) -> Result<(), KeymapError>;
    /// Apply `wl_keyboard.modifiers`.
    fn update_modifiers(&mut self, c: Components);
    /// Resolve an evdev keycode (xkb keycode minus 8) under the current components.
    fn lookup(&self, evdev: u32) -> Option<KeyLookup>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_text_truncates_on_char_boundary() {
        assert_eq!(InlineText::new("abc").as_str(), "abc");
        assert_eq!(InlineText::new("").as_str(), "");
        let t = InlineText::new("日日日日日日");
        assert_eq!(t.as_str(), "日日日日日");
    }

    #[test]
    fn lookup_builds_event() {
        let l = KeyLookup {
            key: Key::Char('a'),
            shifted: Some('A'),
            base: Some('a'),
            text: InlineText::new("A"),
            mods: Mods::SHIFT,
            ctrl_fallback: None,
            repeats: true,
        };
        let e = l.event(KeyKind::Press);
        assert_eq!((e.text, e.shifted, e.mods), ("A", Some('A'), Mods::SHIFT));
    }
}
