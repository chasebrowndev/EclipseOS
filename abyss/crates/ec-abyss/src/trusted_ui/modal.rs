// SPDX-License-Identifier: AGPL-3.0-only
//! One modal prompt: what it says, its buttons, and how it is laid out and
//! drawn (COMP-10 §3, §5). Pure. No state, no renderer, no seat, so every rule
//! here is tested without a compositor.
//!
//! Trusted text is `&'static str`. The type is the guarantee: only a string
//! compiled into abyss can reach the trusted position, so runtime text (a
//! widget name, a command) can only go into the untrusted block. There it is
//! sanitised, clamped and drawn in its own frame (§3.2).
//!
//! Two narrower kinds of runtime text exist, and each has its own place:
//!
//! - **Facts** (§3.2): values the compositor holds about a request, such as
//!   the principal, the window title or the task statement. Each sits after
//!   a compiled-in label, on one sanitised, clamped line, so it can never be
//!   a heading, a warning or a button.
//! - **The personal secret** (§2): passed to [`rasterize`] at draw time and
//!   never stored in a [`Modal`], drawn in its own fixed frame at the top.
//!
//! A prompt can also take one line of typed text ([`Modal::with_entry`]),
//! for setting the secret. What is typed is held as [`Typed`], whose `Debug`
//! never shows it: human input is never logged by content.

use smithay::input::keyboard::Keysym;

use crate::render::{
    font::{ADVANCE, GLYPH_H, LINE_H},
    text::{self, Canvas, Raster, Rgba},
};

/// Fixed palette, not themeable (§5: a themeable trusted surface is a
/// spoofable one). Gold on near-black, like the rest of the system chrome.
const PANEL: Rgba = [0.020, 0.016, 0.012, 1.0];
const EDGE: Rgba = [1.0, 0.72, 0.20, 1.0];
const HEADING: Rgba = [1.0, 0.78, 0.26, 1.0];
const BODY: Rgba = [0.87, 0.76, 0.47, 1.0];
/// Fact labels: dimmer than the value beside them, which is what the human
/// must read. Bold stays for the heading and the warning.
const LABEL: Rgba = [0.66, 0.55, 0.30, 1.0];
/// The warning line. Brighter than the body and a different hue, so an
/// altered widget does not read like routine text (§3.2, habituation).
const WARN: Rgba = [1.0, 0.36, 0.20, 1.0];
/// An irreversible request's edge (§3.2): a different hue from every routine
/// prompt, so habituation on routine prompts does not carry over.
const ACCENT: Rgba = [1.0, 0.36, 0.20, 1.0];
/// The personal secret's frame.
const PHRASE_GROUND: Rgba = [0.10, 0.075, 0.03, 1.0];
/// The untrusted block sits on a different ground with its own dim edge, so it
/// cannot pass for the panel around it.
const WELL: Rgba = [0.070, 0.060, 0.050, 1.0];
const WELL_EDGE: Rgba = [0.40, 0.29, 0.09, 1.0];
const WELL_TEXT: Rgba = [0.80, 0.80, 0.78, 1.0];
const INK: Rgba = [0.020, 0.016, 0.012, 1.0];

/// Text columns. Fixed: the caller has no say in the panel's width.
pub const COLS: usize = 60;
const PAD: usize = 16;
const BORDER: usize = 2;
const GAP: usize = 10;
const WELL_PAD: usize = 8;
const BTN_PAD: usize = 12;
const BTN_H: usize = GLYPH_H + 8;
const BTN_GAP: usize = 12;
/// Lines of untrusted text kept. The rest is dropped, and says so.
pub const MAX_UNTRUSTED_LINES: usize = 8;

