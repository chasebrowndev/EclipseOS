// SPDX-License-Identifier: AGPL-3.0-only
//! Consent / error bar (T-08 §4-§5, T-18 §3, T-05 §6, T-10 config-error overlay).
//!
//! Drawn outside the pane's grid area (it takes space from the top of the pane, so cell
//! content cannot imitate it), never steals focus, ignores all input for 500 ms after it
//! appears, and is never answered by Return or Space.

use crate::draw::{DrawList, IconId, Rim};
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{ControlState, UiState, WidgetId, label_colour, surface};
use crate::text::{Measure, TextRun};
use crate::tokens::{Palette, metrics, motion, radius, size, space};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    /// Allow once / always for this pane / deny (T-18 §3), paste confirm (T-05 §6).
    Consent,
    /// Config errors with file:line (T-10).
    Error,
    /// Information with a dismiss answer: "restart to apply" (T-10 §4).
    Notice,
}

/// An explicit answer key shown on its button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Escape,
}

/// What the keyboard handed us. Return and Space exist only to be refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyInput {
    Char(char),
    Escape,
    Return,
    Space,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ButtonSpec {
    pub label: String,
    pub key: Key,
    /// Brighter fill: the suggested answer. Never answered by Return regardless.
    pub primary: bool,
}

#[derive(Clone, Debug)]
pub struct BarSpec {
    pub id: WidgetId,
    pub kind: BarKind,
    /// Interface-owned headline.
    pub headline: TextRun,
    /// Detail line: file:line message, or the pane title (program text, sanitised).
    pub detail: Option<TextRun>,
    pub buttons: Vec<ButtonSpec>,
    /// Button answered by Escape (deny / dismiss / cancel).
    pub escape: Option<usize>,
}

fn button(label: &str, key: Key, primary: bool) -> ButtonSpec {
    ButtonSpec {
        label: label.to_string(),
        key,
        primary,
    }
}

impl BarSpec {
    /// Consent bar naming the pane and the requesting class (T-18 §6).
    pub fn consent(id: WidgetId, class_label: &str, pane_title: &str) -> BarSpec {
        BarSpec {
            id,
            kind: BarKind::Consent,
            headline: TextRun::interface(format!("Allow {class_label}?"), size::BODY).strong(),
            detail: Some(TextRun::program(pane_title, size::BODY_SMALL)),
            buttons: vec![
                button("Allow once", Key::Char('o'), false),
                button("Always for this pane", Key::Char('a'), false),
                button("Deny", Key::Char('d'), false),
            ],
            escape: Some(2),
        }
    }

    /// T-05 §6 paste confirmation: a short bar form of the T-18 §4 guard.
    pub fn paste_confirm(id: WidgetId, bytes: usize, lines: usize, pane_title: &str) -> BarSpec {
        BarSpec {
            id,
            kind: BarKind::Consent,
            headline: TextRun::interface(
                format!("Paste {bytes} bytes, {lines} lines?"),
                size::BODY,
            )
            .strong(),
            detail: Some(TextRun::program(pane_title, size::BODY_SMALL)),
            buttons: vec![
                button("Paste", Key::Char('p'), false),
                button("Cancel", Key::Char('c'), false),
            ],
            escape: Some(1),
        }
    }

    /// T-10 config-error overlay.
    pub fn config_error(id: WidgetId, file: &str, line: u32, message: &str) -> BarSpec {
        BarSpec {
            id,
            kind: BarKind::Error,
            headline: TextRun::interface(format!("Config error at {file}:{line}"), size::BODY)
                .strong(),
            detail: Some(TextRun::program(message, size::BODY_SMALL)),
            buttons: vec![
                button("Open config", Key::Char('o'), true),
                button("Dismiss", Key::Escape, false),
            ],
            escape: Some(1),
        }
    }

