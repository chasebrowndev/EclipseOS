// SPDX-License-Identifier: AGPL-3.0-only
//! Tracks, and the per-window [`Transform`] they compose into (COMP-02 §9).
//!
//! A track is one leg run over a set of [`Channels`]: offset, scale and alpha,
//! each a displacement from the identity. A window has at most one track per
//! kind (move, open) plus a focus crossfade; its transform is the identity
//! with every running track's displacement applied. Scale is applied about the
//! window's centre, so a centre-anchored offset plus a scale is exactly a
//! rectangle morphing from one place and size to another.
//!
//! [`ScaledElement`] is how a scale reaches the screen. smithay sizes a
//! surface element from the scale the damage tracker hands it at draw time —
//! the output's — so the scale passed to `render_elements` moves subsurfaces
//! and popups but never resizes the buffer. The wrapper rescales an element's
//! geometry about the window centre and, when the window is rounded, carries
//! the rounded-corner mask too, so a scaled window is one variant rather than
//! a scaled × rounded × cropped product.

use std::time::Instant;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::{
    Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Transform as BufferTransform,
};

use super::curve::Leg;

/// One displacement per animated channel. Zero is the identity.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Channels {
    /// Offset, logical px.
    pub dx: f64,
    pub dy: f64,
    /// Scale minus one, per axis: −0.08 draws at 0.92.
    pub sx: f64,
    pub sy: f64,
    /// Alpha minus one: −1 is transparent.
    pub alpha: f64,
}

impl Channels {
    pub const ZERO: Channels = Channels {
        dx: 0.0,
        dy: 0.0,
        sx: 0.0,
        sy: 0.0,
        alpha: 0.0,
    };

    fn map(self, other: Channels, f: impl Fn(f64, f64) -> f64) -> Channels {
        Channels {
            dx: f(self.dx, other.dx),
            dy: f(self.dy, other.dy),
            sx: f(self.sx, other.sx),
            sy: f(self.sy, other.sy),
            alpha: f(self.alpha, other.alpha),
        }
    }

    pub fn is_zero(&self) -> bool {
        *self == Channels::ZERO
    }
}

impl std::ops::Neg for Channels {
    type Output = Channels;
    fn neg(self) -> Channels {
        self.map(Channels::ZERO, |a, _| -a)
    }
}

/// One leg over a set of channels: from `from` (released with `velocity`)
/// to `end`. An arriving track ends at zero — the window at its target;
/// a leaving one (a ghost) starts at zero and ends displaced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Track {
    pub leg: Leg,
    pub end: Channels,
    /// Displacement from `end` at the leg's start.
    pub from: Channels,
    /// Velocity at the leg's start, per second.
    pub velocity: Channels,
}

impl Track {
    /// Arrive at the identity from `from`.
    pub fn arrive(leg: Leg, from: Channels) -> Self {
        Self {
            leg,
            end: Channels::ZERO,
            from,
            velocity: Channels::ZERO,
        }
    }

    /// Leave the identity for `end`.
    pub fn leave(leg: Leg, end: Channels) -> Self {
        Self {
            leg,
            end,
            from: -end,
            velocity: Channels::ZERO,
        }
    }

    /// Displacement and velocity at `now`.
    pub fn at(&self, now: Instant) -> (Channels, Channels) {
        let r = self.leg.response(now);
        let pos = self.from.map(self.velocity, |d, v| r.pos(d, v));
        let vel = self.from.map(self.velocity, |d, v| r.vel(d, v));
        (pos.map(self.end, |p, e| p + e), vel)
    }

    pub fn done(&self, now: Instant) -> bool {
        self.leg.done(now)
    }

    /// The target moved mid-flight: carry on from exactly where the track is
    /// drawn, at the speed it is moving, under a new leg. `shift` is how far
    /// the old target sits from the new one (offset channels); `ratio` is old
    /// size over new (scale channels), since a scale displacement is relative
    /// to the target size it was measured against.
    pub fn retarget(&mut self, leg: Leg, shift: (f64, f64), ratio: (f64, f64), now: Instant) {
        let (pos, vel) = self.at(now);
        self.from = Channels {
            dx: pos.dx + shift.0,
            dy: pos.dy + shift.1,
            sx: (1.0 + pos.sx) * ratio.0 - 1.0,
            sy: (1.0 + pos.sy) * ratio.1 - 1.0,
            alpha: pos.alpha,
        };
        self.velocity = Channels {
            sx: vel.sx * ratio.0,
            sy: vel.sy * ratio.1,
            ..vel
        };
        self.end = Channels::ZERO;
        self.leg = leg;
    }
}