/// COMP-10 §2: with no personal secret set, every prompt says anti-spoofing
/// is unconfigured.
const UNSPOOFED: &str = "Anti-spoofing is not configured: no personal secret is set.";
/// The consent prompt's five answers (§3.2) are the most any prompt has.
const MAX_BUTTONS: usize = 5;
/// Fact lines kept. The emergency panel's agent page is the longest: who,
/// what, its grants and the last 20 actions (COMP-10 §3.3).
pub const MAX_FACTS: usize = 32;
/// The widest label a fact may have, so the value always has room.
const MAX_LABEL: usize = 16;

/// What pressing a button means, as far as the prompt itself is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Changes nothing, for example "Not now". Every prompt has exactly one.
    /// It holds the default focus and is what Escape and the timeout pick.
    Safe,
    /// Grants something. Enter never activates it (COMP-10 §3.2, §3.11).
    Grant,
    /// Anything else, for example Revert or Remove.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub label: &'static str,
    pub role: Role,
}

/// Text the human typed into a prompt. Its `Debug` says only how long it is.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Typed(String);

impl Typed {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Typed(<{} chars>)", self.0.chars().count())
    }
}

/// A one-line text field.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    text: Typed,
    max: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modal {
    /// Opaque to the prompt. The owner maps it back to what was asked.
    pub token: u64,
    heading: &'static str,
    warning: Option<&'static str>,
    body: &'static str,
    /// Labelled single-line facts, sanitised and clamped by `with_facts`.
    facts: Vec<(&'static str, String)>,
    /// Trusted caption over the untrusted block.
    well_label: &'static str,
    /// Sanitised, wrapped and clamped at construction.
    untrusted: Vec<String>,
    buttons: Vec<Button>,
    /// Drawn with the irreversible accent (§3.2).
    accent: bool,
    entry: Option<Entry>,
}

/// Why a prompt could not be built. A malformed prompt is refused rather than
/// drawn with a guessed default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// Not exactly one `Safe` button.
    Safe,
    /// No buttons, or more than [`MAX_BUTTONS`].
    Buttons,
    /// A fact that must be shown whole did not fit.
    TooLong,
}

impl Modal {
    pub fn new(
        token: u64,
        heading: &'static str,
        warning: Option<&'static str>,
        body: &'static str,
        well_label: &'static str,
        untrusted: &str,
        buttons: Vec<Button>,
    ) -> Result<Modal, Invalid> {
        if buttons.is_empty() || buttons.len() > MAX_BUTTONS {
            return Err(Invalid::Buttons);
        }
        if buttons.iter().filter(|b| b.role == Role::Safe).count() != 1 {
            return Err(Invalid::Safe);
        }
        Ok(Modal {
            token,
            heading,
            warning,
            body,
            facts: Vec::new(),
            well_label,
            untrusted: clamp_untrusted(untrusted),
            buttons,
            accent: false,
            entry: None,
        })
    }

