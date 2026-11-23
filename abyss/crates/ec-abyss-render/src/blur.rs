// SPDX-License-Identifier: AGPL-3.0-only
//! Background blur behind translucent windows (COMP-02 §9).
//!
//! Dual-Kawase: the backdrop (everything the frame draws *below* a blurred
//! window) is rendered once into an offscreen texture, downsampled `passes`
//! times and upsampled back, and the result is inserted into the element list
//! directly beneath the window that asked for it. The window's own surfaces are
//! drawn on top unchanged, so what shows through their alpha is the blurred
//! backdrop.
//!
//! Two rules from the spec drive the shape of this module:
//!
//! * **Skipped entirely when the blurred surface is opaque.** A window with no
//!   translucency samples nothing, so it gets no backdrop pass at all. This is
//!   what keeps blur free on a normal desktop rather than a per-frame cost.
//! * **Blur invalidation** (COMP-02 §3): a blurred surface samples what is
//!   behind it, so damage behind a blurred region expands to cover the blurred
//!   region plus the blur kernel radius. [`invalidates`] is that rule, and
//!   [`BlurElement`] carries it into the output damage tracker by reporting its
//!   whole geometry as damaged on any frame where the backdrop under it moved.
//!
//! Blur also makes the window a non-candidate for direct scanout: the pixels
//! under it are composited by us, so the buffer can never go straight to a
//! plane. See [`crate::scanout_candidate`].

use std::collections::HashMap;
use std::sync::Arc;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{
                texture::TextureRenderElement, utils::CropRenderElement, Element, Id, Kind, RenderElement,
                UnderlyingStorage,
            },
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
                UniformType,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions},
            Bind, ContextId, Frame, Offscreen, Renderer, Texture,
        },
    },
    desktop::{LayerSurface, Window},
    output::Output,
    reexports::wayland_server::DisplayHandle,
    utils::{Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::compositor::{Cacheable, RectangleKind, RegionAttributes},
};

use crate::effects::{FbMap, GlassPreset};
use crate::glass_profile::{GlassProfile, Rebuild};
use crate::gpu_timer::Span;
use ec_abyss_config::Blur;

/// The offscreen chain is rendered in the same format the capture path uses.
const FORMAT: Fourcc = Fourcc::Xbgr8888;

/// Clear colour for the backdrop pass. Opaque black: the backdrop is a full
/// image and every visible pixel is overdrawn by an element.
const CLEAR: smithay::backend::renderer::Color32F =
    smithay::backend::renderer::Color32F::new(0.0, 0.0, 0.0, 1.0);

/// Effective reach of the kernel, in physical pixels.
///
/// Each downsample halves the image, so a fixed sampling `offset` at level `n`
/// reaches `2^n` full-resolution pixels. This is deliberately the conservative
/// upper bound rather than the exact Gaussian-equivalent radius: over-expanding
/// damage costs a few pixels of redraw, under-expanding leaves stale garbage.
pub fn kernel_radius(blur: &Blur) -> i32 {
    let passes = blur.passes.clamp(1, 6) as u32;
    blur.size.clamp(1, 64).saturating_mul(1 << passes)
}

/// Grow `rect` by `radius` on every side, saturating.
pub fn grow(rect: Rectangle<i32, Physical>, radius: i32) -> Rectangle<i32, Physical> {
    Rectangle::new(
        (
            rect.loc.x.saturating_sub(radius),
            rect.loc.y.saturating_sub(radius),
        )
            .into(),
        (
            rect.size.w.saturating_add(radius.saturating_mul(2)),
            rect.size.h.saturating_add(radius.saturating_mul(2)),
        )
            .into(),
    )
}

/// The part of the output a blur of `region` has to compute: `region` grown by
/// `radius` and clamped to the render space, as `(tex, phys)`. `phys` is that
/// rectangle in render-space physical pixels; `tex` is the same pixels in the
/// backdrop texture, which is `gl_FragCoord` space (the framebuffer, mode
/// size, see [`FbMap`]).
///
/// `tex` is snapped outwards to the chain's coarsest level (`1 << passes`
/// px), so every level samples the same pixel grid as an output-sized chain
/// would: a window dragged across the output does not swim, and an edge that
/// meets the output edge still clamps exactly where the full chain did.
/// `None` when nothing of the grown region is on the output.
pub fn crop_rects(
    region: Rectangle<i32, Physical>,
    radius: i32,
    fb: FbMap,
    passes: usize,
) -> Option<(Rectangle<i32, Physical>, Rectangle<i32, Physical>)> {
    let want = grow(region, radius).intersection(Rectangle::from_size(fb.render_size()))?;
    if want.is_empty() {
        return None;
    }
    let want = fb.gl_rect(want);
    let d = 1i32 << passes.min(6);
    let snap = |lo: i32, len: i32, max: i32| {
        let a = lo.div_euclid(d) * d;
        let b = if lo + len >= max {
            max
        } else {
            ((lo + len + d - 1).div_euclid(d) * d).min(max)
        };
        (a, b - a)
    };
    let (x, w) = snap(want.loc.x, want.size.w, fb.fb.w);
    let (y, h) = snap(want.loc.y, want.size.h, fb.fb.h);
    let tex = Rectangle::new((x, y).into(), (w, h).into());
    Some((tex, fb.phys_rect(tex)))
}

/// The blur invalidation rule (COMP-02 §3).
///
/// `region` is a blurred region in output-local physical pixels and `damage` is
/// the damage of everything behind it. Any damage landing within `radius` of the
/// region changes pixels the blur samples, so the *whole* region has to be
/// recomputed and repainted — blur is not a local operation, which is precisely
/// why damage cannot be assumed local once it is on.
pub fn invalidates(
    region: Rectangle<i32, Physical>,
    radius: i32,
    damage: &[Rectangle<i32, Physical>],
) -> bool {
    let sampled = grow(region, radius);
    damage.iter().any(|d| sampled.overlaps(*d))
}

/// The four strips of a `size` region that a glass bezel of width `inner`
/// can show through over an opaque window, relative to the region's origin:
/// top and bottom deep enough to hold the window's corners (`corner` is its
/// physical radius), left and right `inner` wide, each with a pixel of margin
/// for the window's antialiased edge. A titlebar `bar` px deep (from the
/// region's top; 0 for none) is glass whatever the window is, so the top strip
/// reaches down over it. Disjoint, and clamped to `size`.
pub fn ring_strips(
    size: Size<i32, Physical>,
    inner: i32,
    corner: i32,
    bar: i32,
) -> [Rectangle<i32, Physical>; 4] {
    let (w, h) = (size.w.max(0), size.h.max(0));
    let base = (inner + corner.max(0) + 1).clamp(0, h / 2);
    let top = base.max(bar).clamp(0, h);
    let t = base.min(h - top);
    let side = (inner + 1).clamp(0, w / 2);
    let mid = h - top - t;
    [
        Rectangle::new((0, 0).into(), (w, top).into()),
        Rectangle::new((0, h - t).into(), (w, t).into()),
        Rectangle::new((0, top).into(), (side, mid).into()),
        Rectangle::new((w - side, top).into(), (side, mid).into()),
    ]
}

/// A blurred backdrop, drawn immediately beneath the window it belongs to.
///
/// Drawing delegates to a plain [`TextureRenderElement`]; placement and damage
/// do not. The element sits on exactly `region`, the physical rectangle the
/// caller asked to blur, not on the inner element's geometry: that one is
/// rebuilt from a *logical* size re-rounded at the live scale, which at a
/// fractional scale lands a pixel off (a 5 px region at 1.9 becomes 6), so
/// the backdrop was stretched and painted a row past its surface. The
/// element's damage is *not* the texture's own damage: it is the invalidation
/// rule above, folded into a commit counter that the caller bumps whenever the
/// backdrop was recomputed. `underlying_storage` is `None` so no backend ever
/// mistakes this for a client buffer it could scan out.
///
/// Under an opaque window's glass bezel (`ring`) only the ring's strips can
/// ever show, so only they are reported damaged and only they are drawn.
///
/// Generic over the texture only so the placement is testable without a GPU.
#[derive(Debug)]
pub struct BlurElement<T: Texture = GlesTexture> {
    inner: TextureRenderElement<T>,
    region: Rectangle<i32, Physical>,
    commit: CommitCounter,
    program: Option<GlesTexProgram>,
    /// The program's uniforms: shared with the slot that cached them, so a
    /// frame that changes nothing allocates nothing.
    uniforms: Option<Arc<[Uniform<'static>]>>,
    /// What the draw needs without going through `inner` (which cannot take a
    /// program and a uniform slice): the texture, its context and the fade.
    texture: T,
    context: ContextId<T>,
    alpha: f32,
    /// An opaque window's bezel strips (see [`ring_strips`]), element-local.
    ring: Option<[Rectangle<i32, Physical>; 4]>,
}

/// An empty uniform set (a slot that has drawn nothing yet).
fn no_uniforms() -> Arc<[Uniform<'static>]> {
    Arc::from(Vec::new())
}

/// Keep `look` equal to `fresh`: when it already is, nothing is allocated and
/// `false` comes back; when it is not, a new shared copy replaces it (once, on
/// the change) and `true` says the element's look changed.
fn refresh_look(look: &mut Arc<[Uniform<'static>]>, fresh: &[Uniform<'static>]) -> bool {
    if look[..] == *fresh {
        return false;
    }
    *look = Arc::from(fresh);
    true
}

thread_local! {
    /// Scratch for the damage clipped to an opaque bezel's ring, reused by every
    /// draw so the draw allocates nothing once it has grown.
    static RING_DAMAGE: std::cell::Cell<Vec<Rectangle<i32, Physical>>> = const { std::cell::Cell::new(Vec::new()) };
}

impl<T: Texture + Clone + 'static> BlurElement<T> {
    /// Place `texture` (buffer scale 1, upright, covering `crop` of the output
    /// at whatever resolution it was stored, e.g. half) so the element covers
    /// exactly `region` and samples exactly `region` of it, stretched.
    #[cfg(test)]
    fn placed(
        id: Id,
        context: ContextId<T>,
        texture: T,
        crop: Rectangle<i32, Physical>,
        region: Rectangle<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Self {
        Self::placed_in(id, context, texture, crop, region, scale, alpha, false)
    }

    /// [`BlurElement::placed`] for a `packed` texture: the blurred result in its
    /// left half, the lightly blurred level in its right (chromatic aberration),
    /// so the element addresses the left half only.
    #[allow(clippy::too_many_arguments)]
    fn placed_in(
        id: Id,
        context: ContextId<T>,
        texture: T,
        crop: Rectangle<i32, Physical>,
        region: Rectangle<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
        packed: bool,
    ) -> Self {
        // The result texture is built with a literal `texture_scale` of 1, so
        // `Element::src()` hands this rectangle straight to the GPU with no
        // further scaling. It must therefore already be in the texture's own
        // buffer space, i.e. `region` relative to `crop`, times the texture's
        // texels per output pixel (below 1 for a reduced-resolution result).
        let wide = if packed { 2.0 } else { 1.0 };
        let sx = texture.width() as f64 / (crop.size.w.max(1) as f64 * wide);
        let sy = texture.height() as f64 / crop.size.h.max(1) as f64;
        let src = Rectangle::<f64, smithay::utils::Logical>::new(
            (
                (region.loc.x - crop.loc.x) as f64 * sx,
                (region.loc.y - crop.loc.y) as f64 * sy,
            )
                .into(),
            (region.size.w as f64 * sx, region.size.h as f64 * sy).into(),
        );
        // The inner element still wants a logical footprint. It only feeds the
        // inner `geometry`, which this element never reports.
        let size = Size::<i32, smithay::utils::Logical>::from((
            (region.size.w as f64 / scale.x).round().max(1.0) as i32,
            (region.size.h as f64 / scale.y).round().max(1.0) as i32,
        ));
        let inner = TextureRenderElement::from_static_texture(
            id,
            context.clone(),
            region.loc.to_f64(),
            texture.clone(),
            1,
            Transform::Normal,
            // 1.0 for a surface; an annotation fading in or out passes its
            // fade (the damage tracker sees an alpha change on its own).
            (alpha < 1.0).then_some(alpha),
            Some(src),
            Some(size),
            None,
            Kind::Unspecified,
        );
        BlurElement {
            inner,
            region,
            commit: CommitCounter::default(),
            program: None,
            uniforms: None,
            texture,
            context,
            alpha,
            ring: None,
        }
    }
}

impl<T: Texture + Clone + 'static> Element for BlurElement<T> {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.region.loc
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.region
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            // Expanded damage: the whole blurred region, not the region behind
            // it that actually changed — or, under an opaque bezel, the ring.
            match &self.ring {
                Some(strips) => DamageSet::from_slice(strips),
                None => DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)]),
            }
        }
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for BlurElement<GlesTexture> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        // An opaque bezel draws nothing inside its ring: skip those pixels. The
        // clipped rectangles go through a reused scratch vector, not a fresh one.
        let mut scratch = RING_DAMAGE.take();
        let damage = match &self.ring {
            Some(strips) => {
                scratch.clear();
                scratch.extend(
                    damage
                        .iter()
                        .flat_map(|d| strips.iter().filter_map(move |s| s.intersection(*d))),
                );
                &scratch[..]
            }
            None => damage,
        };
        if damage.is_empty() {
            RING_DAMAGE.set(scratch);
            return Ok(());
        }
        let res = if let Some(program) = &self.program {
            if frame.context_id() != self.context {
                Ok(())
            } else {
                // The program and its uniforms go in by reference: the override
                // path would take a fresh `Vec` of them per draw.
                crate::gpu_timer::begin_in(frame, Span::Glass);
                let res = frame.render_texture_from_to(
                    &self.texture,
                    src,
                    dst,
                    damage,
                    opaque_regions,
                    Transform::Normal,
                    self.alpha,
                    Some(program),
                    self.uniforms.as_deref().unwrap_or(&[]),
                );
                crate::gpu_timer::end_in(frame);
                res
            }
        } else {
            RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions)
        };
        RING_DAMAGE.set(scratch);
        res
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// Does a surface covering `region` show anything through it?
///
/// COMP-02 §9: blur is "skipped entirely when the blurred surface is opaque".
/// A client declares its opacity through the surface's opaque region, so what
/// is left of `region` after subtracting `opaque` is exactly the glass. An
/// empty region is never blurred.
///
/// `radius` (physical px, 0 for square) is the corner radius the surface is
/// masked with. Toolkits such as GTK4 leave their rounded corners out of the
/// opaque region, and those corners are hidden by the mask anyway, so only
/// the cross the corners leave — `region` inset by `radius` horizontally,
/// then vertically — has to be covered.
pub fn shows_through(
    region: Rectangle<i32, Physical>,
    radius: i32,
    opaque: impl IntoIterator<Item = Rectangle<i32, Physical>>,
) -> bool {
    if region.is_empty() {
        return false;
    }
    // At least a 1 px strip is left, so a tiny surface still tests its middle.
    let r = radius.clamp(0, ((region.size.w.min(region.size.h) - 1) / 2).max(0));
    let cross = [
        Rectangle::new(
            (region.loc.x + r, region.loc.y).into(),
            (region.size.w - 2 * r, region.size.h).into(),
        ),
        Rectangle::new(
            (region.loc.x, region.loc.y + r).into(),
            (region.size.w, region.size.h - 2 * r).into(),
        ),
    ];
    let cross = cross.into_iter().filter(|c| !c.is_empty());
    !Rectangle::subtract_rects_many(cross, opaque).is_empty()
}

/// Most boxes a shaped backdrop unions.
pub const MAX_SHAPE: usize = 4;

/// A layer surface's glass cut to its input region (Vol 1 §5.2: blur behind
/// layer-shell follows the surface's own shape, not its bounding box).
///
/// A layer client that sets its input region to 2..=4 plain rectangles — a
/// bar pill with a panel hanging off it — gets its backdrop masked to the
/// union of those rectangles, each rounded by the surface's corner radius
/// and joined by a polynomial smooth-min of width `fillet`, so a concave
/// fillet forms wherever two boxes meet (see [`shape_sd`]). Everything else
/// (no input region, one rectangle, more than four, or any subtracted
/// rectangle) is not a shape and keeps the single rounded box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    /// Output-local physical rectangles; the first `len` are live.
    rects: [Rectangle<i32, Physical>; MAX_SHAPE],
    len: usize,
    /// Smooth-min width `k`, physical px (`bar.rounding` at the output scale).
    pub fillet: f32,
}

