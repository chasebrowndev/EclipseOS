// SPDX-License-Identifier: Apache-2.0
//! Key chords, sequences, mouse chords, bind options and chord resolution (T-11 §3).
//!
//! Grammar (specified here because T-11 §3 gives only the outline):
//! - `<mods>+<key>`, mods in any order, case-insensitive: `ctrl|control`, `alt|option`,
//!   `shift`, `super|cmd|win|logo`. Canonical order on format: ctrl, alt, shift, super.
//! - `<key>` is an XKB keysym name (`a`, `Return`, `asciitilde`, `plus`, `greater`;
//!   `[A-Za-z0-9_]`, case-sensitive) or `physical:<name>` (position-dependent).
//!   Literal `+` and `>` are not key names: write `plus` / `greater`.
//! - A sequence is chords joined by `>` (`ctrl+a>c`), at most [`MAX_SEQUENCE`] chords.

use crate::arg::{ArgType, ParamKind};
use crate::error::{ErrorKind, ParseError};

/// Longest chord sequence.
pub const MAX_SEQUENCE: usize = 8;
/// Longest key or physical name in bytes.
pub const MAX_KEY_NAME: usize = 64;

/// Modifier bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods(u8);

impl Mods {
    /// No modifiers.
    pub const NONE: Mods = Mods(0);
    /// Shift.
    pub const SHIFT: Mods = Mods(1);
    /// Control.
    pub const CTRL: Mods = Mods(2);
    /// Alt / Option.
    pub const ALT: Mods = Mods(4);
    /// Super / Logo.
    pub const SUPER: Mods = Mods(8);

    /// Union.
    #[must_use]
    pub const fn union(self, o: Mods) -> Mods {
        Mods(self.0 | o.0)
    }

    /// Contains all of `o`.
    pub const fn contains(self, o: Mods) -> bool {
        self.0 & o.0 == o.0
    }

    /// True when empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn from_name(s: &str) -> Option<Mods> {
        match s.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(Mods::CTRL),
            "alt" | "option" => Some(Mods::ALT),
            "shift" => Some(Mods::SHIFT),
            "super" | "cmd" | "win" | "logo" => Some(Mods::SUPER),
            _ => None,
        }
    }

    fn push_names(self, out: &mut String) {
        for (m, n) in [
            (Mods::CTRL, "ctrl"),
            (Mods::ALT, "alt"),
            (Mods::SHIFT, "shift"),
            (Mods::SUPER, "super"),
        ] {
            if self.contains(m) {
                out.push_str(n);
                out.push('+');
            }
        }
    }
}

/// The key part of a chord.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Layout-dependent keysym name.
    Symbol(String),
    /// Position-dependent key name.
    Physical(String),
}

/// A key with modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyChord {
    /// Modifiers.
    pub mods: Mods,
    /// Key.
    pub key: Key,
}

