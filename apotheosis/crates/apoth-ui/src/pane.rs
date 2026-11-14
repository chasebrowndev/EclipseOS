// SPDX-License-Identifier: AGPL-3.0-only
//! Pane frame, split handle and resize margin (T-08 §4, T-13 §3).

use crate::draw::{DrawList, Rim};
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{UiState, WidgetId, surface};
use crate::text::TextRun;
use crate::tokens::{Palette, metrics, radius, size, space};

/// `panes.frame`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameMode {
    Full,
    Titles,
    None,
}

#[derive(Clone, Debug)]
pub struct PaneSpec {
    pub id: WidgetId,
    pub rect: Rect,
    pub focused: bool,
    /// Built with [`TextRun::program`].
    pub title: Option<TextRun>,
    pub mode: FrameMode,
    /// `panes.inactive-dim`, 0 disables.
    pub inactive_dim: f32,
    /// A floating pane (MUX-18): stands on its own glass ([`draw_float_backing`]).
    pub floating: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneLayout {
    pub frame: Rect,
    pub title_row: Option<Rect>,
    /// Where the terminal grid is drawn.
    pub grid: Rect,
}

pub fn layout(s: &PaneSpec) -> PaneLayout {
    let inner = match s.mode {
        FrameMode::Full => s.rect.inset(metrics::RIM_W),
        FrameMode::Titles | FrameMode::None => s.rect,
    };
    if s.mode != FrameMode::None && s.title.is_some() {
        let (row, grid) = inner.split_top(metrics::PANE_TITLE_H);
        PaneLayout {
            frame: s.rect,
            title_row: Some(row),
            grid,
        }
    } else {
        PaneLayout {
            frame: s.rect,
            title_row: None,
            grid: inner,
        }
    }
}

impl PaneLayout {
    /// Frame (below handles) and the resize margin ring (above the grid).
    pub fn register(&self, id: WidgetId, tree: &mut HitTree) {
        tree.add(id, Layer::PaneFrame, self.frame);
        tree.add(id.child(1), Layer::Grid, self.grid);
    }
}

pub fn draw(s: &PaneSpec, l: &PaneLayout, pal: &Palette, out: &mut DrawList) {
    if s.mode == FrameMode::Full {
        // Focused pane = brighter rim.
        let colour = if s.focused {
            pal.cell_focus_rim
        } else {
            pal.cell_rim
        };
        out.rect(
            s.rect,
            radius::INSET,
            crate::tokens::Rgba::CLEAR,
            Some(Rim {
                width: metrics::RIM_W,
                colour,
            }),
        );
    }
    if let (Some(row), Some(title)) = (l.title_row, &s.title) {
        let mut run = title.clone();
        run.max_width = Some((row.w - 2.0 * space::SM).max(0.0));
        let colour = if s.focused {
            pal.text
        } else {
            pal.text_secondary
        };
        out.text(
            run,
            Point::new(row.x + space::SM, row.y + (row.h - size::BODY_SMALL) / 2.0),
            colour,
        );
    }
    if !s.focused && s.inactive_dim > 0.0 {
        out.rect(
            l.grid,
            0.0,
            pal.dim.alpha(s.inactive_dim.clamp(0.0, 1.0)),
            None,
        );
    }
}

/// The glass a floating pane stands on, drawn before (under) its grid: a soft shadow for
/// depth and the frost + tint layer, so the panes beneath cannot ghost through its text.
/// Its rim and dim come from [`draw`], over the grid.
pub fn draw_float_backing(s: &PaneSpec, pal: &Palette, out: &mut DrawList) {
    out.cmds.push(crate::draw::Cmd::Shadow {
        rect: s.rect,
        radius: radius::INSET,
        spread: space::MD,
        alpha: pal.shadow.a(),
    });
    crate::state::glass(out, s.rect, radius::INSET, crate::tokens::Rgba::CLEAR, pal);
}

/// A draggable divider between panes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitHandle {
    pub id: WidgetId,
    pub rect: Rect,
    /// The divider line runs vertically (panes side by side).
    pub vertical: bool,
}

/// Handle centred on `pos` (x for a vertical divider, y otherwise) across `area`.
pub fn split_handle(id: WidgetId, area: Rect, pos: f32, vertical: bool) -> SplitHandle {
    let t = metrics::SPLIT_HANDLE;
    let rect = if vertical {
        Rect::new(pos - t / 2.0, area.y, t, area.h)
    } else {
        Rect::new(area.x, pos - t / 2.0, area.w, t)
    };
    SplitHandle { id, rect, vertical }
}

impl SplitHandle {
    pub fn register(&self, tree: &mut HitTree) {
        tree.add(self.id, Layer::Handle, self.rect);
    }

    /// New divider ratio (0..1 of `area`) for the pointer, keeping `min` px per side.
    pub fn ratio_for(&self, area: Rect, pointer: Point, min: f32) -> f32 {
        let (lo, len, p) = if self.vertical {
            (area.x, area.w, pointer.x)
        } else {
            (area.y, area.h, pointer.y)
        };
        if len <= 0.0 {
            return 0.5;
        }
        let lo_r = (min / len).min(0.5);
        ((p - lo) / len).clamp(lo_r, 1.0 - lo_r)
    }

    /// Only shown while hovered or dragged: a short grip, brighter when pressed.
    pub fn draw(&self, ui: &UiState, pal: &Palette, out: &mut DrawList) {
        let st = ui.control(self.id, false, false);
        if !(st.hover || st.pressed || st.focus_kb) {
            return;
        }
        let g = space::XS;
        let long = space::LG * 2.0;
        let grip = if self.vertical {
            Rect::new(
                self.rect.x + (self.rect.w - g) / 2.0,
                self.rect.y + (self.rect.h - long) / 2.0,
                g,
                long,
            )
        } else {
            Rect::new(
                self.rect.x + (self.rect.w - long) / 2.0,
                self.rect.y + (self.rect.h - g) / 2.0,
                long,
                g,
            )
        };
        let (fill, rim) = surface(
            pal,
            crate::state::ControlState {
                selected: true,
                ..st
            },
        );
        out.rect(grip, radius::PILL, fill, Some(rim));
    }
}

/// Thin ring just outside a pane that starts a resize (below frames, above the grid).
pub fn register_resize_margin(id: WidgetId, rect: Rect, tree: &mut HitTree) {
    let m = metrics::RESIZE_MARGIN;
    let (top, rest) = rect.split_top(m);
    let (mid, bottom) = rest.split_bottom(m);
    tree.add(id.child(2), Layer::ResizeMargin, top);
    tree.add(id.child(3), Layer::ResizeMargin, bottom);
    tree.add(
        id.child(4),
        Layer::ResizeMargin,
        Rect::new(mid.x, mid.y, m, mid.h),
    );
    tree.add(
        id.child(5),
        Layer::ResizeMargin,
        Rect::new(mid.right() - m, mid.y, m, mid.h),
    );
}

/// The visual bell (`bell.visual`): the pane's rim brightens for a moment. A single pane has
/// no frame, so the bell draws its own rim; it never moves anything and never glows.
pub fn draw_bell(rect: Rect, pal: &Palette, out: &mut DrawList) {
    out.rect(
        rect,
        radius::INSET,
        crate::tokens::Rgba::CLEAR,
        Some(Rim {
            width: metrics::BELL_RIM_W,
            colour: pal.cell_focus_rim_hover.scaled(crate::tokens::alpha::BELL),
        }),
    );
}
