// SPDX-License-Identifier: AGPL-3.0-only
//! Compositor-side overscan compensation (COMP-03 §2).
//!
//! Televisions routinely crop 2–5% off every edge of an HDMI signal and there
//! is no way to talk them out of it. The kernel exposes `underscan`,
//! `underscan hborder` and `underscan vborder` connector properties for exactly
//! this — but `examples/underscan_probe.rs` shows nvidia-open advertises none
//! of them, on any connector. So abyss does it itself.
//!
//! **Nothing is cropped.** The scene is *scaled down* to fit inside an inset
//! rectangle of the unchanged framebuffer and the four margins are left black;
//! the panel's own crop then eats the black instead of the desktop. Clients see
//! an unchanged logical size, so window management, layer shell and the space
//! are untouched by this feature — the whole cost is one rescale + relocate on
//! the way into the frame, and the inverse map on absolute pointer input.
//!
//! Insets are in **physical framebuffer pixels**, per edge, because that is the
//! unit the human is actually calibrating against: they are matching a real
//! black border on a real panel.

use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

/// Largest inset accepted on one edge, as a fraction of that axis. Past this a
/// typo has stopped being a calibration and started being a way to lose the
/// desktop; the setter clamps rather than refusing so a bad value from any of
/// the three front ends is still recoverable from the same UI.
const MAX_FRACTION: f64 = 0.25;

/// Per-edge inset in physical pixels. All zero means "no overscan
/// compensation", which is the only state in which the render path is
/// untouched and direct scanout stays available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overscan {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

impl Overscan {
    /// The same inset on all four edges.
    pub fn uniform(px: i32) -> Self {
        Self {
            top: px,
            bottom: px,
            left: px,
            right: px,
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// Clamp to non-negative and to [`MAX_FRACTION`] of each axis of `size`.
    /// Applied on every path in — config, state file, IPC and calibration — so
    /// no later stage has to re-check.
    pub fn clamped(self, size: Size<i32, Physical>) -> Self {
        let hmax = ((size.w as f64) * MAX_FRACTION) as i32;
        let vmax = ((size.h as f64) * MAX_FRACTION) as i32;
        Self {
            top: self.top.clamp(0, vmax.max(0)),
            bottom: self.bottom.clamp(0, vmax.max(0)),
            left: self.left.clamp(0, hmax.max(0)),
            right: self.right.clamp(0, hmax.max(0)),
        }
    }

    /// The rectangle of the framebuffer the desktop is scaled into.
    pub fn inset(&self, size: Size<i32, Physical>) -> Rectangle<i32, Physical> {
        let w = (size.w - self.left - self.right).max(1);
        let h = (size.h - self.top - self.bottom).max(1);
        Rectangle::new((self.left, self.top).into(), (w, h).into())
    }

    /// Non-uniform scale taking the full framebuffer onto [`Overscan::inset`].
    /// Non-uniform on purpose: a TV's horizontal and vertical crop are set by
    /// separate parts of its scaler and are routinely different.
    pub fn scale(&self, size: Size<i32, Physical>) -> (f64, f64) {
        let r = self.inset(size);
        (
            r.size.w as f64 / (size.w.max(1) as f64),
            r.size.h as f64 / (size.h.max(1) as f64),
        )
    }

    /// Map a point in *panel* coordinates back to compositor-output
    /// coordinates — the inverse of what the render path does, needed so an
    /// absolute pointer (touchscreen, tablet, VM) still lands under the finger.
    /// Points inside the black margins clamp to the nearest desktop edge.
    pub fn untransform(&self, p: Point<f64, Physical>, size: Size<i32, Physical>) -> Point<f64, Physical> {
        let (sx, sy) = self.scale(size);
        let x = (p.x - self.left as f64) / sx;
        let y = (p.y - self.top as f64) / sy;
        (x.clamp(0.0, size.w as f64), y.clamp(0.0, size.h as f64)).into()
    }

    /// The same inverse in the normalised 0..1 space that
    /// `AbsolutePositionEvent::position_transformed` works in, which is where
    /// the input path actually needs it.
    pub fn untransform_unit(&self, p: Point<f64, Logical>, size: Size<i32, Physical>) -> Point<f64, Logical> {
        let phys = (p.x * size.w as f64, p.y * size.h as f64).into();
        let back = self.untransform(phys, size);
        (back.x / size.w.max(1) as f64, back.y / size.h.max(1) as f64).into()
    }

    /// Step one edge. `outward` grows the desktop back towards the panel edge.
    pub fn nudge(&mut self, edge: Edge, step: i32, outward: bool) {
        let d = if outward { -step } else { step };
        let slot = match edge {
            Edge::Top => &mut self.top,
            Edge::Bottom => &mut self.bottom,
            Edge::Left => &mut self.left,
            Edge::Right => &mut self.right,
        };
        *slot = (*slot + d).max(0);
    }

    /// Step all four edges at once.
    pub fn nudge_all(&mut self, step: i32, outward: bool) {
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            self.nudge(edge, step, outward);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fhd() -> Size<i32, Physical> {
        Size::from((1920, 1080))
    }

    #[test]
    fn zero_is_identity() {
        let o = Overscan::default();
        assert!(o.is_zero());
        assert_eq!(o.inset(fhd()), Rectangle::new((0, 0).into(), (1920, 1080).into()));
        assert_eq!(o.scale(fhd()), (1.0, 1.0));
    }

    #[test]
    fn inset_and_scale_agree() {
        let o = Overscan {
            top: 20,
            bottom: 20,
            left: 40,
            right: 40,
        };
        let r = o.inset(fhd());
        assert_eq!(r.loc, Point::from((40, 20)));
        assert_eq!(r.size, Size::from((1840, 1040)));
        let (sx, sy) = o.scale(fhd());
        assert!((sx - 1840.0 / 1920.0).abs() < 1e-9);
        assert!((sy - 1040.0 / 1080.0).abs() < 1e-9);
    }

    #[test]
    fn clamping_bounds_each_axis() {
        let o = Overscan::uniform(10_000).clamped(fhd());
        assert_eq!(o.left, 480);
        assert_eq!(o.top, 270);
        assert_eq!(Overscan::uniform(-5).clamped(fhd()), Overscan::default());
    }

    #[test]
    fn untransform_is_the_inverse_of_the_render_map() {
        let o = Overscan {
            top: 20,
            bottom: 20,
            left: 40,
            right: 40,
        };
        // A point at the desktop's own centre must come back as the centre.
        let (sx, sy) = o.scale(fhd());
        let painted = Point::from((960.0 * sx + 40.0, 540.0 * sy + 20.0));
        let back = o.untransform(painted, fhd());
        assert!((back.x - 960.0).abs() < 1e-6);
        assert!((back.y - 540.0).abs() < 1e-6);
    }

    #[test]
    fn margins_clamp_into_the_desktop() {
        let o = Overscan::uniform(50);
        let back = o.untransform((0.0, 0.0).into(), fhd());
        assert_eq!(back, Point::from((0.0, 0.0)));
        let back = o.untransform((1920.0, 1080.0).into(), fhd());
        assert_eq!(back, Point::from((1920.0, 1080.0)));
    }

    #[test]
    fn nudging_never_goes_negative() {
        let mut o = Overscan::default();
        o.nudge(Edge::Top, 10, true);
        assert_eq!(o.top, 0);
        o.nudge(Edge::Top, 10, false);
        assert_eq!(o.top, 10);
        o.nudge_all(5, true);
        assert_eq!(
            o,
            Overscan {
                top: 5,
                ..Default::default()
            }
        );
    }
}
