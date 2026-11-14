// SPDX-License-Identifier: AGPL-3.0-only
//! Search sheet (T-08 §4, T-14 §6): a floating glass row holding the query field with the
//! match count, the mode chips (case, regex, whole word, scope), previous / next and close.
//!
//! The chips mirror `Alt+C`, `Alt+R`, `Alt+W` and `Alt+S`, which stay the keyboard path;
//! every control here is also clickable. Hit geometry uses the shared geometry measure so
//! the main thread and the renderer agree on every rectangle.
//!
//! Look: one glass layer, an inset field (brighter fill and a crisp rim while it has the
//! keyboard), chips that are plain until hovered or on (an on chip is the brighter fill,
//! gold on its label only), icon buttons that show a cell only under the pointer.

use crate::draw::{DrawList, IconId, Rim};
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{ControlState, UiState, WidgetId, glass, label_colour, surface};
use crate::text::{Measure, TextRun};
use crate::tokens::{Palette, metrics, radius, size, space};

/// The four query modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Case,
    Regex,
    Word,
    Scope,
}

/// What a click on the sheet lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchTarget {
    Field,
    Chip(Chip),
    Prev,
    Next,
    Close,
}

/// How the count reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountKind {
    /// Nothing typed yet, or still searching with nothing found.
    Idle,
    Found,
    Missing,
    /// The pattern is refused (over budget, invalid).
    Error,
}

#[derive(Clone, Debug)]
pub struct ChipSpec {
    pub chip: Chip,
    pub label: String,
    pub on: bool,
}

#[derive(Clone, Debug)]
pub struct SearchSpec {
    pub id: WidgetId,
    /// The query as typed (user text: drawn, never logged).
    pub query: String,
    pub placeholder: TextRun,
    /// "3/12", "No matches", or the refusal message.
    pub count: String,
    pub count_kind: CountKind,
    pub chips: Vec<ChipSpec>,
    /// The field has the keyboard.
    pub focused: bool,
    /// Previous and next have somewhere to go.
    pub can_step: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchLayout {
    pub sheet: Rect,
    pub field: Rect,
    pub chips: Vec<(Chip, WidgetId, Rect)>,
    pub prev: Rect,
    pub next: Rect,
    pub close: Rect,
    id: WidgetId,
}

/// Id of control `t` of the search sheet `id`.
pub fn target_id(id: WidgetId, t: SearchTarget) -> WidgetId {
    match t {
        SearchTarget::Field => id.child(1),
        SearchTarget::Chip(c) => chip_id(id, c),
        SearchTarget::Prev => id.child(10),
        SearchTarget::Next => id.child(11),
        SearchTarget::Close => id.child(12),
    }
}

impl SearchLayout {
    pub fn id_of(&self, t: SearchTarget) -> WidgetId {
        target_id(self.id, t)
    }

    /// The control under `p`.
    pub fn target_at(&self, p: Point) -> Option<SearchTarget> {
        if self.close.contains(p) {
            return Some(SearchTarget::Close);
        }
        if self.next.contains(p) {
            return Some(SearchTarget::Next);
        }
        if self.prev.contains(p) {
            return Some(SearchTarget::Prev);
        }
        if let Some((c, _, _)) = self.chips.iter().find(|(_, _, r)| r.contains(p)) {
            return Some(SearchTarget::Chip(*c));
        }
        self.field.contains(p).then_some(SearchTarget::Field)
    }