impl Shape {
    /// The shape of a layer surface whose input region is `region`, or `None`
    /// for the plain single-box path.
    ///
    /// `region` is surface-local and logical (`None` is the protocol's
    /// infinite region); `size` is the surface's logical size and `loc` its
    /// output-local physical origin. Each `Add` rectangle is clipped to the
    /// surface and converted edge by edge, so rectangles that touch in
    /// logical space still touch in physical space. Empty rectangles are
    /// ignored; a `Subtract` or a fifth non-empty rectangle means no shape.
    pub fn from_region(
        region: Option<&RegionAttributes>,
        size: Size<i32, Logical>,
        loc: Point<i32, Physical>,
        scale: Scale<f64>,
        fillet: f32,
    ) -> Option<Shape> {
        let region = region?;
        let bounds = Rectangle::<i32, Logical>::from_size(size);
        let mut rects = [Rectangle::default(); MAX_SHAPE];
        let mut len = 0;
        // Everything up to the last covering subtract is erased by it, so it
        // is skipped unread: iced_layershell shares one wl_region across a
        // process's surfaces, and an earlier subtract made at *another*
        // surface's size (a second output) must not poison this one.
        let start = region
            .rects
            .iter()
            .rposition(|(kind, rect)| matches!(kind, RectangleKind::Subtract) && rect.contains_rect(bounds))
            .map_or(0, |i| i + 1);
        for (kind, rect) in &region.rects[start..] {
            if matches!(kind, RectangleKind::Subtract) {
                // A partial subtract is not a shape.
                return None;
            }
            let Some(r) = rect.intersection(bounds).filter(|r| !r.is_empty()) else {
                continue;
            };
            if len == MAX_SHAPE {
                return None;
            }
            let x = |v: i32| loc.x + (v as f64 * scale.x).round() as i32;
            let y = |v: i32| loc.y + (v as f64 * scale.y).round() as i32;
            let (x0, y0) = (x(r.loc.x), y(r.loc.y));
            let (x1, y1) = (x(r.loc.x + r.size.w), y(r.loc.y + r.size.h));
            rects[len] = Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into());
            len += 1;
        }
        (len >= 2).then_some(Shape {
            rects,
            len,
            fillet: fillet.max(0.0),
        })
    }

    /// The live rectangles, output-local physical.
    pub fn rects(&self) -> &[Rectangle<i32, Physical>] {
        &self.rects[..self.len]
    }

    /// The smallest rectangle holding every box.
    pub fn bounds(&self) -> Option<Rectangle<i32, Physical>> {
        self.rects().iter().copied().reduce(|a, b| a.merge(b))
    }
}

/// A client's `ext_background_effect_v1` blur region, double-buffered on the
/// surface (the protocol handler writes `pending`; `wl_surface.commit` latches
/// it into `current`, which is all the renderer reads).
///
/// `None` is "no object": the compositor's own choice of backdrop stands.
/// `Some(region)` is a live object's committed region, surface-local and
/// logical; the protocol's initial value and a NULL `wl_region` are both the
/// empty region, which means no blur at all.
#[derive(Debug, Clone, Default)]
pub struct BackgroundEffectState {
    pub blur: Option<RegionAttributes>,
}

impl Cacheable for BackgroundEffectState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Where a client asked for blur, in output-local physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlurArea {
    /// Nothing is to be blurred (an empty region, or one wholly off the surface).
    Nothing,
    /// One box: the plain rounded-box backdrop, cut to it.
    Rect(Rectangle<i32, Physical>),
    /// 2..=4 boxes, unioned exactly (no fillet).
    Shape(Shape),
}

impl BlurArea {
    /// The box the backdrop texture is cut to: the box itself, or the
    /// bounds of the shape's boxes. `None` for [`BlurArea::Nothing`].
    pub fn bounds(&self) -> Option<Rectangle<i32, Physical>> {
        match self {
            BlurArea::Nothing => None,
            BlurArea::Rect(r) => Some(*r),
            BlurArea::Shape(s) => s.bounds(),
        }
    }
}

/// The backdrop area a committed background-effect `region` asks for.
///
/// `size` is the surface's logical size (the region is clipped to it) and `loc`
/// the surface origin, output-local physical. 2..=4 added boxes become a
/// [`Shape`] (fillet 0, so the union is exact). Anything [`Shape`] cannot
/// carry (one box, more than four, or a partial subtract) falls back to the
/// bounding box of the added boxes clipped to the surface, which over-blurs
/// in the last two cases; a subtract that covers the surface erases what came
/// before it, as in [`Shape::from_region`].
pub fn blur_area(
    region: &RegionAttributes,
    size: Size<i32, Logical>,
    loc: Point<i32, Physical>,
    scale: Scale<f64>,
) -> BlurArea {
    if let Some(shape) = Shape::from_region(Some(region), size, loc, scale, 0.0) {
        return BlurArea::Shape(shape);
    }
    let bounds = Rectangle::<i32, Logical>::from_size(size);
    let start = region
        .rects
        .iter()
        .rposition(|(kind, rect)| matches!(kind, RectangleKind::Subtract) && rect.contains_rect(bounds))
        .map_or(0, |i| i + 1);
    let hull = region.rects[start..]
        .iter()
        .filter(|(kind, _)| matches!(kind, RectangleKind::Add))
        .filter_map(|(_, rect)| rect.intersection(bounds).filter(|r| !r.is_empty()))
        .reduce(|a, b| a.merge(b));
    let Some(r) = hull else {
        return BlurArea::Nothing;
    };
    let x = |v: i32| loc.x + (v as f64 * scale.x).round() as i32;
    let y = |v: i32| loc.y + (v as f64 * scale.y).round() as i32;
    let (x0, y0) = (x(r.loc.x), y(r.loc.y));
    let (x1, y1) = (x(r.loc.x + r.size.w), y(r.loc.y + r.size.h));
    let out = Rectangle::new((x0, y0).into(), (x1 - x0, y1 - y0).into());
    if out.is_empty() {
        BlurArea::Nothing
    } else {
        BlurArea::Rect(out)
    }
}

/// Signed distance from `p` (output-local physical px) to `shape`, negative
/// inside: each box rounded by `radius` (clamped to its half size), unioned
/// in order by [`join`] with `k = shape.fillet`.
///
/// This is the mask the shaped backdrop programs draw (`SHAPE_FNS` in
/// [`crate::effects`], line for line). A client that paints a fill
/// under the same shape uses the same formula so its tint meets the blur's
/// edge.
pub fn shape_sd(shape: &Shape, radius: f32, p: (f32, f32)) -> f32 {
    // A box's signed distance, and the centre of the corner arc `p` is
    // nearest to (the nearest point of the box's inner rectangle, inset by
    // the radius). Two boxes that share a corner share that centre.
    let box_sd = |b: &Rectangle<i32, Physical>| {
        let (hx, hy) = (b.size.w as f32 * 0.5, b.size.h as f32 * 0.5);
        let r = radius.min(hx.min(hy)).max(0.0);
        let (x0, y0) = (b.loc.x as f32, b.loc.y as f32);
        let qx = (p.0 - (x0 + hx)).abs() - hx + r;
        let qy = (p.1 - (y0 + hy)).abs() - hy + r;
        let d = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
        let arc = (
            p.0.max(x0 + r).min(x0 + 2.0 * hx - r),
            p.1.max(y0 + r).min(y0 + 2.0 * hy - r),
        );
        (d, arc)
    };
    let k = shape.fillet;
    let mut rects = shape.rects().iter();
    let mut d = rects.next().map_or((f32::INFINITY, (0.0, 0.0)), box_sd);
    for b in rects {
        d = join(d, box_sd(b), k, radius);
    }
    d.0
}

/// The union of two boxes' `(distance, arc centre)` pairs, the polynomial
/// smooth-min `min(a, b) - gap · h² k / 4` with `h = max(k - |a - b|, 0) / k`.
///
/// The bite is the concave fillet, and it belongs only where the two boxes'
/// arcs differ. Where they share a centre (a panel flush with the pill's end
/// shares the pill's top-right corner) `gap` is 0 and the union keeps that
/// corner's own radius. `gap` is the centres' distance in units of the corner
/// `radius` (at least 1 px), capped at 1: a real join's centres sit about a
/// radius apart and get the full bite, a corner offset by a pixel gets almost
/// none. The GLSL `join` in `SHAPE_FNS` mirrors this line for line.
fn join(a: (f32, (f32, f32)), b: (f32, (f32, f32)), k: f32, radius: f32) -> (f32, (f32, f32)) {
    let m = if b.0 < a.0 { b } else { a };
    if k <= 0.0 {
        return m;
    }
    let h = (k - (a.0 - b.0).abs()).max(0.0) / k;
    let gap = ((a.1 .0 - b.1 .0).hypot(a.1 .1 - b.1 .1) / radius.max(1.0)).min(1.0);
    (m.0 - gap * h * h * k * 0.25, m.1)
}

/// [`shows_through`] for a shaped surface: only the shape's boxes can be
/// seen, so it shows through when any of them is left uncovered.
pub fn shape_shows_through(shape: &Shape, opaque: &[Rectangle<i32, Physical>]) -> bool {
    shape
        .rects()
        .iter()
        .any(|r| shows_through(*r, 0, opaque.iter().copied()))
}

/// What a cached backdrop belongs to.
///
/// Windows and layer-shell surfaces both get blurred (Vol 1 §5.2: "background
/// blur for transparent surfaces and layer-shell"), and both need their own
/// damage history, so they share one keyed table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlurKey {
    /// The shared pyramid of one depth tier of an output (Phase 1a, see
    /// [`crate::groups`]); only ever a key of the store's group table.
    Tier(u8),
    Window(Window),
    Layer(LayerSurface),
    /// An annotation panel (ADR 0071), by its handle. Annotations keep their
    /// own [`BlurStore`], so this never shares a table with a surface.
    Annotation(u64),
    /// A trusted prompt's backdrop (ADR 0128): the only key of its private
    /// store, never shared with a window's.
    Prompt,
    /// The `n`th popup of a window with a keyed strip (ADR 0079), glassed
    /// over its whole surface.
    Popup(Window, usize),
    /// A window's `eclipse_glass_v1` panes: one backdrop blurred over their
    /// union, kept apart from the window's own whole-window backdrop.
    Panes(Window),
    /// The panes of one subsurface of a window, blurred over everything
    /// composited under it, apart from the window's root-surface panes.
    SubPanes(
        Window,
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ),
    /// A layer surface's `eclipse_glass_v1` panes, kept apart from the
    /// layer's own shaped backdrop when it keeps one (the bar's sheet).
    LayerPanes(LayerSurface),
    /// The session locker's `eclipse_glass_v1` panes (ADR 0129): blurred over
    /// the wallpaper alone, apart from every other key.
    LockPanes(smithay::reexports::wayland_server::protocol::wl_surface::WlSurface),
}

/// What a tier's textures were made for: crop size, passes, output size.
type ChainShape = (Size<i32, Physical>, usize, Size<i32, Physical>);

/// One persistent shared pyramid (Phase 1a): the blur of the union of its
/// members' crops, drawn by each member from its own part of `result`.
struct GroupBlur {
    /// Damage of everything below the group's lowest member, scanned once a
    /// frame for all the members.
    damage: OutputDamageTracker,
    /// The shared half-resolution result.
    result: Option<GlesTexture>,
    /// The output rectangle `result` covers: the union of the member crops.
    crop: Option<Rectangle<i32, Physical>>,
    /// `(size, passes, packed)` the result was built with. `packed`: the result
    /// texture is twice as wide, the lightly blurred level beside the blurred one
    /// (chromatic aberration, only while `glass.dispersion` is above 0).
    params: (i32, usize, bool),
    /// The members it was built for, back to front.
    members: Vec<BlurKey>,
    /// Bumped on every rebuild; a member whose `seen` differs redraws.
    rebuilds: u64,
    /// Phase 1b: this tier's own persistent chain, so a partial rebuild only
    /// redoes the damaged part and the rest stays as the last build left it.
    /// The backdrop, output-sized, drawn over `crop` only.
    level0: Option<GlesTexture>,
    /// Renders the backdrop into `level0` with `age = 1` between rebuilds:
    /// its diff against the last build is the damage since then.
    render: Option<OutputDamageTracker>,
    /// Downsample results, levels `1..=passes`.
    down: Vec<GlesTexture>,
    /// Upsample results, levels `2..passes` (the top level is the
    /// downsample's own).
    up: Vec<GlesTexture>,
    /// `(crop size, passes, output size)` the textures above were made for.
    built: Option<ChainShape>,
    /// Damage landed while crowded and the rebuild was deferred.
    pending: bool,
    /// The store frame `result` was last rebuilt on.
    last: u64,
    /// The store frame the group was last asked for.
    seen: u64,
}

/// A grouped surface's element bookkeeping (its backdrop is the group's).
struct MemberSlot {
    id: Id,
    look: Arc<[Uniform<'static>]>,
    commit: CommitCounter,
    /// The group's `rebuilds` this slot last saw.
    seen: u64,
}

/// What a group's refresh needs of one member.
#[derive(Clone)]
pub struct GroupMember<'a> {
    pub key: &'a BlurKey,
    pub region: Rectangle<i32, Physical>,
    /// How far past `region` the final draw samples (glass refraction).
    pub reach: i32,
    /// An opaque bezel's `(width, corner radius, titlebar depth)`.
    pub ring: Option<(i32, i32, i32)>,
    pub shape: Option<&'a Shape>,
    /// Which glass program draws it: sets the pyramid's passes and packing.
    pub preset: GlassPreset,
}

/// A member's blur crop: `(texture space, output space)` (see [`crop_rects`]).
pub fn member_crop(
    region: Rectangle<i32, Physical>,
    blur: &Blur,
    reach: i32,
    fb: FbMap,
) -> Option<(Rectangle<i32, Physical>, Rectangle<i32, Physical>)> {
    let passes = blur.passes.clamp(1, 6) as usize;
    crop_rects(
        region,
        kernel_radius(blur).saturating_add(reach.max(0)),
        fb,
        passes,
    )
}

/// The texture-space twin of an output-space `crop` that is already snapped
/// (as a union of [`member_crop`] crops is): the same map [`crop_rects`] makes.
pub fn crop_tex(crop: Rectangle<i32, Physical>, fb: FbMap) -> Rectangle<i32, Physical> {
    fb.gl_rect(crop)
}

/// Sizes of a tier's built region are rounded up to this many pixels, so the
/// chains (keyed by crop size) are shared by every build of about that size
/// instead of one per exact size.
pub const CROP_BUCKET: i32 = 512;

/// How far past its members' crops a tier pyramid is built, so a member dragged
/// or resized inside the margin only moves its UV offset (Phase 1c). An eighth
/// of the longer side of the output, clamped to 128..=512 px: wide enough that
/// a drag rebuilds about once per 360 px of travel at 2880 px rather than every
/// step, narrow enough that a rebuild stays a few times a window's own crop
/// (the margin is paid in texels on every rebuild).
pub fn crop_margin(fb: Size<i32, Physical>) -> i32 {
    (fb.w.max(fb.h) / 8).clamp(128, 512)
}

/// The region a tier pyramid is built over for members needing `needed` (the
/// union of their snapped crops, output space): `needed` grown by the margin,
/// on the chain grid, its size rounded up to [`CROP_BUCKET`] and kept on the
/// output. Always contains `needed`.
pub fn padded_crop(needed: Rectangle<i32, Physical>, fb: FbMap, passes: usize) -> Rectangle<i32, Physical> {
    let Some((_, grown)) = crop_rects(needed, crop_margin(fb.render_size()), fb, passes) else {
        return needed;
    };
    let d = 1i32 << passes.min(6);
    // Round the length up to a bucket; past the output's edge slide back onto
    // the grid, keeping everything `grown` covered.
    let fit = |lo: i32, len: i32, max: i32| {
        let want = (((len + CROP_BUCKET - 1) / CROP_BUCKET) * CROP_BUCKET).min(max);
        if lo + want <= max {
            return (lo, want);
        }
        let a = (max - want).div_euclid(d) * d;
        (a, (a + want).max(lo + len) - a)
    };
    let render = fb.render_size();
    let (x, w) = fit(grown.loc.x, grown.size.w, render.w);
    let (y, h) = fit(grown.loc.y, grown.size.h, render.h);
    Rectangle::new((x, y).into(), (w, h).into())
}

/// Whether a surface of `preset` stores its result packed: the blurred result
/// with the lightly blurred level beside it, which the lens and regular
/// optics read at the rim and frost's chromatic fringe reads. Only glass mode
/// pays for it, and frost only with `dispersion` above 0.
pub fn packs(blur: &Blur, preset: GlassPreset) -> bool {
    blur.mode == ec_abyss_config::BlurMode::Glass
        && (preset != GlassPreset::Frost || blur.glass.dispersion > 0.0)
}

/// Blur passes a preset's interior is made with: a lens is a light blur (1 or
/// 2 passes), a regular frame 3, frost the configured count.
pub fn preset_passes(passes: i32, preset: GlassPreset) -> i32 {
    let cap = match preset {
        GlassPreset::Lens => 2,
        GlassPreset::Regular => 3,
        GlassPreset::Frost => 6,
    };
    passes.clamp(1, 6).min(cap)
}

/// `blur` with the preset's pass count (glass mode only; any other mode keeps
/// its own).
pub fn tuned(blur: &Blur, preset: GlassPreset) -> Blur {
    let mut b = blur.clone();
    if blur.mode == ec_abyss_config::BlurMode::Glass {
        b.passes = preset_passes(blur.passes, preset);
    }
    b
}