    /// Add labelled facts, in order. Each value is reduced to one sanitised
    /// line and cut to the space after its label; a cut ends in `...`.
    /// Facts past [`MAX_FACTS`] are dropped.
    pub fn with_facts(mut self, facts: &[(&'static str, &str)]) -> Modal {
        for &(label, value) in facts.iter().take(MAX_FACTS.saturating_sub(self.facts.len())) {
            let label = &label[..label.len().min(MAX_LABEL)];
            let room = COLS - MAX_LABEL - 1;
            let mut v = text::sanitize_line(value);
            if v.chars().count() > room {
                v = v.chars().take(room - 3).collect::<String>() + "...";
            }
            if v.is_empty() {
                v = "(none)".into();
            }
            self.facts.push((label, v));
        }
        self
    }

    /// Add one fact that must be shown whole, such as the exact scope a
    /// grant would carry (§3.2): wrapped onto as many lines as it needs, the
    /// label on the first. If it needs more than `max_lines`, or would push
    /// the facts past [`MAX_FACTS`], the prompt is refused rather than drawn
    /// with part of it missing.
    pub fn with_whole_fact(
        mut self,
        label: &'static str,
        value: &str,
        max_lines: usize,
    ) -> Result<Modal, Invalid> {
        let label = &label[..label.len().min(MAX_LABEL)];
        let room = COLS - MAX_LABEL - 1;
        let v = text::sanitize_line(value);
        if v.chars().count() >= text::MAX_CHARS {
            return Err(Invalid::TooLong);
        }
        // Hard wrap at the column, so no character is lost to word wrapping.
        let chars: Vec<char> = v.chars().collect();
        let mut lines: Vec<String> = chars.chunks(room).map(|c| c.iter().collect()).collect();
        if lines.is_empty() {
            lines.push("(none)".into());
        }
        if lines.len() > max_lines || self.facts.len() + lines.len() > MAX_FACTS {
            return Err(Invalid::TooLong);
        }
        for (i, line) in lines.into_iter().enumerate() {
            self.facts.push((if i == 0 { label } else { "" }, line));
        }
        Ok(self)
    }

    /// Draw with the irreversible accent.
    pub fn irreversible(mut self) -> Modal {
        self.accent = true;
        self
    }

    /// Give the prompt one line of typed text, at most `max` characters.
    pub fn with_entry(mut self, max: usize) -> Modal {
        self.entry = Some(Entry {
            text: Typed::default(),
            max: max.clamp(1, COLS),
        });
        self
    }

    /// What has been typed, if the prompt takes text.
    pub fn typed(&self) -> Option<&Typed> {
        self.entry.as_ref().map(|e| &e.text)
    }

    pub fn takes_text(&self) -> bool {
        self.entry.is_some()
    }

    pub fn buttons(&self) -> &[Button] {
        &self.buttons
    }

    /// Index of the one `Safe` button. `new` guarantees there is one.
    pub fn safe(&self) -> usize {
        self.buttons
            .iter()
            .position(|b| b.role == Role::Safe)
            .unwrap_or(0)
    }

    pub fn facts(&self) -> &[(&'static str, String)] {
        &self.facts
    }

    pub fn heading(&self) -> &'static str {
        self.heading
    }

    pub fn untrusted(&self) -> &[String] {
        &self.untrusted
    }

    /// Whether the untrusted text was longer than the well shows.
    pub fn is_cut(&self) -> bool {
        self.untrusted.last().is_some_and(|l| l == CUT)
    }
}

const CUT: &str = "[... cut: longer than shown]";

/// §3.2 treatment for text the compositor did not write: control characters
/// stripped, no markup, hard length and line clamp. A cut is marked, so the
/// human knows there is more than they can see.
fn clamp_untrusted(input: &str) -> Vec<String> {
    let clean = text::sanitize(input);
    let mut lines = text::wrap(&clean, COLS);
    let cut = lines.len() > MAX_UNTRUSTED_LINES || clean.len() >= text::MAX_CHARS;
    lines.truncate(MAX_UNTRUSTED_LINES);
    if cut {
        if lines.len() == MAX_UNTRUSTED_LINES {
            lines.pop();
        }
        lines.push(CUT.into());
    }
    if lines.is_empty() {
        lines.push("(empty)".into());
    }
    lines
}

/// A key while a prompt holds the seat.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Next,
    Prev,
    /// Activate the focused button, whatever it is.
    Activate,
    /// Enter: activate the focused button unless it grants.
    Enter,
    /// Pick the `Safe` button.
    Escape,
    /// A printable character, only while the prompt takes text.
    Char(char),
    Backspace,
    /// Swallowed. The prompt owns the keyboard, so nothing bound elsewhere
    /// may fire under it.
    Ignored,
}

impl std::fmt::Debug for Key {
    /// Never the character itself: human input is not logged by content.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Next => f.write_str("Next"),
            Key::Prev => f.write_str("Prev"),
            Key::Activate => f.write_str("Activate"),
            Key::Enter => f.write_str("Enter"),
            Key::Escape => f.write_str("Escape"),
            Key::Char(_) => f.write_str("Char(_)"),
            Key::Backspace => f.write_str("Backspace"),
            Key::Ignored => f.write_str("Ignored"),
        }
    }
}

