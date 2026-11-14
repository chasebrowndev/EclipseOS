// SPDX-License-Identifier: AGPL-3.0-only
//! Consent / error bar (T-08 §4-§5, T-18 §3, T-05 §6, T-10 config-error overlay).
//!
//! Drawn outside the pane's grid area (it takes space from the top of the pane, so cell
//! content cannot imitate it), never steals focus, ignores all input for 500 ms after it
//! appears, and is never answered by Return or Space.

use crate::draw::{DrawList, IconId, Rim};
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{ControlState, UiState, WidgetId, glass, label_colour, surface};
use crate::text::{Measure, TextOrigin, TextRun};
use crate::tokens::{Palette, alpha, metrics, motion, radius, size, space};

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
    /// Program text shown in a bounded monospace box. A spec with a body is drawn as a
    /// centred modal sheet instead of the bar at the top of the pane (T-11 §5 confirm-once).
    pub body: Option<TextRun>,
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
            body: None,
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
            body: None,
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
            body: None,
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
            body: None,
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
    /// Width the label may use before it is clipped.
    pub label_w: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BarLayout {
    pub id: WidgetId,
    /// The bar, or the sheet when the spec has a body.
    pub bar: Rect,
    /// What remains of the pane for the grid (the whole area for a sheet).
    pub rest: Rect,
    /// A sheet dims the window behind it: the area it covers.
    pub scrim: Option<Rect>,
    pub icon: Rect,
    pub head_pos: Point,
    pub text_x: f32,
    pub text_max: f32,
    /// The detail line's position, and its well when it is program text.
    pub detail_pos: Option<Point>,
    pub detail_well: Option<Rect>,
    /// The monospace box of a sheet, and its wrapped lines.
    pub body: Option<Rect>,
    pub body_lines: Vec<String>,
    pub buttons: Vec<ButtonCell>,
}

fn key_label(k: Key) -> String {
    match k {
        Key::Char(c) => c.to_ascii_uppercase().to_string(),
        Key::Escape => "Esc".to_string(),
    }
}

/// The buttons of `spec`, right-aligned so the last ends at `right`, centred on `row`.
fn button_cells(spec: &BarSpec, measure: &dyn Measure, right: f32, row: Rect) -> Vec<ButtonCell> {
    let mut x = right;
    let mut cells = Vec::with_capacity(spec.buttons.len());
    for (i, b) in spec.buttons.iter().enumerate().rev() {
        let lw = measure.width(&TextRun::interface(b.label.clone(), size::BODY_SMALL));
        // The cap holds its label with `XS` either side.
        let kw = (measure.width(&TextRun::interface(key_label(b.key), size::MICRO))
            + 2.0 * space::XS)
            .max(metrics::KEYCAP_W);
        let w = metrics::BAR_BUTTON_PAD / 2.0 + kw + space::XS + lw + metrics::BAR_BUTTON_PAD;
        let rect = row.centred_y(x - w, w, metrics::BAR_BUTTON_H);
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
            label_w: lw,
        });
        x = rect.x - space::SM;
    }
    cells.reverse();
    cells
}

/// Hard-wrap `run` into at most `max_lines` lines of `max_w`, breaking at a space when one
/// is in the second half of the line; the last line ends with an ellipsis if text is left.
pub fn wrap_body(
    measure: &dyn Measure,
    run: &TextRun,
    max_w: f32,
    max_lines: usize,
) -> Vec<String> {
    let chars: Vec<char> = run.text.chars().collect();
    let width = |c: &[char]| {
        let mut r = TextRun::interface(c.iter().collect::<String>(), run.size);
        r.weight = run.weight;
        measure.width(&r)
    };
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() && lines.len() < max_lines {
        let rest = &chars[i..];
        let all = width(rest);
        let mut n = rest.len();
        if all > max_w && all > 0.0 {
            n = ((n as f32 * max_w / all) as usize).clamp(1, n);
            while n > 1 && width(&rest[..n]) > max_w {
                n -= 1;
            }
            if let Some(sp) = rest[..n].iter().rposition(|c| *c == ' ')
                && sp > n / 2
                && i + n < chars.len()
            {
                n = sp + 1;
            }
        }
        lines.push(rest[..n].iter().collect::<String>().trim_end().to_string());
        i += n;
    }
    if i < chars.len()
        && let Some(last) = lines.last_mut()
    {
        while width(&last.chars().collect::<Vec<_>>()) + width(&['\u{2026}']) > max_w
            && last.pop().is_some()
        {}
        last.push('\u{2026}');
    }
    lines
}

