// SPDX-License-Identifier: AGPL-3.0-only
//! Overlay scrollbar with prompt / search / bell marks (T-08 §4).

use crate::draw::DrawList;
use crate::geom::Rect;
use crate::hit::{HitTree, Layer};
use crate::motion::{MotionMode, fade};
use crate::state::{UiState, WidgetId};
use crate::tokens::{Palette, Rgba, metrics, motion, radius};

/// `scrollbar.mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    AutoHide,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    /// Coloured by exit status of the command it begins.
    Prompt(Option<i32>),
    Search,
    Bell,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    /// Position 0..=1 down the whole scrollback.
    pub pos: f32,
    pub kind: MarkKind,
}

/// Lines: `total` rows of history+screen, `viewport` visible, `offset` rows above the view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    pub total: f32,
    pub viewport: f32,
    pub offset: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarLayout {
    pub id: WidgetId,
    pub track: Rect,
    pub thumb: Rect,
    /// Pointer target, wider than the visible track.
    pub hit: Rect,
}

/// `None` when everything fits (nothing to scroll).
pub fn layout(id: WidgetId, pane: Rect, s: Scroll) -> Option<ScrollbarLayout> {
    if s.total <= s.viewport || s.viewport <= 0.0 {
        return None;
    }
    let w = metrics::SCROLLBAR_W;
    let track = Rect::new(
        pane.right() - w - metrics::SCROLLBAR_EDGE,
        pane.y + metrics::SCROLLBAR_EDGE,
        w,
        (pane.h - 2.0 * metrics::SCROLLBAR_EDGE).max(0.0),
    );
    let th = (track.h * s.viewport / s.total).clamp(metrics::THUMB_MIN.min(track.h), track.h);
    let max_off = s.total - s.viewport;
    let frac = (s.offset / max_off).clamp(0.0, 1.0);
    let thumb = Rect::new(track.x, track.y + (track.h - th) * frac, w, th);
    let pad = metrics::SCROLLBAR_HIT_PAD;
    let hit = Rect::new(
        track.x - pad,
        track.y,
        w + pad + metrics::SCROLLBAR_EDGE,
        track.h,
    );
    Some(ScrollbarLayout {
        id,
        track,
        thumb,
        hit,
    })
}

impl ScrollbarLayout {
    pub fn register(&self, tree: &mut HitTree) {
        tree.add(self.id, Layer::PaneFrame, self.hit);
    }

    /// Scroll offset (rows) when the thumb's top edge is dragged to `thumb_top_y`.
    pub fn offset_for_thumb_top(&self, s: Scroll, thumb_top_y: f32) -> f32 {
        let room = self.track.h - self.thumb.h;
        if room <= 0.0 {
            return 0.0;
        }
        let frac = ((thumb_top_y - self.track.y) / room).clamp(0.0, 1.0);
        frac * (s.total - s.viewport)
    }

    /// Offset for a click on the track: centre the thumb on the pointer.
    pub fn offset_for_track_click(&self, s: Scroll, y: f32) -> f32 {
        self.offset_for_thumb_top(s, y - self.thumb.h / 2.0)
    }
}

/// Overall opacity 0..=1. `idle_ms` since the last scroll or output; `engaged` while the
/// pointer is over the bar or the thumb is dragged (always fully visible then).
pub fn visibility(mode: Mode, idle_ms: u64, engaged: bool, motion_mode: MotionMode) -> f32 {
    match mode {
        Mode::Never => 0.0,
        Mode::Always => 1.0,
        Mode::AutoHide => {
            if engaged || idle_ms < motion::SCROLLBAR_HOLD_MS {
                1.0
            } else {
                1.0 - fade(idle_ms - motion::SCROLLBAR_HOLD_MS, motion_mode)
            }
        }
    }
}

/// Whether a frame must be requested: mid-fade only. Settled (fully shown or hidden,
/// or still inside the hold) requests none; the hold end is a timer, see `hold_ends_in`.
pub fn animating(mode: Mode, idle_ms: u64, engaged: bool, motion_mode: MotionMode) -> bool {
    mode == Mode::AutoHide
        && !engaged
        && motion_mode != MotionMode::None
        && (motion::SCROLLBAR_HOLD_MS..motion::SCROLLBAR_HOLD_MS + motion::FADE_MS)
            .contains(&idle_ms)
}

/// Milliseconds until the fade starts, for arming a timer (None when not pending).
pub fn hold_ends_in(mode: Mode, idle_ms: u64, engaged: bool) -> Option<u64> {
    (mode == Mode::AutoHide && !engaged && idle_ms < motion::SCROLLBAR_HOLD_MS)
        .then(|| motion::SCROLLBAR_HOLD_MS - idle_ms)
}

fn mark_colour(p: &Palette, k: MarkKind) -> Rgba {
    match k {
        MarkKind::Prompt(Some(0)) => p.ok,
        MarkKind::Prompt(Some(_)) => p.danger,
        MarkKind::Prompt(None) => p.text_tertiary,
        MarkKind::Search => p.accent,
        MarkKind::Bell => p.accent_text,
    }
}

pub fn draw(
    l: &ScrollbarLayout,
    marks: &[Mark],
    visibility: f32,
    ui: &UiState,
    pal: &Palette,
    out: &mut DrawList,
) {
    if visibility <= 0.0 {
        return;
    }
    let st = ui.control(l.id, false, false);
    let active = st.hover || st.pressed;
    let (track, thumb) = if active {
        let grow = metrics::SCROLLBAR_W_ACTIVE - metrics::SCROLLBAR_W;
        (
            Rect::new(l.track.x - grow, l.track.y, l.track.w + grow, l.track.h),
            Rect::new(l.thumb.x - grow, l.thumb.y, l.thumb.w + grow, l.thumb.h),
        )
    } else {
        (l.track, l.thumb)
    };
    if active {
        out.rect(track, radius::PILL, pal.track.scaled(visibility), None);
    }
    for m in marks {
        let y = l.track.y + m.pos.clamp(0.0, 1.0) * (l.track.h - metrics::MARK_H);
        out.rect(
            Rect::new(track.x, y, track.w, metrics::MARK_H),
            0.0,
            mark_colour(pal, m.kind).scaled(visibility),
            None,
        );
    }
    let colour = if st.pressed || st.hover {
        pal.scroller_hover
    } else {
        pal.scroller
    };
    out.rect(thumb, radius::PILL, colour.scaled(visibility), None);
}
