// SPDX-License-Identifier: AGPL-3.0-only
//! Tab strip (T-08 §4): floating or flat tabs, badges, close and new-tab buttons.

use crate::draw::{DrawList, IconId};
use crate::frame::TabStyle;
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{UiState, WidgetId, label_colour, surface};
use crate::text::TextRun;
use crate::tokens::{Palette, Rgba, metrics, radius, size, space};

/// Per-tab status badges (T-08 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Unseen,
    Bell,
    Running,
    /// OSC 9;4 progress, 0..=100.
    Progress(u8),
    /// Exit status of the last command.
    Exit(i32),
}

#[derive(Clone, Debug)]
pub struct TabSpec {
    pub id: WidgetId,
    /// Built with [`TextRun::program`]: titles come from the program.
    pub title: TextRun,
    pub active: bool,
    pub badges: Vec<Badge>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabCell {
    /// Index into the `tabs` slice given to [`layout`].
    pub index: usize,
    pub id: WidgetId,
    pub rect: Rect,
    pub close_id: WidgetId,
    pub close: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TabStripLayout {
    pub strip: Rect,
    pub strip_id: WidgetId,
    pub cells: Vec<TabCell>,
    pub new_tab_id: WidgetId,
    pub new_tab: Rect,
    /// Tabs that did not fit.
    pub overflow: usize,
}

pub fn layout(strip: Rect, strip_id: WidgetId, tabs: &[TabSpec]) -> TabStripLayout {
    let h = (strip.h - 2.0 * space::XS).max(0.0);
    let y = strip.y + space::XS;
    let new_tab = Rect::new(
        strip.right() - space::SM - metrics::NEW_TAB,
        y + (h - metrics::NEW_TAB) / 2.0,
        metrics::NEW_TAB,
        metrics::NEW_TAB,
    );
    let new_tab_id = strip_id.child(u64::MAX);
    let left = strip.x + space::SM;
    let limit = new_tab.x - metrics::TAB_GAP;
    let span = (limit - left).max(0.0);
    let n = tabs.len();
    let mut cells = Vec::new();
    if n == 0 {
        return TabStripLayout {
            strip,
            strip_id,
            cells,
            new_tab_id,
            new_tab,
            overflow: 0,
        };
    }
    let per = (span + metrics::TAB_GAP) / n as f32 - metrics::TAB_GAP;
    let w = per.clamp(metrics::TAB_MIN_W, metrics::TAB_MAX_W);
    let fit = (((span + metrics::TAB_GAP) / (w + metrics::TAB_GAP)).floor() as usize).max(1);
    let shown = n.min(fit);
    let active = tabs.iter().position(|t| t.active).unwrap_or(0);
    let first = if active >= shown {
        active + 1 - shown
    } else {
        0
    };
    for (slot, index) in (first..first + shown).enumerate() {
        let x = left + slot as f32 * (w + metrics::TAB_GAP);
        let rect = Rect::new(x, y, w, h);
        let close = rect.centred_y(
            rect.right() - space::SM - metrics::TAB_CLOSE,
            metrics::TAB_CLOSE,
            metrics::TAB_CLOSE,
        );
        let id = tabs[index].id;
        cells.push(TabCell {
            index,
            id,
            rect,
            close_id: id.child(0),
            close,
        });
    }
    TabStripLayout {
        strip,
        strip_id,
        cells,
        new_tab_id,
        new_tab,
        overflow: n - shown,
    }
}

impl TabStripLayout {
    /// Register front-to-back-capable entries (later = in front within the layer).
    pub fn register(&self, tree: &mut HitTree) {
        tree.add(self.strip_id, Layer::TabStrip, self.strip);
        for c in &self.cells {
            tree.add(c.id, Layer::TabStrip, c.rect);
            tree.add(c.close_id, Layer::TabStrip, c.close);
        }
        tree.add(self.new_tab_id, Layer::TabStrip, self.new_tab);
    }

    /// Slot a dragged tab would drop into for pointer x: index into `tabs`.
    pub fn reorder_target(&self, x: f32) -> Option<usize> {
        let first = self.cells.first()?;
        let last = self.cells.last()?;
        if x <= first.rect.x {
            return Some(first.index);
        }
        if x >= last.rect.right() {
            return Some(last.index);
        }
        self.cells
            .iter()
            .find(|c| x >= c.rect.x && x < c.rect.right() + metrics::TAB_GAP)
            .map(|c| c.index)
    }
}

/// Wheel over the strip switches tabs: positive steps go right, clamped.
pub fn wheel_switch(active: usize, steps: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    (active as i64 + steps as i64).clamp(0, count as i64 - 1) as usize
}

fn badge_colour(p: &Palette, b: Badge) -> Rgba {
    match b {
        Badge::Unseen => p.accent,
        Badge::Bell => p.accent_text,
        Badge::Running => p.text_secondary,
        Badge::Progress(_) => p.accent,
        Badge::Exit(0) => p.ok,
        Badge::Exit(_) => p.danger,
    }
}

pub fn draw(
    l: &TabStripLayout,
    tabs: &[TabSpec],
    style: TabStyle,
    ui: &UiState,
    pal: &Palette,
    out: &mut DrawList,
) {
    if style == TabStyle::Hidden {
        return;
    }
    if style == TabStyle::Flat {
        out.rect(l.strip, 0.0, pal.bar_tint, None);
        out.rect(
            Rect::new(
                l.strip.x,
                l.strip.bottom() - metrics::RIM_W,
                l.strip.w,
                metrics::RIM_W,
            ),
            0.0,
            pal.hairline,
            None,
        );
    }
    for c in &l.cells {
        let tab = &tabs[c.index];
        let hover_tab = ui.hover == Some(c.id) || ui.hover == Some(c.close_id);
        let mut st = ui.control(c.id, false, tab.active);
        st.hover = hover_tab;
        let (fill, rim) = surface(pal, st);
        let show_bg =
            style == TabStyle::Floating || tab.active || st.hover || st.focus_kb || st.pressed;
        if show_bg {
            out.rect(c.rect, radius::INSET, fill, Some(rim));
        }
        // Badges, then title, then close.
        let mut x = c.rect.x + space::SM;
        let cy = c.rect.y + c.rect.h / 2.0;
        for b in &tab.badges {
            let d = metrics::BADGE_D;
            out.rect(
                Rect::new(x, cy - d / 2.0, d, d),
                radius::PILL,
                badge_colour(pal, *b),
                None,
            );
            x += d + space::XS;
        }
        let show_close = tab.active || hover_tab || st.focus_kb;
        let title_right = if show_close {
            c.close.x - space::XS
        } else {
            c.rect.right() - space::SM
        };
        let mut run = tab.title.clone();
        run.max_width = Some((title_right - x).max(0.0));
        out.text(
            run,
            Point::new(x, c.rect.y + (c.rect.h - size::BODY) / 2.0),
            label_colour(pal, st),
        );
        if show_close {
            let cst = ui.control(c.close_id, false, false);
            if cst.hover || cst.pressed || cst.focus_kb {
                let (cf, cr) = surface(pal, cst);
                out.rect(c.close, radius::INSET, cf, Some(cr));
            }
            let ic = c.close.inset(space::XS);
            out.icon(IconId::Close, ic, label_colour(pal, cst));
        }
    }
    let nst = ui.control(l.new_tab_id, false, false);
    let (nf, nr) = surface(pal, nst);
    if style == TabStyle::Floating || nst.hover || nst.pressed || nst.focus_kb {
        out.rect(l.new_tab, radius::INSET, nf, Some(nr));
    }
    out.icon(
        IconId::Plus,
        l.new_tab.inset(space::SM),
        label_colour(pal, nst),
    );
}
