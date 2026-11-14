// SPDX-License-Identifier: AGPL-3.0-only
//! Command palette sheet (T-08 §4, T-14 §9): a query field over a list of entries, each with
//! its title and current binding. The palette owns the keyboard while open (T-08 §5 focus
//! stack); Escape always dismisses. Filtering and scoring are the caller's: this module lays
//! out and draws the window of entries it is given.
//!
//! Look: one glass sheet (sheet tint, hairline rim), an inset field, rows that are plain until
//! hovered; the selected row is a brighter fill with a crisp rim, exactly like a selected tab.

use crate::draw::{DrawList, Rim};
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{ControlState, UiState, WidgetId, label_colour, surface};
use crate::text::{Measure, TextRun};
use crate::tokens::{Palette, metrics, radius, size, space};

/// One entry. Titles are interface text (registry titles, tab titles are sanitised by the
/// caller through [`TextRun::program`]).
#[derive(Clone, Debug)]
pub struct Entry {
    pub title: TextRun,
    /// Secondary text on the right: the binding (`ctrl+shift+t`) or a mode hint.
    pub hint: Option<TextRun>,
}

#[derive(Clone, Debug)]
pub struct PaletteSpec {
    pub id: WidgetId,
    /// The query as typed (user text: drawn, never logged).
    pub query: String,
    /// Shown while the query is empty.
    pub placeholder: TextRun,
    /// The entries currently in view (already scrolled), at most [`max_rows`] of them.
    pub entries: Vec<Entry>,
    /// Index into `entries` of the selected row.
    pub selected: Option<usize>,
    /// Matches in total (the list may be longer than the window).
    pub total: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteLayout {
    pub sheet: Rect,
    pub field: Rect,
    /// `(index into entries, id, rect)`.
    pub rows: Vec<(usize, WidgetId, Rect)>,
    /// "No matches" line when the list is empty.
    pub empty: Option<Rect>,
}

/// Rows that fit in `area` (at least one, at most [`metrics::PALETTE_MAX_ROWS`]).
pub fn max_rows(area: Rect) -> usize {
    let room = area.h * 0.7 - metrics::PALETTE_FIELD_H - 3.0 * space::SM;
    ((room / metrics::PALETTE_ROW_H).floor().max(1.0) as usize).min(metrics::PALETTE_MAX_ROWS)
}

/// Id of row `i` of the palette `id`.
pub fn row_id(id: WidgetId, i: usize) -> WidgetId {
    id.child(1 + i as u64)
}

pub fn layout(area: Rect, spec: &PaletteSpec) -> PaletteLayout {
    let w = (area.w - 2.0 * space::LG).clamp(0.0, metrics::PALETTE_W);
    let n = spec.entries.len();
    let list_h = if n == 0 {
        metrics::PALETTE_ROW_H
    } else {
        n as f32 * metrics::PALETTE_ROW_H
    };
    let h = space::SM + metrics::PALETTE_FIELD_H + space::SM + list_h + space::SM;
    let x = area.x + (area.w - w) / 2.0;
    let y = area.y + (area.h * 0.12).max(space::LG);
    let sheet = Rect::new(x, y, w, h);
    let inner = sheet.inset(space::SM);
    let (field, rest) = inner.split_top(metrics::PALETTE_FIELD_H);
    let list = Rect::new(rest.x, rest.y + space::SM, rest.w, list_h);
    let rows = (0..n)
        .map(|i| {
            let r = Rect::new(
                list.x,
                list.y + i as f32 * metrics::PALETTE_ROW_H,
                list.w,
                metrics::PALETTE_ROW_H,
            );
            (i, row_id(spec.id, i), r)
        })
        .collect();
    PaletteLayout {
        sheet,
        field,
        rows,
        empty: (n == 0).then_some(list),
    }
}

impl PaletteLayout {
    /// The sheet is the front layer: nothing under it receives the pointer.
    pub fn register(&self, id: WidgetId, tree: &mut HitTree) {
        tree.add(id, Layer::Sheet, self.sheet);
        for (_, rid, r) in &self.rows {
            tree.add(*rid, Layer::Sheet, *r);
        }
    }

    /// The entry index under `p`, if any.
    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.rows
            .iter()
            .find(|(_, _, r)| r.contains(p))
            .map(|(i, _, _)| *i)
    }
}

pub fn draw(
    l: &PaletteLayout,
    spec: &PaletteSpec,
    ui: &UiState,
    pal: &Palette,
    measure: &dyn Measure,
    out: &mut DrawList,
) {
    out.rect(
        l.sheet,
        radius::CARD,
        pal.sheet_tint,
        Some(Rim {
            width: metrics::RIM_W,
            colour: pal.hairline,
        }),
    );
    // The field: an inset well that always has keyboard focus while the sheet is open.
    out.rect(
        l.field,
        radius::INSET,
        pal.cell,
        Some(Rim {
            width: metrics::RIM_W,
            colour: pal.cell_focus_rim,
        }),
    );
    let tx = l.field.x + space::MD;
    let ty = l.field.y + (l.field.h - size::BODY) / 2.0;
    let text_w = (l.field.w - 2.0 * space::MD).max(0.0);
    let caret_x = if spec.query.is_empty() {
        out.text(
            spec.placeholder.clone().clipped(text_w),
            Point::new(tx, ty),
            pal.text_tertiary,
        );
        tx
    } else {
        let run = TextRun::interface(spec.query.clone(), size::BODY).clipped(text_w);
        let w = measure.width(&run);
        out.text(run, Point::new(tx, ty), pal.text);
        tx + w
    };
    out.rect(
        Rect::new(
            (caret_x + 1.0).min(l.field.right() - space::MD),
            ty,
            metrics::CARET_W,
            size::BODY,
        ),
        0.0,
        pal.accent,
        None,
    );
    if let Some(e) = l.empty {
        out.text(
            TextRun::interface("No matches", size::BODY_SMALL),
            Point::new(e.x + space::MD, e.y + (e.h - size::BODY_SMALL) / 2.0),
            pal.text_tertiary,
        );
    }
    for (i, id, r) in &l.rows {
        let e = &spec.entries[*i];
        let selected = spec.selected == Some(*i);
        let st = ui.control(*id, false, selected);
        if selected || st.hover || st.pressed {
            let (fill, rim) = surface(pal, st);
            out.rect(*r, radius::INSET, fill, Some(rim));
        }
        let mut hint_w = 0.0;
        if let Some(h) = &e.hint {
            let run = h.clone().clipped(r.w * 0.4);
            hint_w = measure.width(&run);
            out.text(
                run,
                Point::new(
                    r.right() - space::MD - hint_w,
                    r.y + (r.h - size::BODY_SMALL) / 2.0,
                ),
                pal.text_secondary,
            );
        }
        let title_w = (r.w - 2.0 * space::MD - hint_w - space::SM).max(0.0);
        out.text(
            e.title.clone().clipped(title_w),
            Point::new(r.x + space::MD, r.y + (r.h - size::BODY) / 2.0),
            label_colour(pal, ControlState { selected, ..st }),
        );
    }
}
