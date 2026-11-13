// SPDX-License-Identifier: AGPL-3.0-only
//! Integer cell metrics (T-06 §3, FONTS-06). Every value is whole physical pixels at the
//! surface's current scale; recompute on scale change (the inputs are the pixel size).

use crate::config::Adjust;
use crate::face::FaceInstance;
use skrifa::{
    MetadataProvider,
    instance::{LocationRef, Size},
};

/// Unrounded font-level inputs, already scaled to pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawMetrics {
    /// Ascent, positive up from the baseline.
    pub ascent: f32,
    /// Descent as a positive magnitude below the baseline.
    pub descent: f32,
    /// Line gap.
    pub line_gap: f32,
    /// Advance of the primary face's reference glyph.
    pub advance: f32,
    /// Underline top edge above the baseline (usually negative) and thickness.
    pub underline: Option<(f32, f32)>,
    /// Strikeout top edge above the baseline and thickness.
    pub strikeout: Option<(f32, f32)>,
    /// x-height, used for the strikethrough fallback.
    pub x_height: Option<f32>,
    /// The pixel size the values were scaled for.
    pub px_size: f32,
}

/// The cell grid metrics. Vertical positions are the y of the line's top edge measured down
/// from the top of the cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    /// Cell width, at least 1.
    pub width: i32,
    /// Cell height, at least 1.
    pub height: i32,
    /// Baseline distance above the cell bottom.
    pub baseline_from_bottom: i32,
    /// Underline top edge from the cell top.
    pub underline_position: i32,
    /// Underline thickness, at least 1.
    pub underline_thickness: i32,
    /// Strikethrough top edge from the cell top.
    pub strikethrough_position: i32,
    /// Strikethrough thickness, at least 1.
    pub strikethrough_thickness: i32,
    /// Overline top edge from the cell top (always 0).
    pub overline_position: i32,
    /// Overline thickness, at least 1.
    pub overline_thickness: i32,
    /// Cursor bar/underline thickness, at least 1.
    pub cursor_thickness: i32,
}

impl CellMetrics {
    /// Distance from the cell top to the baseline.
    pub fn ascent_from_top(&self) -> i32 {
        self.height - self.baseline_from_bottom
    }

    /// Computes metrics from unrounded inputs. Rounds (never ceils), splits the line gap
    /// evenly above and below, floors every derived metric at 1 px, then applies `adjust`.
    pub fn from_raw(raw: &RawMetrics, adjust: &Adjust) -> Self {
        let px = raw.px_size.max(1.0);
        let r = |v: f32| v.round() as i32;

        let mut width = r(raw.advance).max(1);
        let mut height = r(raw.ascent + raw.descent + raw.line_gap).max(1);
        let mut below = r(raw.descent + raw.line_gap * 0.5).clamp(0, height);

        let default_thick = (px / 16.0).max(1.0);
        let (ul_off, ul_thick) = raw.underline.unwrap_or((-(px * 0.1), default_thick));
        let (st_off, st_thick) = raw.strikeout.unwrap_or((
            raw.x_height.map_or(raw.ascent * 0.3, |x| x * 0.5) + default_thick * 0.5,
            ul_thick.max(default_thick),
        ));

        let mut ul_t = r(ul_thick).max(1);
        let mut st_t = r(st_thick).max(1);
        let mut ov_t = ul_t;
        let mut cur_t = r(width as f32 / 8.0).max(1);

        // Positions are computed from the unadjusted ascent so a cell-height adjustment moves
        // the text and the lines together.
        let ascent_px = height - below;
        let mut ul_pos = ascent_px - r(ul_off);
        let mut st_pos = ascent_px - r(st_off);

        if let Some(a) = adjust.cell_width {
            width = a.apply(width).max(1);
        }
        if let Some(a) = adjust.cell_height {
            let new_h = a.apply(height).max(1);
            let delta = new_h - height;
            // The change is split evenly above and below the text.
            below = (below + delta.div_euclid(2)).clamp(0, new_h);
            ul_pos += delta - delta.div_euclid(2);
            st_pos += delta - delta.div_euclid(2);
            height = new_h;
        }
        if let Some(a) = adjust.baseline {
            let old = below;
            below = a.apply(below).clamp(0, height);
            let moved = old - below;
            ul_pos += moved;
            st_pos += moved;
        }
        if let Some(a) = adjust.underline_thickness {
            ul_t = a.apply(ul_t).max(1);
        }
        if let Some(a) = adjust.strikethrough_thickness {
            st_t = a.apply(st_t).max(1);
        }
        if let Some(a) = adjust.underline_position {
            ul_pos = a.apply(ul_pos);
        }
        if let Some(a) = adjust.strikethrough_position {
            st_pos = a.apply(st_pos);
        }
        if let Some(a) = adjust.cursor_thickness {
            cur_t = a.apply(cur_t).max(1);
        }
        ov_t = ov_t.max(1);

        let fit = |pos: i32, t: i32| pos.clamp(0, (height - t).max(0));
        Self {
            width,
            height,
            baseline_from_bottom: below,
            underline_position: fit(ul_pos, ul_t),
            underline_thickness: ul_t,
            strikethrough_position: fit(st_pos, st_t),
            strikethrough_thickness: st_t,
            overline_position: 0,
            overline_thickness: ov_t,
            cursor_thickness: cur_t,
        }
    }