/// What one pyramid is built with for members of these presets: the most
/// passes any of them needs (a lens sharing a tier with a frost member shows
/// the frost interior), packed if any of them reads the sharp half.
pub fn tier_variant(blur: &Blur, presets: impl Iterator<Item = GlassPreset>) -> (Blur, bool) {
    let mut passes = 1;
    let mut sharp = false;
    let mut any = false;
    for p in presets {
        any = true;
        passes = passes.max(tuned(blur, p).passes);
        sharp |= packs(blur, p);
    }
    let mut b = blur.clone();
    if any {
        b.passes = passes;
    }
    (b, sharp)
}

/// Whether a tier must be rebuilt in full rather than from damage: nothing
/// built yet, its region no longer covers the members, the blur parameters (size,
/// passes, packed: so a runtime dispersion toggle allocates or drops the packed
/// texture) or the membership changed.
pub fn tier_fresh(
    has_result: bool,
    built: Option<Rectangle<i32, Physical>>,
    needed: Rectangle<i32, Physical>,
    old: (i32, usize, bool),
    new: (i32, usize, bool),
    same_members: bool,
) -> bool {
    !has_result || !covers(built, needed) || old != new || !same_members
}

/// Whether `region` is wholly hidden behind one opaque rectangle of `above`
/// (elements in front of it, as the damage tracker sees them). One rectangle,
/// not a union: a conservative test that never hides what could show, and costs
/// no allocation (smithay keeps an element's first few opaque regions inline).
pub fn hidden_behind<E: Element>(region: Rectangle<i32, Physical>, above: &[E], scale: Scale<f64>) -> bool {
    above.iter().any(|e| {
        let geo = e.geometry(scale);
        geo.contains_rect(region)
            && e.opaque_regions(scale)
                .iter()
                .any(|o| Rectangle::new(o.loc + geo.loc, o.size).contains_rect(region))
    })
}

/// Whether a pyramid built over `built` still serves members needing `needed`.
pub fn covers(built: Option<Rectangle<i32, Physical>>, needed: Rectangle<i32, Physical>) -> bool {
    built.is_some_and(|b| b.contains_rect(needed))
}

/// The invalidation test for one region: whole region, an opaque bezel's
/// strips (`region`-relative), or a shape's boxes grown by its fillet.
fn region_dirty(
    region: Rectangle<i32, Physical>,
    strips: Option<&[Rectangle<i32, Physical>; 4]>,
    shape: Option<&Shape>,
    radius: i32,
    rects: &[Rectangle<i32, Physical>],
) -> bool {
    match (strips, shape) {
        (Some(strips), _) => strips
            .iter()
            .any(|s| invalidates(Rectangle::new(s.loc + region.loc, s.size), radius, rects)),
        (None, Some(shape)) => {
            let radius = radius.saturating_add(shape.fillet.ceil() as i32);
            shape.rects().iter().any(|b| invalidates(*b, radius, rects))
        }
        (None, None) => invalidates(region, radius, rects),
    }
}

/// Per-surface blur bookkeeping, kept alive between frames.
struct SurfaceBlur {
    /// Damage of the backdrop behind this window, tracked across frames. Only
    /// ever queried (`damage_output`), never rendered through.
    damage: OutputDamageTracker,
    /// The upsampled result, held so the element can borrow it.
    result: Option<GlesTexture>,
    /// The output rectangle `result` covers (see [`crop_rects`]). A region
    /// that needs a different one recomputes even if nothing behind it moved.
    crop: Option<Rectangle<i32, Physical>>,
    /// Bumped whenever `result` was recomputed; drives [`BlurElement`] damage.
    commit: CommitCounter,
    /// A stable id so the damage tracker can follow this element across frames.
    id: Id,
    /// The final draw's uniforms last frame. Each program has its own uniform
    /// names (none for smithay's plain one), so this also stands for the
    /// program: a mode switch or a tint/rim change bumps `commit` even though
    /// the chain behind it did not change.
    look: Arc<[Uniform<'static>]>,
    /// `eclipse_glass_v1` panes drawn from `result`, by index in the pane list.
    panes: Vec<PaneSlot>,
    /// Damage landed while the store was crowded and the recompute was
    /// deferred; flushed once `last` is [`CROWDED_INTERVAL`] frames old.
    pending: bool,
    /// The store frame `result` was last recomputed on.
    last: u64,
    /// `(passes, packed)` `result` was built with: a preset change rebuilds
    /// once, in full.
    variant: (usize, bool),
}

/// What [`BlurStore::refresh`] hands back: the crop the texture covers, an
/// opaque bezel's strips and whether the result is packed.
type Refreshed = (
    Rectangle<i32, Physical>,
    Option<[Rectangle<i32, Physical>; 4]>,
    bool,
);

/// One glass pane's bookkeeping: its own element id, uniforms and commit
/// counter, so the damage tracker follows it across frames.
struct PaneSlot {
    id: Id,
    look: Arc<[Uniform<'static>]>,
    commit: CommitCounter,
    /// The backdrop's commit this pane last saw.
    seen: CommitCounter,
}

/// Compiled programs, the offscreen chain and per-window state.
#[derive(Default)]
pub struct BlurStore {
    down: Option<GlesTexProgram>,
    up: Option<GlesTexProgram>,
    /// The backdrop, output-sized; only the blurred region of it is drawn.
    level0: Option<GlesTexture>,
    /// Levels `1..=passes` sized for a crop (level `n` is half of `n-1`),
    /// kept per crop size so windows that alternate do not reallocate.
    chain: HashMap<(i32, i32, usize), Vec<GlesTexture>>,
    chain_size: Size<i32, Physical>,
    /// Renders the backdrop into level 0. Always driven with `age = 0`, so its
    /// own damage history is never consulted and it can be shared by every
    /// blurred window on the output.
    backdrop: Option<OutputDamageTracker>,
    surfaces: HashMap<BlurKey, SurfaceBlur>,
    /// Frames seen, bumped by [`BlurStore::retain`].
    frame: u64,
    /// First-time backdrops built this frame.
    started: usize,
    /// Blurred surfaces that asked this frame (`retain`'s keep list).
    live: usize,
    /// Shared pyramids (Phase 1a), by the key of their lowest member.
    groups: HashMap<BlurKey, GroupBlur>,
    /// Per-surface element state of the grouped surfaces.
    members: HashMap<BlurKey, MemberSlot>,
    /// A pane's uniforms while its element is built, kept between frames.
    pane_uniforms: Vec<Uniform<'static>>,
    /// Scratch of a partial rebuild (Phase 1b), kept between rebuilds.
    plan: crate::partial::Plan,
    damage: Vec<Rectangle<i32, Physical>>,
    /// Profiling counters (`EC_GLASS_PROFILE`).
    profile: GlassProfile,
}

impl std::fmt::Debug for BlurStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlurStore")
            .field("chains", &self.chain.len())
            .field("surfaces", &self.surfaces.len())
            .finish()
    }
}

impl BlurStore {
    /// Drop every cached backdrop whose surface is not in `keep`.
    ///
    /// Called once a frame with exactly the surfaces that asked for blur, so a
    /// window that closed — or simply stopped being translucent — does not
    /// leave a texture behind.
    pub fn retain(&mut self, keep: &[BlurKey]) {
        self.frame = self.frame.wrapping_add(1);
        self.started = 0;
        self.profile.frame(keep.len());
        self.live = keep.len();
        self.surfaces.retain(|key, _| keep.contains(key));
        self.members.retain(|key, _| keep.contains(key));
        // A group that nothing used last frame is gone.
        let now = self.frame;
        self.groups.retain(|_, g| g.seen.wrapping_add(1) >= now);
    }

    /// Count this frame's tiered and per-window surfaces for the profile.
    pub fn note_tiers(&mut self, tiered: usize, fallback: usize, culled: usize) {
        self.profile.tiers(tiered, fallback);
        self.profile.culled(culled);
    }

    /// Drop everything: called when blur is switched off so the textures do not
    /// sit in GPU memory for a config that no longer draws them.
    pub fn clear(&mut self) {
        self.chain.clear();
        self.level0 = None;
        self.chain_size = Size::default();
        self.surfaces.clear();
        self.groups.clear();
        self.members.clear();
        self.backdrop = None;
    }

