// SPDX-License-Identifier: AGPL-3.0-only
//! Procedural decorations: underline styles, strikethrough, overline (T-07 §4 pass 3c;
//! RENDERING-17, -18). Every periodic pattern is computed from the absolute pixel x, in
//! integers, so it joins across cells.
//!
//! Geometry (T-07 gives only the joining rule):
//! * dotted: period `2t`, on for `t`; dashed: period `4t`, on for `2t` (`t` = thickness);
//! * curly: a triangle wave of period `cell_w` and amplitude `min(2t, room)`;
//! * double: two bars of `t` separated by `t`, shifted up if they would leave the cell.

use crate::scene::RectInstance;
use apoth_font::metrics::CellMetrics;
use apoth_grid::Underline;

/// Pushes the rects of one cell's underline. `x0`/`y0` are the cell's pixel origin in the grid.
pub fn underline(
    style: Underline,
    x0: i32,
    y0: i32,
    m: &CellMetrics,
    color: [u8; 4],
    out: &mut Vec<RectInstance>,
) {
    let t = m.underline_thickness.max(1);
    let w = m.width.max(1);
    let h = m.height.max(1);
    let pos = m.underline_position.clamp(0, (h - t).max(0));
    match style {
        Underline::None => {}
        Underline::Single => out.push(RectInstance::new(x0, y0 + pos, w, t, color)),
        Underline::Double => {
            let pos = pos.min((h - 3 * t).max(0));
            out.push(RectInstance::new(x0, y0 + pos, w, t, color));
            if pos + 2 * t + t <= h {
                out.push(RectInstance::new(x0, y0 + pos + 2 * t, w, t, color));
            }
        }
        Underline::Dotted => dashes(x0, y0 + pos, w, t, 2 * t, t, color, out),
        Underline::Dashed => dashes(x0, y0 + pos, w, t, 4 * t, 2 * t, color, out),
        Underline::Curly => curly(x0, y0, pos, w, h, t, color, out),
    }
}

/// Runs of `on` pixels every `period` pixels, phase fixed to absolute x = 0.
#[allow(clippy::too_many_arguments)]
fn dashes(
    x0: i32,
    y: i32,
    w: i32,
    t: i32,
    period: i32,
    on: i32,
    color: [u8; 4],
    out: &mut Vec<RectInstance>,
) {
    let mut x = x0;
    let end = x0 + w;
    while x < end {
        let phase = x.rem_euclid(period);
        if phase < on {
            let run = (on - phase).min(end - x);
            out.push(RectInstance::new(x, y, run, t, color));
            x += run;
        } else {
            x += period - phase;
        }
    }
}

/// Triangle-wave underline; consecutive columns with equal offset merge into one rect.
#[allow(clippy::too_many_arguments)]
fn curly(
    x0: i32,
    y0: i32,
    pos: i32,
    w: i32,
    h: i32,
    t: i32,
    color: [u8; 4],
    out: &mut Vec<RectInstance>,
) {
    let amp = (2 * t).min(h - pos - t).max(0);
    if w < 2 || amp == 0 {
        out.push(RectInstance::new(x0, y0 + pos, w, t, color));
        return;
    }
    let mut run_start = x0;
    let mut run_dy = wave_dy(0, w, amp);
    for i in 1..=w {
        let dy = if i < w { wave_dy(i, w, amp) } else { -1 };
        if dy != run_dy {
            out.push(RectInstance::new(
                run_start,
                y0 + pos + run_dy,
                x0 + i - run_start,
                t,
                color,
            ));
            run_start = x0 + i;
            run_dy = dy;
        }
    }
}

/// Offset of the wave at column `u` of a period `p`: 0 at u = 0, `amp` at u = p/2.
#[must_use]
pub fn wave_dy(u: i32, p: i32, amp: i32) -> i32 {
    let u = u.rem_euclid(p);
    let tri = if 2 * u <= p { u } else { p - u };
    // tri in 0..=p/2; scale to 0..=amp with rounding.
    (2 * tri * amp + p / 2) / p
}

/// Strikethrough bar for one cell.
pub fn strikethrough(
    x0: i32,
    y0: i32,
    m: &CellMetrics,
    color: [u8; 4],
    out: &mut Vec<RectInstance>,
) {
    let t = m.strikethrough_thickness.max(1);
    let pos = m.strikethrough_position.clamp(0, (m.height - t).max(0));
    out.push(RectInstance::new(x0, y0 + pos, m.width.max(1), t, color));
}