/// How to draw one window this frame, relative to its target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// Offset from the target, logical px.
    pub offset: Point<f64, Logical>,
    /// Scale about the window centre, per axis.
    pub scale: (f64, f64),
    pub alpha: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self::identity()
    }
}

/// The smallest scale drawn: a ringing `bounce` must not invert a window.
const MIN_SCALE: f64 = 0.01;

impl Transform {
    /// Drawn at the target, full size, opaque.
    pub fn identity() -> Transform {
        Transform {
            offset: (0.0, 0.0).into(),
            scale: (1.0, 1.0),
            alpha: 1.0,
        }
    }

    /// This transform with one more track's displacement applied: offsets
    /// add, scales and alphas multiply.
    pub fn compose(self, d: Channels) -> Transform {
        Transform {
            offset: (self.offset.x + d.dx, self.offset.y + d.dy).into(),
            scale: (
                (self.scale.0 * (1.0 + d.sx)).max(MIN_SCALE),
                (self.scale.1 * (1.0 + d.sy)).max(MIN_SCALE),
            ),
            alpha: (self.alpha * (1.0 + d.alpha).clamp(0.0, 1.0) as f32).clamp(0.0, 1.0),
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Transform::identity()
    }

    /// True when the window has to be drawn through [`ScaledElement`].
    pub fn scaled(&self) -> bool {
        self.scale != (1.0, 1.0)
    }

    /// The offset rounded to whole logical px, as the unscaled path draws it.
    pub fn loc(&self) -> Point<i32, Logical> {
        (self.offset.x.round() as i32, self.offset.y.round() as i32).into()
    }

    /// `r` (global logical, at the target) as drawn: moved by the offset, then
    /// scaled about `pivot` (global logical, after the offset). With no scale
    /// this is exactly `r` moved by [`Transform::loc`].
    pub fn map(&self, r: Rectangle<i32, Logical>, pivot: Point<f64, Logical>) -> Rectangle<i32, Logical> {
        let off = self.loc();
        if !self.scaled() {
            return Rectangle::new(r.loc + off, r.size);
        }
        let edge = |e: i32, o: i32, p: f64, s: f64| (p + ((e + o) as f64 - p) * s).round() as i32;
        let x0 = edge(r.loc.x, off.x, pivot.x, self.scale.0);
        let y0 = edge(r.loc.y, off.y, pivot.y, self.scale.1);
        let x1 = edge(r.loc.x + r.size.w, off.x, pivot.x, self.scale.0);
        let y1 = edge(r.loc.y + r.size.h, off.y, pivot.y, self.scale.1);
        Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into())
    }
}

/// The centre of `r` moved by `t`'s offset: the pivot a window scales about.
pub fn pivot(t: &Transform, r: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    let off = t.loc();
    (
        (r.loc.x + off.x) as f64 + r.size.w as f64 / 2.0,
        (r.loc.y + off.y) as f64 + r.size.h as f64 / 2.0,
    )
        .into()
}

/// `r` scaled by `scale` about `origin`, physical. Shared by the element and
/// the opaque-region estimate the blur gate reads.
pub fn scale_about(
    r: Rectangle<i32, Physical>,
    origin: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let mut r = r;
    r.loc -= origin;
    let mut r = r.to_f64().upscale(scale).to_i32_round();
    r.loc += origin;
    r
}

/// A window surface drawn at an animation scale, optionally through the
/// rounded-corner mask. See the module comment for why it exists.
///
/// Like `effects::RoundedElement`, it never offers its storage for scanout: a
/// plane cannot scale-and-mask, and an animating window is about to land on
/// the plain path anyway.
///
/// Generic so a [`super::Ghost::Snapshot`]'s texture elements scale the same
/// way as a live surface.
#[derive(Debug)]
pub struct ScaledElement<E = WaylandSurfaceRenderElement<GlesRenderer>> {
    inner: E,
    origin: Point<i32, Physical>,
    scale: Scale<f64>,
    mask: Option<(GlesTexProgram, Vec<Uniform<'static>>)>,
}

impl<E> ScaledElement<E> {
    pub fn new(
        inner: E,
        origin: Point<i32, Physical>,
        scale: Scale<f64>,
        mask: Option<(GlesTexProgram, Vec<Uniform<'static>>)>,
    ) -> Self {
        Self {
            inner,
            origin,
            scale,
            mask,
        }
    }
}

impl<E: Element> Element for ScaledElement<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn transform(&self) -> BufferTransform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        scale_about(self.inner.geometry(scale), self.origin, self.scale)
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        // Damage is element-relative, so it only scales.
        self.inner
            .damage_since(scale, commit)
            .into_iter()
            .map(|rect| rect.to_f64().upscale(self.scale).to_i32_up())
            .collect()
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.mask.is_some() {
            // Rounded corners are not opaque.
            return OpaqueRegions::default();
        }
        self.inner
            .opaque_regions(scale)
            .into_iter()
            .map(|rect| rect.to_f64().upscale(self.scale).to_i32_round())
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for ScaledElement<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let Some((program, uniforms)) = &self.mask else {
            return RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        };
        frame.override_default_tex_program(program.clone(), uniforms.clone());
        let res = RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
        frame.clear_tex_program_override();
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::curve::Curve;
    use std::time::Duration;