fn err(msg: impl Into<String>) -> ParseError {
    ParseError::new(ErrorKind::BadValue, msg)
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= MAX_KEY_NAME
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl KeyChord {
    /// Parse `ctrl+shift+a`, `physical:KeyA`, `alt+asciitilde`.
    pub fn parse(s: &str) -> Result<KeyChord, ParseError> {
        if s.is_empty() {
            return Err(err("empty chord"));
        }
        let parts: Vec<&str> = s.split('+').collect();
        let (key_part, mod_parts) = parts.split_last().ok_or_else(|| err("empty chord"))?;
        let mut mods = Mods::NONE;
        for m in mod_parts {
            let Some(bit) = Mods::from_name(m) else {
                return Err(if m.is_empty() {
                    err("empty modifier; a literal `+` key is written `plus`")
                } else {
                    err(format!("unknown modifier `{m}` (ctrl, alt, shift, super)"))
                });
            };
            if mods.contains(bit) {
                return Err(err(format!("duplicate modifier `{m}`")));
            }
            mods = mods.union(bit);
        }
        if key_part.is_empty() {
            return Err(err("missing key; a literal `+` key is written `plus`"));
        }
        if Mods::from_name(key_part).is_some() {
            return Err(err("chord has modifiers but no key"));
        }
        let key = if let Some(p) = key_part.strip_prefix("physical:") {
            if !valid_name(p) {
                return Err(err("bad physical key name ([A-Za-z0-9_])"));
            }
            Key::Physical(p.to_string())
        } else {
            if !valid_name(key_part) {
                return Err(err(
                    "key must be an XKB keysym name ([A-Za-z0-9_]); use `plus`, `greater`, \
                     `asciitilde` rather than the character",
                ));
            }
            Key::Symbol((*key_part).to_string())
        };
        Ok(KeyChord { mods, key })
    }

    /// Canonical text form.
    pub fn format(&self) -> String {
        let mut s = String::new();
        self.mods.push_names(&mut s);
        match &self.key {
            Key::Symbol(n) => s.push_str(n),
            Key::Physical(n) => {
                s.push_str("physical:");
                s.push_str(n);
            }
        }
        s
    }
}

/// Parse `ctrl+a>c` into chords.
pub fn parse_sequence(s: &str) -> Result<Vec<KeyChord>, ParseError> {
    let chords: Result<Vec<KeyChord>, ParseError> = s
        .split('>')
        .map(|p| {
            if p.is_empty() {
                Err(err(
                    "empty chord in sequence; a literal `>` key is written `greater`",
                ))
            } else {
                KeyChord::parse(p)
            }
        })
        .collect();
    let chords = chords?;
    if chords.len() > MAX_SEQUENCE {
        return Err(err(format!("sequence longer than {MAX_SEQUENCE} chords")));
    }
    Ok(chords)
}

/// Canonical text form of a sequence.
pub fn format_sequence(chords: &[KeyChord]) -> String {
    chords
        .iter()
        .map(KeyChord::format)
        .collect::<Vec<_>>()
        .join(">")
}

/// A whitespace-separated list of chords (the argument of `input.send-keys`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordList(pub Vec<KeyChord>);

impl ArgType for ChordList {
    const KIND: ParamKind = ParamKind::Keys;
    fn parse_one(s: &str) -> Result<Self, String> {
        let v: Result<Vec<KeyChord>, ParseError> =
            s.split_whitespace().map(KeyChord::parse).collect();
        let v = v.map_err(|e| e.message)?;
        if v.is_empty() {
            return Err("expected at least one key chord".to_string());
        }
        if v.len() > 64 {
            return Err("more than 64 chords".to_string());
        }
        Ok(ChordList(v))
    }
    fn to_one(&self) -> String {
        self.0
            .iter()
            .map(KeyChord::format)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How well a chord matched a key event. Higher wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    /// Matched the symbol the key would produce unshifted.
    Unshifted,
    /// Matched the produced symbol.
    Symbol,
    /// Matched the physical key (wins when both match).
    Physical,
}

/// What the seat reported for a key press.
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent<'a> {
    /// Modifiers held (locks excluded by the caller).
    pub mods: Mods,
    /// Keysym name produced by the layout with the current modifiers.
    pub produced: &'a str,
    /// Keysym name the key produces with no shift.
    pub unshifted: &'a str,
    /// Physical key name.
    pub physical: &'a str,
}

impl KeyChord {
    /// Match against a key event: physical beats symbol beats unshifted symbol (T-11 §3).
    /// Modifiers must be exactly equal.
    pub fn match_strength(&self, ev: &KeyEvent<'_>) -> Option<Strength> {
        if self.mods != ev.mods {
            return None;
        }
        match &self.key {
            Key::Physical(p) => (p == ev.physical).then_some(Strength::Physical),
            Key::Symbol(s) => {
                if s == ev.produced {
                    Some(Strength::Symbol)
                } else if s == ev.unshifted {
                    Some(Strength::Unshifted)
                } else {
                    None
                }
            }
        }
    }
}

/// Index of the best-matching chord, if any (first wins on equal strength).
pub fn best_match(chords: &[KeyChord], ev: &KeyEvent<'_>) -> Option<usize> {
    let mut best: Option<(Strength, usize)> = None;
    for (i, c) in chords.iter().enumerate() {
        if let Some(s) = c.match_strength(ev)
            && best.is_none_or(|(bs, _)| s > bs)
        {
            best = Some((s, i));
        }
    }
    best.map(|(_, i)| i)
}

crate::string_enum! {
    /// Mouse button or wheel direction.
    MouseButton {
        Left = "left",
        Middle = "middle",
        Right = "right",
        Back = "back",
        Forward = "forward",
        WheelUp = "wheel-up",
        WheelDown = "wheel-down",
        WheelLeft = "wheel-left",
        WheelRight = "wheel-right",
    }
}

impl MouseButton {
    /// True for the wheel directions.
    pub const fn is_wheel(self) -> bool {
        matches!(
            self,
            MouseButton::WheelUp
                | MouseButton::WheelDown
                | MouseButton::WheelLeft
                | MouseButton::WheelRight
        )
    }
}

crate::string_enum! {
    /// When a mouse bind fires.
    MouseOn {
        Press = "press",
        Release = "release",
        Click = "click",
        Double = "double",
        Triple = "triple",
        Drag = "drag",
    }
}

/// A mouse bind trigger: `"ctrl+wheel-up"` with `on=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MouseBind {
    /// Modifiers.
    pub mods: Mods,
    /// Button or wheel.
    pub button: MouseButton,
    /// Phase; default `press`.
    pub on: MouseOn,
}