    /// Build the blurred backdrop for one surface.
    ///
    /// `behind` is the slice of the frame's element list that sits below the
    /// surface, in front-to-back order — exactly what the blur samples.
    /// `reach` is how far past `region` the final draw samples on top of the
    /// kernel (the glass refraction; 0 for every other mode), so damage that
    /// far out invalidates too. `ring` is an opaque window's bezel, as
    /// `(width, corner radius, titlebar depth)` in physical px: only its strips are ever seen,
    /// so only damage near them invalidates, and only they report damage.
    /// `shape` is a shaped layer's boxes: only damage within reach of one of
    /// them (grown by the fillet, which can only fill in between them)
    /// invalidates. `alpha` fades the whole backdrop (1.0 but for an
    /// annotation panel fading in or out).
    /// Returns `None` when nothing needs redrawing *and* nothing is cached, or
    /// when any GL step failed (blur is an effect; a failure drops the effect,
    /// never the frame).
    #[allow(clippy::too_many_arguments)]
    pub fn element<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        key: &BlurKey,
        region: Rectangle<i32, Physical>,
        behind: &[E],
        blur: &Blur,
        scale: Scale<f64>,
        rounding: Option<(GlesTexProgram, &mut Vec<Uniform<'static>>)>,
        reach: i32,
        ring: Option<(i32, i32, i32)>,
        shape: Option<&Shape>,
        alpha: f32,
        preset: GlassPreset,
    ) -> Option<BlurElement>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        let fb = crate::effects::output_fb_map(output)?;
        let (crop, strips, packed) = self.refresh(
            renderer, output, fb, key, region, behind, blur, scale, reach, ring, shape, preset,
        )?;
        let entry = self.surfaces.get_mut(key)?;
        let mut none = Vec::new();
        let (program, uniforms) = match rounding {
            Some((program, uniforms)) => (Some(program), uniforms),
            None => (None, &mut none),
        };
        // The element samples `result`, which spans `crop`: one physical px is
        // 1/crop in its UV, not 1/output (and half that in x when packed).
        let wide = if packed { 2 } else { 1 };
        crate::effects::set_px_uv(uniforms, (crop.size.w * wide, crop.size.h));
        crate::effects::set_packed(uniforms, packed);
        if refresh_look(&mut entry.look, uniforms) {
            entry.commit.increment();
        }
        let texture = entry.result.clone()?;
        let mut element = BlurElement::placed_in(
            entry.id.clone(),
            renderer.context_id(),
            texture,
            crop,
            region,
            scale,
            alpha,
            packed,
        );
        element.commit = entry.commit;
        element.program = program;
        element.uniforms = Some(entry.look.clone());
        element.ring = strips;
        Some(element)
    }

    /// `eclipse_glass_v1`: blur the backdrop ONCE over `union` (the bounds of
    /// every pane), then push one element per pane into `out`, each cut from
    /// that one texture and drawn with its own `program` uniforms. The cache
    /// is keyed on `union`, not on the panes, so a pane that resizes inside
    /// the same bounds only changes its own uniforms. `alpha` fades every pane
    /// (1.0 but for a window animating). `pane(i)` gives pane
    /// `i`'s output-local rect and uniforms, or `None` to skip it.
    /// Returns `None` when the backdrop could not be built (nothing pushed).
    #[allow(clippy::too_many_arguments)]
    pub fn pane_elements<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        fb: FbMap,
        key: &BlurKey,
        union: Rectangle<i32, Physical>,
        behind: &[E],
        blur: &Blur,
        scale: Scale<f64>,
        reach: i32,
        program: &GlesTexProgram,
        count: usize,
        alpha: f32,
        mut pane: impl FnMut(usize, &mut Vec<Uniform<'static>>) -> Option<Rectangle<i32, Physical>>,
        out: &mut Vec<BlurElement>,
        preset: GlassPreset,
    ) -> Option<()>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        let (crop, _, packed) = self.refresh(
            renderer, output, fb, key, union, behind, blur, scale, reach, None, None, preset,
        )?;
        let entry = self.surfaces.get_mut(key)?;
        let texture = entry.result.clone()?;
        while entry.panes.len() < count {
            entry.panes.push(PaneSlot {
                id: Id::new(),
                look: no_uniforms(),
                commit: CommitCounter::default(),
                seen: entry.commit,
            });
        }
        entry.panes.truncate(count);
        let backdrop = entry.commit;
        let mut uniforms = std::mem::take(&mut self.pane_uniforms);
        let entry = self.surfaces.get_mut(key)?;
        for (i, slot) in entry.panes.iter_mut().enumerate() {
            uniforms.clear();
            let Some(rect) = pane(i, &mut uniforms) else {
                continue;
            };
            let wide = if packed { 2 } else { 1 };
            crate::effects::set_px_uv(&mut uniforms, (crop.size.w * wide, crop.size.h));
            crate::effects::set_packed(&mut uniforms, packed);
            if slot.seen != backdrop {
                slot.seen = backdrop;
                slot.commit.increment();
            }
            if refresh_look(&mut slot.look, &uniforms) {
                slot.commit.increment();
            }
            let mut element = BlurElement::placed_in(
                slot.id.clone(),
                renderer.context_id(),
                texture.clone(),
                crop,
                rect,
                scale,
                alpha,
                packed,
            );
            element.commit = slot.commit;
            element.program = Some(program.clone());
            element.uniforms = Some(slot.look.clone());
            out.push(element);
        }
        self.pane_uniforms = uniforms;
        Some(())
    }

    /// Bring the cached backdrop for `key` up to date over `region`: recompute
    /// the chain if damage landed in it or its crop changed. Returns the crop
    /// the texture covers and an opaque bezel's strips.
    #[allow(clippy::too_many_arguments)]
    fn refresh<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        fb: FbMap,
        key: &BlurKey,
        region: Rectangle<i32, Physical>,
        behind: &[E],
        blur: &Blur,
        scale: Scale<f64>,
        reach: i32,
        ring: Option<(i32, i32, i32)>,
        shape: Option<&Shape>,
        preset: GlassPreset,
    ) -> Option<Refreshed>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        let fb_size = fb.fb;
        if fb_size.w <= 0 || fb_size.h <= 0 || region.size.w <= 0 || region.size.h <= 0 {
            return None;
        }
        self.ensure_programs(renderer)?;
        let sharp = packs(blur, preset);
        let tuned_blur = tuned(blur, preset);
        let blur = &tuned_blur;
        let passes = blur.passes.clamp(1, 6) as usize;
        self.ensure_chain(renderer, fb_size)?;
        // Only this much of the backdrop is ever sampled (kernel plus the
        // final draw's own reach), so only this much is computed.
        let sampled = kernel_radius(blur).saturating_add(reach.max(0));
        let (tex, crop) = crop_rects(region, sampled, fb, passes)?;

        let entry = self.surfaces.entry(key.clone()).or_insert_with(|| SurfaceBlur {
            damage: OutputDamageTracker::from_output(output),
            result: None,
            crop: None,
            variant: (passes, sharp),
            commit: CommitCounter::default(),
            id: Id::new(),
            look: no_uniforms(),
            panes: Vec::new(),
            pending: false,
            last: 0,
        });

        // The invalidation rule: recompute only when damage behind this surface
        // lands inside the region (or only the bezel's ring of it) grown by
        // the kernel radius.
        let strips = ring.map(|(inner, corner, bar)| ring_strips(region.size, inner, corner, bar));
        let dirty = {
            let (damage, _) = entry.damage.damage_output(1, behind).ok()?;
            let radius = kernel_radius(blur).saturating_add(reach.max(0));
            match damage {
                Some(rects) => region_dirty(region, strips.as_ref(), shape, radius, rects),
                // smithay returns `None` when there is no damage at all (an
                // empty damage set), not "everything changed": the only reason
                // left to rebuild is a backdrop that was never built.
                None => entry.result.is_none(),
            }
        };
        // Past CROWDED surfaces, a backdrop whose only reason to redraw is
        // damage behind it is refreshed at most every CROWDED_INTERVAL frames.
        // A new surface or a moved/resized one still redraws at once.
        let crowded = self.live.max(self.surfaces.len()) > CROWDED;
        let frame = self.frame;
        let entry = self.surfaces.get_mut(key)?;
        let mut dirty = dirty || entry.pending;
        let fresh = entry.result.is_none() || entry.crop != Some(crop) || entry.variant != (passes, sharp);
        if dirty && !fresh && crowded && frame.wrapping_sub(entry.last) < CROWDED_INTERVAL {
            entry.pending = true;
            dirty = false;
        }
        // A crowd of new surfaces (a workspace opening) builds a few backdrops
        // a frame; the rest show unblurred until their turn.
        if crowded && entry.result.is_none() {
            if self.started >= CROWDED_STARTS {
                return None;
            }
            self.started += 1;
        }
        let entry = self.surfaces.get_mut(key)?;
        if dirty || fresh {
            entry.pending = false;
            entry.last = frame;
            let reuse = entry.result.take();
            let texture = self.rebuild(
                renderer,
                output,
                blur,
                scale,
                (tex, crop),
                behind,
                reuse,
                sharp,
                fb.orient(),
            )?;
            let entry = self.surfaces.get_mut(key)?;
            entry.result = Some(texture);
            entry.crop = Some(crop);
            entry.variant = (passes, sharp);
            entry.commit.increment();
        }
        Some((crop, strips, sharp))
    }

    /// Phase 1a: bring the shared pyramid of one group up to date over the
    /// union of its members' crops. Called once, for the group's lowest member
    /// (`behind` is `elements[base..]`, exactly what every member's blur
    /// samples, see [`crate::groups`]); the members then draw with
    /// [`BlurStore::group_element`]. One damage scan serves the whole group.
    /// Returns `None` when nothing could be built.
    #[allow(clippy::too_many_arguments)]
    pub fn group_refresh<'a, E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        key: &BlurKey,
        members: impl Iterator<Item = GroupMember<'a>> + Clone,
        behind: &[E],
        blur: &Blur,
        scale: Scale<f64>,
    ) -> Option<()>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        // A quarter-turned output has no verified backdrop map yet (see above).
        let fb = crate::effects::output_fb_map(output)?;
        let fb_size = fb.fb;
        if fb_size.w <= 0 || fb_size.h <= 0 {
            return None;
        }
        self.ensure_programs(renderer)?;
        // The tier's own blur: the passes its members' presets need.
        let (tier_blur, sharp) = tier_variant(blur, members.clone().map(|m| m.preset));
        let blur = &tier_blur;
        let passes = blur.passes.clamp(1, 6) as usize;
        self.ensure_chain(renderer, fb_size)?;
        let crops = || {
            members
                .clone()
                .filter(|m| !m.region.is_empty())
                .filter_map(|m| member_crop(m.region, blur, m.reach, fb))
                .map(|(_, crop)| crop)
        };
        let needed = crops().reduce(|a, b| a.merge(b))?;
        let params = (blur.size, passes, sharp);
        let frame = self.frame;
        let entry = self.groups.entry(key.clone()).or_insert_with(|| GroupBlur {
            damage: OutputDamageTracker::from_output(output),
            result: None,
            crop: None,
            params,
            members: Vec::new(),
            rebuilds: 0,
            level0: None,
            render: None,
            down: Vec::new(),
            up: Vec::new(),
            built: None,
            pending: false,
            last: 0,
            seen: frame,
        });
        entry.seen = frame;
        // One scan for the whole group; each member tests its own region.
        let dirty = {
            let (damage, _) = entry.damage.damage_output(1, behind).ok()?;
            match damage {
                Some(rects) => {
                    // Damage in the margin (inside the built region, outside
                    // every member's own crop) would be stale when a member
                    // moves onto it, so it rebuilds too.
                    let margin = entry.crop.is_some_and(|built| {
                        rects
                            .iter()
                            .any(|d| built.overlaps(*d) && !crops().any(|c| c.contains_rect(*d)))
                    });
                    margin
                        || members.clone().any(|m| {
                            let strips = m
                                .ring
                                .map(|(inner, corner, bar)| ring_strips(m.region.size, inner, corner, bar));
                            let radius = kernel_radius(blur).saturating_add(m.reach.max(0));
                            region_dirty(m.region, strips.as_ref(), m.shape, radius, rects)
                        })
                }
                // `None` is no damage at all: only a never-built backdrop
                // is a reason left to rebuild.
                None => entry.result.is_none(),
            }
        };
        let same_members = entry.members.len() == members.clone().count()
            && entry.members.iter().zip(members.clone()).all(|(a, m)| a == m.key);
        let crowded = self.live > CROWDED;
        let entry = self.groups.get_mut(key)?;
        let mut dirty = dirty || entry.pending;
        let fresh = tier_fresh(
            entry.result.is_some(),
            entry.crop,
            needed,
            entry.params,
            params,
            same_members,
        );
        // Same throttle as a surface's, but the unit is the group's rebuild.
        if dirty && !fresh && crowded && frame.wrapping_sub(entry.last) < CROWDED_INTERVAL {
            entry.pending = true;
            dirty = false;
        }
        if crowded && entry.result.is_none() {
            if self.started >= CROWDED_STARTS {
                return None;
            }
            self.started += 1;
        }
        let entry = self.groups.get_mut(key)?;
        if dirty || fresh {
            entry.pending = false;
            entry.last = frame;
            // A rebuild with nothing new to cover (damage) keeps its region.
            let crop = match entry.crop.filter(|_| !fresh) {
                Some(built) => built,
                None => padded_crop(needed, fb, passes),
            };
            let tex = crop_tex(crop, fb);
            let changed = self.rebuild_group(
                renderer,
                output,
                key,
                blur,
                scale,
                (tex, crop),
                behind,
                fresh || self.profile.full_only(),
                sharp,
            )?;
            let entry = self.groups.get_mut(key)?;
            if fresh {
                entry.crop = Some(crop);
                entry.params = params;
                entry.members.clear();
                entry.members.extend(members.map(|m| m.key.clone()));
            }
            // Damage that turned out to change nothing the blur sees leaves
            // the members as they were.
            if changed {
                entry.rebuilds = entry.rebuilds.wrapping_add(1);
            }
        }
        Some(())
    }

    /// Phase 1b: bring one tier's chain up to date. `full` rebuilds every
    /// level over the whole `crop`; otherwise the backdrop is redrawn only
    /// where it changed since the last build and each pass only over the
    /// texels that damage can reach (see [`crate::partial`]), which is
    /// texel-for-texel what the full rebuild would have written. Returns
    /// whether anything was rebuilt (`None` on a GL failure).
    #[allow(clippy::too_many_arguments)]
    fn rebuild_group<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        key: &BlurKey,
        blur: &Blur,
        scale: Scale<f64>,
        (tex, crop): (Rectangle<i32, Physical>, Rectangle<i32, Physical>),
        behind: &[E],
        full: bool,
        sharp: bool,
    ) -> Option<bool>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        let Self {
            down,
            up,
            groups,
            profile,
            plan,
            damage,
            chain_size,
            ..
        } = self;
        let g = groups.get_mut(key)?;
        let orient = crate::effects::output_fb_map(output).map_or(Transform::Normal, |fb| fb.orient());
        match render_group(
            renderer,
            output,
            g,
            plan,
            damage,
            (down.as_ref()?, up.as_ref()?),
            (*chain_size, orient),
            (tex, crop),
            scale,
            behind,
            blur,
            full,
            sharp,
        ) {
            Ok(done) => {
                if let Some(rebuild) = done {
                    profile.rebuilt(rebuild);
                }
                Some(done.is_some())
            }
            Err(err) => {
                tracing::warn!(?err, "rendering the blur chain; blur skipped this frame");
                // Whatever was half-written is not to be trusted next time.
                g.built = None;
                g.render = None;
                g.result = None;
                None
            }
        }
    }

    /// Phase 1a: one member's element, cut from its group's shared result.
    /// `rounding` is the final draw's program and uniforms, as for
    /// [`BlurStore::element`]; the members share one texture, so `px_uv` is
    /// one physical pixel of the group's crop, and the element's source
    /// rectangle is the member's region relative to that crop.
    #[allow(clippy::too_many_arguments)]
    pub fn group_element(
        &mut self,
        renderer: &mut GlesRenderer,
        group: &BlurKey,
        key: &BlurKey,
        region: Rectangle<i32, Physical>,
        rounding: Option<(GlesTexProgram, &mut Vec<Uniform<'static>>)>,
        ring: Option<(i32, i32, i32)>,
        scale: Scale<f64>,
    ) -> Option<BlurElement> {
        let g = self.groups.get(group)?;
        let texture = g.result.clone()?;
        let crop = g.crop?;
        let packed = g.params.2;
        let rebuilds = g.rebuilds;
        let slot = self.members.entry(key.clone()).or_insert_with(|| MemberSlot {
            id: Id::new(),
            look: no_uniforms(),
            commit: CommitCounter::default(),
            seen: rebuilds,
        });
        if slot.seen != rebuilds {
            slot.seen = rebuilds;
            slot.commit.increment();
        }
        let mut none = Vec::new();
        let (program, uniforms) = match rounding {
            Some((program, uniforms)) => (Some(program), uniforms),
            None => (None, &mut none),
        };
        // A packed texture holds two copies side by side: one physical px is
        // half as wide in its UV.
        let wide = if packed { 2 } else { 1 };
        crate::effects::set_px_uv(uniforms, (crop.size.w * wide, crop.size.h));
        crate::effects::set_packed(uniforms, packed);
        if refresh_look(&mut slot.look, uniforms) {
            slot.commit.increment();
        }
        let mut element = BlurElement::placed_in(
            slot.id.clone(),
            renderer.context_id(),
            texture,
            crop,
            region,
            scale,
            1.0,
            packed,
        );
        element.commit = slot.commit;
        element.program = program;
        element.uniforms = Some(slot.look.clone());
        element.ring = ring.map(|(inner, corner, bar)| ring_strips(region.size, inner, corner, bar));
        Some(element)
    }

    /// Run the chain over `crop` and hand back the upright half-resolution
    /// result (into `reuse` when it is the right size).
    #[allow(clippy::too_many_arguments)]
    fn rebuild<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        blur: &Blur,
        scale: Scale<f64>,
        (tex, crop): (Rectangle<i32, Physical>, Rectangle<i32, Physical>),
        behind: &[E],
        reuse: Option<GlesTexture>,
        sharp: bool,
        orient: Transform,
    ) -> Option<GlesTexture>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        let passes = blur.passes.clamp(1, 6) as usize;
        let Self {
            down,
            up,
            level0,
            chain,
            chain_size,
            backdrop,
            profile,
            ..
        } = self;
        // The chain runs in framebuffer space, so its levels take the crop's
        // framebuffer size (`tex`), which a quarter turn swaps against `crop`.
        if chain.len() >= MAX_CHAINS && !chain.contains_key(&(tex.size.w, tex.size.h, passes)) {
            chain.clear();
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = chain.entry((tex.size.w, tex.size.h, passes))
        {
            match alloc_levels(renderer, tex.size, passes) {
                Ok(levels) => {
                    slot.insert(levels);
                }
                Err(err) => {
                    tracing::warn!(?err, "allocating the blur chain; blur skipped this frame");
                    return None;
                }
            }
        }
        let tracker = backdrop.get_or_insert_with(|| OutputDamageTracker::from_output(output));
        // The backdrop is drawn with the output's own transform (so the
        // `gl_FragCoord` rounding masks of the elements behind still line
        // up), which under `Flipped180` (winit) stores it bottom row
        // first. The result is sampled back as a `Normal` texture, so the
        // last pass flips it upright; otherwise every window would blur
        // the vertical mirror image of what is behind it.
        match render_chain(
            renderer,
            tracker,
            level0.as_mut()?,
            chain.get_mut(&(tex.size.w, tex.size.h, passes))?,
            (tex, crop),
            scale,
            down.as_ref()?,
            up.as_ref()?,
            behind,
            blur,
            reuse,
            orient,
            sharp,
        ) {
            Ok((texture, ran)) => {
                profile.rebuilt(Rebuild {
                    crop_texels: crop.size.w as u64 * crop.size.h as u64,
                    cleared_texels: chain_size.w as u64 * chain_size.h as u64,
                    passes: ran,
                    pass_texels: chain_texels(crop.size, passes),
                    partial: false,
                });
                Some(texture)
            }
            Err(err) => {
                tracing::warn!(?err, "rendering the blur chain; blur skipped this frame");
                None
            }
        }
    }

    fn ensure_programs(&mut self, renderer: &mut GlesRenderer) -> Option<()> {
        if self.down.is_none() {
            match compile(renderer, DOWN_SRC) {
                Ok(p) => self.down = Some(p),
                Err(err) => {
                    tracing::warn!(?err, "compiling the blur downsample shader; blur disabled");
                    return None;
                }
            }
        }
        if self.up.is_none() {
            match compile(renderer, UP_SRC) {
                Ok(p) => self.up = Some(p),
                Err(err) => {
                    tracing::warn!(?err, "compiling the blur upsample shader; blur disabled");
                    return None;
                }
            }
        }
        Some(())
    }

    fn ensure_chain(&mut self, renderer: &mut GlesRenderer, fb_size: Size<i32, Physical>) -> Option<()> {
        if self.chain_size == fb_size && self.level0.is_some() {
            return Some(());
        }
        self.chain.clear();
        self.level0 = None;
        self.surfaces.clear();
        self.groups.clear();
        self.members.clear();
        self.backdrop = None;
        match Offscreen::<GlesTexture>::create_buffer(
            renderer,
            FORMAT,
            fb_size.to_logical(1).to_buffer(1, Transform::Normal),
        ) {
            Ok(t) => self.level0 = Some(t),
            Err(err) => {
                tracing::warn!(?err, "allocating the blur backdrop; blur disabled");
                return None;
            }
        }
        self.chain_size = fb_size;
        Some(())
    }
}

/// Most distinct crop sizes whose chains are kept before the pool is reset.
const MAX_CHAINS: usize = 16;

/// More blurred surfaces than this and damage-only refreshes are rate-limited.
const CROWDED: usize = 10;
/// Frames between those refreshes (about 100ms at 60Hz).
const CROWDED_INTERVAL: u64 = 6;
/// First-time backdrops built per frame once crowded.
const CROWDED_STARTS: usize = 2;

/// Levels `1..=passes` of a chain whose level 0 covers `crop`.
fn alloc_levels(
    renderer: &mut GlesRenderer,
    crop: Size<i32, Physical>,
    passes: usize,
) -> Result<Vec<GlesTexture>, GlesError> {
    (1..=passes)
        .map(|level| {
            Offscreen::<GlesTexture>::create_buffer(
                renderer,
                FORMAT,
                level_size(crop, level)
                    .to_logical(1)
                    .to_buffer(1, Transform::Normal),
            )
        })
        .collect()
}

/// Size of chain level `level`, never smaller than 1x1.
fn level_size(full: Size<i32, Physical>, level: usize) -> Size<i32, Physical> {
    let div = 1 << level;
    ((full.w / div).max(1), (full.h / div).max(1)).into()
}

/// Sampling offset handed to both shaders, derived from `blur.size`.
fn sample_offset(blur: &Blur) -> f32 {
    (blur.size.clamp(1, 64) as f32 / 4.0).max(1.0)
}

#[allow(clippy::too_many_arguments)]
fn render_chain<E>(
    renderer: &mut GlesRenderer,
    backdrop: &mut OutputDamageTracker,
    level0: &mut GlesTexture,
    mid: &mut [GlesTexture],
    (tex, crop): (Rectangle<i32, Physical>, Rectangle<i32, Physical>),
    scale: Scale<f64>,
    down: &GlesTexProgram,
    up: &GlesTexProgram,
    behind: &[E],
    blur: &Blur,
    reuse: Option<GlesTexture>,
    orient: Transform,
    sharp: bool,
) -> Result<(GlesTexture, u32), GlesError>
where
    E: Element + RenderElement<GlesRenderer>,
{
    // 1. The backdrop, rendered whole (`age = 0`): the chain texture is shared
    //    between windows, so partial damage would leave another window's
    //    backdrop in it. Each element is cut to `crop` so only the part the
    //    blur samples is drawn; the texture itself stays output-sized, which
    //    keeps `gl_FragCoord` (the elements' rounding masks) where they expect.
    //    `crop` is physical, so the elements' geometry must be taken at the
    //    output's own `scale`: at 1.0 a surface's size is its logical one, and
    //    the crop cuts its source in the wrong proportion (a wallpaper squashed
    //    into the window).
    {
        let cropped = crop_behind(behind, scale, crop);
        let mut fb = Bind::bind(renderer, level0)?;
        backdrop
            .render_output(renderer, &mut fb, 0, &cropped, CLEAR)
            .map_err(|_| GlesError::UnknownPixelFormat)?;
    }

    let offset = sample_offset(blur);
    let passes = mid.len();

    // 2. Downsample. Level 1 reads just `tex` of the backdrop.
    let tex = Rectangle::<f64, BufferCoords>::new(
        (tex.loc.x as f64, tex.loc.y as f64).into(),
        (tex.size.w as f64, tex.size.h as f64).into(),
    );
    blit_full(
        renderer,
        level0,
        &mut mid[0],
        Some(down),
        offset,
        Transform::Normal,
        Some(tex),
    )?;
    for level in 1..passes {
        let (src, dst) = split_pair(mid, level - 1, level);
        blit_full(renderer, src, dst, Some(down), offset, Transform::Normal, None)?;
    }

    // 3. Upsample down to level 2. The last step writes level 1 (half size)
    //    into the per-window result texture so the element can hold it while
    //    the chain is reused for the next window; the element stretches it to
    //    the region with bilinear filtering. One pass has no level 2: level 1
    //    is copied as is.
    for level in (2..passes).rev() {
        let (src, dst) = split_pair(mid, level, level - 1);
        blit_full(renderer, src, dst, Some(up), offset, Transform::Normal, None)?;
    }
    // The result is upright: a quarter turn swaps the framebuffer's extent.
    let half = orient.transform_size(mid[0].size());
    // Packed (lens and regular glass): twice as wide, the lightly blurred level 1
    // beside the blurred result, as `render_group` lays it out.
    let wide = if sharp {
        Size::from((half.w * 2, half.h))
    } else {
        half
    };
    let mut result = sized(renderer, reuse, wide)?;
    let left = Rectangle::<i32, Physical>::from_size((half.w, half.h).into());
    let all = [left];
    if passes >= 2 {
        let src = mid[1].clone();
        blit_to(
            renderer,
            &src,
            &mut result,
            Some(up),
            offset,
            orient,
            None,
            left,
            &all,
        )?;
    } else {
        let src = mid[0].clone();
        blit_to(
            renderer,
            &src,
            &mut result,
            None,
            offset,
            orient,
            None,
            left,
            &all,
        )?;
    }
    if sharp {
        let right = Rectangle::new((half.w, 0).into(), left.size);
        blit_to(
            renderer,
            &mid[0],
            &mut result,
            None,
            offset,
            orient,
            None,
            right,
            &all,
        )?;
    }
    // Down passes, up passes (levels `passes..=2`), then the final write.
    let ran = passes + passes.saturating_sub(2) + 1 + usize::from(sharp);
    Ok((result, ran as u32))
}

