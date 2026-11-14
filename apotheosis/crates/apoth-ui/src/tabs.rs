// SPDX-License-Identifier: AGPL-3.0-only
//! Tab strip (T-08 §4): floating or flat tabs, badges, close and new-tab buttons.

use crate::draw::{DrawList, IconId, Rim};
use crate::frame::TabStyle;
use crate::geom::{Point, Rect};
use crate::hit::{HitTree, Layer};
use crate::state::{UiState, WidgetId, label_colour, surface};
use crate::text::{Measure, TextRun, geometry};
use crate::tokens::{Palette, Rgba, alpha, metrics, radius, size, space};

/// Per-tab status badges (T-08 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    Unseen,
    Bell,
    Running,
    /// OSC 9;4 progress, 0..=100, drawn as a line along the tab's foot.
    Progress(u8),
    /// OSC 9;4 error state: the line is the danger colour.
    ProgressError(u8),
    /// OSC 9;4 paused state: the line is quiet.
    ProgressPaused(u8),
    /// OSC 9;4 indeterminate: the line is full and dimmed (a settled surface animates nothing).
    ProgressBusy,
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
    /// The profile in force when it is not the default (T-11 §6): a small pill before the
    /// title. Built with [`TextRun::program`]: profile names come from config files.
    pub profile: Option<TextRun>,
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

fn is_progress(b: Badge) -> bool {
    matches!(
        b,
        Badge::Progress(_)
            | Badge::ProgressError(_)
            | Badge::ProgressPaused(_)
            | Badge::ProgressBusy
    )
}

/// The OSC 9;4 line along the foot of a tab: a track and the fill to `pct`.
fn progress_line(tab: Rect, b: Badge, pal: &Palette, out: &mut DrawList) {
    let track = Rect::new(
        tab.x + space::MD,
        tab.bottom() - metrics::PROGRESS_INSET - metrics::PROGRESS_H,
        (tab.w - 2.0 * space::MD).max(0.0),
        metrics::PROGRESS_H,
    );
    out.rect(track, radius::PILL, pal.track, None);
    let (pct, colour) = match b {
        Badge::Progress(p) => (f32::from(p.min(100)) / 100.0, pal.accent),
        Badge::ProgressError(p) => (f32::from(p.min(100)) / 100.0, pal.danger),
        Badge::ProgressPaused(p) => (f32::from(p.min(100)) / 100.0, pal.text_tertiary),
        _ => (1.0, pal.accent.scaled(alpha::BUSY)),
    };
    let w = track.w * pct;
    if w > 0.0 {
        out.rect(
            Rect::new(track.x, track.y, w.max(metrics::PROGRESS_H), track.h),
            radius::PILL,
            colour,
            None,
        );
    }
}

fn badge_colour(p: &Palette, b: Badge) -> Rgba {
    match b {
        Badge::Unseen => p.accent,
        Badge::Bell => p.accent_text,
        Badge::Running => p.text_secondary,
        Badge::Progress(_) | Badge::ProgressBusy => p.accent,
        Badge::ProgressError(_) => p.danger,
        Badge::ProgressPaused(_) => p.text_tertiary,
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
        if let Some(b) = tab.badges.iter().find(|b| is_progress(**b)) {
            progress_line(c.rect, *b, pal, out);
        }
        // Badges, then title, then close.
        let mut x = c.rect.x + space::SM;
        let cy = c.rect.y + c.rect.h / 2.0;
        for b in tab.badges.iter().filter(|b| !is_progress(**b)) {
            let d = metrics::BADGE_D;
            out.rect(
                Rect::new(x, cy - d / 2.0, d, d),
                radius::PILL,
                badge_colour(pal, *b),
                None,
            );
            x += d + space::XS;
        }
        if let Some(p) = &tab.profile {
            let m = geometry();
            let tw = m.width(&TextRun::interface(p.text.clone(), size::MICRO));
            let w = (tw + 2.0 * space::SM).min(metrics::PROFILE_MAX_W);
            let pill = Rect::new(x, cy - metrics::PROFILE_H / 2.0, w, metrics::PROFILE_H);
            out.rect(
                pill,
                radius::PILL,
                pal.cell_hover,
                Some(Rim {
                    width: metrics::RIM_W,
                    colour: pal.cell_rim,
                }),
            );
            let mut run = p.clone();
            run.size = size::MICRO;
            out.text(
                run.clipped((w - 2.0 * space::SM).max(0.0)),
                Point::new(pill.x + space::SM, pill.y + (pill.h - size::MICRO) / 2.0),
                pal.text_secondary,
            );
            x += w + space::XS;
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
