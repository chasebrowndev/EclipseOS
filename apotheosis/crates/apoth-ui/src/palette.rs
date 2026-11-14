// SPDX-License-Identifier: AGPL-3.0-only
//! Command palette sheet (T-08 §4, T-14 §9) and the Open menu and picker that ride on it
//! (T-14 §11): an optional header (the target as program text, its type), a query field over
//! a list of entries, each with its title and current binding. The palette owns the keyboard
//! while open (T-08 §5 focus stack); Escape always dismisses. Filtering and scoring are the
//! caller's: this module lays out and draws the window of entries it is given.
//!
//! Look: one glass sheet (sheet tint, hairline rim), an inset field, rows that are plain until
//! hovered; the selected row is a brighter fill with a crisp rim, exactly like a selected tab.
//! A disabled row is dimmed and takes no hover; a pinned row is parted from the list by a
//! hairline; a switch row carries a real toggle; the default handler carries a quiet tag. An
//! anchored sheet (a right click) opens at the pointer, clamped into the window.

use crate::draw::{DrawList, Rim};
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{ControlState, UiState, WidgetId, glass, label_colour, surface};
use crate::text::{Measure, TextRun, truncate_middle};
use crate::tokens::{Palette, metrics, radius, size, space};

/// What sits at a row's trailing edge besides its hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Marker {
    #[default]
    None,
    /// The default handler for the type: a quiet tag.
    Default,
    /// A switch, on or off.
    Switch(bool),
}

/// One entry. Titles are interface text (registry titles, tab titles are sanitised by the
/// caller through [`TextRun::program`]).
#[derive(Clone, Debug)]
pub struct Entry {
    pub title: TextRun,
    /// Secondary text on the right: the binding (`ctrl+shift+t`) or a mode hint.
    pub hint: Option<TextRun>,
    /// Dimmed, no hover; the hint says why.
    pub disabled: bool,
    pub marker: Marker,
}

impl Entry {
    pub fn new(title: TextRun, hint: Option<TextRun>) -> Entry {
        Entry {
            title,
            hint,
            disabled: false,
            marker: Marker::None,
        }
    }
}