    /// Computes the metrics of a face instance at `px_size` pixels per em. The advance comes
    /// from `0` (then `M`, then the average width).
    pub fn compute(inst: &FaceInstance, px_size: f32, adjust: &Adjust) -> Option<Self> {
        Some(Self::from_raw(&raw_metrics(inst, px_size)?, adjust))
    }
}

/// Reads [`RawMetrics`] for an instance.
pub fn raw_metrics(inst: &FaceInstance, px_size: f32) -> Option<RawMetrics> {
    let font = inst.face().font()?;
    let loc = LocationRef::new(inst.location().coords());
    let size = Size::new(px_size);
    let m = font.metrics(size, loc);
    let gm = font.glyph_metrics(size, loc);
    let cm = font.charmap();
    let advance = ['0', 'M', ' ']
        .iter()
        .find_map(|c| cm.map(*c as u32).and_then(|g| gm.advance_width(g)))
        .or(m.average_width)?;
    Some(RawMetrics {
        ascent: m.ascent,
        descent: m.descent.abs(),
        line_gap: m.leading.max(0.0),
        advance,
        underline: m.underline.map(|d| (d.offset, d.thickness)),
        strikeout: m.strikeout.map(|d| (d.offset, d.thickness)),
        x_height: m.x_height,
        px_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Adjustment;

    fn raw() -> RawMetrics {
        RawMetrics {
            ascent: 14.4,
            descent: 3.6,
            line_gap: 1.0,
            advance: 9.6,
            underline: Some((-1.7, 0.6)),
            strikeout: Some((5.2, 0.7)),
            x_height: Some(9.0),
            px_size: 16.0,
        }
    }

    #[test]
    fn rounds_not_ceils_and_splits_gap() {
        let m = CellMetrics::from_raw(&raw(), &Adjust::default());
        assert_eq!(m.width, 10); // round(9.6)
        assert_eq!(m.height, 19); // round(14.4+3.6+1.0) = 19
        assert_eq!(m.baseline_from_bottom, 4); // round(3.6 + 0.5)
        assert_eq!(m.ascent_from_top(), 15);
    }

    #[test]
    fn rounding_boundary_is_round_half_away() {
        let mut r = raw();
        r.advance = 9.4;
        r.ascent = 14.0;
        r.descent = 3.4;
        r.line_gap = 0.0;
        let m = CellMetrics::from_raw(&r, &Adjust::default());
        assert_eq!((m.width, m.height), (9, 17)); // 17.4 rounds down: not ceil
    }

    #[test]
    fn minimum_one_pixel() {
        let mut r = raw();
        r.underline = Some((-1.0, 0.01));
        r.strikeout = Some((5.0, 0.0));
        r.px_size = 8.0;
        let m = CellMetrics::from_raw(&r, &Adjust::default());
        assert_eq!(m.underline_thickness, 1);
        assert_eq!(m.strikethrough_thickness, 1);
        assert!(m.cursor_thickness >= 1 && m.overline_thickness >= 1);
    }

    #[test]
    fn underline_sits_below_baseline() {
        let m = CellMetrics::from_raw(&raw(), &Adjust::default());
        assert_eq!(m.underline_position, 15 + 2); // ascent 15, offset -1.7 -> +2 below
        assert!(m.strikethrough_position < m.ascent_from_top());
    }

    #[test]
    fn adjustments_apply_after_minimum() {
        let adj = Adjust {
            underline_thickness: Some(Adjustment::Px(2)),
            cell_width: Some(Adjustment::Percent(10.0)),
            ..Default::default()
        };
        let m = CellMetrics::from_raw(&raw(), &adj);
        assert_eq!(m.underline_thickness, 3); // max(1, round(0.6)) = 1, then +2
        assert_eq!(m.width, 11); // 10 * 1.1
    }

    #[test]
    fn cell_height_adjust_splits_evenly() {
        let base = CellMetrics::from_raw(&raw(), &Adjust::default());
        let adj = Adjust {
            cell_height: Some(Adjustment::Px(4)),
            ..Default::default()
        };
        let m = CellMetrics::from_raw(&raw(), &adj);
        assert_eq!(m.height, base.height + 4);
        assert_eq!(m.baseline_from_bottom, base.baseline_from_bottom + 2);
        assert_eq!(m.ascent_from_top(), base.ascent_from_top() + 2);
        // Lines move with the text.
        assert_eq!(m.underline_position, base.underline_position + 2);
    }

    #[test]
    fn everything_stays_inside_the_cell() {
        let adj = Adjust {
            underline_position: Some(Adjustment::Px(500)),
            strikethrough_position: Some(Adjustment::Px(-500)),
            ..Default::default()
        };
        let m = CellMetrics::from_raw(&raw(), &adj);
        assert!(m.underline_position + m.underline_thickness <= m.height);
        assert_eq!(m.strikethrough_position, 0);
    }

    #[test]
    fn scale_change_scales_integers() {
        let mut r = raw();
        let a = CellMetrics::from_raw(&r, &Adjust::default());
        r.ascent *= 2.0;
        r.descent *= 2.0;
        r.line_gap *= 2.0;
        r.advance *= 2.0;
        r.px_size *= 2.0;
        r.underline = r.underline.map(|(o, t)| (o * 2.0, t * 2.0));
        r.strikeout = r.strikeout.map(|(o, t)| (o * 2.0, t * 2.0));
        let b = CellMetrics::from_raw(&r, &Adjust::default());
        assert!((b.width - 2 * a.width).abs() <= 1);
        assert!((b.height - 2 * a.height).abs() <= 1);
    }
}