/// `typing` is whether the prompt takes text: then space types a space
/// rather than activating, and only Tab moves the focus.
pub fn key(sym: Keysym, typing: bool) -> Key {
    if typing {
        return match sym {
            Keysym::Tab => Key::Next,
            Keysym::ISO_Left_Tab => Key::Prev,
            Keysym::Return | Keysym::KP_Enter => Key::Enter,
            Keysym::Escape => Key::Escape,
            Keysym::BackSpace => Key::Backspace,
            // The panel's font is printable ASCII; nothing else is typed.
            _ => match sym.key_char() {
                Some(c) if (' '..='~').contains(&c) => Key::Char(c),
                _ => Key::Ignored,
            },
        };
    }
    match sym {
        Keysym::Tab | Keysym::Right | Keysym::Down => Key::Next,
        Keysym::ISO_Left_Tab | Keysym::Left | Keysym::Up => Key::Prev,
        Keysym::space => Key::Activate,
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Escape => Key::Escape,
        _ => Key::Ignored,
    }
}

/// What a key does to a prompt whose focus is `focus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Focus(usize),
    Choose(usize),
    /// The typed text changed.
    Edited,
    Nothing,
}

pub fn apply(modal: &mut Modal, focus: usize, k: Key) -> Outcome {
    let n = modal.buttons.len();
    match k {
        Key::Char(c) => match modal.entry.as_mut() {
            Some(e) if e.text.0.chars().count() < e.max => {
                e.text.0.push(c);
                Outcome::Edited
            }
            _ => Outcome::Nothing,
        },
        Key::Backspace => match modal.entry.as_mut().and_then(|e| e.text.0.pop()) {
            Some(_) => Outcome::Edited,
            None => Outcome::Nothing,
        },
        Key::Next => Outcome::Focus((focus + 1) % n),
        Key::Prev => Outcome::Focus((focus + n - 1) % n),
        Key::Activate => Outcome::Choose(focus),
        Key::Enter if modal.buttons[focus].role == Role::Grant => Outcome::Nothing,
        Key::Enter => Outcome::Choose(focus),
        Key::Escape => Outcome::Choose(modal.safe()),
        Key::Ignored => Outcome::Nothing,
    }
}

/// Where everything goes, panel-local and logical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub w: usize,
    pub h: usize,
    body: Vec<String>,
    warning: Vec<String>,
    heading_y: usize,
    facts_y: usize,
    well_y: usize,
    well_h: usize,
    entry_y: usize,
    /// `(x, y, w, h)` of each button.
    pub buttons: Vec<(usize, usize, usize, usize)>,
}

/// Taller than the entry field, and framed twice as heavy below: the personal
/// secret is the anti-spoofing anchor and must not read as one more box.
const PHRASE_H: usize = LINE_H + 10;
const PHRASE_EDGE: usize = 2;
const ENTRY_H: usize = LINE_H + 8;

fn block_h(rows: usize) -> usize {
    rows * LINE_H
}