/// The Open menu's header: what is being opened and what it is.
#[derive(Clone, Debug)]
pub struct Header {
    /// The target, program text (middle-truncated to fit).
    pub target: TextRun,
    /// The detected type (`image/png`).
    pub kind: TextRun,
    /// Short type label on the tile (`PNG`).
    pub tile: TextRun,
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
    pub header: Option<Header>,
    /// Open at this point (interface units), clamped into the area, instead of centred.
    pub anchor: Option<Point>,
    /// Entries in view that are pinned: a hairline follows the last of them.
    pub pinned: usize,
    /// Draw the query field.
    pub field: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteLayout {
    pub sheet: Rect,
    pub header: Option<Rect>,
    /// Zero-height when the spec hides the field.
    pub field: Rect,
    /// `(index into entries, id, rect)`.
    pub rows: Vec<(usize, WidgetId, Rect)>,
    /// "No matches" line when the list is empty.
    pub empty: Option<Rect>,
    /// The hairline between pinned rows and the list.
    pub divider: Option<Rect>,
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
    let full = if spec.anchor.is_some() {
        metrics::MENU_W
    } else {
        metrics::PALETTE_W
    };
    let w = (area.w - 2.0 * space::LG).clamp(0.0, full);
    let n = spec.entries.len();
    let pinned = spec.pinned.min(n);
    let gap = if pinned > 0 && pinned < n {
        space::SM
    } else {
        0.0
    };
    let list_h = if n == 0 {
        metrics::PALETTE_ROW_H
    } else {
        n as f32 * metrics::PALETTE_ROW_H + gap
    };
    let header_h = if spec.header.is_some() {
        metrics::MENU_HEADER_H + space::SM
    } else {
        0.0
    };
    let field_h = if spec.field {
        metrics::PALETTE_FIELD_H + space::SM
    } else {
        0.0
    };
    let h = space::SM + header_h + field_h + list_h + space::SM;
    let (x, y) = match spec.anchor {
        Some(a) => {
            let hi_x = (area.right() - w - space::LG).max(area.x + space::LG);
            let x = a.x.clamp(area.x + space::LG, hi_x);
            // Below the pointer when it fits, else above it, else pinned to the window.
            let y = if a.y + h <= area.bottom() - space::LG {
                a.y
            } else if a.y - h >= area.y + space::SM {
                a.y - h
            } else {
                (area.bottom() - space::LG - h).max(area.y + space::SM)
            };
            (x, y)
        }
        None => (
            area.x + (area.w - w) / 2.0,
            area.y + (area.h * 0.12).max(space::LG),
        ),
    };
    let sheet = Rect::new(x, y, w, h);
    let inner = sheet.inset(space::SM);
    let mut top = inner.y;
    let header = spec.header.as_ref().map(|_| {
        let r = Rect::new(inner.x, top, inner.w, metrics::MENU_HEADER_H);
        top += header_h;
        r
    });
    let field = Rect::new(
        inner.x,
        top,
        inner.w,
        if spec.field {
            metrics::PALETTE_FIELD_H
        } else {
            0.0
        },
    );
    top += field_h;
    let list = Rect::new(inner.x, top, inner.w, list_h);
    let rows: Vec<_> = (0..n)
        .map(|i| {
            let lift = if pinned > 0 && i >= pinned { gap } else { 0.0 };
            let r = Rect::new(
                list.x,
                list.y + i as f32 * metrics::PALETTE_ROW_H + lift,
                list.w,
                metrics::PALETTE_ROW_H,
            );
            (i, row_id(spec.id, i), r)
        })
        .collect();
    let divider = (gap > 0.0).then(|| {
        let above = rows[pinned - 1].2.bottom();
        Rect::new(
            list.x + space::SM,
            above + (gap - metrics::RIM_W) / 2.0,
            (list.w - 2.0 * space::SM).max(0.0),
            metrics::RIM_W,
        )
    });
    PaletteLayout {
        sheet,
        header,
        field,
        rows,
        empty: (n == 0).then_some(list),
        divider,
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

fn draw_header(r: Rect, h: &Header, pal: &Palette, measure: &dyn Measure, out: &mut DrawList) {
    let tile = Rect::new(
        r.x,
        r.y + (r.h - metrics::TILE_H) / 2.0,
        metrics::TILE_W,
        metrics::TILE_H,
    );
    out.rect(
        tile,
        radius::INSET,
        pal.cell,
        Some(Rim {
            width: metrics::RIM_W,
            colour: pal.cell_rim,
        }),
    );
    let label = h.tile.clone().strong().clipped(tile.w - space::SM);
    let lw = measure.width(&label);
    out.text(
        label,
        Point::new(
            tile.x + (tile.w - lw) / 2.0,
            tile.y + (tile.h - size::MICRO) / 2.0,
        ),
        pal.text_secondary,
    );
    let tx = tile.right() + space::MD;
    let text_w = (r.right() - tx).max(0.0);
    let block = size::BODY_SMALL + space::XS + size::MICRO;
    let top = r.y + (r.h - block) / 2.0;
    out.text(
        truncate_middle(&h.target, text_w, measure),
        Point::new(tx, top),
        pal.text,
    );
    out.text(
        h.kind.clone().clipped(text_w),
        Point::new(tx, top + size::BODY_SMALL + space::XS),
        pal.text_secondary,
    );
}

pub fn draw(
    l: &PaletteLayout,
    spec: &PaletteSpec,
    ui: &UiState,
    pal: &Palette,
    measure: &dyn Measure,
    out: &mut DrawList,
) {
    glass(out, l.sheet, radius::CARD, pal.cell_focus_rim, pal);
    if let (Some(r), Some(h)) = (l.header, &spec.header) {
        draw_header(r, h, pal, measure, out);
        out.rect(
            Rect::new(
                r.x,
                r.bottom() + (space::SM - metrics::RIM_W) / 2.0,
                r.w,
                metrics::RIM_W,
            ),
            0.0,
            pal.hairline,
            None,
        );
    }
    if spec.field {
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
            // As native search fields: the caret stands just before the placeholder, in the
            // field's leading pad, so it never strikes through the first letter.
            tx - metrics::CARET_GAP - metrics::CARET_W
        } else {
            let run = TextRun::interface(spec.query.clone(), size::BODY).clipped(text_w);
            let w = measure.width(&run);
            out.text(run, Point::new(tx, ty), pal.text);
            tx + w + metrics::CARET_GAP
        };
        out.rect(
            Rect::new(
                caret_x.min(l.field.right() - space::MD),
                ty,
                metrics::CARET_W,
                size::BODY,
            ),
            0.0,
            pal.accent,
            None,
        );
    }
    if let Some(e) = l.empty {
        out.text(
            TextRun::interface("No matches", size::BODY_SMALL),
            Point::new(e.x + space::MD, e.y + (e.h - size::BODY_SMALL) / 2.0),
            pal.text_tertiary,
        );
    }
    if let Some(d) = l.divider {
        out.rect(d, 0.0, pal.hairline, None);
    }
    for (i, id, r) in &l.rows {
        let e = &spec.entries[*i];
        let selected = spec.selected == Some(*i);
        let st = ui.control(*id, e.disabled, selected);
        // A disabled row lights only when the keyboard is on it.
        if selected || (!e.disabled && (st.hover || st.pressed)) {
            let (fill, rim) = surface(pal, st);
            out.rect(*r, radius::INSET, fill, Some(rim));
        }
        let mut right = r.right() - space::MD;
        match e.marker {
            Marker::Switch(on) => {
                let cell = Rect::new(right - metrics::TOGGLE_W, r.y, metrics::TOGGLE_W, r.h);
                crate::toggle::draw(cell, on, st, pal, out);
                right = cell.x - space::SM;
            }
            Marker::Default => {
                let run = TextRun::interface("default", size::MICRO);
                let tw = measure.width(&run);
                let tag = Rect::new(
                    right - tw - 2.0 * space::SM,
                    r.y + (r.h - metrics::TAG_H) / 2.0,
                    tw + 2.0 * space::SM,
                    metrics::TAG_H,
                );
                out.rect(
                    tag,
                    radius::PILL,
                    pal.accent_fill,
                    Some(Rim {
                        width: metrics::RIM_W,
                        colour: pal.accent_border,
                    }),
                );
                out.text(
                    run,
                    Point::new(tag.x + space::SM, tag.y + (tag.h - size::MICRO) / 2.0),
                    pal.accent_text,
                );
                right = tag.x - space::SM;
            }
            Marker::None => {}
        }
        let mut hint_w = 0.0;
        if let Some(h) = &e.hint {
            let run = h.clone().clipped(r.w * 0.4);
            hint_w = measure.width(&run);
            out.text(
                run,
                Point::new(right - hint_w, r.y + (r.h - size::BODY_SMALL) / 2.0),
                if e.disabled {
                    pal.text_tertiary
                } else {
                    pal.text_secondary
                },
            );
        }
        let title_w = (right - r.x - space::MD - hint_w - space::SM).max(0.0);
        out.text(
            e.title.clone().clipped(title_w),
            Point::new(r.x + space::MD, r.y + (r.h - size::BODY) / 2.0),
            label_colour(pal, ControlState { selected, ..st }),
        );
    }
}
