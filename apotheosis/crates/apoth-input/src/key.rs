// SPDX-License-Identifier: Apache-2.0
//! The key event model shared by the legacy and kitty encoders.

/// Modifier set in kitty bit positions (INPUT-04): shift 1, alt 2, ctrl 4, super 8,
/// hyper 16, meta 32, caps 64, num 128.
///
/// Callers pass modifiers *after* removing the ones the keymap consumed to produce the
/// symbol (Shift+; giving `:` carries no Shift, INPUT-06). Lock bits are set while the
/// lock is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods(pub u8);

impl Mods {
    /// No modifiers.
    pub const NONE: Mods = Mods(0);
    /// Shift.
    pub const SHIFT: Mods = Mods(1);
    /// Alt.
    pub const ALT: Mods = Mods(2);
    /// Control.
    pub const CTRL: Mods = Mods(4);
    /// Super.
    pub const SUPER: Mods = Mods(8);
    /// Hyper.
    pub const HYPER: Mods = Mods(16);
    /// Meta.
    pub const META: Mods = Mods(32);
    /// Caps Lock active.
    pub const CAPS: Mods = Mods(64);
    /// Num Lock active.
    pub const NUM: Mods = Mods(128);

    /// Are all bits of `o` set.
    pub const fn contains(self, o: Mods) -> bool {
        self.0 & o.0 == o.0
    }
    /// Union.
    pub const fn union(self, o: Mods) -> Mods {
        Mods(self.0 | o.0)
    }
    /// Without the Caps and Num lock bits.
    pub const fn without_locks(self) -> Mods {
        Mods(self.0 & 0x3f)
    }
    /// No bits set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, o: Mods) -> Mods {
        self.union(o)
    }
}

/// Press, auto-repeat or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeyKind {
    /// Key went down.
    #[default]
    Press,
    /// Client-side auto repeat.
    Repeat,
    /// Key went up.
    Release,
}

/// Keypad key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kp {
    /// `0`..`9`.
    Digit(u8),
    /// Decimal.
    Decimal,
    /// Divide.
    Divide,
    /// Multiply.
    Multiply,
    /// Subtract.
    Subtract,
    /// Add.
    Add,
    /// Enter.
    Enter,
    /// Equal.
    Equal,
    /// Separator.
    Separator,
    /// Left.
    Left,
    /// Right.
    Right,
    /// Up.
    Up,
    /// Down.
    Down,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Insert.
    Insert,
    /// Delete.
    Delete,
    /// Keypad 5 without NumLock.
    Begin,
}

/// Non-text keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    /// Escape.
    Escape,
    /// Enter / Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Insert.
    Insert,
    /// Delete.
    Delete,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// F1..F35.
    F(u8),
    /// Menu.
    Menu,
    /// Caps Lock key.
    CapsLock,
    /// Scroll Lock key.
    ScrollLock,
    /// Num Lock key.
    NumLock,
    /// Print Screen.
    PrintScreen,
    /// Pause.
    Pause,
    /// Keypad key.
    Kp(Kp),
    /// Media, modifier-only or ISO shift key by its kitty PUA code (see [`crate::kitty::pua`]).
    Pua(u32),
}

/// A logical key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A text key, by its unshifted, lower-cased codepoint (INPUT-65).
    Char(char),
    /// A functional key.
    Named(Named),
}

/// One key event. `text` is the text the event produces (after modifiers consumed by the
/// keymap, compose and dead keys); it is empty for non-text keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent<'a> {
    /// Logical key.
    pub key: Key,
    /// Modifiers (consumed ones removed, locks included).
    pub mods: Mods,
    /// Press, repeat or release.
    pub kind: KeyKind,
    /// Text produced by the event.
    pub text: &'a str,
    /// Shifted key from the Shift-only lookup (INPUT-66).
    pub shifted: Option<char>,
    /// Base-layout key from the evdev to US table (INPUT-67).
    pub base: Option<char>,
    /// First-group keysym <= 127 for Ctrl on non-Latin layouts (INPUT-70); `None` when the
    /// config switch is off.
    pub ctrl_fallback: Option<char>,
}

impl<'a> KeyEvent<'a> {
    /// A bare press of `key`.
    pub const fn new(key: Key) -> Self {
        KeyEvent {
            key,
            mods: Mods::NONE,
            kind: KeyKind::Press,
            text: "",
            shifted: None,
            base: None,
            ctrl_fallback: None,
        }
    }
    /// Set modifiers.
    pub const fn mods(mut self, m: Mods) -> Self {
        self.mods = m;
        self
    }
    /// Set kind.
    pub const fn kind(mut self, k: KeyKind) -> Self {
        self.kind = k;
        self
    }
    /// Set produced text.
    pub const fn text(mut self, t: &'a str) -> Self {
        self.text = t;
        self
    }
    /// Set shifted key.
    pub const fn shifted(mut self, c: char) -> Self {
        self.shifted = Some(c);
        self
    }
    /// Set base-layout key.
    pub const fn base(mut self, c: char) -> Self {
        self.base = Some(c);
        self
    }
    /// Set Ctrl fallback key.
    pub const fn ctrl_fallback(mut self, c: char) -> Self {
        self.ctrl_fallback = Some(c);
        self
    }
}