pub fn layout(modal: &Modal) -> Layout {
    let text_w = COLS * ADVANCE;
    let w = BORDER + PAD + text_w + PAD + BORDER;
    let warning = modal.warning.map(|s| text::wrap(s, COLS)).unwrap_or_default();
    let body = text::wrap(modal.body, COLS);

    // The personal secret's frame, always first, then the heading.
    let mut y = BORDER + PAD + PHRASE_H + GAP;
    let heading_y = y;
    y += LINE_H + GAP;
    if !warning.is_empty() {
        y += block_h(warning.len()) + GAP;
    }
    if !body.is_empty() {
        y += block_h(body.len()) + GAP;
    }
    let facts_y = y;
    if !modal.facts.is_empty() {
        y += block_h(modal.facts.len()) + GAP;
    }
    y += LINE_H;
    let well_y = y;
    let well_h = WELL_PAD + block_h(modal.untrusted.len()) + WELL_PAD;
    y += well_h + GAP;
    let entry_y = y;
    if modal.entry.is_some() {
        y += ENTRY_H + GAP;
    }

    // Buttons right-aligned, in the order given, wrapped onto a new row when
    // the next one would not fit.
    let inner = w - 2 * (BORDER + PAD);
    let widths: Vec<usize> = modal
        .buttons
        .iter()
        .map(|b| b.label.len() * ADVANCE + BTN_PAD * 2)
        .collect();
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0;
    for (i, &bw) in widths.iter().enumerate() {
        let need = if used == 0 { bw } else { used + BTN_GAP + bw };
        if need > inner && used > 0 {
            rows.push(Vec::new());
            used = bw;
        } else {
            used = need;
        }
        if let Some(row) = rows.last_mut() {
            row.push(i);
        }
    }
    let mut buttons = vec![(0, 0, 0, 0); widths.len()];
    for row in &rows {
        let total = row.iter().map(|&i| widths[i]).sum::<usize>() + BTN_GAP * row.len().saturating_sub(1);
        let mut x = w - BORDER - PAD - total.min(inner);
        for &i in row {
            buttons[i] = (x, y, widths[i], BTN_H);
            x += widths[i] + BTN_GAP;
        }
        y += BTN_H + BTN_GAP;
    }
    y += PAD + BORDER - BTN_GAP;
    Layout {
        w,
        h: y,
        body,
        warning,
        heading_y,
        facts_y,
        well_y,
        well_h,
        entry_y,
        buttons,
    }
}

/// The button under a panel-local logical point, if any.
pub fn hit(layout: &Layout, x: i32, y: i32) -> Option<usize> {
    let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
    layout
        .buttons
        .iter()
        .position(|&(bx, by, bw, bh)| x >= bx && x < bx + bw && y >= by && y < by + bh)
}