/// Texels the blits of a full chain over `crop` write.
fn chain_texels(crop: Size<i32, Physical>, passes: usize) -> u64 {
    let area = |l: usize| {
        let s = level_size(crop, l);
        s.w as u64 * s.h as u64
    };
    let down: u64 = (1..=passes).map(area).sum();
    let up: u64 = (2..passes).map(area).sum();
    down + up + area(1)
}

/// One tier's rebuild (Phase 1b). `chain` is the output size and the
/// orientation of the final write.
///
/// Persistent per tier: `level0`, the down and up levels, the result and the
/// damage tracker. A full rebuild renders the backdrop at age 0 (clearing the
/// whole output) and runs every pass over its whole texture. A partial one
/// renders at age 1, so the tracker reports exactly the damage since this
/// tier's last build, then runs each pass only over [`crate::partial::Plan`]'s
/// rectangles. Nothing outside them can differ from a full rebuild, so the
/// result is identical to one (the unit tests bound the footprint against the
/// shaders' taps; `docs/GLASS_PROFILING.md` has the pixel comparison).
#[allow(clippy::too_many_arguments)]
fn render_group<E>(
    renderer: &mut GlesRenderer,
    output: &Output,
    g: &mut GroupBlur,
    plan: &mut crate::partial::Plan,
    damage: &mut Vec<Rectangle<i32, Physical>>,
    (down, up): (&GlesTexProgram, &GlesTexProgram),
    (fb, orient): (Size<i32, Physical>, Transform),
    (tex, crop): (Rectangle<i32, Physical>, Rectangle<i32, Physical>),
    scale: Scale<f64>,
    behind: &[E],
    blur: &Blur,
    full: bool,
    sharp: bool,
) -> Result<Option<Rebuild>, GlesError>
where
    E: Element + RenderElement<GlesRenderer>,
{
    let passes = blur.passes.clamp(1, 6) as usize;
    let offset = sample_offset(blur);
    let mirrored = orient == Transform::Flipped180;
    // `orient` is the output's own transform; the map takes the render one.
    let map = FbMap {
        fb,
        transform: orient.invert(),
    };
    let mut full = full;
    let want = (crop.size, passes, fb);
    if g.built != Some(want) {
        full = true;
        if g.built.map(|b| b.2) != Some(fb) || g.level0.is_none() {
            g.level0 = Some(Offscreen::<GlesTexture>::create_buffer(
                renderer,
                FORMAT,
                fb.to_logical(1).to_buffer(1, Transform::Normal),
            )?);
        }
        g.down = alloc_levels(renderer, crop.size, passes)?;
        g.up = (2..passes)
            .map(|level| {
                Offscreen::<GlesTexture>::create_buffer(
                    renderer,
                    FORMAT,
                    level_size(crop.size, level)
                        .to_logical(1)
                        .to_buffer(1, Transform::Normal),
                )
            })
            .collect::<Result<_, _>>()?;
        g.built = Some(want);
    }
    if g.render.is_none() || g.result.is_none() || g.level0.is_none() {
        full = true;
    }
    let tracker = g
        .render
        .get_or_insert_with(|| OutputDamageTracker::from_output(output));

    // 1. The backdrop over `crop`: whole at age 0, else just what changed.
    damage.clear();
    let mut drawn = 0u64;
    {
        let cropped = crop_behind(behind, scale, crop);
        let level0 = g.level0.as_mut().ok_or(GlesError::UnknownPixelFormat)?;
        let mut target = Bind::bind(renderer, level0)?;
        crate::gpu_timer::begin(renderer, Span::Backdrop);
        let res = tracker.render_output(renderer, &mut target, if full { 0 } else { 1 }, &cropped, CLEAR);
        crate::gpu_timer::end(renderer);
        let res = res.map_err(|_| GlesError::UnknownPixelFormat)?;
        match res.damage {
            // Nothing the blur samples changed.
            None if !full => return Ok(None),
            None => {}
            Some(rects) => {
                for d in rects {
                    drawn += d.size.w.max(0) as u64 * d.size.h.max(0) as u64;
                    if full {
                        continue;
                    }
                    // Render space to the texture's `gl_FragCoord` space.
                    if let Some(c) = d.intersection(crop) {
                        damage.push(map.gl_rect(c));
                    }
                }
            }
        }
    }
    let sizes: [Size<i32, Physical>; 6] = std::array::from_fn(|i| level_size(crop.size, i + 1));
    let partial = !full && plan.fill(damage, tex, &sizes[..passes], offset, mirrored);
    if !full && !partial && damage.is_empty() {
        // Damage that lies wholly outside the region.
        return Ok(None);
    }

    // 2. Down, 3. up, over the full textures or just the planned rectangles.
    let GroupBlur {
        level0,
        down: dn,
        up: upv,
        result,
        ..
    } = g;
    let level0 = level0.as_ref().ok_or(GlesError::UnknownPixelFormat)?;
    let whole = |t: &GlesTexture| {
        let s = t.size();
        [Rectangle::<i32, Physical>::from_size((s.w, s.h).into())]
    };
    let from = Rectangle::<f64, BufferCoords>::new(
        (tex.loc.x as f64, tex.loc.y as f64).into(),
        (tex.size.w as f64, tex.size.h as f64).into(),
    );
    timed(renderer, Span::Down, |renderer| -> Result<(), GlesError> {
        for level in 1..=passes {
            let rects = &plan.down;
            if level == 1 {
                let all = whole(&dn[0]);
                let r = if partial { &rects[0][..] } else { &all[..] };
                blit(
                    renderer,
                    level0,
                    &mut dn[0],
                    Some(down),
                    offset,
                    Transform::Normal,
                    Some(from),
                    r,
                )?;
            } else {
                let (src, dst) = split_pair(dn, level - 2, level - 1);
                let all = whole(dst);
                let r = if partial { &rects[level - 1][..] } else { &all[..] };
                blit(renderer, src, dst, Some(down), offset, Transform::Normal, None, r)?;
            }
        }
        Ok(())
    })?;
    let half = dn[0].size();
    // Packed: twice as wide, the blurred result on the left and a copy of down
    // level 1 (same extent and resolution, lightly blurred) on the right. A
    // toggle of `sharp` changes the size, so `sized` makes a new texture, and
    // `params` changed, so the tier is rebuilt in full.
    let wide = if sharp {
        Size::from((half.w * 2, half.h))
    } else {
        half
    };
    let mut out = sized(renderer, result.take(), wide)?;
    let all = [Rectangle::<i32, Physical>::from_size((half.w, half.h).into())];
    let r = if partial { &plan.result[..] } else { &all[..] };
    let left = all[0];
    timed(renderer, Span::Up, |renderer| -> Result<(), GlesError> {
        for level in (3..=passes).rev() {
            let all = whole(&upv[level - 3]);
            let r = if partial {
                &plan.up[level - 3][..]
            } else {
                &all[..]
            };
            // The top level is the down chain's own; below it, two up levels.
            if level == passes {
                blit(
                    renderer,
                    &dn[passes - 1],
                    &mut upv[level - 3],
                    Some(up),
                    offset,
                    Transform::Normal,
                    None,
                    r,
                )?;
            } else {
                let (src, dst) = split_pair(upv, level - 2, level - 3);
                blit(renderer, src, dst, Some(up), offset, Transform::Normal, None, r)?;
            }
        }
        match passes {
            1 => blit_to(renderer, &dn[0], &mut out, None, offset, orient, None, left, r),
            2 => blit_to(
                renderer,
                &dn[1],
                &mut out,
                Some(up),
                offset,
                orient,
                None,
                left,
                r,
            ),
            _ => blit_to(
                renderer,
                &upv[0],
                &mut out,
                Some(up),
                offset,
                orient,
                None,
                left,
                r,
            ),
        }
    })?;
    if sharp {
        // The same rectangles, flipped the same way: level 1 as it is, written
        // upright like the result.
        let right = Rectangle::new((half.w, 0).into(), left.size);
        timed(renderer, Span::Pack, |renderer| {
            blit_to(renderer, &dn[0], &mut out, None, offset, orient, None, right, r)
        })?;
    }
    *result = Some(out);
    let ran = passes + passes.saturating_sub(2) + 1 + usize::from(sharp);
    Ok(Some(Rebuild {
        crop_texels: if partial {
            drawn
        } else {
            crop.size.w as u64 * crop.size.h as u64
        },
        cleared_texels: if full { fb.w as u64 * fb.h as u64 } else { drawn },
        passes: ran as u32,
        pass_texels: if partial {
            plan.texels()
        } else {
            chain_texels(crop.size, passes)
        },
        partial,
    }))
}

/// Run `f` between a GPU timer span of `kind` (a no-op unless `EC_GLASS_PROFILE`
/// and the driver allow it), closing the span on every path out of `f`.
fn timed<R>(renderer: &mut GlesRenderer, kind: Span, f: impl FnOnce(&mut GlesRenderer) -> R) -> R {
    crate::gpu_timer::begin(renderer, kind);
    let r = f(renderer);
    crate::gpu_timer::end(renderer);
    r
}

/// `reuse` if it is already `size`, else a fresh offscreen texture of `size`.
fn sized(
    renderer: &mut GlesRenderer,
    reuse: Option<GlesTexture>,
    size: Size<i32, BufferCoords>,
) -> Result<GlesTexture, GlesError> {
    match reuse {
        Some(t) if t.size() == size => Ok(t),
        _ => Offscreen::<GlesTexture>::create_buffer(renderer, FORMAT, size),
    }
}

/// `behind` cut to the physical `crop`. The crop's source rectangle is worked
/// out from each element's geometry *at `scale`*, which therefore has to be the
/// scale the output (and so `crop`) is in.
fn crop_behind<E: Element>(
    behind: &[E],
    scale: Scale<f64>,
    crop: Rectangle<i32, Physical>,
) -> Vec<CropRenderElement<&E>> {
    behind
        .iter()
        .filter_map(|e| CropRenderElement::from_element(e, scale, crop))
        .collect()
}

/// Two distinct elements of `chain` as `(&src, &mut dst)`.
fn split_pair(chain: &mut [GlesTexture], src: usize, dst: usize) -> (&GlesTexture, &mut GlesTexture) {
    debug_assert_ne!(src, dst);
    if src < dst {
        let (a, b) = chain.split_at_mut(dst);
        (&a[src], &mut b[0])
    } else {
        let (a, b) = chain.split_at_mut(src);
        (&b[0], &mut a[dst])
    }
}

/// [`blit`] over the whole of `dst`.
fn blit_full(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    program: Option<&GlesTexProgram>,
    offset: f32,
    transform: Transform,
    from: Option<Rectangle<f64, BufferCoords>>,
) -> Result<(), GlesError> {
    let size = dst.size();
    let whole = [Rectangle::<i32, Physical>::from_size((size.w, size.h).into())];
    blit(renderer, src, dst, program, offset, transform, from, &whole)
}

/// One Kawase pass: draw `src` (or just its `from` rectangle) over `dst`,
/// sampling it under `transform` (`Normal` except for the final, upright-ing
/// pass), through `program` or the default shader. Only the `damage`
/// rectangles of `dst` are written (the whole of it for a full pass); the
/// sampling coordinates of a texel are the same either way. The half-pixel is
/// always one of `src`'s own.
#[allow(clippy::too_many_arguments)]
fn blit(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    program: Option<&GlesTexProgram>,
    offset: f32,
    transform: Transform,
    from: Option<Rectangle<f64, BufferCoords>>,
    damage: &[Rectangle<i32, Physical>],
) -> Result<(), GlesError> {
    let size = dst.size();
    let dest = Rectangle::<i32, Physical>::from_size((size.w, size.h).into());
    blit_to(renderer, src, dst, program, offset, transform, from, dest, damage)
}

/// [`blit`] into the `dest` rectangle of `dst` (the left or right half of a
/// packed result); `damage` is relative to `dest`.
#[allow(clippy::too_many_arguments)]
fn blit_to(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    program: Option<&GlesTexProgram>,
    offset: f32,
    transform: Transform,
    from: Option<Rectangle<f64, BufferCoords>>,
    dest: Rectangle<i32, Physical>,
    damage: &[Rectangle<i32, Physical>],
) -> Result<(), GlesError> {
    let src_size = src.size();
    let dst_size = dst.size();
    let uniforms = [
        Uniform::new("halfpixel", [0.5 / src_size.w as f32, 0.5 / src_size.h as f32]),
        Uniform::new("blur_offset", offset),
    ];
    let mut fb = Bind::bind(renderer, dst)?;
    let mut frame = renderer.render(&mut fb, (dst_size.w, dst_size.h).into(), Transform::Normal)?;
    frame.render_texture_from_to(
        src,
        from.unwrap_or_else(|| Rectangle::from_size((src_size.w as f64, src_size.h as f64).into())),
        dest,
        damage,
        &[],
        transform,
        1.0,
        program,
        &uniforms,
    )?;
    let _ = frame.finish()?;
    Ok(())
}

fn compile(renderer: &mut GlesRenderer, src: &str) -> Result<GlesTexProgram, GlesError> {
    renderer.compile_custom_texture_shader(
        src,
        &[
            UniformName::new("halfpixel", UniformType::_2f),
            UniformName::new("blur_offset", UniformType::_1f),
        ],
    )
}

/// Dual-Kawase downsample. Alpha is forced to 1.0 so each pass *replaces* the
/// target rather than blending into whatever the shared chain texture last held.
const DOWN_SRC: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 halfpixel;
uniform float blur_offset;

void main() {
    vec2 o = halfpixel * blur_offset;
    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords - o);
    sum += texture2D(tex, v_coords + o);
    sum += texture2D(tex, v_coords + vec2(o.x, -o.y));
    sum += texture2D(tex, v_coords - vec2(o.x, -o.y));
    vec3 color = (sum / 8.0).rgb * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec3(0.0, 0.2, 0.0) + color * 0.8;
#endif

    gl_FragColor = vec4(color, 1.0);
}
"#;

/// Dual-Kawase upsample.
const UP_SRC: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform vec2 halfpixel;
uniform float blur_offset;