pub fn layout(area: Rect, spec: &BarSpec, measure: &dyn Measure) -> BarLayout {
    match &spec.body {
        Some(body) => sheet_layout(area, spec, body, measure),
        None => bar_layout(area, spec, measure),
    }
}

fn bar_layout(area: Rect, spec: &BarSpec, measure: &dyn Measure) -> BarLayout {
    let (outer, rest) = area.split_top(metrics::BAR_H + space::SM);
    let bar = Rect::new(
        outer.x + space::SM,
        outer.y + space::XS,
        (outer.w - 2.0 * space::SM).max(0.0),
        metrics::BAR_H,
    );
    let cells = button_cells(spec, measure, bar.right() - space::MD, bar);
    let icon = bar.centred_y(bar.x + space::MD, metrics::TAB_CLOSE, metrics::TAB_CLOSE);
    let text_x = icon.right() + space::SM;
    let text_max = (cells.first().map_or(bar.right(), |c| c.rect.x) - space::MD - text_x).max(0.0);
    let detail = spec.detail.as_ref().filter(|d| !d.text.is_empty());
    let total = match detail {
        Some(_) => size::BODY + metrics::DETAIL_GAP + metrics::DETAIL_H,
        None => size::BODY,
    };
    let y1 = bar.y + (bar.h - total) / 2.0;
    let (detail_pos, detail_well) = match detail {
        Some(d) => {
            let wy = y1 + size::BODY + metrics::DETAIL_GAP;
            let program = d.origin == TextOrigin::Program;
            let pad = if program { space::XS } else { 0.0 };
            let w = (measure.width(d) + 2.0 * pad).min(text_max);
            let well = Rect::new(text_x, wy, w, metrics::DETAIL_H);
            (
                Some(Point::new(
                    text_x + pad,
                    wy + (metrics::DETAIL_H - size::BODY_SMALL) / 2.0,
                )),
                program.then_some(well),
            )
        }
        None => (None, None),
    };
    BarLayout {
        id: spec.id,
        bar,
        rest,
        scrim: None,
        icon,
        head_pos: Point::new(text_x, y1),
        text_x,
        text_max,
        detail_pos,
        detail_well,
        body: None,
        body_lines: Vec::new(),
        buttons: cells,
    }
}

fn sheet_layout(area: Rect, spec: &BarSpec, body: &TextRun, measure: &dyn Measure) -> BarLayout {
    let w = (area.w - 2.0 * space::LG).clamp(0.0, metrics::SHEET_W);
    let inner_w = (w - 2.0 * space::LG).max(0.0);
    let box_w = (inner_w - 2.0 * space::SM).max(0.0);
    let lines = wrap_body(measure, body, box_w, metrics::SHEET_BODY_LINES);
    let box_h = lines.len().max(1) as f32 * metrics::SHEET_LINE_H + 2.0 * space::SM;
    let sub_h = spec
        .detail
        .as_ref()
        .map_or(0.0, |_| size::BODY_SMALL + space::XS);
    let h = space::LG
        + metrics::TAB_CLOSE
        + space::XS
        + sub_h
        + space::SM
        + box_h
        + space::LG
        + metrics::BAR_BUTTON_H
        + space::LG;
    let x = area.x + (area.w - w) / 2.0;
    let y = area.y + (area.h * 0.12).max(space::LG);
    let sheet = Rect::new(x, y, w, h.min((area.h - y).max(0.0)));
    let left = sheet.x + space::LG;
    let icon = Rect::new(
        left,
        sheet.y + space::LG,
        metrics::TAB_CLOSE,
        metrics::TAB_CLOSE,
    );
    let text_x = icon.right() + space::SM;
    let text_max = (sheet.right() - space::LG - text_x).max(0.0);
    let head_pos = Point::new(text_x, icon.y + (icon.h - size::BODY) / 2.0);
    let detail_pos = spec
        .detail
        .as_ref()
        .map(|_| Point::new(text_x, icon.bottom() + space::XS));
    let bx = Rect::new(
        left,
        icon.bottom() + space::XS + sub_h + space::SM,
        inner_w,
        box_h,
    );
    let row = Rect::new(
        left,
        bx.bottom() + space::LG,
        inner_w,
        metrics::BAR_BUTTON_H,
    );
    let cells = button_cells(spec, measure, row.right(), row);
    BarLayout {
        id: spec.id,
        bar: sheet,
        rest: area,
        scrim: Some(area),
        icon,
        head_pos,
        text_x,
        text_max,
        detail_pos,
        detail_well: None,
        body: Some(bx),
        body_lines: lines,
        buttons: cells,
    }
}

