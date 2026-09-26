// SPDX-License-Identifier: AGPL-3.0-only
//! One modal prompt: what it says, its buttons, and how it is laid out and
//! drawn (COMP-10 §3, §5). Pure. No state, no renderer, no seat, so every rule
//! here is tested without a compositor.
//!
//! Trusted text is `&'static str`. The type is the guarantee: only a string
//! compiled into abyss can reach the trusted position, so runtime text (a
//! widget name, a command) can only go into the untrusted block. There it is
//! sanitised, clamped and drawn in its own frame (§3.2).

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
/// The warning line. Brighter than the body and a different hue, so an
/// altered widget does not read like routine text (§3.2, habituation).
const WARN: Rgba = [1.0, 0.36, 0.20, 1.0];
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
const MAX_BUTTONS: usize = 4;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modal {
    /// Opaque to the prompt. The owner maps it back to what was asked.
    pub token: u64,
    heading: &'static str,
    warning: Option<&'static str>,
    body: &'static str,
    /// Trusted caption over the untrusted block.
    well_label: &'static str,
    /// Sanitised, wrapped and clamped at construction.
    untrusted: Vec<String>,
    buttons: Vec<Button>,
}

/// Why a prompt could not be built. A malformed prompt is refused rather than
/// drawn with a guessed default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// Not exactly one `Safe` button.
    Safe,
    /// No buttons, or more than [`MAX_BUTTONS`].
    Buttons,
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
            well_label,
            untrusted: clamp_untrusted(untrusted),
            buttons,
        })
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

    pub fn untrusted(&self) -> &[String] {
        &self.untrusted
    }
}

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
        lines.push("[... cut: longer than shown]".into());
    }
    if lines.is_empty() {
        lines.push("(empty)".into());
    }
    lines
}

/// A key while a prompt holds the seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Next,
    Prev,
    /// Activate the focused button, whatever it is.
    Activate,
    /// Enter: activate the focused button unless it grants.
    Enter,
    /// Pick the `Safe` button.
    Escape,
    /// Swallowed. The prompt owns the keyboard, so nothing bound elsewhere
    /// may fire under it.
    Ignored,
}

pub fn key(sym: Keysym) -> Key {
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
    Nothing,
}

pub fn apply(modal: &Modal, focus: usize, k: Key) -> Outcome {
    let n = modal.buttons.len();
    match k {
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
    well_y: usize,
    well_h: usize,
    /// `(x, y, w, h)` of each button.
    pub buttons: Vec<(usize, usize, usize, usize)>,
}

fn block_h(rows: usize) -> usize {
    rows * LINE_H
}

pub fn layout(modal: &Modal) -> Layout {
    let text_w = COLS * ADVANCE;
    let w = BORDER + PAD + text_w + PAD + BORDER;
    let warning = modal.warning.map(|s| text::wrap(s, COLS)).unwrap_or_default();
    let body = text::wrap(modal.body, COLS);

    let mut y = BORDER + PAD + LINE_H + GAP;
    if !warning.is_empty() {
        y += block_h(warning.len()) + GAP;
    }
    if !body.is_empty() {
        y += block_h(body.len()) + GAP;
    }
    y += LINE_H;
    let well_y = y;
    let well_h = WELL_PAD + block_h(modal.untrusted.len()) + WELL_PAD;
    y += well_h + GAP * 2;

    // Buttons right-aligned, in the order given.
    let widths: Vec<usize> = modal
        .buttons
        .iter()
        .map(|b| b.label.len() * ADVANCE + BTN_PAD * 2)
        .collect();
    let total = widths.iter().sum::<usize>() + BTN_GAP * widths.len().saturating_sub(1);
    let mut x = w - BORDER - PAD - total;
    let mut buttons = Vec::with_capacity(widths.len());
    for bw in widths {
        buttons.push((x, y, bw, BTN_H));
        x += bw + BTN_GAP;
    }
    y += BTN_H + PAD + BORDER;
    Layout {
        w,
        h: y,
        body,
        warning,
        well_y,
        well_h,
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

/// Draw the panel at `scale` device pixels per logical pixel.
pub fn rasterize(modal: &Modal, focus: usize, scale: usize) -> Raster {
    let l = layout(modal);
    let mut c = Canvas::new(l.w, l.h);
    c.fill(0, 0, l.w, l.h, EDGE);
    c.fill(BORDER, BORDER, l.w - BORDER * 2, l.h - BORDER * 2, PANEL);

    let x0 = BORDER + PAD;
    let mut y = BORDER + PAD;
    c.text(x0, y, modal.heading, HEADING, true);
    y += LINE_H + GAP;
    if !l.warning.is_empty() {
        for (i, line) in l.warning.iter().enumerate() {
            c.text(x0, y + i * LINE_H, line, WARN, true);
        }
        y += block_h(l.warning.len()) + GAP;
    }
    for (i, line) in l.body.iter().enumerate() {
        c.text(x0, y + i * LINE_H, line, BODY, false);
    }
    c.text(x0, l.well_y - LINE_H, modal.well_label, BODY, false);

    let ww = COLS * ADVANCE;
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

    for (i, (b, &(bx, by, bw, bh))) in modal.buttons.iter().zip(&l.buttons).enumerate() {
        let tx = bx + BTN_PAD;
        let ty = by + (bh - GLYPH_H) / 2 + 1;
        if i == focus {
            c.fill(bx, by, bw, bh, EDGE);
            c.text(tx, ty, b.label, INK, true);
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
        let m = modal("x");
        for focus in 0..3 {
            assert_eq!(apply(&m, focus, Key::Escape), Outcome::Choose(1));
        }
    }

    #[test]
    fn enter_never_grants() {
        let m = modal("x");
        assert_eq!(apply(&m, 2, key(Keysym::Return)), Outcome::Nothing);
        assert_eq!(apply(&m, 2, key(Keysym::KP_Enter)), Outcome::Nothing);
        assert_eq!(apply(&m, 1, key(Keysym::Return)), Outcome::Choose(1));
        assert_eq!(apply(&m, 0, key(Keysym::Return)), Outcome::Choose(0));
        // A deliberate Space on a focused Grant is the keyboard path to it.
        assert_eq!(apply(&m, 2, key(Keysym::space)), Outcome::Choose(2));
    }

    #[test]
    fn focus_cycles_and_other_keys_are_swallowed() {
        let m = modal("x");
        assert_eq!(apply(&m, 2, key(Keysym::Tab)), Outcome::Focus(0));
        assert_eq!(apply(&m, 0, key(Keysym::ISO_Left_Tab)), Outcome::Focus(2));
        for sym in [Keysym::a, Keysym::y, Keysym::Delete, Keysym::F1] {
            assert_eq!(apply(&m, 1, key(sym)), Outcome::Nothing, "{sym:?}");
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
            let r = rasterize(&m, m.safe(), scale);
            assert_eq!((r.w as usize, r.h as usize), (l.w * scale, l.h * scale));
            assert_eq!(r.px.len(), l.w * l.h * scale * scale * 4);
        }
    }

    #[test]
    fn the_panel_is_opaque() {
        // Nothing behind a prompt may show through it and be read as part of it.
        let m = modal("echo hi");
        let r = rasterize(&m, m.safe(), 1);
        assert!(r.px.chunks(4).all(|p| p[3] == 255));
    }
}
