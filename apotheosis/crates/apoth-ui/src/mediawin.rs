// SPDX-License-Identifier: AGPL-3.0-only
//! A media window's chrome (`apoth play --ext`): a keyboard-focus rim, and on hover a glass
//! close button at the top right and a small "Esc to close" hint at the bottom. Nothing is
//! drawn at rest, so the picture stays clean. All interface copy (no program text).

use crate::draw::{DrawList, IconId, Rim};
use crate::geom::{Point, Rect};
use crate::state::{ControlState, glass, label_colour, surface};
use crate::text::{Measure, TextRun};
use crate::tokens::{Palette, Rgba, metrics, radius, size, space};

/// What a media window shows over its picture.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaWinSpec {
    /// The window has the keyboard focus.
    pub focused: bool,
    /// The pointer is over the window.
    pub hovered: bool,
    /// State of the close button.
    pub close: ControlState,
}

/// The close button: top right, inset from the edge.
pub fn close_rect(area: Rect) -> Rect {
    let d = metrics::MEDIA_CLOSE;
    Rect::new(area.right() - space::SM - d, area.y + space::SM, d, d)
}

/// The hint pill: bottom centre.
pub fn hint_rect(area: Rect, text: &TextRun, measure: &dyn Measure) -> Rect {
    let w = measure.width(text) + 2.0 * space::MD;
    let h = metrics::TOOLTIP_H;
    Rect::new(
        area.x + (area.w - w) / 2.0,
        area.bottom() - space::SM - h,
        w,
        h,
    )
}

/// The hint text.
pub fn hint() -> TextRun {
    TextRun::interface("Esc to close", size::BODY_SMALL)
}

pub fn draw(
    area: Rect,
    s: &MediaWinSpec,
    pal: &Palette,
    measure: &dyn Measure,
    out: &mut DrawList,
) {
    if s.hovered {
        let r = close_rect(area);
        let (fill, rim) = surface(pal, s.close);
        glass(out, r, radius::PILL, rim.colour, pal);
        if fill != Rgba::CLEAR {
            out.rect(r, radius::PILL, fill, Some(rim));
        }
        out.icon(
            IconId::Close,
            r.inset(space::SM),
            label_colour(pal, s.close),
        );
        let t = hint();
        let h = hint_rect(area, &t, measure);
        glass(out, h, radius::PILL, pal.cell_rim, pal);
        out.text(
            t,
            Point::new(h.x + space::MD, h.y + (h.h - size::BODY_SMALL) / 2.0),
            pal.text_secondary,
        );
    }
    if s.focused {
        out.rect(
            area.inset(metrics::FOCUS_RIM_W / 2.0),
            radius::WINDOW,
            Rgba::CLEAR,
            Some(Rim {
                width: metrics::FOCUS_RIM_W,
                colour: pal.cell_focus_rim_hover,
            }),
        );
    }
}