impl BarLayout {
    pub fn register(&self, tree: &mut HitTree) {
        let layer = if self.scrim.is_some() {
            Layer::Sheet
        } else {
            Layer::Bar
        };
        if let Some(s) = self.scrim {
            tree.add(self.id.child(0), layer, s);
        }
        tree.add(self.id, layer, self.bar);
        for b in &self.buttons {
            tree.add(b.id, layer, b.rect);
        }
    }

    /// The button under `p`, whatever the guard says (hover).
    pub fn button_at(&self, p: Point) -> Option<usize> {
        self.buttons.iter().position(|b| b.rect.contains(p))
    }

    /// Pointer press: the button index under `p`, `None` inside the guard window.
    pub fn click(&self, state: &BarState, now_ms: u64, p: Point) -> Option<usize> {
        if !state.accepts_input(now_ms) {
            return None;
        }
        self.button_at(p)
    }

    /// The pointer is over the bar or sheet (a modal sheet owns its whole area).
    pub fn covers(&self, p: Point) -> bool {
        self.scrim.unwrap_or(self.bar).contains(p)
    }
}

/// The program-text well: an inset the text sits in, so it never reads as interface copy.
fn well(out: &mut DrawList, r: Rect, pal: &Palette) {
    out.rect(
        r,
        radius::KEYCAP,
        pal.cell,
        Some(Rim {
            width: metrics::RIM_W,
            colour: pal.cell_rim,
        }),
    );
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
    let (rim_c, icon, icon_fill, icon_c) = match spec.kind {
        BarKind::Consent => (
            pal.cell_rim,
            IconId::Shield,
            pal.accent_fill,
            pal.accent_text,
        ),
        BarKind::Error => (pal.danger, IconId::Warning, pal.danger_fill, pal.danger),
        BarKind::Notice => (
            pal.cell_rim,
            IconId::Warning,
            pal.accent_fill,
            pal.accent_text,
        ),
    };
    if let Some(s) = l.scrim {
        out.rect(s, 0.0, pal.dim.alpha(alpha::SCRIM), None);
    }
    let radius = if l.scrim.is_some() {
        radius::CARD
    } else {
        radius::INSET
    };
    glass(out, l.bar, radius, rim_c, pal);
    if spec.kind == BarKind::Error {
        out.rect(
            l.bar,
            radius,
            pal.danger_fill,
            Some(Rim {
                width: metrics::RIM_W,
                colour: pal.danger,
            }),
        );
    }
    out.rect(l.icon, radius::PILL, icon_fill, None);
    out.icon(icon, l.icon, icon_c);

    out.text(
        spec.headline.clone().clipped(l.text_max),
        l.head_pos,
        pal.text,
    );
    if let (Some(d), Some(pos)) = (&spec.detail, l.detail_pos) {
        if let Some(w) = l.detail_well {
            well(out, w, pal);
        }
        let colour = if d.origin == TextOrigin::Program {
            pal.text
        } else {
            pal.text_secondary
        };
        let max = l.detail_well.map_or(l.text_max, |w| w.w - 2.0 * space::XS);
        out.text(d.clone().clipped(max.max(0.0)), pos, colour);
    }
    if let (Some(b), Some(run)) = (l.body, &spec.body) {
        well(out, b, pal);
        for (n, line) in l.body_lines.iter().enumerate() {
            let mut r = run.clone();
            r.text.clone_from(line);
            out.text(
                r.clipped(b.w - 2.0 * space::SM),
                Point::new(
                    b.x + space::SM,
                    b.y + space::SM
                        + n as f32 * metrics::SHEET_LINE_H
                        + (metrics::SHEET_LINE_H - run.size) / 2.0,
                ),
                pal.text,
            );
        }
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
            TextRun::interface(b.label.clone(), size::BODY_SMALL).clipped(cell.label_w),
            cell.label_pos,
            label_colour(pal, st),
        );
    }
}