    pub fn register(&self, tree: &mut HitTree) {
        tree.add(self.id, Layer::Bar, self.sheet);
        tree.add(self.id_of(SearchTarget::Field), Layer::Bar, self.field);
        for (_, id, r) in &self.chips {
            tree.add(*id, Layer::Bar, *r);
        }
        tree.add(self.id_of(SearchTarget::Prev), Layer::Bar, self.prev);
        tree.add(self.id_of(SearchTarget::Next), Layer::Bar, self.next);
        tree.add(self.id_of(SearchTarget::Close), Layer::Bar, self.close);
    }
}

fn chip_id(id: WidgetId, c: Chip) -> WidgetId {
    id.child(2 + c as u64)
}

/// Where the sheet may sit: below the tab strip (or the overlay bar), the whole width.
pub fn free_area(window: Rect, strip: Option<Rect>, bar_inset: f32) -> Rect {
    let mut top = window.y + bar_inset;
    if let Some(s) = strip
        && s.y < window.y + window.h / 2.0
    {
        top = top.max(s.bottom());
    }
    let (_, rest) = window.split_top((top - window.y).max(0.0));
    rest
}

pub fn layout(area: Rect, spec: &SearchSpec, measure: &dyn Measure) -> SearchLayout {
    let w = (area.w - 2.0 * space::SM).clamp(0.0, metrics::SEARCH_W);
    let sheet = Rect::new(
        area.right() - space::SM - w,
        area.y + space::SM,
        w,
        metrics::SEARCH_H,
    );
    let row = sheet.inset(space::SM);
    let square = |x: f32| row.centred_y(x, metrics::ICON_BUTTON, metrics::ICON_BUTTON);
    let close = square(row.right() - metrics::ICON_BUTTON);
    let next = square(close.x - space::XS - metrics::ICON_BUTTON);
    let prev = square(next.x - space::XS - metrics::ICON_BUTTON);
    let mut x = prev.x - space::SM;
    let mut chips = Vec::with_capacity(spec.chips.len());
    for c in spec.chips.iter().rev() {
        let lw = measure.width(&TextRun::interface(c.label.clone(), size::BODY_SMALL));
        let cw = (lw + 2.0 * space::SM).max(metrics::CHIP_MIN_W);
        x -= cw;
        chips.push((
            c.chip,
            chip_id(spec.id, c.chip),
            row.centred_y(x, cw, metrics::CHIP_H),
        ));
        x -= space::XS;
    }
    chips.reverse();
    let right = x + space::XS - space::SM;
    let field = row.centred_y(
        row.x,
        (right - row.x).max(metrics::FIELD_MIN_W),
        metrics::SEARCH_FIELD_H,
    );
    SearchLayout {
        sheet,
        field,
        chips,
        prev,
        next,
        close,
        id: spec.id,
    }
}

pub fn draw(
    l: &SearchLayout,
    spec: &SearchSpec,
    ui: &UiState,
    pal: &Palette,
    measure: &dyn Measure,
    out: &mut DrawList,
) {
    glass(out, l.sheet, radius::CARD, pal.cell_rim, pal);

    // The field: an inset well; keyboard focus is the brighter fill and the crisp rim.
    let fst = ui.control(l.id_of(SearchTarget::Field), false, spec.focused);
    let (ff, fr) = surface(
        pal,
        ControlState {
            focus_kb: false,
            ..fst
        },
    );
    out.rect(l.field, radius::INSET, ff, Some(fr));
    let tx = l.field.x + space::MD;
    let ty = l.field.y + (l.field.h - size::BODY) / 2.0;
    let count_w = if spec.count.is_empty() {
        0.0
    } else {
        measure.width(
            &TextRun::interface(spec.count.clone(), size::BODY_SMALL).clipped(l.field.w * 0.5),
        )
    };
    if !spec.count.is_empty() {
        let colour = match spec.count_kind {
            CountKind::Error => pal.danger,
            CountKind::Found => pal.text_secondary,
            CountKind::Missing | CountKind::Idle => pal.text_tertiary,
        };
        out.text(
            TextRun::interface(spec.count.clone(), size::BODY_SMALL).clipped(count_w),
            Point::new(
                l.field.right() - space::MD - count_w,
                l.field.y + (l.field.h - size::BODY_SMALL) / 2.0,
            ),
            colour,
        );
    }
    let text_w = (l.field.w - 2.0 * space::MD - count_w - space::SM).max(0.0);
    let caret_x = if spec.query.is_empty() {
        out.text(
            spec.placeholder.clone().clipped(text_w),
            Point::new(tx, ty),
            pal.text_tertiary,
        );
        // As native search fields: the caret stands just before the placeholder, in the
        // field's leading pad, so it never strikes through the first letter.
        tx - metrics::CARET_GAP - metrics::CARET_W
    } else {
        let run = TextRun::interface(spec.query.clone(), size::BODY).clipped(text_w);
        let w = measure.width(&run);
        out.text(run, Point::new(tx, ty), pal.text);
        tx + w + metrics::CARET_GAP
    };
    if spec.focused {
        out.rect(
            Rect::new(caret_x.min(tx + text_w), ty, metrics::CARET_W, size::BODY),
            0.0,
            pal.accent,
            None,
        );
    }

    for (c, id, r) in &l.chips {
        let Some(cs) = spec.chips.iter().find(|s| s.chip == *c) else {
            continue;
        };
        let st = ui.control(*id, false, cs.on);
        let (fill, rim) = surface(pal, st);
        if cs.on || st.hover || st.pressed || st.focus_kb {
            out.rect(*r, radius::INSET, fill, Some(rim));
        } else {
            out.rect(
                *r,
                radius::INSET,
                pal.cell,
                Some(Rim {
                    width: metrics::RIM_W,
                    colour: pal.cell_rim,
                }),
            );
        }
        let colour = if cs.on {
            pal.accent_text
        } else {
            label_colour(pal, st)
        };
        let lw = (r.w - 2.0 * space::SM).max(0.0);
        let run = TextRun::interface(cs.label.clone(), size::BODY_SMALL).clipped(lw);
        let w = measure.width(&run);
        out.text(
            run,
            Point::new(r.x + (r.w - w) / 2.0, r.y + (r.h - size::BODY_SMALL) / 2.0),
            colour,
        );
    }

    for (t, r, icon, disabled) in [
        (SearchTarget::Prev, l.prev, IconId::ArrowUp, !spec.can_step),
        (
            SearchTarget::Next,
            l.next,
            IconId::ArrowDown,
            !spec.can_step,
        ),
        (SearchTarget::Close, l.close, IconId::Close, false),
    ] {
        let st = ui.control(l.id_of(t), disabled, false);
        if !disabled && (st.hover || st.pressed || st.focus_kb) {
            let (f, rm) = surface(pal, st);
            out.rect(r, radius::INSET, f, Some(rm));
        }
        out.icon(icon, r.inset(space::XS), label_colour(pal, st));
    }
}