/// Overline bar for one cell.
pub fn overline(x0: i32, y0: i32, m: &CellMetrics, color: [u8; 4], out: &mut Vec<RectInstance>) {
    let t = m.overline_thickness.max(1);
    let pos = m.overline_position.clamp(0, (m.height - t).max(0));
    out.push(RectInstance::new(x0, y0 + pos, m.width.max(1), t, color));
}

#[cfg(test)]
pub(crate) fn test_metrics() -> CellMetrics {
    CellMetrics {
        width: 8,
        height: 16,
        baseline_from_bottom: 4,
        underline_position: 13,
        underline_thickness: 1,
        strikethrough_position: 8,
        strikethrough_thickness: 1,
        overline_position: 0,
        overline_thickness: 1,
        cursor_thickness: 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: [u8; 4] = [255, 255, 255, 255];

    fn coverage(rects: &[RectInstance], x: i32, y: i32) -> bool {
        rects
            .iter()
            .any(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
    }

    #[test]
    fn single_is_one_bar() {
        let m = test_metrics();
        let mut v = Vec::new();
        underline(Underline::Single, 16, 32, &m, C, &mut v);
        assert_eq!(v, vec![RectInstance::new(16, 45, 8, 1, C)]);
    }

    #[test]
    fn double_has_gap_and_stays_in_cell() {
        let m = test_metrics();
        let mut v = Vec::new();
        underline(Underline::Double, 0, 0, &m, C, &mut v);
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|r| r.y + r.h <= m.height));
        assert!(v[1].y - (v[0].y + v[0].h) >= 1);
    }

    #[test]
    fn dotted_and_dashed_join_across_cells() {
        let m = test_metrics();
        for style in [Underline::Dotted, Underline::Dashed] {
            // Draw 3 adjacent cells; compare to the pattern from a single wide cell.
            let mut cells = Vec::new();
            for c in 0..3 {
                underline(style, c * m.width, 0, &m, C, &mut cells);
            }
            let (period, on) = if style == Underline::Dotted {
                (2, 1)
            } else {
                (4, 2)
            };
            for x in 0..3 * m.width {
                assert_eq!(coverage(&cells, x, 13), x % period < on, "{style:?} x={x}");
            }
        }
    }

    #[test]
    fn curly_is_periodic_per_cell_and_continuous() {
        let m = test_metrics();
        let mut v = Vec::new();
        underline(Underline::Curly, 0, 0, &m, C, &mut v);
        let mut v2 = Vec::new();
        underline(Underline::Curly, m.width, 0, &m, C, &mut v2);
        // Second cell is the first translated by one cell width.
        let shifted: Vec<_> = v
            .iter()
            .map(|r| RectInstance::new(r.x + m.width, r.y, r.w, r.h, C))
            .collect();
        assert_eq!(shifted, v2);
        // One column per x, no overlap, no leaving the cell.
        for x in 0..m.width {
            let n = v.iter().filter(|r| x >= r.x && x < r.x + r.w).count();
            assert_eq!(n, 1, "column {x}");
        }
        assert!(v.iter().all(|r| r.y >= 0 && r.y + r.h <= m.height));
        // Wave starts and ends at the same height (joins to the next cell).
        assert_eq!(wave_dy(0, m.width, 2), wave_dy(m.width, m.width, 2));
    }

    #[test]
    fn strike_and_over() {
        let m = test_metrics();
        let mut v = Vec::new();
        strikethrough(0, 0, &m, C, &mut v);
        overline(0, 0, &m, C, &mut v);
        assert_eq!(v[0], RectInstance::new(0, 8, 8, 1, C));
        assert_eq!(v[1], RectInstance::new(0, 0, 8, 1, C));
    }

    #[test]
    fn degenerate_metrics_do_not_panic() {
        let mut m = test_metrics();
        m.width = 1;
        m.height = 1;
        let mut v = Vec::new();
        for s in [
            Underline::Single,
            Underline::Double,
            Underline::Curly,
            Underline::Dotted,
            Underline::Dashed,
        ] {
            underline(s, 0, 0, &m, C, &mut v);
        }
        assert!(!v.is_empty());
    }
}
