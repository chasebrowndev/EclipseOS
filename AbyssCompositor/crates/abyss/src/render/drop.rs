// SPDX-License-Identifier: AGPL-3.0-only
//! Radiant drop guides (COMP-05 §3.1): while a window is dragged onto the
//! tiling tree, outline every tile, tint the screen-edge bands and draw a
//! ghost of exactly where the window will land.
//!
//! Drawn as a backend-prepended pass like the region selector, so it never
//! reaches `capture.rs`: guides are compositor chrome, not screen content.

use smithay::{
    backend::renderer::element::{
        solid::{SolidColorBuffer, SolidColorRenderElement},
        Kind,
    },
    output::Output,
    utils::{Logical, Point, Rectangle, Scale, Size},
};

use super::AbyssRenderElement;
use crate::shell::TileDrag;

const OUTLINE: i32 = 2;
const TILE_ALPHA: f32 = 0.55;
const BAND_ALPHA: f32 = 0.12;
const GHOST_FILL_ALPHA: f32 = 0.22;
const GHOST_LINE_ALPHA: f32 = 1.0;

fn quad(
    r: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    color: [f32; 4],
    alpha: f32,
    out: &mut Vec<AbyssRenderElement>,
) {
    if r.size.w <= 0 || r.size.h <= 0 {
        return;
    }
    let buffer = SolidColorBuffer::new(r.size, color);
    out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        r.loc.to_f64().to_physical(scale).to_i32_round(),
        scale,
        alpha,
        Kind::Unspecified,
    )));
}

/// The four edges of `r`, `t` thick, inside it.
fn outline(r: Rectangle<i32, Logical>, t: i32) -> [Rectangle<i32, Logical>; 4] {
    let t = t.min(r.size.w / 2).min(r.size.h / 2).max(0);
    let (x, y, w, h) = (r.loc.x, r.loc.y, r.size.w, r.size.h);
    [
        Rectangle::new(Point::from((x, y)), Size::from((w, t))),
        Rectangle::new(Point::from((x, y + h - t)), Size::from((w, t))),
        Rectangle::new(Point::from((x, y + t)), Size::from((t, h - 2 * t))),
        Rectangle::new(Point::from((x + w - t, y + t)), Size::from((t, h - 2 * t))),
    ]
}

/// The four screen-edge bands of the tiling area `a`, `band` wide.
fn bands(a: Rectangle<i32, Logical>, band: i32) -> [Rectangle<i32, Logical>; 4] {
    let bw = band.min(a.size.w / 2).max(0);
    let bh = band.min(a.size.h / 2).max(0);
    let (x, y, w, h) = (a.loc.x, a.loc.y, a.size.w, a.size.h);
    [
        Rectangle::new(Point::from((x, y)), Size::from((bw, h))),
        Rectangle::new(Point::from((x + w - bw, y)), Size::from((bw, h))),
        Rectangle::new(Point::from((x + bw, y)), Size::from((w - 2 * bw, bh))),
        Rectangle::new(Point::from((x + bw, y + h - bh)), Size::from((w - 2 * bw, bh))),
    ]
}

/// The guide elements for one output, front-to-back. Empty unless a drag is
/// running on `output` and `general.drop-guides` is on.
pub fn drop_elements(
    drag: Option<&TileDrag>,
    enabled: bool,
    color: [f32; 4],
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    let Some(drag) = drag.filter(|d| enabled && d.armed && &d.output == output) else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let local = |r: Rectangle<i32, Logical>| Rectangle::new(r.loc - output_loc, r.size);
    let mut out = Vec::new();
    if let Some(g) = drag.preview.ghost.map(local) {
        for e in outline(g, OUTLINE) {
            quad(e, scale, color, GHOST_LINE_ALPHA, &mut out);
        }
        quad(g, scale, color, GHOST_FILL_ALPHA, &mut out);
    }
    for (_, r) in &drag.tiles {
        for e in outline(local(*r), 1) {
            quad(e, scale, color, TILE_ALPHA, &mut out);
        }
    }
    if drag.band > 0 {
        for b in bands(local(drag.area), drag.band) {
            quad(b, scale, color, BAND_ALPHA, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn outline_edges_stay_inside_the_rect_without_overlap() {
        let e = outline(r(10, 20, 100, 50), 2);
        assert_eq!(e[0], r(10, 20, 100, 2));
        assert_eq!(e[1], r(10, 68, 100, 2));
        assert_eq!(e[2], r(10, 22, 2, 46));
        assert_eq!(e[3], r(108, 22, 2, 46));
    }

    #[test]
    fn bands_hug_each_edge_without_overlapping() {
        let b = bands(r(0, 30, 1920, 1050), 40);
        assert_eq!(b[0], r(0, 30, 40, 1050));
        assert_eq!(b[1], r(1880, 30, 40, 1050));
        assert_eq!(b[2], r(40, 30, 1840, 40));
        assert_eq!(b[3], r(40, 1040, 1840, 40));
    }

    #[test]
    fn the_capture_pass_cannot_see_the_guides() {
        let src = include_str!("capture.rs");
        assert!(
            !src.contains("drop_elements") && !src.contains("tile_drag"),
            "capture.rs names the drop guides; they are compositor chrome, not screen content"
        );
    }

    #[test]
    fn every_backend_draws_the_guides() {
        for (src, path) in [
            (include_str!("../backend/drm.rs"), "drm.rs"),
            (include_str!("../backend/winit.rs"), "winit.rs"),
            (include_str!("../backend/headless.rs"), "headless.rs"),
        ] {
            assert!(
                src.contains("drop::drop_elements"),
                "{path} does not draw the drop guides"
            );
        }
    }
}
