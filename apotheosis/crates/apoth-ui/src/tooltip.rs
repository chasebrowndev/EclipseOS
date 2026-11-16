// SPDX-License-Identifier: AGPL-3.0-only
//! Tooltip (T-08 §2, T-14 §5): a small glass label by the pointer. Its text is program
//! supplied (a hovered link's URI), so it is sanitised, capped, and set in the program-text
//! well treatment: an inset in the monospace face, never interface copy.

use crate::draw::DrawList;
use crate::geom::{Point, Rect};
use crate::state::glass;
use crate::text::{Measure, TextRun};
use crate::tokens::{Palette, metrics, radius, size, space};

#[derive(Clone, Debug)]
pub struct TooltipSpec {
    /// Built with [`TextRun::program`].
    pub text: TextRun,
    /// The pointer, interface units.
    pub anchor: Point,
}

/// Below and right of the pointer; above it when the bottom edge would clip, and shifted
/// left when the right edge would.
pub fn layout(area: Rect, spec: &TooltipSpec, measure: &dyn Measure) -> Rect {
    let max_w = (area.w - 2.0 * space::SM).clamp(0.0, metrics::TOOLTIP_MAX_W);
    let tw = measure.width(&spec.text.clone().clipped(max_w - 2.0 * space::MD));
    let w = (tw + 2.0 * space::MD).min(max_w);
    let h = metrics::TOOLTIP_H;
    let mut x = spec.anchor.x + metrics::TOOLTIP_OFFSET;
    let mut y = spec.anchor.y + metrics::TOOLTIP_OFFSET;
    if x + w > area.right() - space::SM {
        x = area.right() - space::SM - w;
    }
    if y + h > area.bottom() - space::SM {
        y = spec.anchor.y - metrics::TOOLTIP_OFFSET - h;
    }
    Rect::new(x.max(area.x + space::SM), y.max(area.y + space::SM), w, h)
}

pub fn draw(r: Rect, spec: &TooltipSpec, pal: &Palette, out: &mut DrawList) {
    glass(out, r, radius::INSET, pal.cell_rim, pal);
    out.text(
        spec.text.clone().clipped((r.w - 2.0 * space::MD).max(0.0)),
        Point::new(r.x + space::MD, r.y + (r.h - size::BODY_SMALL) / 2.0),
        pal.text,
    );
}