/// Draw the panel at `scale` device pixels per logical pixel. `phrase` is
/// the personal secret, `None` when none is set (COMP-10 §2): it is drawn in
/// its fixed frame at the top, verbatim, or the unconfigured warning is.
pub fn rasterize(modal: &Modal, focus: usize, scale: usize, phrase: Option<&str>) -> Raster {
    let l = layout(modal);
    let edge = if modal.accent { ACCENT } else { EDGE };
    let mut c = Canvas::new(l.w, l.h);
    c.fill(0, 0, l.w, l.h, edge);
    c.fill(BORDER, BORDER, l.w - BORDER * 2, l.h - BORDER * 2, PANEL);

    let x0 = BORDER + PAD;
    let ww = COLS * ADVANCE;
    c.fill(x0 - WELL_PAD, BORDER + PAD, ww + WELL_PAD * 2, PHRASE_H, EDGE);
    c.fill(
        x0 - WELL_PAD + PHRASE_EDGE,
        BORDER + PAD + PHRASE_EDGE,
        ww + WELL_PAD * 2 - 2 * PHRASE_EDGE,
        PHRASE_H - 2 * PHRASE_EDGE,
        PHRASE_GROUND,
    );
    let py = BORDER + PAD + (PHRASE_H - GLYPH_H) / 2;
    match phrase {
        Some(p) => c.text(x0, py, p, HEADING, true),
        None => c.text(x0, py, UNSPOOFED, WARN, false),
    }

    let mut y = l.heading_y;
    c.text(x0, y, modal.heading, HEADING, true);
    y += LINE_H + GAP;
    if !l.warning.is_empty() {
        for (i, line) in l.warning.iter().enumerate() {
            c.text(x0, y + i * LINE_H, line, WARN, true);
        }
        y += block_h(l.warning.len()) + GAP;
    }
    for (i, line) in l.body.iter().enumerate() {
        // An irreversible prompt's first body line is its reversal wording
        // (consent.rs): it carries the warning colour, not just the edge.
        if modal.accent && i == 0 {
            c.text(x0, y + i * LINE_H, line, WARN, true);
        } else {
            c.text(x0, y + i * LINE_H, line, BODY, false);
        }
    }
    for (i, (label, value)) in modal.facts.iter().enumerate() {
        let fy = l.facts_y + i * LINE_H;
        c.text(x0, fy, label, LABEL, false);
        c.text(x0 + (MAX_LABEL + 1) * ADVANCE, fy, value, BODY, false);
    }
    c.text(x0, l.well_y - LINE_H, modal.well_label, BODY, false);

    c.fill(x0 - WELL_PAD, l.well_y, ww + WELL_PAD * 2, l.well_h, WELL_EDGE);
    c.fill(
        x0 - WELL_PAD + 1,
        l.well_y + 1,
        ww + WELL_PAD * 2 - 2,
        l.well_h - 2,
        WELL,
    );
    for (i, line) in modal.untrusted.iter().enumerate() {
        c.text(x0, l.well_y + WELL_PAD + i * LINE_H, line, WELL_TEXT, false);
    }
    if let Some(e) = &modal.entry {
        c.fill(x0 - WELL_PAD, l.entry_y, ww + WELL_PAD * 2, ENTRY_H, EDGE);
        c.fill(
            x0 - WELL_PAD + 1,
            l.entry_y + 1,
            ww + WELL_PAD * 2 - 2,
            ENTRY_H - 2,
            PANEL,
        );
        let ty = l.entry_y + (ENTRY_H - GLYPH_H) / 2;
        let typed = e.text.as_str();
        c.text(x0, ty, typed, HEADING, false);
        // The caret: a bar after the last character.
        let cx = x0 + typed.chars().count() * ADVANCE;
        c.fill(cx, ty, 2, GLYPH_H, HEADING);
    }

    for (i, (b, &(bx, by, bw, bh))) in modal.buttons.iter().zip(&l.buttons).enumerate() {
        let tx = bx + BTN_PAD;
        let ty = by + (bh - GLYPH_H) / 2 + 1;
        if i == focus {
            c.fill(bx, by, bw, bh, EDGE);
            // Not bold: a 1 px double strike fills the counters of dark
            // glyphs on the bright fill. The inverted fill is the focus cue.
            c.text(tx, ty, b.label, INK, false);
        } else {
            c.fill(bx, by, bw, bh, EDGE);
            c.fill(bx + 1, by + 1, bw - 2, bh - 2, PANEL);
            c.text(tx, ty, b.label, HEADING, false);
        }
    }
    c.into_raster(scale.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons() -> Vec<Button> {
        vec![
            Button {
                label: "Revert",
                role: Role::Other,
            },
            Button {
                label: "Not now",
                role: Role::Safe,
            },
            Button {
                label: "Accept",
                role: Role::Grant,
            },
        ]
    }

    fn key_(sym: Keysym) -> Key {
        key(sym, false)
    }

    fn modal(untrusted: &str) -> Modal {
        Modal::new(
            7,
            "Command approval",
            Some("Warning"),
            "Body",
            "Command:",
            untrusted,
            buttons(),
        )
        .unwrap()
    }

    #[test]
    fn a_prompt_needs_exactly_one_safe_button() {
        let none = vec![Button {
            label: "Accept",
            role: Role::Grant,
        }];
        assert_eq!(Modal::new(0, "", None, "", "", "", none), Err(Invalid::Safe));
        let two = vec![
            Button {
                label: "A",
                role: Role::Safe,
            },
            Button {
                label: "B",
                role: Role::Safe,
            },
        ];
        assert_eq!(Modal::new(0, "", None, "", "", "", two), Err(Invalid::Safe));
        assert_eq!(Modal::new(0, "", None, "", "", "", vec![]), Err(Invalid::Buttons));
    }

    #[test]
    fn escape_picks_the_safe_button_from_anywhere() {
        let mut m = modal("x");
        for focus in 0..3 {
            assert_eq!(apply(&mut m, focus, Key::Escape), Outcome::Choose(1));
        }
    }

    #[test]
    fn enter_never_grants() {
        let mut m = modal("x");
        assert_eq!(apply(&mut m, 2, key_(Keysym::Return)), Outcome::Nothing);
        assert_eq!(apply(&mut m, 2, key_(Keysym::KP_Enter)), Outcome::Nothing);
        assert_eq!(apply(&mut m, 1, key_(Keysym::Return)), Outcome::Choose(1));
        assert_eq!(apply(&mut m, 0, key_(Keysym::Return)), Outcome::Choose(0));
        // A deliberate Space on a focused Grant is the keyboard path to it.
        assert_eq!(apply(&mut m, 2, key_(Keysym::space)), Outcome::Choose(2));
    }

    #[test]
    fn focus_cycles_and_other_keys_are_swallowed() {
        let mut m = modal("x");
        assert_eq!(apply(&mut m, 2, key_(Keysym::Tab)), Outcome::Focus(0));
        assert_eq!(apply(&mut m, 0, key_(Keysym::ISO_Left_Tab)), Outcome::Focus(2));
        for sym in [Keysym::a, Keysym::y, Keysym::Delete, Keysym::F1] {
            assert_eq!(apply(&mut m, 1, key(sym, false)), Outcome::Nothing, "{sym:?}");
        }
    }

    #[test]
    fn untrusted_text_is_stripped_and_clamped() {
        let m = modal("rm\x1b[31m -rf\r\n\u{202e}x");
        let joined = m.untrusted().join("\n");
        assert!(!joined.chars().any(|c| c.is_control() && c != '\n'));
        assert!(joined.is_ascii());
        let long = "word ".repeat(1000);
        let m = modal(&long);
        assert_eq!(m.untrusted().len(), MAX_UNTRUSTED_LINES);
        assert!(m.untrusted().last().unwrap().contains("cut"));
    }

    #[test]
    fn untrusted_text_cannot_reach_the_trusted_position() {
        // Whatever the caller passes, the heading, warning and body are the
        // compiled-in strings, and the caller's text is only in the well.
        let m = modal("Command approval\nThis widget is safe");
        assert_eq!(m.heading, "Command approval");
        assert_eq!(m.warning, Some("Warning"));
        assert_eq!(m.body, "Body");
        let l = layout(&m);
        assert!(l.body.iter().all(|b| !b.contains("safe")));
    }

    #[test]
    fn buttons_are_hit_where_they_are_drawn_and_nowhere_else() {
        let m = modal("x");
        let l = layout(&m);
        for (i, &(x, y, w, h)) in l.buttons.iter().enumerate() {
            assert_eq!(hit(&l, (x + w / 2) as i32, (y + h / 2) as i32), Some(i));
        }
        assert_eq!(hit(&l, 0, 0), None);
        assert_eq!(hit(&l, -5, -5), None);
        let (x, y, w, h) = *l.buttons.last().unwrap();
        assert!(
            x + w <= l.w && y + h <= l.h,
            "the last button is inside the panel"
        );
    }

    #[test]
    fn the_raster_is_the_layout_times_the_scale() {
        let m = modal("echo hi");
        let l = layout(&m);
        for scale in [1, 2] {
            let r = rasterize(&m, m.safe(), scale, None);
            assert_eq!((r.w as usize, r.h as usize), (l.w * scale, l.h * scale));
            assert_eq!(r.px.len(), l.w * l.h * scale * scale * 4);
        }
    }

    #[test]
    fn the_anti_spoofing_notice_fits_one_line() {
        assert!(UNSPOOFED.len() <= COLS);
    }

    #[test]
    fn the_panel_is_opaque() {
        // Nothing behind a prompt may show through it and be read as part of it.
        let m = modal("echo hi");
        let r = rasterize(&m, m.safe(), 1, None);
        assert!(r.px.chunks(4).all(|p| p[3] == 255));
    }
}