impl MouseBind {
    /// Parse the chord string and the optional `on=` value.
    pub fn parse(chord: &str, on: Option<&str>) -> Result<MouseBind, ParseError> {
        let parts: Vec<&str> = chord.split('+').collect();
        let (btn, mod_parts) = parts.split_last().ok_or_else(|| err("empty chord"))?;
        let mut mods = Mods::NONE;
        for m in mod_parts {
            let bit = Mods::from_name(m)
                .ok_or_else(|| err(format!("unknown modifier `{m}` (ctrl, alt, shift, super)")))?;
            if mods.contains(bit) {
                return Err(err(format!("duplicate modifier `{m}`")));
            }
            mods = mods.union(bit);
        }
        let button = MouseButton::parse(btn).ok_or_else(|| {
            err(format!(
                "unknown mouse button; one of: {}",
                MouseButton::NAMES.join(", ")
            ))
        })?;
        let on = match on {
            None => MouseOn::Press,
            Some(s) => MouseOn::parse(s).ok_or_else(|| {
                err(format!(
                    "unknown `on`; one of: {}",
                    MouseOn::NAMES.join(", ")
                ))
            })?,
        };
        if button.is_wheel() && on != MouseOn::Press {
            return Err(err("wheel binds only support on=press"));
        }
        Ok(MouseBind { mods, button, on })
    }

    /// Canonical chord string.
    pub fn format_chord(&self) -> String {
        let mut s = String::new();
        self.mods.push_names(&mut s);
        s.push_str(self.button.as_str());
        s
    }
}

crate::string_enum! {
    /// Runtime conditions for `mode=` on a bind.
    Mode {
        AltScreen = "alt-screen",
        AppCursor = "app-cursor",
        CopyMode = "copy-mode",
        Searching = "searching",
        HasSelection = "has-selection",
        KittyKeyboard = "kitty-keyboard",
    }
}

/// One term of a mode condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeTerm {
    /// The mode.
    pub mode: Mode,
    /// `!` prefix.
    pub negated: bool,
}

/// A conjunction of mode terms, comma-separated in text: `alt-screen,!app-cursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeCond(pub Vec<ModeTerm>);

impl ModeCond {
    /// Parse.
    pub fn parse(s: &str) -> Result<ModeCond, ParseError> {
        let mut terms: Vec<ModeTerm> = Vec::new();
        for raw in s.split(',') {
            let raw = raw.trim();
            let (negated, name) = match raw.strip_prefix('!') {
                Some(r) => (true, r),
                None => (false, raw),
            };
            let mode = Mode::parse(name).ok_or_else(|| {
                err(format!(
                    "unknown mode condition; one of: {}",
                    Mode::NAMES.join(", ")
                ))
            })?;
            if let Some(prev) = terms.iter().find(|t| t.mode == mode) {
                return Err(err(if prev.negated == negated {
                    "duplicate mode condition"
                } else {
                    "contradictory mode conditions"
                }));
            }
            terms.push(ModeTerm { mode, negated });
        }
        Ok(ModeCond(terms))
    }

