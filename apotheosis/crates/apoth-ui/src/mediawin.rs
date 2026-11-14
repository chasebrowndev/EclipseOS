// SPDX-License-Identifier: AGPL-3.0-only
//! A media window's chrome (`apoth play --ext`): a keyboard-focus rim, and on hover a glass
//! close button at the top right and a small "Esc to close" hint at the top left. The
//! play/pause disc and the scrub pill over the picture are [`crate::mediactl`]'s, the same
//! as inline. Nothing is drawn at rest, so the picture stays clean. All interface copy (no
//! program text).

use crate::draw::{DrawList, IconId, Rim};
use crate::geom::{Point, Rect};
use crate::state::{ControlState, glass_faded, label_colour, surface};
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
    /// The window's padding around the picture (interface units): the controls sit on the
    /// picture, not across its edge.
    pub inset: (f32, f32),
}

/// The picture's part of window `area`: the controls are placed in it.
pub fn content(area: Rect, s: &MediaWinSpec) -> Rect {
    Rect::new(
        area.x + s.inset.0,
        area.y + s.inset.1,
        (area.w - 2.0 * s.inset.0).max(0.0),
        (area.h - 2.0 * s.inset.1).max(0.0),
    )
}

/// The close button: top right, inset from the edge.
pub fn close_rect(area: Rect) -> Rect {
    let d = metrics::MEDIA_CLOSE;
    Rect::new(area.right() - space::SM - d, area.y + space::SM, d, d)
}

/// The hint pill: top left, level with the close button (the bottom edge is the scrub's).
pub fn hint_rect(area: Rect, text: &TextRun, measure: &dyn Measure) -> Rect {
    let w = measure.width(text) + 2.0 * space::MD;
    let h = metrics::TOOLTIP_H;
    Rect::new(
        area.x + space::SM,
        area.y + space::SM + (metrics::MEDIA_CLOSE - h) / 2.0,
        w,
        h,
    )
}

/// The hint text.
pub fn hint() -> TextRun {
    TextRun::interface("Esc to close", size::BODY_SMALL)
}

/// Draws the chrome; `alpha` is the hover fade level (`0..=1`, 0 draws no hover chrome).
pub fn draw(
    area: Rect,
    s: &MediaWinSpec,
    alpha: f32,
    pal: &Palette,
    measure: &dyn Measure,
    out: &mut DrawList,
) {
    let k = alpha.clamp(0.0, 1.0);
    if k > 0.0 {
        let pic = content(area, s);
        let r = close_rect(pic);
        let (fill, rim) = surface(pal, s.close);
        glass_faded(out, r, radius::PILL, rim.colour, pal, k);
        if fill != pal.cell && fill != Rgba::CLEAR {
            out.rect(
                r,
                radius::PILL,
                fill.scaled(k),
                Some(Rim {
                    width: rim.width,
                    colour: rim.colour.scaled(k),
                }),
            );
        }
        out.icon(
            IconId::Cross,
            r.inset(r.w * (1.0 - metrics::MEDIA_GLYPH) / 2.0),
            label_colour(pal, s.close).scaled(k),
        );
        let t = hint();
        let h = hint_rect(pic, &t, measure);
        glass_faded(out, h, radius::PILL, pal.cell_rim, pal, k);
        out.text(
            t,
            Point::new(h.x + space::MD, h.y + (h.h - size::BODY_SMALL) / 2.0),
            pal.text_secondary.scaled(k),
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