    /// T-10 §4 restart toast: Restart-class keys changed and apply on the next start. The key
    /// paths are interface-known schema names, but are drawn as program-class detail text so
    /// they are sanitised and capped like any other variable string.
    pub fn restart_required(id: WidgetId, keys: &[&str]) -> Option<BarSpec> {
        if keys.is_empty() {
            return None;
        }
        Some(BarSpec {
            id,
            kind: BarKind::Notice,
            headline: TextRun::interface("Restart to apply some changes", size::BODY).strong(),
            detail: Some(TextRun::program(&keys.join(", "), size::BODY_SMALL)),
            buttons: vec![button("Dismiss", Key::Escape, false)],
            escape: Some(0),
        })
    }

    /// The button an input answers, if any, before the guard is considered.
    fn button_for(&self, k: KeyInput) -> Option<usize> {
        match k {
            KeyInput::Return | KeyInput::Space | KeyInput::Other => None,
            KeyInput::Escape => self
                .escape
                .or_else(|| self.buttons.iter().position(|b| b.key == Key::Escape)),
            KeyInput::Char(c) => {
                let c = c.to_ascii_lowercase();
                self.buttons.iter().position(|b| b.key == Key::Char(c))
            }
        }
    }
}

/// Runtime state: when the bar appeared (ms, monotonic).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarState {
    pub shown_at_ms: u64,
}

impl BarState {
    /// Input is ignored for [`motion::CONSENT_GUARD_MS`] after appearing.
    pub fn accepts_input(&self, now_ms: u64) -> bool {
        now_ms >= self.shown_at_ms.saturating_add(motion::CONSENT_GUARD_MS)
    }

    /// When the guard ends, for arming a repaint timer; `None` once it has.
    pub fn guard_ends_at(&self, now_ms: u64) -> Option<u64> {
        let end = self.shown_at_ms.saturating_add(motion::CONSENT_GUARD_MS);
        (now_ms < end).then_some(end)
    }