    /// Canonical text.
    pub fn format(&self) -> String {
        self.0
            .iter()
            .map(|t| format!("{}{}", if t.negated { "!" } else { "" }, t.mode.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Evaluate against the active modes.
    pub fn eval(&self, active: impl Fn(Mode) -> bool) -> bool {
        self.0.iter().all(|t| active(t.mode) != t.negated)
    }
}

/// Options on a `bind` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindOptions {
    /// Fire on key repeat (default true).
    pub repeat: bool,
    /// Runtime condition.
    pub mode: Option<ModeCond>,
    /// Fire on release instead of press.
    pub release: bool,
    /// Global shortcut through the portal (R2).
    pub global: bool,
    /// Pass the key through when the action would be a no-op.
    pub performable: bool,
}

impl Default for BindOptions {
    fn default() -> Self {
        Self {
            repeat: true,
            mode: None,
            release: false,
            global: false,
            performable: false,
        }
    }
}

/// A parsed `bind` header: the sequence and its options (the actions are separate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// One chord, or several for a sequence.
    pub sequence: Vec<KeyChord>,
    /// Options.
    pub opts: BindOptions,
}

fn prop_bool(k: &str, v: &str) -> Result<bool, ParseError> {
    match v {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(err(format!("`{k}` expects true or false"))),
    }
}

impl Bind {
    /// Parse `bind "<chord>" key=value…`. Boolean values are `true`/`false`.
    pub fn parse(chord: &str, props: &[(&str, &str)]) -> Result<Bind, ParseError> {
        let sequence = parse_sequence(chord)?;
        let mut o = BindOptions::default();
        let mut seen: Vec<&str> = Vec::new();
        for (k, v) in props {
            if seen.contains(k) {
                return Err(err(format!("duplicate option `{k}`")));
            }
            seen.push(*k);
            match *k {
                "repeat" => o.repeat = prop_bool(k, v)?,
                "release" => o.release = prop_bool(k, v)?,
                "global" => o.global = prop_bool(k, v)?,
                "performable" => o.performable = prop_bool(k, v)?,
                "mode" => o.mode = Some(ModeCond::parse(v)?),
                _ => {
                    return Err(ParseError::new(
                        ErrorKind::UnknownParam,
                        format!("unknown bind option `{k}`"),
                    )
                    .with_suggestion(crate::error::suggest(
                        k,
                        ["repeat", "mode", "release", "global", "performable"].into_iter(),
                    )));
                }
            }
        }
        if sequence.len() > 1 && (o.release || o.global) {
            return Err(err("sequences cannot use release or global"));
        }
        Ok(Bind { sequence, opts: o })
    }

    /// Canonical chord string.
    pub fn format_chord(&self) -> String {
        format_sequence(&self.sequence)
    }