void main() {
    vec2 o = halfpixel * blur_offset;
    vec4 sum = texture2D(tex, v_coords + vec2(-o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(-o.x, o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, o.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(o.x, o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(o.x, -o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(0.0, -o.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(-o.x, -o.y)) * 2.0;
    vec3 color = (sum / 12.0).rgb * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec3(0.0, 0.2, 0.0) + color * 0.8;
#endif

    gl_FragColor = vec4(color, 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // The crop tests speak in the old `(fb size, mirrored)` pair: a quarter
    // turn has its own tests on `FbMap`. These shims build the map for it.
    fn map(fb: Size<i32, Physical>, mirrored: bool) -> FbMap {
        let transform = if mirrored {
            Transform::Flipped180
        } else {
            Transform::Normal
        };
        FbMap { fb, transform }
    }
    fn crop_rects(
        region: Rectangle<i32, Physical>,
        radius: i32,
        fb: Size<i32, Physical>,
        passes: usize,
        mirrored: bool,
    ) -> Option<(Rectangle<i32, Physical>, Rectangle<i32, Physical>)> {
        super::crop_rects(region, radius, map(fb, mirrored), passes)
    }
    fn member_crop(
        region: Rectangle<i32, Physical>,
        blur: &Blur,
        reach: i32,
        fb: Size<i32, Physical>,
        mirrored: bool,
    ) -> Option<(Rectangle<i32, Physical>, Rectangle<i32, Physical>)> {
        super::member_crop(region, blur, reach, map(fb, mirrored))
    }
    fn padded_crop(
        needed: Rectangle<i32, Physical>,
        fb: Size<i32, Physical>,
        passes: usize,
        mirrored: bool,
    ) -> Rectangle<i32, Physical> {
        super::padded_crop(needed, map(fb, mirrored), passes)
    }
    fn crop_tex(
        crop: Rectangle<i32, Physical>,
        fb: Size<i32, Physical>,
        mirrored: bool,
    ) -> Rectangle<i32, Physical> {
        super::crop_tex(crop, map(fb, mirrored))
    }

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn blur(size: i32, passes: i32) -> Blur {
        Blur {
            size,
            passes,
            ..Blur::default()
        }
    }

    /// The glass element samples the blur result, which spans the crop, so a
    /// bend of `strength` px is `strength / crop` in UV whatever the output
    /// size, never `strength / output`.
    #[test]
    fn px_uv_follows_the_crop_not_the_output() {
        use crate::effects::{px_uv_for, set_px_uv};
        use smithay::backend::renderer::gles::UniformValue;
        let blur = blur(8, 3);
        let window = r(400, 200, 800, 600);
        let strength = 12.0f32;
        for fb_w in [1920, 2880] {
            let fb: Size<i32, Physical> = (fb_w, fb_w * 9 / 16).into();
            let (_, crop) = crop_rects(window, kernel_radius(&blur), fb, 3, false).unwrap();
            assert!(crop.size.w < fb.w, "the crop is a part of the output");
            let mut u = vec![Uniform::new("px_uv", px_uv_for((fb.w, fb.h)))];
            set_px_uv(&mut u, (crop.size.w, crop.size.h));
            let UniformValue::_2f(x, y) = u[0].value else {
                panic!("px_uv is a vec2")
            };
            assert_eq!(x, 1.0 / crop.size.w as f32);
            assert_eq!(y, 1.0 / crop.size.h as f32);
            assert!((strength * x - strength / crop.size.w as f32).abs() < 1e-9);
            assert!((strength * x - strength / fb.w as f32).abs() > 1e-6);
        }
        // Same window, bigger output: the crop (so the scale) does not change.
        let crop_at = |w: i32| {
            crop_rects(window, kernel_radius(&blur), (w, w * 9 / 16).into(), 3, false)
                .unwrap()
                .1
                .size
        };
        assert_eq!(crop_at(1920), crop_at(2880));
    }

    /// COMP-02 §3: the glass refraction widens the invalidation reach by its
    /// displacement (with the dispersion margin), scaled to physical px;
    /// flat glass adds nothing.
    #[test]
    fn glass_refraction_widens_the_invalidation_reach() {
        use crate::effects::glass_reach;
        assert_eq!(glass_reach(0.0, 0.0, 1.5), 0);
        assert_eq!(glass_reach(12.0, 0.0, 1.0), 13);
        assert_eq!(glass_reach(12.0, 0.0, 2.0), 26);
        // A fractional default still reserves whole pixels (ceil).
        assert_eq!(glass_reach(1.5, 0.0, 1.0), 2);
        let region = r(100, 100, 200, 200);
        let k = kernel_radius(&blur(8, 2));
        let just_outside = [r(100 - k - 5, 150, 2, 2)];
        assert!(!invalidates(region, k, &just_outside));
        assert!(invalidates(
            region,
            k + glass_reach(12.0, 0.0, 1.0),
            &just_outside
        ));
    }

    /// An opaque window's bezel: the strips cover the ring and its corners,
    /// never overlap, and leave the window's middle out.
    #[test]
    fn ring_strips_cover_only_the_bezel() {
        // A 300x200 window inside a 6 px bezel, corner radius 9.
        let size = Size::<i32, Physical>::from((312, 212));
        let strips = ring_strips(size, 6, 9, 0);
        assert_eq!(
            strips,
            [
                r(0, 0, 312, 16),
                r(0, 196, 312, 16),
                r(0, 16, 7, 180),
                r(305, 16, 7, 180)
            ]
        );
        for (i, a) in strips.iter().enumerate() {
            for b in &strips[i + 1..] {
                assert!(!a.overlaps(*b), "{a:?} overlaps {b:?}");
            }
        }
        // Every bezel pixel is in a strip; the window's middle is in none.
        let bezel = [
            r(0, 0, 312, 6),
            r(0, 206, 312, 6),
            r(0, 6, 6, 200),
            r(306, 6, 6, 200),
        ];
        assert!(Rectangle::subtract_rects_many(bezel, strips).is_empty());
        assert!(strips.iter().all(|s| !s.overlaps(r(20, 20, 272, 172))));
        // Damage in the window's middle does not invalidate a ring…
        let k = 8;
        let middle = [r(150, 100, 4, 4)];
        assert!(!strips.iter().any(|s| invalidates(*s, k, &middle)));
        // …but it does the whole region, and damage near the ring does both.
        assert!(invalidates(r(0, 0, 312, 212), k, &middle));
        assert!(strips.iter().any(|s| invalidates(*s, k, &[r(12, 100, 2, 2)])));
    }

    /// A titlebar is glass over an opaque window: the top strip reaches down
    /// over it, and the strips stay disjoint and inside the region.
    #[test]
    fn ring_strips_take_in_the_titlebar() {
        let size = Size::<i32, Physical>::from((312, 212));
        let strips = ring_strips(size, 6, 9, 40);
        assert_eq!(
            strips,
            [
                r(0, 0, 312, 40),
                r(0, 196, 312, 16),
                r(0, 40, 7, 156),
                r(305, 40, 7, 156)
            ]
        );
        for (i, a) in strips.iter().enumerate() {
            for b in &strips[i + 1..] {
                assert!(!a.overlaps(*b), "{a:?} overlaps {b:?}");
            }
        }
        // A bar deeper than the region never overruns it.
        for s in ring_strips(size, 6, 9, 500) {
            assert!(s.size.h >= 0 && s.loc.y + s.size.h <= 212);
        }
    }

    #[test]
    fn ring_strips_clamp_to_a_tiny_region() {
        for s in ring_strips(Size::from((10, 6)), 6, 9, 0) {
            assert!(s.loc.x >= 0 && s.loc.y >= 0 && s.size.w >= 0 && s.size.h >= 0);
            assert!(s.loc.x + s.size.w <= 10 && s.loc.y + s.size.h <= 6);
        }
    }

    #[test]
    fn radius_grows_with_both_knobs() {
        assert_eq!(kernel_radius(&blur(8, 2)), 32);
        assert_eq!(kernel_radius(&blur(8, 3)), 64);
        assert_eq!(kernel_radius(&blur(16, 2)), 64);
        assert_eq!(kernel_radius(&blur(1, 1)), 2);
        // The default is wide enough to erase fine backdrop structure.
        assert_eq!(kernel_radius(&Blur::default()), 128);
    }

    #[test]
    fn radius_clamps_out_of_range_config() {
        // The config validator already refuses these, but the geometry must
        // not overflow if one ever reaches here.
        assert_eq!(kernel_radius(&blur(1000, 99)), 64 * 64);
        assert_eq!(kernel_radius(&blur(-4, 0)), 2);
    }

    #[test]
    fn grow_expands_on_every_side() {
        assert_eq!(grow(r(100, 100, 50, 40), 10), r(90, 90, 70, 60));
        assert_eq!(grow(r(0, 0, 10, 10), 0), r(0, 0, 10, 10));
    }

    #[test]
    fn grow_saturates_rather_than_overflowing() {
        let huge = grow(r(0, 0, i32::MAX, i32::MAX), i32::MAX);
        assert_eq!(huge.size.w, i32::MAX);
    }

    #[test]
    fn damage_inside_the_region_invalidates() {
        let region = r(100, 100, 200, 200);
        assert!(invalidates(region, 32, &[r(150, 150, 10, 10)]));
    }

    #[test]
    fn damage_within_the_kernel_radius_invalidates() {
        // The whole point of the rule: this damage is *outside* the blurred
        // region, but the kernel samples it, so the region is stale.
        let region = r(100, 100, 200, 200);
        assert!(invalidates(region, 32, &[r(80, 150, 5, 5)]));
        assert!(invalidates(region, 32, &[r(150, 310, 5, 5)]));
    }

    #[test]
    fn damage_beyond_the_kernel_radius_does_not() {
        let region = r(100, 100, 200, 200);
        assert!(!invalidates(region, 32, &[r(40, 150, 5, 5)]));
        assert!(!invalidates(region, 32, &[r(150, 340, 5, 5)]));
        // Same damage, a wider kernel: now it reaches.
        assert!(invalidates(region, 128, &[r(40, 150, 5, 5)]));
    }

    #[test]
    fn no_damage_never_invalidates() {
        assert!(!invalidates(r(0, 0, 100, 100), 64, &[]));
    }

    #[test]
    fn any_one_damage_rect_is_enough() {
        let region = r(100, 100, 200, 200);
        let damage = [r(0, 0, 5, 5), r(1000, 1000, 5, 5), r(150, 150, 1, 1)];
        assert!(invalidates(region, 8, &damage));
    }

    #[test]
    fn chain_levels_halve_and_never_reach_zero() {
        let full = Size::<i32, Physical>::from((1920, 1080));
        assert_eq!(level_size(full, 0), (1920, 1080).into());
        assert_eq!(level_size(full, 1), (960, 540).into());
        assert_eq!(level_size(full, 2), (480, 270).into());
        assert_eq!(level_size(Size::from((3, 1)), 6), (1, 1).into());
    }

    #[test]
    fn sample_offset_never_drops_below_one_pixel() {
        assert_eq!(sample_offset(&blur(1, 1)), 1.0);
        assert_eq!(sample_offset(&blur(8, 1)), 2.0);
        assert_eq!(sample_offset(&blur(64, 1)), 16.0);
    }

    #[test]
    fn split_pair_hands_back_distinct_levels() {
        // Guards the unsafe-looking index juggling in `render_chain`: the two
        // borrows must never alias, in either direction.
        for (a, b) in [(0usize, 2usize), (2, 0), (1, 2), (2, 1)] {
            let lo = a.min(b);
            let hi = a.max(b);
            assert!(lo < hi);
        }
    }

    #[test]
    fn a_surface_with_no_opaque_region_shows_through() {
        assert!(shows_through(r(0, 0, 100, 44), 0, []));
        assert!(shows_through(r(0, 0, 100, 44), 13, []));
    }

    #[test]
    fn a_fully_opaque_surface_does_not() {
        assert!(!shows_through(r(0, 0, 100, 44), 0, [r(0, 0, 100, 44)]));
    }

    #[test]
    fn a_partly_opaque_surface_still_does() {
        assert!(shows_through(r(0, 0, 100, 44), 0, [r(0, 0, 100, 43)]));
        assert!(shows_through(r(0, 0, 100, 44), 8, [r(0, 0, 100, 30)]));
    }

    #[test]
    fn an_oversized_opaque_region_covers_the_surface() {
        assert!(!shows_through(r(10, 10, 20, 20), 0, [r(0, 0, 100, 100)]));
    }

    #[test]
    fn an_empty_surface_is_never_blurred() {
        assert!(!shows_through(r(0, 0, 0, 44), 0, []));
        assert!(!shows_through(r(0, 0, 0, 44), 13, []));
    }

    /// GTK4/libadwaita leave their rounded corners out of the opaque region;
    /// the mask hides those pixels, so they must not buy a backdrop pass.
    #[test]
    fn corners_left_out_under_the_mask_do_not_show_through() {
        let region = r(100, 50, 300, 200);
        // The opaque region as a cross plus the in-corner rows GTK emits:
        // everything but the 8x8 corner squares.
        let opaque = [r(108, 50, 284, 200), r(100, 58, 300, 184)];
        assert!(!shows_through(region, 8, opaque));
        assert!(!shows_through(region, 12, opaque), "a larger mask hides more");
        assert!(shows_through(region, 0, opaque), "square corners would show");
        assert!(shows_through(region, 4, opaque), "a smaller mask shows corner px");
    }

    /// `(add, (x, y, w, h))`: one region rect, surface-local.
    type RegionRect = (bool, (i32, i32, i32, i32));

    fn region(rects: &[RegionRect]) -> RegionAttributes {
        RegionAttributes {
            rects: rects
                .iter()
                .map(|&(add, (x, y, w, h))| {
                    let kind = if add {
                        RectangleKind::Add
                    } else {
                        RectangleKind::Subtract
                    };
                    (kind, Rectangle::new((x, y).into(), (w, h).into()))
                })
                .collect(),
        }
    }

    fn shape_of(rects: &[RegionRect], scale: f64, fillet: f32) -> Option<Shape> {
        let region = region(rects);
        Shape::from_region(
            Some(&region),
            Size::from((400, 300)),
            Point::from((0, 0)),
            Scale::from(scale),
            fillet,
        )
    }

    /// Vol 1 §5.2: only a finite region of 2..=4 added boxes is a shape;
    /// everything else keeps the old single-box path.
    #[test]
    fn only_a_small_set_of_boxes_is_a_shape() {
        // The protocol's infinite region (no `set_input_region`).
        let none = Shape::from_region(
            None,
            Size::from((400, 300)),
            Point::from((0, 0)),
            Scale::from(1.0),
            20.0,
        );
        assert_eq!(none, None);
        let pill = (true, (0, 0, 400, 40));
        let panel = (true, (100, 40, 200, 200));
        assert_eq!(shape_of(&[pill], 1.0, 20.0), None, "one box is the plain path");
        assert_eq!(shape_of(&[pill, (false, (0, 0, 10, 10))], 1.0, 20.0), None);
        assert_eq!(shape_of(&[pill; 5], 1.0, 20.0), None, "five boxes");
        let two = shape_of(&[pill, panel], 1.0, 20.0).expect("pill and panel");
        assert_eq!(two.rects(), &[r(0, 0, 400, 40), r(100, 40, 200, 200)]);
        assert!(shape_of(&[pill; 4], 1.0, 20.0).is_some());
        // A subtract covering the surface is a reset (iced_layershell's
        // replace), however much history came before it.
        let reset = (false, (0, 0, 400, 300));
        let replaced = shape_of(&[pill, pill, pill, reset, pill, panel], 1.0, 20.0).expect("reset");
        assert_eq!(replaced.rects(), &[r(0, 0, 400, 40), r(100, 40, 200, 200)]);
        assert_eq!(
            shape_of(&[pill, panel, reset], 1.0, 20.0),
            None,
            "reset to nothing"
        );
        // A stale subtract from another surface's size (shared wl_region,
        // second output) before the reset is erased by it; after it, not.
        let stale = (false, (0, 0, 300, 200));
        let two_outputs = shape_of(&[stale, pill, reset, pill, panel], 1.0, 20.0).expect("stale");
        assert_eq!(two_outputs.rects(), &[r(0, 0, 400, 40), r(100, 40, 200, 200)]);
        assert_eq!(shape_of(&[reset, pill, stale, panel], 1.0, 20.0), None);
        // Empty boxes and boxes wholly off the surface do not count.
        assert_eq!(
            shape_of(&[pill, (true, (500, 0, 10, 10)), (true, (0, 0, 0, 5))], 1.0, 20.0),
            None
        );
    }

    fn area(rects: &[RegionRect], scale: f64) -> BlurArea {
        blur_area(
            &region(rects),
            Size::from((400, 300)),
            Point::from((10, 20)),
            Scale::from(scale),
        )
    }

    /// An explicit background-effect region: empty is nothing, one box is a
    /// rect, 2..=4 a hard-edged shape, and what a shape cannot carry falls
    /// back to the bounding box clipped to the surface.
    #[test]
    fn a_background_effect_region_becomes_an_area() {
        assert_eq!(area(&[], 1.0), BlurArea::Nothing, "empty region, no blur");
        assert_eq!(
            area(&[(true, (500, 0, 10, 10))], 1.0),
            BlurArea::Nothing,
            "off the surface"
        );
        assert_eq!(
            area(&[(true, (0, 0, 400, 300)), (false, (0, 0, 400, 300))], 1.0),
            BlurArea::Nothing,
            "added then wholly subtracted"
        );
        // One box, clipped to the surface, scaled by edges and placed.
        assert_eq!(
            area(&[(true, (50, 40, 1000, 100))], 1.5),
            BlurArea::Rect(r(85, 80, 525, 150))
        );
        let two = [(true, (0, 0, 100, 40)), (true, (200, 100, 100, 100))];
        match area(&two, 1.0) {
            BlurArea::Shape(s) => {
                assert_eq!(s.rects(), &[r(10, 20, 100, 40), r(210, 120, 100, 100)]);
                assert_eq!(s.fillet, 0.0);
                assert_eq!(s.bounds(), Some(r(10, 20, 300, 200)));
            }
            other => panic!("expected a shape, got {other:?}"),
        }
        // Five boxes, and a partial subtract: bounding box of the adds.
        let five = [(true, (0, 0, 10, 10)); 4]
            .into_iter()
            .chain([(true, (90, 90, 10, 10))])
            .collect::<Vec<_>>();
        assert_eq!(area(&five, 1.0), BlurArea::Rect(r(10, 20, 100, 100)));
        let holed = [(true, (0, 0, 100, 100)), (false, (40, 40, 10, 10))];
        assert_eq!(area(&holed, 1.0), BlurArea::Rect(r(10, 20, 100, 100)));
        assert_eq!(area(&two, 1.0).bounds(), Some(r(10, 20, 300, 200)));
        assert_eq!(BlurArea::Nothing.bounds(), None);
    }

    #[test]
    fn a_shape_is_clipped_and_placed_edge_by_edge() {
        let region = region(&[(true, (0, 0, 400, 40)), (true, (101, 40, 399, 500))]);
        let shape = Shape::from_region(
            Some(&region),
            Size::from((400, 300)),
            Point::from((10, 20)),
            Scale::from(1.5),
            30.0,
        )
        .unwrap();
        // Clipped to 400x300, scaled by edges (101 * 1.5 = 151.5 → 152), and
        // the two boxes still meet at y = 60 + 20.
        assert_eq!(shape.rects(), &[r(10, 20, 600, 60), r(162, 80, 448, 390)]);
        assert_eq!(shape.fillet, 30.0);
    }

    /// The mask's signed distance: inside a box, inside the concave fillet
    /// where two boxes meet (outside both boxes), and outside the union.
    #[test]
    fn the_shape_sdf_fills_the_join_and_nothing_else() {
        let shape = shape_of(&[(true, (0, 0, 400, 40)), (true, (100, 40, 200, 200))], 1.0, 20.0).unwrap();
        let radius = 12.0;
        let plain = shape_of(&[(true, (0, 0, 400, 40)), (true, (100, 40, 200, 200))], 1.0, 0.0).unwrap();
        // Inside the pill and inside the panel.
        assert!(shape_sd(&shape, radius, (200.0, 20.0)) < -10.0);
        assert!(shape_sd(&shape, radius, (200.0, 150.0)) < -10.0);
        // In the concave corner under the pill, right of the panel: outside
        // both boxes (the plain union), inside the fillet.
        assert!(shape_sd(&plain, radius, (303.0, 43.0)) > 2.0);
        assert!(shape_sd(&shape, radius, (303.0, 43.0)) < 0.0);
        // Outside the union: beside the panel, beyond the fillet's reach, and
        // the pill's own rounded outer corner.
        assert!(shape_sd(&shape, radius, (380.0, 100.0)) > 20.0);
        assert!(shape_sd(&shape, radius, (1.0, 1.0)) > 0.0);
        assert!(shape_sd(&shape, radius, (200.0, 260.0)) > 0.0);
    }

    /// The calendar drawer hung off a bar's far end: its panel is flush with
    /// the surface's right edge, so its top-right corner and the pill's share
    /// one corner centre and radius. The smooth union must not push that
    /// corner outward (the fillet is for concave joins only); the mask has to
    /// stay on the painted rounded outline there.
    #[test]
    fn a_panel_flush_with_the_pill_end_keeps_the_corner_round() {
        let (w, radius) = (400.0_f32, 20.0_f32);
        let shape = shape_of(&[(true, (0, 0, 400, 40)), (true, (200, 0, 200, 300))], 1.0, 20.0).unwrap();
        // The top-right arc is centred at (w - r, r); test 2 px outside and 2 px
        // inside it along the diagonal.
        let c = (w - radius, radius);
        let step = |d: f32| {
            let t = d * std::f32::consts::FRAC_1_SQRT_2;
            (c.0 + t, c.1 - t)
        };
        let outside = step(radius + 2.0);
        let inside = step(radius - 2.0);
        assert!(
            shape_sd(&shape, radius, outside) > 1.0,
            "corner pushed out: {}",
            shape_sd(&shape, radius, outside)
        );
        assert!(
            shape_sd(&shape, radius, inside) < -1.0,
            "corner pulled in: {}",
            shape_sd(&shape, radius, inside)
        );
        // A panel one pixel short of the edge is not a shared corner: its arc
        // centre is a pixel off, which must not buy the full bite either.
        let offset = shape_of(&[(true, (0, 0, 400, 40)), (true, (200, 0, 199, 300))], 1.0, 20.0).unwrap();
        let sd = shape_sd(&offset, radius, outside);
        assert!(sd > 1.0, "a one-pixel offset bit the corner: {sd}");
    }

    #[test]
    fn a_shape_shows_through_unless_every_box_is_covered() {
        let shape = shape_of(&[(true, (0, 0, 400, 40)), (true, (100, 40, 200, 200))], 1.0, 20.0).unwrap();
        assert!(shape_shows_through(&shape, &[]));
        assert!(shape_shows_through(&shape, &[r(0, 0, 400, 40)]));
        // Covering the two boxes is enough; the rest of the surface is cut.
        assert!(!shape_shows_through(
            &shape,
            &[r(0, 0, 400, 40), r(100, 40, 200, 200)]
        ));
    }

    #[test]
    fn a_radius_past_half_the_surface_still_tests_its_middle() {
        assert!(shows_through(r(0, 0, 20, 20), 50, []));
        assert!(!shows_through(r(0, 0, 20, 20), 50, [r(0, 0, 20, 20)]));
    }

    /// Stands in for the output-sized chain texture; placement never reads it.
    #[derive(Debug, Clone)]
    struct Backdrop(Size<i32, BufferCoords>);

    impl Texture for Backdrop {
        fn width(&self) -> u32 {
            self.0.w as u32
        }
        fn height(&self) -> u32 {
            self.0.h as u32
        }
        fn format(&self) -> Option<Fourcc> {
            Some(FORMAT)
        }
    }

    /// BLUR-02: a region on the output's top edge (and right edge) is drawn on
    /// exactly that region and samples exactly that region of the backdrop,
    /// nothing outside the texture, at every scale the owner's hardware uses.
    #[test]
    fn a_region_on_the_top_edge_is_placed_and_sampled_exactly() {
        let fb = Size::<i32, BufferCoords>::from((2880, 1920));
        // (scale, region): full-width strips at y = 0, a thin top-right strip
        // like an idle toast surface, and odd heights that do not divide by the
        // scale (a logical round trip turns 5 px at 1.9 into 6).
        let cases = [
            (1.0, r(0, 0, 2880, 30)),
            (1.0, r(2476, 0, 404, 1)),
            (1.5, r(0, 0, 2880, 45)),
            (1.5, r(2274, 0, 606, 2)),
            (1.5, r(2000, 0, 880, 7)),
            (2.0, r(0, 0, 2880, 60)),
            (2.0, r(2072, 0, 808, 2)),
            (2.0, r(2000, 0, 880, 5)),
            (1.9, r(2112, 0, 768, 5)),
        ];
        for (s, region) in cases {
            let scale = Scale::from(s);
            let el = BlurElement::placed(
                Id::new(),
                ContextId::new(),
                Backdrop(fb),
                Rectangle::from_size((fb.w, fb.h).into()),
                region,
                scale,
                1.0,
            );
            assert_eq!(el.geometry(scale), region, "geometry at scale {s}");
            assert_eq!(el.location(scale), region.loc, "location at scale {s}");
            let src = el.src();
            let want = Rectangle::<f64, BufferCoords>::new(
                (region.loc.x as f64, region.loc.y as f64).into(),
                (region.size.w as f64, region.size.h as f64).into(),
            );
            assert_eq!(src, want, "src at scale {s}");
            assert!(
                src.loc.x >= 0.0 && src.loc.y >= 0.0,
                "src above/left of the texture at {s}"
            );
            assert!(
                src.loc.x + src.size.w <= fb.w as f64 && src.loc.y + src.size.h <= fb.h as f64,
                "src past the texture at {s}"
            );
        }
    }

    /// A full-output image whose size at scale 1 is its logical one, like a
    /// wallpaper surface: only its geometry depends on the scale.
    struct Wallpaper(Id);

    impl Element for Wallpaper {
        fn id(&self) -> &Id {
            &self.0
        }
        fn current_commit(&self) -> CommitCounter {
            CommitCounter::default()
        }
        fn src(&self) -> Rectangle<f64, BufferCoords> {
            Rectangle::from_size((1920.0, 1080.0).into())
        }
        fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
            Rectangle::from_size(
                (
                    (1516.0 * scale.x).round() as i32,
                    (1011.0 * scale.y).round() as i32,
                )
                    .into(),
            )
        }
        fn transform(&self) -> Transform {
            Transform::Normal
        }
        fn damage_since(&self, _: Scale<f64>, _: Option<CommitCounter>) -> DamageSet<i32, Physical> {
            DamageSet::default()
        }
        fn opaque_regions(&self, _: Scale<f64>) -> OpaqueRegions<i32, Physical> {
            OpaqueRegions::default()
        }
        fn alpha(&self) -> f32 {
            1.0
        }
        fn kind(&self) -> Kind {
            Kind::Unspecified
        }
    }

    /// The backdrop is cut with the output's scale: at 1.0 on a 1.9 output the
    /// crop took a wallpaper's whole width for the part of it under the window
    /// (the squashed backdrop).
    #[test]
    fn the_backdrop_crop_cuts_the_source_in_proportion_at_the_output_scale() {
        let behind = [Wallpaper(Id::new())];
        let crop = r(0, 0, 1584, 1920);
        let scale = Scale::from(1.9);
        let geo = behind[0].geometry(scale);
        let cropped = crop_behind(&behind, scale, crop);
        assert_eq!(cropped.len(), 1);
        let src = cropped[0].src();
        let want_w = 1920.0 * crop.size.w as f64 / geo.size.w as f64;
        assert!((src.size.w - want_w).abs() < 1.0, "{} vs {want_w}", src.size.w);
        assert!(src.size.w < 1920.0 * 0.6, "the crop took most of the image");
        // Documents the old behaviour: the whole image, hence the squash.
        let wrong = crop_behind(&behind, Scale::from(1.0), crop);
        assert!(wrong[0].src().size.w > 1900.0);
    }

    /// A cropped result texture is sampled at the region's offset into it.
    #[test]
    fn a_cropped_result_is_sampled_relative_to_its_origin() {
        let region = r(1000, 500, 400, 300);
        let (_, crop) = crop_rects(region, 64, Size::from((1920, 1080)), 3, false).unwrap();
        let tex = Backdrop(Size::from((crop.size.w, crop.size.h)));
        let scale = Scale::from(1.0);
        let el = BlurElement::placed(Id::new(), ContextId::new(), tex, crop, region, scale, 1.0);
        assert_eq!(el.geometry(scale), region);
        let want = Rectangle::<f64, BufferCoords>::new(
            (
                (region.loc.x - crop.loc.x) as f64,
                (region.loc.y - crop.loc.y) as f64,
            )
                .into(),
            (400.0, 300.0).into(),
        );
        assert_eq!(el.src(), want);
        assert!(crop.contains_rect(grow(region, 64)));
    }

    /// Phase 1a: a group of one is the per-window chain. Its union crop is the
    /// member's own crop, the texture-space twin is the same one `crop_rects`
    /// makes (flipped under a y-mirrored output), and the element samples the
    /// same output pixels.
    #[test]
    fn a_group_of_one_is_the_per_window_crop() {
        let blur = blur(8, 4);
        let fb = Size::<i32, Physical>::from((2880, 1620));
        for mirrored in [false, true] {
            for region in [
                r(36, 72, 800, 600),
                r(0, 0, 400, 30),
                r(2400, 1500, 480, 120),
                r(1000, 500, 1, 1),
            ] {
                for reach in [0, 2, 5] {
                    let (tex, crop) = member_crop(region, &blur, reach, fb, mirrored).unwrap();
                    assert_eq!(
                        crop_tex(crop, fb, mirrored),
                        tex,
                        "{region:?} mirrored {mirrored}"
                    );
                    // The union of one crop is the crop.
                    assert_eq!([crop].into_iter().reduce(|a, b| a.merge(b)), Some(crop));
                }
            }
        }
    }

    /// Phase 1a: members share one texture, so `px_uv` is one physical pixel of
    /// the union crop, and each member's UV rectangle is its own part of it:
    /// the same output pixels a per-window result would have shown.
    #[test]
    fn group_members_sample_their_own_part_of_the_union() {
        use crate::effects::{px_uv_for, set_px_uv};
        use smithay::backend::renderer::gles::UniformValue;
        let blur = blur(8, 4);
        let fb = Size::<i32, Physical>::from((2880, 1620));
        let a = r(100, 100, 400, 300);
        let b = r(700, 150, 500, 300);
        let (_, ca) = member_crop(a, &blur, 2, fb, false).unwrap();
        let (_, cb) = member_crop(b, &blur, 2, fb, false).unwrap();
        let union = ca.merge(cb);
        assert!(union.contains_rect(ca) && union.contains_rect(cb));
        // Snapped to the coarsest level, like each member's crop.
        let d = 1 << 4;
        assert!(union.loc.x % d == 0 && union.loc.y % d == 0);
        assert!((union.loc.x + union.size.w) % d == 0 || union.loc.x + union.size.w == fb.w);
        let scale = Scale::from(1.0);
        for (region, own) in [(a, ca), (b, cb)] {
            // The shared result is half the union's size.
            let shared = Backdrop(Size::from((union.size.w / 2, union.size.h / 2)));
            let el = BlurElement::placed(Id::new(), ContextId::new(), shared, union, region, scale, 1.0);
            let src = el.src();
            // Back to output pixels: the same ones as the member's own result.
            let own_tex = Backdrop(Size::from((own.size.w / 2, own.size.h / 2)));
            let alone =
                BlurElement::placed(Id::new(), ContextId::new(), own_tex, own, region, scale, 1.0).src();
            let at = |s: Rectangle<f64, BufferCoords>, origin: Point<i32, Physical>| {
                (s.loc.x * 2.0 + origin.x as f64, s.loc.y * 2.0 + origin.y as f64)
            };
            assert_eq!(at(src, union.loc), (region.loc.x as f64, region.loc.y as f64));
            assert_eq!(at(alone, own.loc), (region.loc.x as f64, region.loc.y as f64));
            assert_eq!((src.size.w, src.size.h), (alone.size.w, alone.size.h));
            let s = el.src();
            assert!(s.loc.x >= 0.0 && s.loc.x + s.size.w <= (union.size.w / 2) as f64);
            assert!(s.loc.y >= 0.0 && s.loc.y + s.size.h <= (union.size.h / 2) as f64);
        }
        // One physical pixel in the shared texture's UV is 1 / union, so a bend
        // of `strength` px is strength / union.size.
        let mut u = vec![smithay::backend::renderer::gles::Uniform::new(
            "px_uv",
            px_uv_for((fb.w, fb.h)),
        )];
        set_px_uv(&mut u, (union.size.w, union.size.h));
        let UniformValue::_2f(x, y) = u[0].value else {
            panic!("px_uv is a vec2")
        };
        assert_eq!(x, 1.0 / union.size.w as f32);
        assert_eq!(y, 1.0 / union.size.h as f32);
        assert!(union.size.w > ca.size.w, "the union is wider than one member");
    }

    /// Phase 1c: the built region carries a margin and is bucketed, so a member
    /// moving inside it needs no rebuild; one leaving it does.
    #[test]
    fn a_member_moving_inside_the_margin_needs_no_rebuild() {
        let blur = blur(8, 4);
        let fb = Size::<i32, Physical>::from((2880, 1620));
        let needed = |region| member_crop(region, &blur, 2, fb, false).unwrap().1;
        let a = r(1000, 600, 400, 300);
        let built = padded_crop(needed(a), fb, 4, false);
        assert!(built.contains_rect(needed(a)));
        // The size is a bucket (or the whole side) and the corner is on the grid.
        assert!(built.size.w % CROP_BUCKET < 16 || built.size.w == fb.w);
        assert!(built.size.h % CROP_BUCKET < 16 || built.size.h == fb.h);
        assert!(built.loc.x % 16 == 0 && built.loc.y % 16 == 0);
        assert!(Rectangle::from_size(fb).contains_rect(built));
        // Every step of a drag inside the margin is still covered; no new key.
        let m = crop_margin(fb);
        assert_eq!(m, 360);
        for dx in (-m / 2..=m / 2).step_by(20) {
            let moved = r(a.loc.x + dx, a.loc.y + dx / 2, a.size.w, a.size.h);
            assert!(covers(Some(built), needed(moved)), "dx {dx}");
        }
        // Outside the margin it is not.
        assert!(!covers(
            Some(built),
            needed(r(a.loc.x + m + 300, a.loc.y, 400, 300))
        ));
        // Resize growth inside the margin is covered, past it is not.
        assert!(covers(
            Some(built),
            needed(r(1000, 600, 400 + m / 2, 300 + m / 2))
        ));
        assert!(!covers(Some(built), needed(r(1000, 600, 400 + 2 * m, 300))));
        // Never built: nothing is covered.
        assert!(!covers(None, needed(a)));
    }

    /// Chains are keyed by crop size, so the built sizes are few: every member
    /// position of one size lands in the same bucket.
    #[test]
    fn built_sizes_are_bucketed() {
        let blur = blur(8, 4);
        let fb = Size::<i32, Physical>::from((2880, 1620));
        let mut sizes = std::collections::BTreeSet::new();
        for x in (0..2400).step_by(37) {
            for y in (0..1300).step_by(61) {
                let c = member_crop(r(x, y, 400, 300), &blur, 2, fb, false).unwrap().1;
                let built = padded_crop(c, fb, 4, false);
                assert!(built.contains_rect(c));
                sizes.insert((built.size.w, built.size.h));
            }
        }
        // Two widths and two heights, each plus a grid-slide variant at the edge.
        assert!(sizes.len() <= 8, "{sizes:?}");
    }

    /// A tier change or new membership is a rebuild whatever the region is
    /// (`group_refresh` ORs `!same_members` into `fresh`); the margin test
    /// itself is only about geometry, and a member at the output edge clamps.
    #[test]
    fn the_padded_region_stays_on_the_output() {
        let blur = blur(8, 4);
        let fb = Size::<i32, Physical>::from((1920, 1080));
        for region in [r(0, 0, 300, 200), r(1700, 900, 220, 180), r(0, 0, 1920, 1080)] {
            let c = member_crop(region, &blur, 0, fb, true).unwrap().1;
            let built = padded_crop(c, fb, 4, true);
            assert!(
                Rectangle::from_size(fb).contains_rect(built),
                "{region:?} {built:?}"
            );
            assert!(built.contains_rect(c));
        }
    }

    /// The shared texture addresses a member by its offset into the *built*
    /// region: moving a member inside the margin changes only that offset.
    #[test]
    fn a_moved_member_changes_only_its_uv_offset() {
        let blur = blur(8, 4);
        let fb = Size::<i32, Physical>::from((2880, 1620));
        let a = r(1000, 600, 400, 300);
        let built = padded_crop(member_crop(a, &blur, 2, fb, false).unwrap().1, fb, 4, false);
        let tex = Backdrop(Size::from((built.size.w / 2, built.size.h / 2)));
        let scale = Scale::from(1.0);
        let src = |region| {
            BlurElement::placed(
                Id::new(),
                ContextId::new(),
                tex.clone(),
                built,
                region,
                scale,
                1.0,
            )
            .src()
        };
        let (s0, s1) = (src(a), src(r(1100, 640, 400, 300)));
        assert_eq!((s1.loc.x - s0.loc.x, s1.loc.y - s0.loc.y), (50.0, 20.0));
        assert_eq!(s0.size, s1.size);
    }

    /// Chromatic aberration: a packed result is twice as wide, with the blurred
    /// part in its left half, so an element addresses the left half only and its
    /// source rectangle is the unpacked one's, in the same texels.
    #[test]
    fn a_packed_element_addresses_the_left_half() {
        let region = r(1000, 500, 400, 300);
        let (_, crop) = crop_rects(region, 64, Size::from((1920, 1080)), 3, false).unwrap();
        let (hw, hh) = (crop.size.w / 2, crop.size.h / 2);
        let scale = Scale::from(1.0);
        let one = BlurElement::placed(
            Id::new(),
            ContextId::new(),
            Backdrop(Size::from((hw, hh))),
            crop,
            region,
            scale,
            1.0,
        );
        let two = BlurElement::placed_in(
            Id::new(),
            ContextId::new(),
            Backdrop(Size::from((hw * 2, hh))),
            crop,
            region,
            scale,
            1.0,
            true,
        );
        assert_eq!(one.src(), two.src(), "same texels, only the texture is wider");
        let s = two.src();
        assert!(s.loc.x + s.size.w <= hw as f64, "inside the left half");
    }

    /// The chromatic fringe is real pixels: the UV offset times the sampled
    /// texture's size in px is the same fringe whatever the output or the tier's
    /// union is, packed (half-width UV px) or not.
    #[test]
    fn the_fringe_is_physical_px_whatever_the_union_size() {
        use crate::effects::{fringe_px, px_uv_for, DISPERSION_PX};
        for upscale in [1.0, 1.5, 2.0] {
            let want = DISPERSION_PX as f64 * upscale;
            assert!((fringe_px(1.0, upscale) as f64 - want).abs() < 1e-4);
            assert!((fringe_px(0.5, upscale) as f64 - want / 2.0).abs() < 1e-4);
            for size in [(800, 600), (1536, 1024), (2880, 1620)] {
                // Unpacked: the texture spans `size` px across UV 0..1.
                let uv = px_uv_for(size);
                let off = fringe_px(1.0, upscale) * uv[0];
                assert!((off as f64 * size.0 as f64 - want).abs() < 1e-3, "{size:?}");
                // Packed: the blurred half spans `size` px across UV 0..0.5.
                let uv = px_uv_for((size.0 * 2, size.1));
                let off = fringe_px(1.0, upscale) * uv[0];
                assert!((off as f64 * size.0 as f64 * 2.0 - want).abs() < 1e-3, "{size:?}");
            }
        }
        assert_eq!(fringe_px(0.0, 2.0), 0.0);
        assert_eq!(fringe_px(7.0, 1.0), DISPERSION_PX, "dispersion is clamped to 1");
    }

    /// The glass program's taps stay inside the built region: the reach covers
    /// the bend plus the widest fringe at every scale; without dispersion it is
    /// the old reach.
    #[test]
    fn the_reach_covers_the_widest_fringe() {
        use crate::effects::{fringe_px, glass_reach};
        for scale in [1.0, 1.25, 2.0] {
            for refraction in [0.0f32, 1.5, 12.0] {
                let bend = (refraction as f64 * scale).ceil() as i32;
                assert_eq!(
                    glass_reach(refraction, 0.0, scale),
                    (refraction as f64 * scale * 1.05).ceil() as i32
                );
                for d in [0.25f32, 0.5, 1.0] {
                    let reach = glass_reach(refraction, d, scale);
                    assert!(
                        reach as f64 >= refraction as f64 * scale + fringe_px(d, scale) as f64,
                        "{scale} {refraction} {d}"
                    );
                    assert!(reach > bend);
                }
            }
        }
        // And the margin test: damage within the fringe of the region invalidates.
        let region = r(100, 100, 200, 200);
        let k = kernel_radius(&blur(8, 2));
        let d = [r(100 - k - 6, 150, 2, 2)];
        assert!(!invalidates(region, k + glass_reach(0.0, 0.0, 1.0), &d));
        assert!(invalidates(region, k + glass_reach(0.0, 1.0, 1.0), &d));
    }

    /// A runtime dispersion toggle changes what the tier is built as (packed or
    /// not), which forces a full rebuild and reallocates the result; neither a
    /// change in blur mode nor in the other glass knobs does.
    #[test]
    fn toggling_dispersion_rebuilds_the_tier_in_full() {
        let mut b = blur(8, 4);
        assert!(!packs(&b, GlassPreset::Frost));
        let off = (b.size, 4, packs(&b, GlassPreset::Frost));
        let needed = r(0, 0, 512, 512);
        let built = Some(r(0, 0, 1024, 1024));
        assert!(!tier_fresh(true, built, needed, off, off, true));
        b.glass.dispersion = 0.5;
        assert!(packs(&b, GlassPreset::Frost));
        let on = (b.size, 4, packs(&b, GlassPreset::Frost));
        assert!(
            tier_fresh(true, built, needed, off, on, true),
            "on: allocate the packed texture"
        );
        assert!(tier_fresh(true, built, needed, on, off, true), "off: drop it");
        assert!(!tier_fresh(true, built, needed, on, on, true));
        // Only glass mode packs; another dispersion value stays packed.
        b.glass.dispersion = 1.0;
        assert_eq!((b.size, 4, packs(&b, GlassPreset::Frost)), on);
        b.mode = ec_abyss_config::BlurMode::Frost;
        assert!(!packs(&b, GlassPreset::Frost));
        // The usual reasons stay.
        assert!(tier_fresh(false, built, needed, on, on, true));
        assert!(tier_fresh(true, None, needed, on, on, true));
        assert!(tier_fresh(true, built, needed, on, on, false));
    }

    /// The pass count and packing are per preset: a lens is a light blur, packed
    /// even without dispersion; frost keeps the configured passes and packs
    /// only for dispersion. A tier takes the heaviest of its members.
    #[test]
    fn presets_pick_their_own_passes_and_packing() {
        let b = blur(8, 4);
        assert_eq!(preset_passes(4, GlassPreset::Lens), 2);
        assert_eq!(preset_passes(1, GlassPreset::Lens), 1);
        assert_eq!(preset_passes(4, GlassPreset::Regular), 3);
        assert_eq!(preset_passes(2, GlassPreset::Regular), 2);
        assert_eq!(preset_passes(4, GlassPreset::Frost), 4);
        assert_eq!(tuned(&b, GlassPreset::Lens).passes, 2);
        // Only glass mode retunes; frost/blur mode keep the configured passes.
        let mut f = blur(8, 4);
        f.mode = ec_abyss_config::BlurMode::Frost;
        assert_eq!(tuned(&f, GlassPreset::Lens).passes, 4);
        assert!(packs(&b, GlassPreset::Lens) && packs(&b, GlassPreset::Regular));
        assert!(!packs(&b, GlassPreset::Frost));
        assert!(!packs(&f, GlassPreset::Lens));
        // A tier with a lens and a frost member is as heavy as frost and packed.
        let (t, sharp) = tier_variant(&b, [GlassPreset::Lens, GlassPreset::Frost].into_iter());
        assert_eq!((t.passes, sharp), (4, true));
        let (t, sharp) = tier_variant(&b, [GlassPreset::Lens].into_iter());
        assert_eq!((t.passes, sharp), (2, true));
        let (t, sharp) = tier_variant(&b, [GlassPreset::Frost].into_iter());
        assert_eq!((t.passes, sharp), (4, false));
    }

    /// A member's preset changing (frost to lens and back) changes the tier's
    /// `(size, passes, packed)` once, which `tier_fresh` turns into exactly one
    /// full rebuild; unchanged presets leave it alone.
    #[test]
    fn a_preset_change_forces_exactly_one_full_rebuild() {
        let b = blur(8, 4);
        let needed = r(0, 0, 512, 512);
        let built = Some(r(0, 0, 1024, 1024));
        let params = |presets: &[GlassPreset]| {
            let (t, sharp) = tier_variant(&b, presets.iter().copied());
            (t.size, t.passes as usize, sharp)
        };
        let frost = params(&[GlassPreset::Frost]);
        let lens = params(&[GlassPreset::Lens]);
        assert_ne!(frost, lens);
        // The frame the preset changes: fresh, then stored as the new params.
        assert!(tier_fresh(true, built, needed, frost, lens, true));
        // Every later frame with the same preset: not fresh.
        assert!(!tier_fresh(true, built, needed, lens, lens, true));
        assert!(!tier_fresh(
            true,
            built,
            needed,
            lens,
            params(&[GlassPreset::Lens]),
            true
        ));
        // And back: one more.
        assert!(tier_fresh(true, built, needed, lens, frost, true));
        assert!(!tier_fresh(true, built, needed, frost, frost, true));
    }

    /// An opaque slab at a place, for the cull test.
    struct Slab(Id, Rectangle<i32, Physical>, Vec<Rectangle<i32, Physical>>);

    impl Element for Slab {
        fn id(&self) -> &Id {
            &self.0
        }
        fn current_commit(&self) -> CommitCounter {
            CommitCounter::default()
        }
        fn src(&self) -> Rectangle<f64, BufferCoords> {
            Rectangle::from_size((1.0, 1.0).into())
        }
        fn geometry(&self, _: Scale<f64>) -> Rectangle<i32, Physical> {
            self.1
        }
        fn transform(&self) -> Transform {
            Transform::Normal
        }
        fn damage_since(&self, _: Scale<f64>, _: Option<CommitCounter>) -> DamageSet<i32, Physical> {
            DamageSet::default()
        }
        fn opaque_regions(&self, _: Scale<f64>) -> OpaqueRegions<i32, Physical> {
            self.2.iter().copied().collect()
        }
        fn alpha(&self) -> f32 {
            1.0
        }
        fn kind(&self) -> Kind {
            Kind::Unspecified
        }
    }

    /// Occlusion cull: glass wholly under one opaque rectangle above it is hidden;
    /// partly covered, covered only by a translucent element, or covered only by
    /// the union of two opaque ones is not (conservative).
    #[test]
    fn glass_under_an_opaque_rectangle_is_hidden() {
        let scale = Scale::from(1.0);
        let region = r(100, 100, 200, 100);
        let opaque = |g, o| Slab(Id::new(), g, vec![o]);
        let slab = r(50, 50, 400, 300);
        assert!(hidden_behind(region, &[opaque(slab, slab)], scale));
        // The opaque part is the element's own, relative to its geometry.
        assert!(hidden_behind(region, &[opaque(slab, r(0, 0, 400, 300))], scale));
        // Partly covered.
        assert!(!hidden_behind(
            region,
            &[opaque(r(150, 50, 400, 300), r(150, 50, 400, 300))],
            scale
        ));
        // Translucent (no opaque region): never hides.
        assert!(!hidden_behind(region, &[Slab(Id::new(), slab, vec![])], scale));
        // Opaque region smaller than the element.
        assert!(!hidden_behind(region, &[opaque(slab, r(0, 0, 100, 100))], scale));
        // Two halves together cover it, neither alone: kept (conservative).
        let halves = [
            opaque(r(100, 100, 100, 100), r(100, 100, 100, 100)),
            opaque(r(200, 100, 100, 100), r(200, 100, 100, 100)),
        ];
        assert!(!hidden_behind(region, &halves, scale));
        assert!(!hidden_behind::<Slab>(region, &[], scale));
    }

    /// A half-size result is sampled at half the region's offset and size.
    #[test]
    fn a_half_size_result_is_sampled_at_half_coordinates() {
        let region = r(1000, 500, 400, 300);
        let (_, crop) = crop_rects(region, 64, Size::from((1920, 1080)), 3, false).unwrap();
        let tex = Backdrop(Size::from((crop.size.w / 2, crop.size.h / 2)));
        let scale = Scale::from(1.0);
        let el = BlurElement::placed(Id::new(), ContextId::new(), tex, crop, region, scale, 1.0);
        assert_eq!(el.geometry(scale), region);
        let want = Rectangle::<f64, BufferCoords>::new(
            (
                (region.loc.x - crop.loc.x) as f64 / 2.0,
                (region.loc.y - crop.loc.y) as f64 / 2.0,
            )
                .into(),
            (200.0, 150.0).into(),
        );
        assert_eq!(el.src(), want);
        let s = el.src();
        assert!(s.loc.x + s.size.w <= (crop.size.w / 2) as f64);
        assert!(s.loc.y + s.size.h <= (crop.size.h / 2) as f64);
    }

    #[test]
    fn the_crop_covers_the_kernel_snapped_to_the_chain_grid() {
        let fb = Size::from((1920, 1080));
        let region = r(1003, 501, 400, 300);
        for passes in 1..=6usize {
            let d = 1 << passes;
            let (tex, crop) = crop_rects(region, 40, fb, passes, false).unwrap();
            assert_eq!(tex, crop, "unmirrored: same rectangle");
            assert!(crop.contains_rect(grow(region, 40)), "passes {passes}");
            assert_eq!(crop.loc.x % d, 0);
            assert_eq!(crop.loc.y % d, 0);
            assert_eq!((crop.loc.x + crop.size.w) % d, 0);
            assert_eq!((crop.loc.y + crop.size.h) % d, 0);
        }
    }

    #[test]
    fn the_crop_clamps_to_the_output_edges() {
        let fb = Size::from((1920, 1080));
        // Touching every edge: the crop is the whole output, odd size included.
        let (_, crop) = crop_rects(r(0, 0, 1920, 1080), 64, fb, 3, false).unwrap();
        assert_eq!(crop, r(0, 0, 1920, 1080));
        let odd = Size::from((1001, 777));
        let (_, crop) = crop_rects(r(900, 700, 101, 77), 64, odd, 3, false).unwrap();
        assert_eq!(crop.loc.x + crop.size.w, 1001);
        assert_eq!(crop.loc.y + crop.size.h, 777);
        // Overhanging and fully off-screen regions.
        let (_, crop) = crop_rects(r(-100, -50, 300, 200), 16, fb, 2, false).unwrap();
        assert_eq!(crop.loc, (0, 0).into());
        assert!(crop_rects(r(5000, 5000, 10, 10), 16, fb, 2, false).is_none());
    }

    /// Under `Flipped180` the backdrop texture is stored bottom row first: the
    /// texture rectangle is the physical one mirrored in y, and snaps on the
    /// texture's own grid.
    #[test]
    fn a_mirrored_crop_is_mirrored_in_the_texture_and_snapped_there() {
        let fb = Size::from((1920, 1080));
        let region = r(1003, 40, 400, 300);
        let (tex, crop) = crop_rects(region, 40, fb, 3, true).unwrap();
        assert_eq!(tex.loc.y % 8, 0);
        assert_eq!((tex.loc.y + tex.size.h) % 8, 0);
        assert_eq!(tex.loc.y, 1080 - (crop.loc.y + crop.size.h));
        assert_eq!(tex.size, crop.size);
        assert!(crop.contains_rect(grow(region, 40).intersection(r(0, 0, 1920, 1080)).unwrap()));
        // A region on the physical top edge sits on the texture's last row.
        let (tex, crop) = crop_rects(r(0, 0, 400, 30), 8, fb, 1, true).unwrap();
        assert_eq!(crop.loc.y, 0);
        assert_eq!(tex.loc.y + tex.size.h, 1080);
    }
}
