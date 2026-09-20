// SPDX-License-Identifier: AGPL-3.0-only
//! Keeping empty rectangles out of the damage the DRM backend sends the kernel
//! (COMP-02 §4).
//!
//! `DrmCompositor` turns the output damage into a `FB_DAMAGE_CLIPS` property
//! without looking at it, and smithay's damage tracker only clamps rectangles
//! to the output — a zero-height rectangle in the middle of the screen survives
//! both. The kernel then refuses the *whole* atomic commit
//! (`invalid damage clip x y x' y`, EINVAL), the frame never reaches the panel,
//! and the backend retries forever. It showed up on a workspace holding a
//! Firefox window, whose surface tree can carry a zero-height piece.
//!
//! An empty rectangle reaches the tracker three ways, and each has a guard
//! here:
//!
//! * an element reports one from [`Element::damage_since`] — [`Sanitized`]
//!   filters it out;
//! * an element's own geometry is empty, which the tracker uses as damage when
//!   the element appears or moves — [`has_area`] lets the caller drop it, and
//!   an element with no area draws nothing anyway; and
//! * an element reports one from [`Element::opaque_regions`]. The tracker
//!   carries those to the next frame and damages whatever stopped being
//!   opaque, which puts the rectangle back into the damage it reports. An
//!   empty region hides nothing, so dropping it changes no occlusion.
//!
//! These cover what an element hands the tracker, which is all a compositor
//! controls: the tracker composes its own rectangles too, and only clamps them
//! to the output, where a degenerate one inside the output survives
//! ([`Rectangle::overlaps`] is true for a zero-height rectangle within a taller
//! one). [`crate::backend::drm`] handles that residue by redrawing in full
//! after a refused commit, rather than resubmitting the rejected blob.
//!
//! [`Sanitized`] changes nothing else: every other method delegates, including
//! `underlying_storage`, so plane assignment and direct scanout still see the
//! wrapped element exactly as they did.

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Renderer,
    },
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Transform},
};

/// An element that covers at least one pixel at `scale`.
pub fn has_area<E: Element>(element: &E, scale: Scale<f64>) -> bool {
    !element.geometry(scale).is_empty()
}

/// Wraps an element so its damage never contains an empty rectangle.
#[derive(Debug)]
pub struct Sanitized<E>(E);

impl<E> Sanitized<E> {
    pub fn new(element: E) -> Self {
        Self(element)
    }
}

impl<E: Element> Element for Sanitized<E> {
    fn id(&self) -> &Id {
        self.0.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.0.current_commit()
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.0.location(scale)
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.0.src()
    }

    fn transform(&self) -> Transform {
        self.0.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.0.geometry(scale)
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.0
            .damage_since(scale, commit)
            .into_iter()
            .filter(|rect| !rect.is_empty())
            .collect()
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.0
            .opaque_regions(scale)
            .into_iter()
            .filter(|rect| !rect.is_empty())
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.0.alpha()
    }

    fn kind(&self) -> Kind {
        self.0.kind()
    }
}

impl<R: Renderer, E: RenderElement<R>> RenderElement<R> for Sanitized<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        self.0.draw(frame, src, dst, damage, opaque_regions)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer)
    }
}

#[cfg(test)]
mod tests {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;

    use super::*;

    const WHITE: [f32; 4] = [1.0; 4];

    fn solid(x: i32, y: i32, w: i32, h: i32) -> SolidColorRenderElement {
        SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new((x, y).into(), (w, h).into()),
            CommitCounter::default(),
            WHITE,
            Kind::Unspecified,
        )
    }

    fn one() -> Scale<f64> {
        Scale::from(1.0)
    }

    #[test]
    fn a_zero_height_element_damages_an_empty_rect_and_the_wrapper_removes_it() {
        // The shape the kernel rejected: 2800 wide, no height.
        let bare = solid(40, 298, 2800, 0);
        assert!(
            bare.damage_since(one(), None).iter().any(|r| r.is_empty()),
            "the unwrapped element is the source of the empty rectangle"
        );
        let wrapped = Sanitized::new(bare);
        assert!(wrapped.damage_since(one(), None).is_empty());
    }

    #[test]
    fn real_damage_passes_through_untouched() {
        let wrapped = Sanitized::new(solid(10, 20, 300, 4));
        let damage = wrapped.damage_since(one(), None);
        assert_eq!(damage.len(), 1);
        assert_eq!(damage[0], Rectangle::from_size((300, 4).into()));
    }

    #[test]
    fn an_empty_opaque_region_is_dropped_and_a_real_one_kept() {
        // The tracker damages whatever stopped being opaque, so an empty
        // region here comes back as an empty damage rectangle a frame later.
        let flat = Sanitized::new(solid(40, 298, 2800, 0));
        assert!(flat.opaque_regions(one()).is_empty());
        let real = Sanitized::new(solid(40, 298, 2800, 4));
        assert_eq!(real.opaque_regions(one()).len(), 1);
    }

    #[test]
    fn everything_but_damage_is_delegated() {
        let inner = solid(7, 9, 50, 60);
        let (id, geo, commit) = (inner.id().clone(), inner.geometry(one()), inner.current_commit());
        let wrapped = Sanitized::new(inner);
        assert_eq!(wrapped.id(), &id);
        assert_eq!(wrapped.geometry(one()), geo);
        assert_eq!(wrapped.current_commit(), commit);
        assert_eq!(wrapped.kind(), Kind::Unspecified);
    }

    #[test]
    fn has_area_rejects_a_line_and_keeps_a_box() {
        assert!(!has_area(&solid(0, 0, 2800, 0), one()));
        assert!(!has_area(&solid(0, 0, 0, 40), one()));
        assert!(has_area(&solid(0, 0, 2800, 1), one()));
    }
}