    /// Non-default options as `(name, value)` in canonical order.
    pub fn format_props(&self) -> Vec<(&'static str, String)> {
        let mut v = Vec::new();
        if !self.opts.repeat {
            v.push(("repeat", "false".to_string()));
        }
        if let Some(m) = &self.opts.mode {
            v.push(("mode", m.format()));
        }
        if self.opts.release {
            v.push(("release", "true".to_string()));
        }
        if self.opts.global {
            v.push(("global", "true".to_string()));
        }
        if self.opts.performable {
            v.push(("performable", "true".to_string()));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn mods_any_order_canonical_format() {
        assert_eq!(c("shift+ctrl+a").format(), "ctrl+shift+a");
        assert_eq!(
            c("super+alt+shift+ctrl+x").format(),
            "ctrl+alt+shift+super+x"
        );
        assert_eq!(c("Control+Option+a"), c("ctrl+alt+a"));
        assert_eq!(c("CTRL+a"), c("ctrl+a"));
        assert_eq!(c("cmd+a"), c("super+a"));
    }

    #[test]
    fn keys_are_case_sensitive_symbols() {
        assert_ne!(c("ctrl+A"), c("ctrl+a"));
        assert_eq!(c("alt+asciitilde").key, Key::Symbol("asciitilde".into()));
        assert_eq!(c("ctrl+plus").format(), "ctrl+plus");
    }

    #[test]
    fn physical_keys() {
        let k = c("ctrl+physical:KeyA");
        assert_eq!(k.key, Key::Physical("KeyA".into()));
        assert_eq!(k.format(), "ctrl+physical:KeyA");
        assert!(KeyChord::parse("physical:").is_err());
        assert!(KeyChord::parse("physical:a-b").is_err());
    }

    #[test]
    fn rejects_bad_chords() {
        for bad in [
            "",
            "ctrl+",
            "+a",
            "ctrl++",
            "ctrl+ctrl+a",
            "ctrl",
            "hyper+a",
            "ctrl+é",
            "ctrl+a b",
            "a>b",
        ] {
            assert!(KeyChord::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn chord_roundtrip() {
        for s in [
            "a",
            "ctrl+a",
            "ctrl+alt+shift+super+Return",
            "physical:Space",
            "shift+F5",
        ] {
            assert_eq!(c(s).format(), s);
            assert_eq!(KeyChord::parse(&c(s).format()).unwrap(), c(s));
        }
    }

    #[test]
    fn sequences() {
        let s = parse_sequence("ctrl+a>c").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(format_sequence(&s), "ctrl+a>c");
        assert_eq!(parse_sequence("ctrl+a>greater").unwrap().len(), 2);
        assert!(parse_sequence("ctrl+a>").is_err());
        assert!(parse_sequence(">a").is_err());
        assert!(parse_sequence("a>>b").is_err());
        let long = ["a"; MAX_SEQUENCE + 1].join(">");
        assert!(parse_sequence(&long).is_err());
        assert_eq!(
            parse_sequence(&["a"; MAX_SEQUENCE].join(">"))
                .unwrap()
                .len(),
            MAX_SEQUENCE
        );
    }

    #[test]
    fn physical_beats_symbol_beats_unshifted() {
        let ev = KeyEvent {
            mods: Mods::CTRL,
            produced: "plus",
            unshifted: "equal",
            physical: "Equal",
        };
        assert_eq!(c("ctrl+plus").match_strength(&ev), Some(Strength::Symbol));
        assert_eq!(
            c("ctrl+equal").match_strength(&ev),
            Some(Strength::Unshifted)
        );
        assert_eq!(
            c("ctrl+physical:Equal").match_strength(&ev),
            Some(Strength::Physical)
        );
        assert_eq!(c("ctrl+physical:KeyA").match_strength(&ev), None);
        assert_eq!(c("alt+plus").match_strength(&ev), None);
        let chords = [c("ctrl+equal"), c("ctrl+plus"), c("ctrl+physical:Equal")];
        assert_eq!(best_match(&chords, &ev), Some(2));
        assert_eq!(best_match(&chords[..2], &ev), Some(1));
        assert_eq!(best_match(&[c("ctrl+q")], &ev), None);
    }

    #[test]
    fn chord_list_arg() {
        let l = ChordList::parse_one("ctrl+a  Return").unwrap();
        assert_eq!(l.0.len(), 2);
        assert_eq!(l.to_one(), "ctrl+a Return");
        assert!(ChordList::parse_one("").is_err());
        assert!(ChordList::parse_one("ctrl+").is_err());
    }

    #[test]
    fn mouse_binds() {
        let m = MouseBind::parse("ctrl+wheel-up", None).unwrap();
        assert_eq!(m.button, MouseButton::WheelUp);
        assert_eq!(m.on, MouseOn::Press);
        assert_eq!(m.format_chord(), "ctrl+wheel-up");
        let m = MouseBind::parse("alt+left", Some("double")).unwrap();
        assert_eq!(m.on, MouseOn::Double);
        assert!(MouseBind::parse("wheel-up", Some("click")).is_err());
        assert!(MouseBind::parse("ctrl+bogus", None).is_err());
        assert!(MouseBind::parse("left", Some("nope")).is_err());
        assert!(MouseBind::parse("ctrl+ctrl+left", None).is_err());
        for b in MouseButton::ALL {
            let m = MouseBind::parse(b.as_str(), None).unwrap();
            assert_eq!(m.format_chord(), b.as_str());
        }
    }

    #[test]
    fn mode_conditions() {
        let m = ModeCond::parse("alt-screen,!app-cursor").unwrap();
        assert_eq!(m.format(), "alt-screen,!app-cursor");
        assert!(m.eval(|x| x == Mode::AltScreen));
        assert!(!m.eval(|_| true));
        assert!(!m.eval(|_| false));
        assert!(ModeCond::parse("alt-screen,!alt-screen").is_err());
        assert!(ModeCond::parse("alt-screen,alt-screen").is_err());
        assert!(ModeCond::parse("").is_err());
        assert!(ModeCond::parse("nope").is_err());
        assert_eq!(
            ModeCond::parse(" searching , !copy-mode ").unwrap().0.len(),
            2
        );
    }

    #[test]
    fn bind_options() {
        let b = Bind::parse(
            "ctrl+shift+c",
            &[
                ("mode", "has-selection"),
                ("repeat", "false"),
                ("performable", "true"),
            ],
        )
        .unwrap();
        assert!(!b.opts.repeat && b.opts.performable);
        assert_eq!(
            b.format_props(),
            vec![
                ("repeat", "false".to_string()),
                ("mode", "has-selection".to_string()),
                ("performable", "true".to_string())
            ]
        );
        let again = {
            let owned = b.format_props();
            let props: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
            Bind::parse(&b.format_chord(), &props).unwrap()
        };
        assert_eq!(again, b);
        assert!(Bind::parse("a", &[("repeat", "maybe")]).is_err());
        assert!(Bind::parse("a", &[("repeat", "true"), ("repeat", "false")]).is_err());
        assert!(Bind::parse("ctrl+a>c", &[("release", "true")]).is_err());
        let e = Bind::parse("a", &[("relase", "true")]).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnknownParam);
        assert_eq!(e.suggestion.as_deref(), Some("release"));
        assert_eq!(Bind::parse("a", &[]).unwrap().opts, BindOptions::default());
    }
}