    #[test]
    fn identity_maps_a_rect_by_its_offset_only() {
        let r = Rectangle::new((100, 50).into(), (300, 200).into());
        let t = Transform::identity().compose(Channels {
            dx: 10.4,
            dy: -3.6,
            ..Channels::ZERO
        });
        assert!(!t.scaled());
        assert_eq!(
            t.map(r, pivot(&t, r)),
            Rectangle::new((110, 46).into(), (300, 200).into())
        );
    }

    #[test]
    fn scale_is_about_the_centre() {
        let r = Rectangle::new((100, 100).into(), (200, 100).into());
        let t = Transform::identity().compose(Channels {
            sx: -0.5,
            sy: -0.5,
            ..Channels::ZERO
        });
        assert_eq!(
            t.map(r, pivot(&t, r)),
            Rectangle::new((150, 125).into(), (100, 50).into())
        );
    }

    #[test]
    fn alpha_and_scale_stay_in_range_under_a_ringing_curve() {
        let t = Transform::identity().compose(Channels {
            sx: -2.0,
            alpha: 0.4,
            ..Channels::ZERO
        });
        assert!(t.scale.0 > 0.0);
        assert!(t.alpha <= 1.0);
        let t = Transform::identity().compose(Channels {
            alpha: -1.5,
            ..Channels::ZERO
        });
        assert_eq!(t.alpha, 0.0);
    }

    #[test]
    fn a_leaving_track_runs_from_identity_to_its_end() {
        let now = Instant::now();
        let end = Channels {
            dx: -1920.0,
            ..Channels::ZERO
        };
        let track = Track::leave(Leg::new(now, 200, Curve::EaseOut), end);
        assert!(track.at(now).0.is_zero());
        let (late, _) = track.at(now + Duration::from_millis(250));
        assert_eq!(late, end);
    }

    #[test]
    fn a_retarget_keeps_the_drawn_rect() {
        // Morph from 400×300 at (0,0) to 200×100 at (500,500), then retarget
        // mid-flight to 100×100 at (800,100): what is drawn does not jump.
        let now = Instant::now();
        let draw = |track: &Track, at: Instant, target: Rectangle<i32, Logical>| {
            let t = Transform::identity().compose(track.at(at).0);
            t.map(target, pivot(&t, target))
        };
        let a = Rectangle::<i32, Logical>::new((500, 500).into(), (200, 100).into());
        let mut track = Track::arrive(
            Leg::new(now, 220, Curve::Spring),
            Channels {
                dx: 200.0 - 600.0,
                dy: 150.0 - 550.0,
                sx: 400.0 / 200.0 - 1.0,
                sy: 300.0 / 100.0 - 1.0,
                alpha: 0.0,
            },
        );
        assert_eq!(
            draw(&track, now, a),
            Rectangle::new((0, 0).into(), (400, 300).into())
        );
        let mid = now + Duration::from_millis(70);
        let before = draw(&track, mid, a);
        let b = Rectangle::<i32, Logical>::new((800, 100).into(), (100, 100).into());
        track.retarget(
            Leg::new(mid, 220, Curve::Spring),
            (600.0 - 850.0, 550.0 - 150.0),
            (200.0 / 100.0, 100.0 / 100.0),
            mid,
        );
        let after = draw(&track, mid, b);
        assert!((before.loc.x - after.loc.x).abs() <= 1, "{before:?} vs {after:?}");
        assert!((before.loc.y - after.loc.y).abs() <= 1);
        assert!((before.size.w - after.size.w).abs() <= 1);
        assert!((before.size.h - after.size.h).abs() <= 1);
    }
}
