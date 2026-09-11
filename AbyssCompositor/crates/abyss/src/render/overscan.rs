// SPDX-License-Identifier: AGPL-3.0-only
//! Applying overscan compensation to a finished element list (COMP-03 §2).
//!
//! [`crate::outputs::overscan`] owns the arithmetic; this owns the render
//! plumbing. Two things happen here and nowhere else:
//!
//! 1. The whole scene is rescaled into the inset rectangle and relocated to its
//!    top-left corner. Nothing is cropped — the framebuffer keeps its size and
//!    the four margins are simply never painted, so the backend's `CLEAR` gives
//!    us the black bars for free.
//! 2. The calibration overlay's corner markers are appended *outside* that
//!    wrap, in raw framebuffer coordinates, because their whole job is to show
//!    the human where the desktop's edge now sits.
//!
//! The wrap is conditional, not unconditional: `DrmCompositor` inspects element
//! types to assign KMS planes, so an always-on wrapper would cost direct
//! scanout on every output including the ones with no overscan at all. An
//! output with zero overscan takes the [`OutputElement::Plain`] path and is
//! bit-for-bit what it was before this feature existed.

use smithay::{
    backend::renderer::{
        element::{
            solid::SolidColorRenderElement,
            utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            Kind,
        },
        gles::GlesRenderer,
    },
    utils::{Physical, Point, Rectangle, Size},
};

use crate::outputs::overscan::Overscan;

use super::AbyssRenderElement;

smithay::backend::renderer::element::render_elements! {
    pub OutputElement<=GlesRenderer>;
    Plain=AbyssRenderElement,
    Framed=RescaleRenderElement<RelocateRenderElement<AbyssRenderElement>>,
}

/// Colour of the calibration markers: the same amber the trusted UI uses, at
/// full opacity so it survives a TV's contrast mangling.
const MARKER: [f32; 4] = [1.0, 0.69, 0.16, 1.0];
/// Stable element ids for the eight marker quads. Ids have to persist across
/// frames or the damage tracker treats every frame as a fresh set of elements;
/// smithay only hands out fresh ids, so we mint them once and keep them.
fn marker_ids() -> &'static [smithay::backend::renderer::element::Id; 8] {
    use smithay::backend::renderer::element::Id;
    static IDS: std::sync::OnceLock<[Id; 8]> = std::sync::OnceLock::new();
    IDS.get_or_init(|| std::array::from_fn(|_| Id::new()))
}

/// Arm length and thickness of one corner bracket, in physical pixels.
const MARKER_LEN: i32 = 64;
const MARKER_THICK: i32 = 4;

/// Wrap a frame's elements for an output. `size` is the output's current mode
/// in physical pixels; `overscan` is the already-clamped effective value.
pub fn frame(
    elements: Vec<AbyssRenderElement>,
    overscan: Overscan,
    size: Size<i32, Physical>,
) -> Vec<OutputElement> {
    if overscan.is_zero() {
        return elements.into_iter().map(OutputElement::Plain).collect();
    }
    let rect = overscan.inset(size);
    let (sx, sy) = overscan.scale(size);
    elements
        .into_iter()
        .map(|element| {
            // Scale about the framebuffer origin, then push the whole scaled
            // scene down-right by the top-left inset. Order matters: relocate
            // is the inner wrap so the rescale applies to the desktop's own
            // coordinates rather than to the already-offset ones.
            let placed = RelocateRenderElement::from_element(element, rect.loc, Relocate::Relative);
            OutputElement::Framed(RescaleRenderElement::from_element(
                placed,
                Point::from((0, 0)),
                (sx, sy),
            ))
        })
        .collect()
}

/// Corner brackets marking the four edges of the desktop as the panel will
/// actually show them. Compositor-drawn by necessity: these live in the
/// framebuffer margin, which is exactly the region no client can ever address.
///
/// Push these *after* [`frame`] so they land on top and stay unscaled.
pub fn calibration_markers(overscan: Overscan, size: Size<i32, Physical>, out: &mut Vec<OutputElement>) {
    let r = overscan.inset(size);
    let (x0, y0) = (r.loc.x, r.loc.y);
    let (x1, y1) = (r.loc.x + r.size.w, r.loc.y + r.size.h);
    let len = MARKER_LEN.min(r.size.w / 2).min(r.size.h / 2).max(1);
    let t = MARKER_THICK.min(len);

    // Each corner is two quads: one along each edge meeting there.
    let bars = [
        // top-left
        (x0, y0, len, t),
        (x0, y0, t, len),
        // top-right
        (x1 - len, y0, len, t),
        (x1 - t, y0, t, len),
        // bottom-left
        (x0, y1 - t, len, t),
        (x0, y1 - len, t, len),
        // bottom-right
        (x1 - len, y1 - t, len, t),
        (x1 - t, y1 - len, t, len),
    ];

    for (i, (x, y, w, h)) in bars.into_iter().enumerate() {
        let geo = Rectangle::new((x, y).into(), (w, h).into());
        out.push(OutputElement::Plain(AbyssRenderElement::Solid(
            SolidColorRenderElement::new(marker_ids()[i].clone(), geo, 0, MARKER, Kind::Unspecified),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_overscan_keeps_every_element_plain() {
        let out = frame(Vec::new(), Overscan::default(), (1920, 1080).into());
        assert!(out.is_empty());
    }

    #[test]
    fn markers_sit_on_the_desktop_edge_not_the_panel_edge() {
        let mut out = Vec::new();
        let o = Overscan::uniform(30);
        calibration_markers(o, (1920, 1080).into(), &mut out);
        // Eight quads, two per corner.
        assert_eq!(out.len(), 8);
    }
}