    /// Answer a key: the button index, or `None` (guard, Return, Space, unknown key).
    pub fn answer_key(&self, spec: &BarSpec, now_ms: u64, k: KeyInput) -> Option<usize> {
        if !self.accepts_input(now_ms) {
            return None;
        }
        spec.button_for(k)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonCell {
    pub id: WidgetId,
    pub rect: Rect,
    pub keycap: Rect,
    pub label_pos: Point,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    pub id: WidgetId,
    pub bar: Rect,
    /// What remains of the pane for the grid.
    pub rest: Rect,
    pub icon: Rect,
    pub text_x: f32,
    pub text_max: f32,
    pub buttons: Vec<ButtonCell>,
}

fn key_label(k: Key) -> String {
    match k {
        Key::Char(c) => c.to_ascii_uppercase().to_string(),
        Key::Escape => "Esc".to_string(),
    }
}

pub fn layout(area: Rect, spec: &BarSpec, measure: &dyn Measure) -> BarLayout {
    let (outer, rest) = area.split_top(metrics::BAR_H + space::SM);
    let bar = Rect::new(
        outer.x + space::SM,
        outer.y + space::XS,
        (outer.w - 2.0 * space::SM).max(0.0),
        metrics::BAR_H,
    );
    let mut x = bar.right() - space::MD;
    let mut cells = Vec::with_capacity(spec.buttons.len());
    for (i, b) in spec.buttons.iter().enumerate().rev() {
        let lw = measure.width(&TextRun::interface(b.label.clone(), size::BODY_SMALL));
        // The cap holds its label with `XS` either side.
        let kw = (measure.width(&TextRun::interface(key_label(b.key), size::MICRO))
            + 2.0 * space::XS)
            .max(metrics::KEYCAP_W);
        let w = metrics::BAR_BUTTON_PAD / 2.0 + kw + space::XS + lw + metrics::BAR_BUTTON_PAD;
        let rect = bar.centred_y(x - w, w, metrics::BAR_BUTTON_H);
        let keycap = Rect::new(
            rect.x + metrics::BAR_BUTTON_PAD / 2.0,
            rect.y + (rect.h - metrics::KEYCAP_W) / 2.0,
            kw,
            metrics::KEYCAP_W,
        );
        let label_pos = Point::new(
            keycap.right() + space::XS,
            rect.y + (rect.h - size::BODY_SMALL) / 2.0,
        );
        cells.push(ButtonCell {
            id: spec.id.child(i as u64 + 1),
            rect,
            keycap,
            label_pos,
        });
        x = rect.x - space::SM;
    }
    cells.reverse();
    let icon = bar.centred_y(bar.x + space::MD, metrics::TAB_CLOSE, metrics::TAB_CLOSE);
    let text_x = icon.right() + space::SM;
    let text_max = (cells.first().map_or(bar.right(), |c| c.rect.x) - space::MD - text_x).max(0.0);
    BarLayout {
        id: spec.id,
        bar,
        rest,
        icon,
        text_x,
        text_max,
        buttons: cells,
    }
}

impl BarLayout {
    pub fn register(&self, tree: &mut HitTree) {
        tree.add(self.id, Layer::Bar, self.bar);
        for b in &self.buttons {
            tree.add(b.id, Layer::Bar, b.rect);
        }
    }

    /// Pointer press: the button index under `p`, `None` inside the guard window.
    pub fn click(&self, state: &BarState, now_ms: u64, p: Point) -> Option<usize> {
        if !state.accepts_input(now_ms) {
            return None;
        }
        self.buttons.iter().position(|b| b.rect.contains(p))
    }
}

pub fn draw(
    l: &BarLayout,
    spec: &BarSpec,
    state: &BarState,
    now_ms: u64,
    ui: &UiState,
    pal: &Palette,
    out: &mut DrawList,
) {
    let live = state.accepts_input(now_ms);
    let (fill, rim_c, icon, icon_c) = match spec.kind {
        BarKind::Consent => (
            pal.sheet_tint,
            pal.accent_border,
            IconId::Shield,
            pal.accent_text,
        ),
        BarKind::Error => (pal.danger_fill, pal.danger, IconId::Warning, pal.danger),
        BarKind::Notice => (
            pal.sheet_tint,
            pal.accent_border,
            IconId::Warning,
            pal.accent_text,
        ),
    };
    out.rect(
        l.bar,
        radius::INSET,
        fill,
        Some(Rim {
            width: metrics::RIM_W,
            colour: rim_c,
        }),
    );
    out.icon(icon, l.icon, icon_c);

    let two = spec.detail.is_some();
    let total = if two {
        size::BODY + size::BODY_SMALL
    } else {
        size::BODY
    };
    let y1 = l.bar.y + (l.bar.h - total) / 2.0;
    out.text(
        spec.headline.clone().clipped(l.text_max),
        Point::new(l.text_x, y1),
        pal.text,
    );
    if let Some(d) = &spec.detail {
        out.text(
            d.clone().clipped(l.text_max),
            Point::new(l.text_x, y1 + size::BODY),
            pal.text_secondary,
        );
    }

    for (cell, b) in l.buttons.iter().zip(&spec.buttons) {
        let mut st: ControlState = ui.control(cell.id, !live, b.primary);
        // The bar never takes focus by itself: keyboard rim only if focus was moved here.
        st.focus_kb &= live;
        let (bf, br) = surface(pal, st);
        out.rect(cell.rect, radius::PILL, bf, Some(br));
        out.rect(
            cell.keycap,
            radius::KEYCAP,
            pal.cell_hover,
            Some(Rim {
                width: metrics::RIM_W,
                colour: pal.cell_rim,
            }),
        );
        out.text(
            TextRun::interface(key_label(b.key), size::MICRO),
            Point::new(
                cell.keycap.x + space::XS,
                cell.keycap.y + (cell.keycap.h - size::MICRO) / 2.0,
            ),
            label_colour(pal, st),
        );
        out.text(
            TextRun::interface(b.label.clone(), size::BODY_SMALL),
            cell.label_pos,
            label_colour(pal, st),
        );
    }
}
