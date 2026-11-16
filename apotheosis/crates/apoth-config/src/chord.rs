// SPDX-License-Identifier: AGPL-3.0-only
//! Key chord syntax of T-11 §3, enough for config validation and canonical formatting.
//!
//! Chord parsing delegates to `apoth-action` so config and runtime agree (T-10 vs T-11 §3).

use apoth_action::key::{Key, KeyChord, Mods, parse_sequence};

/// Modifier names in canonical order (the set `apoth-action` accepts).
pub const MODIFIERS: &[&str] = &["ctrl", "alt", "shift", "super"];

/// One chord: a modifier set and a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Bit `i` is `MODIFIERS[i]`.
    pub mods: u8,
    /// Keysym name, or `physical:<name>`.
    pub key: String,
}

fn canon_mod(s: &str) -> Option<usize> {
    let l = s.to_ascii_lowercase();
    let name = match l.as_str() {
        "control" => "ctrl",
        "option" => "alt",
        "logo" | "win" | "cmd" => "super",
        other => other,
    };
    MODIFIERS.iter().position(|m| *m == name)
}

fn from_action(k: &KeyChord) -> Chord {
    let mut mods = 0u8;
    for (i, m) in [Mods::CTRL, Mods::ALT, Mods::SHIFT, Mods::SUPER]
        .into_iter()
        .enumerate()
    {
        if k.mods.contains(m) {
            mods |= 1 << i;
        }
    }
    let key = match &k.key {
        Key::Symbol(n) => n.clone(),
        Key::Physical(n) => format!("physical:{n}"),
    };
    Chord { mods, key }
}

/// Parse one chord (`ctrl+shift+t`, `physical:KeyT`, `alt+physical:comma`). The grammar and
/// limits are `apoth-action`'s (T-11 §3); this only converts the result.
pub fn parse_chord(s: &str) -> Result<Chord, String> {
    KeyChord::parse(s)
        .map(|k| from_action(&k))
        .map_err(|e| e.to_string())
}

/// Parse a multi-key sequence `ctrl+a>c` into its chords (at most 8; T-11 §3).
pub fn parse_chord_seq(s: &str) -> Result<Vec<Chord>, String> {
    parse_sequence(s)
        .map(|v| v.iter().map(from_action).collect())
        .map_err(|e| e.to_string())
}

impl Chord {
    /// Canonical text: modifiers in [`MODIFIERS`] order, then the key.
    pub fn format(&self) -> String {
        let mut out = String::new();
        for (i, m) in MODIFIERS.iter().enumerate() {
            if self.mods & (1 << i) != 0 {
                out.push_str(m);
                out.push('+');
            }
        }
        out.push_str(&self.key);
        out
    }
}

/// Canonical text of a sequence.
pub fn format_chord_seq(seq: &[Chord]) -> String {
    seq.iter().map(Chord::format).collect::<Vec<_>>().join(">")
}

/// Mouse binding trigger: `<mods>+<button|wheel-up|…>`.
pub const MOUSE_BUTTONS: &[&str] = &[
    "left",
    "middle",
    "right",
    "back",
    "forward",
    "wheel-up",
    "wheel-down",
    "wheel-left",
    "wheel-right",
];

/// Mouse bind events (`on=`).
pub const MOUSE_EVENTS: &[&str] = &["press", "release", "click", "double", "triple", "drag"];

/// Validate `<mods>+<button>`, returning the canonical text.
pub fn parse_mouse_trigger(s: &str) -> Result<String, String> {
    let parts: Vec<&str> = s.split('+').collect();
    let (button, mod_parts) = parts.split_last().ok_or("empty trigger")?;
    if !MOUSE_BUTTONS.contains(button) {
        return Err(format!("unknown mouse button `{button}`"));
    }
    let mut mods = 0u8;
    for m in mod_parts {
        let i = canon_mod(m).ok_or_else(|| format!("unknown modifier `{m}`"))?;
        if mods & (1 << i) != 0 {
            return Err(format!("duplicate modifier `{m}`"));
        }
        mods |= 1 << i;
    }
    let c = Chord {
        mods,
        key: (*button).to_string(),
    };
    Ok(c.format())
}

/// Bind `mode=` conditions (T-11 §3), with optional `!` negation, joined by commas.
pub const BIND_MODES: &[&str] = &[
    "alt-screen",
    "app-cursor",
    "copy-mode",
    "searching",
    "has-selection",
    "kitty-keyboard",
];

pub fn validate_mode_condition(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty mode condition".into());
    }
    for term in s.split(',') {
        let name = term.trim().strip_prefix('!').unwrap_or(term.trim());
        if !BIND_MODES.contains(&name) {
            return Err(format!("unknown mode `{name}`"));
        }
    }
    Ok(())
}
