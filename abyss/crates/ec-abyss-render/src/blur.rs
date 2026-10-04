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
    utils::{Buffer as BufferCoords, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::compositor::{RectangleKind, RegionAttributes},
};

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
/// `radius` and clamped to the `fb` output, as `(tex, phys)`. `phys` is that
/// rectangle in output-local physical pixels; `tex` is the same pixels in the
/// backdrop texture, which is stored bottom row first when `mirrored`
/// (`Flipped180`, see [`crate::effects::fb_y_mirrored`]).
///
/// `tex` is snapped outwards to the chain's coarsest level (`1 << passes`
/// px), so every level samples the same pixel grid as an output-sized chain
/// would: a window dragged across the output does not swim, and an edge that
/// meets the output edge still clamps exactly where the full chain did.
/// `None` when nothing of the grown region is on the output.
pub fn crop_rects(
    region: Rectangle<i32, Physical>,
    radius: i32,
    fb: Size<i32, Physical>,
    passes: usize,
    mirrored: bool,
) -> Option<(Rectangle<i32, Physical>, Rectangle<i32, Physical>)> {
    let want = grow(region, radius).intersection(Rectangle::from_size(fb))?;
    if want.is_empty() {
        return None;
    }
    let y = if mirrored {
        fb.h - (want.loc.y + want.size.h)
    } else {
        want.loc.y
    };
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
    let (x, w) = snap(want.loc.x, want.size.w, fb.w);
    let (ty, h) = snap(y, want.size.h, fb.h);
    let tex = Rectangle::new((x, ty).into(), (w, h).into());
    let py = if mirrored { fb.h - (ty + h) } else { ty };
    Some((tex, Rectangle::new((x, py).into(), (w, h).into())))
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
/// for the window's antialiased edge. Disjoint, and clamped to `size`.
pub fn ring_strips(size: Size<i32, Physical>, inner: i32, corner: i32) -> [Rectangle<i32, Physical>; 4] {
    let (w, h) = (size.w.max(0), size.h.max(0));
    let t = (inner + corner.max(0) + 1).clamp(0, h / 2);
    let side = (inner + 1).clamp(0, w / 2);
    let mid = h - 2 * t;
    [
        Rectangle::new((0, 0).into(), (w, t).into()),
        Rectangle::new((0, h - t).into(), (w, t).into()),
        Rectangle::new((0, t).into(), (side, mid).into()),
        Rectangle::new((w - side, t).into(), (side, mid).into()),
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
    uniforms: Vec<Uniform<'static>>,
    /// An opaque window's bezel strips (see [`ring_strips`]), element-local.
    ring: Option<[Rectangle<i32, Physical>; 4]>,
}

impl<T: Texture + Clone + 'static> BlurElement<T> {
    /// Place `texture` (buffer scale 1, upright, covering `crop` of the output
    /// at whatever resolution it was stored, e.g. half) so the element covers
    /// exactly `region` and samples exactly `region` of it, stretched.
    fn placed(
        id: Id,
        context: ContextId<T>,
        texture: T,
        crop: Rectangle<i32, Physical>,
        region: Rectangle<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> Self {
        // The result texture is built with a literal `texture_scale` of 1, so
        // `Element::src()` hands this rectangle straight to the GPU with no
        // further scaling. It must therefore already be in the texture's own
        // buffer space, i.e. `region` relative to `crop`, times the texture's
        // texels per output pixel (below 1 for a reduced-resolution result).
        let sx = texture.width() as f64 / crop.size.w.max(1) as f64;
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
            context,
            region.loc.to_f64(),
            texture,
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
            uniforms: Vec::new(),
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
        // An opaque bezel draws nothing inside its ring: skip those pixels.
        let clipped: Vec<_>;
        let damage = match &self.ring {
            Some(strips) => {
                clipped = damage
                    .iter()
                    .flat_map(|d| strips.iter().filter_map(move |s| s.intersection(*d)))
                    .collect();
                &clipped[..]
            }
            None => damage,
        };
        if damage.is_empty() {
            return Ok(());
        }
        if let Some(program) = &self.program {
            frame.override_default_tex_program(program.clone(), self.uniforms.clone());
            let res =
                RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions);
            frame.clear_tex_program_override();
            res
        } else {
            RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions)
        }
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
}

/// Signed distance from `p` (output-local physical px) to `shape`, negative
/// inside: each box rounded by `radius` (clamped to its half size), unioned
/// in order by the polynomial smooth-min `min(a, b) - h² k / 4` with
/// `h = max(k - |a - b|, 0) / k` and `k = shape.fillet`.
///
/// This is the mask the shaped backdrop programs draw (`SHAPE_FNS` in
/// [`crate::effects`], line for line). A client that paints a fill
/// under the same shape uses the same formula so its tint meets the blur's
/// edge.
pub fn shape_sd(shape: &Shape, radius: f32, p: (f32, f32)) -> f32 {
    let box_sd = |b: &Rectangle<i32, Physical>| {
        let (hx, hy) = (b.size.w as f32 * 0.5, b.size.h as f32 * 0.5);
        let r = radius.min(hx.min(hy)).max(0.0);
        let qx = (p.0 - (b.loc.x as f32 + hx)).abs() - hx + r;
        let qy = (p.1 - (b.loc.y as f32 + hy)).abs() - hy + r;
        qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
    };
    let k = shape.fillet;
    let smin = |a: f32, b: f32| {
        if k <= 0.0 {
            return a.min(b);
        }
        let h = (k - (a - b).abs()).max(0.0) / k;
        a.min(b) - h * h * k * 0.25
    };
    let mut rects = shape.rects().iter();
    let mut d = rects.next().map_or(f32::INFINITY, box_sd);
    for b in rects {
        d = smin(d, box_sd(b));
    }
    d
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
    Window(Window),
    Layer(LayerSurface),
    /// An annotation panel (ADR 0071), by its handle. Annotations keep their
    /// own [`BlurStore`], so this never shares a table with a surface.
    Annotation(u64),
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
    look: Vec<Uniform<'static>>,
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
    chain: HashMap<(i32, i32), Vec<GlesTexture>>,
    chain_size: Size<i32, Physical>,
    chain_passes: usize,
    /// Renders the backdrop into level 0. Always driven with `age = 0`, so its
    /// own damage history is never consulted and it can be shared by every
    /// blurred window on the output.
    backdrop: Option<OutputDamageTracker>,
    surfaces: HashMap<BlurKey, SurfaceBlur>,
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
        self.surfaces.retain(|key, _| keep.contains(key));
    }

    /// Drop everything: called when blur is switched off so the textures do not
    /// sit in GPU memory for a config that no longer draws them.
    pub fn clear(&mut self) {
        self.chain.clear();
        self.level0 = None;
        self.chain_size = Size::default();
        self.surfaces.clear();
        self.backdrop = None;
    }

    /// Build the blurred backdrop for one surface.
    ///
    /// `behind` is the slice of the frame's element list that sits below the
    /// surface, in front-to-back order — exactly what the blur samples.
    /// `reach` is how far past `region` the final draw samples on top of the
    /// kernel (the glass refraction; 0 for every other mode), so damage that
    /// far out invalidates too. `ring` is an opaque window's bezel, as
    /// `(width, corner radius)` in physical px: only its strips are ever seen,
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
        rounding: Option<(GlesTexProgram, Vec<Uniform<'static>>)>,
        reach: i32,
        ring: Option<(i32, i32)>,
        shape: Option<&Shape>,
        alpha: f32,
    ) -> Option<BlurElement>
    where
        E: Element + RenderElement<GlesRenderer>,
    {
        let mode = output.current_mode()?;
        let fb_size = output.current_transform().transform_size(mode.size);
        if fb_size.w <= 0 || fb_size.h <= 0 || region.size.w <= 0 || region.size.h <= 0 {
            return None;
        }
        self.ensure_programs(renderer)?;
        let passes = blur.passes.clamp(1, 6) as usize;
        self.ensure_chain(renderer, fb_size, passes)?;
        let mirrored = crate::effects::fb_y_mirrored(output.current_transform()) == Some(true);
        // Only this much of the backdrop is ever sampled (kernel plus the
        // final draw's own reach), so only this much is computed.
        let sampled = kernel_radius(blur).saturating_add(reach.max(0));
        let (tex, crop) = crop_rects(region, sampled, fb_size, passes, mirrored)?;

        let entry = self.surfaces.entry(key.clone()).or_insert_with(|| SurfaceBlur {
            damage: OutputDamageTracker::from_output(output),
            result: None,
            crop: None,
            commit: CommitCounter::default(),
            id: Id::new(),
            look: Vec::new(),
        });

        // The invalidation rule: recompute only when damage behind this surface
        // lands inside the region (or only the bezel's ring of it) grown by
        // the kernel radius.
        let strips = ring.map(|(inner, corner)| ring_strips(region.size, inner, corner));
        let dirty = {
            let (damage, _) = entry.damage.damage_output(1, behind).ok()?;
            let radius = kernel_radius(blur).saturating_add(reach.max(0));
            match damage {
                Some(rects) => match (&strips, shape) {
                    (Some(strips), _) => strips
                        .iter()
                        .any(|s| invalidates(Rectangle::new(s.loc + region.loc, s.size), radius, rects)),
                    (None, Some(shape)) => {
                        let radius = radius.saturating_add(shape.fillet.ceil() as i32);
                        shape.rects().iter().any(|b| invalidates(*b, radius, rects))
                    }
                    (None, None) => invalidates(region, radius, rects),
                },
                // `None` means the tracker could not reason about damage; the
                // fail-safe answer is "everything changed".
                None => entry.result.is_none(),
            }
        };
        if dirty || entry.result.is_none() || entry.crop != Some(crop) {
            let Self {
                down,
                up,
                level0,
                chain,
                backdrop,
                surfaces,
                ..
            } = self;
            if chain.len() >= MAX_CHAINS && !chain.contains_key(&(crop.size.w, crop.size.h)) {
                chain.clear();
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = chain.entry((crop.size.w, crop.size.h)) {
                match alloc_levels(renderer, crop.size, passes) {
                    Ok(levels) => {
                        slot.insert(levels);
                    }
                    Err(err) => {
                        tracing::warn!(?err, "allocating the blur chain; blur skipped this frame");
                        return None;
                    }
                }
            }
            let entry = surfaces.get_mut(key)?;
            let tracker = backdrop.get_or_insert_with(|| OutputDamageTracker::from_output(output));
            // The backdrop is drawn with the output's own transform (so the
            // `gl_FragCoord` rounding masks of the elements behind still line
            // up), which under `Flipped180` (winit) stores it bottom row
            // first. The result is sampled back as a `Normal` texture, so the
            // last pass flips it upright; otherwise every window would blur
            // the vertical mirror image of what is behind it.
            let orient = match crate::effects::fb_y_mirrored(output.current_transform()) {
                Some(true) => Transform::Flipped180,
                _ => Transform::Normal,
            };
            match render_chain(
                renderer,
                tracker,
                level0.as_mut()?,
                chain.get_mut(&(crop.size.w, crop.size.h))?,
                (tex, crop),
                down.as_ref()?,
                up.as_ref()?,
                behind,
                blur,
                entry.result.take(),
                orient,
            ) {
                Ok(texture) => {
                    entry.result = Some(texture);
                    entry.crop = Some(crop);
                    entry.commit.increment();
                }
                Err(err) => {
                    tracing::warn!(?err, "rendering the blur chain; blur skipped this frame");
                    return None;
                }
            }
        }

        let entry = self.surfaces.get_mut(key)?;
        let (program, uniforms) = match rounding {
            Some((program, uniforms)) => (Some(program), uniforms),
            None => (None, Vec::new()),
        };
        if entry.look != uniforms {
            entry.look.clone_from(&uniforms);
            entry.commit.increment();
        }
        let texture = entry.result.clone()?;
        let mut element = BlurElement::placed(
            entry.id.clone(),
            renderer.context_id(),
            texture,
            crop,
            region,
            scale,
            alpha,
        );
        element.commit = entry.commit;
        element.program = program;
        element.uniforms = uniforms;
        element.ring = strips;
        Some(element)
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

    fn ensure_chain(
        &mut self,
        renderer: &mut GlesRenderer,
        fb_size: Size<i32, Physical>,
        passes: usize,
    ) -> Option<()> {
        if self.chain_size == fb_size && self.chain_passes == passes && self.level0.is_some() {
            return Some(());
        }
        self.chain.clear();
        self.level0 = None;
        self.surfaces.clear();
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
        self.chain_passes = passes;
        Some(())
    }
}

/// Most distinct crop sizes whose chains are kept before the pool is reset.
const MAX_CHAINS: usize = 16;

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
    down: &GlesTexProgram,
    up: &GlesTexProgram,
    behind: &[E],
    blur: &Blur,
    reuse: Option<GlesTexture>,
    orient: Transform,
) -> Result<GlesTexture, GlesError>
where
    E: Element + RenderElement<GlesRenderer>,
{
    // 1. The backdrop, rendered whole (`age = 0`): the chain texture is shared
    //    between windows, so partial damage would leave another window's
    //    backdrop in it. Each element is cut to `crop` so only the part the
    //    blur samples is drawn; the texture itself stays output-sized, which
    //    keeps `gl_FragCoord` (the elements' rounding masks) where they expect.
    {
        let cropped: Vec<_> = behind
            .iter()
            .filter_map(|e| CropRenderElement::from_element(e, Scale::from(1.0), crop))
            .collect();
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
    blit(
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
        blit(renderer, src, dst, Some(down), offset, Transform::Normal, None)?;
    }

    // 3. Upsample down to level 2. The last step writes level 1 (half size)
    //    into the per-window result texture so the element can hold it while
    //    the chain is reused for the next window; the element stretches it to
    //    the region with bilinear filtering. One pass has no level 2: level 1
    //    is copied as is.
    for level in (2..passes).rev() {
        let (src, dst) = split_pair(mid, level, level - 1);
        blit(renderer, src, dst, Some(up), offset, Transform::Normal, None)?;
    }
    let half = mid[0].size();
    let mut result = sized(renderer, reuse, half)?;
    if passes >= 2 {
        let src = mid[1].clone();
        blit(renderer, &src, &mut result, Some(up), offset, orient, None)?;
    } else {
        let src = mid[0].clone();
        blit(renderer, &src, &mut result, None, offset, orient, None)?;
    }
    Ok(result)
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

/// One Kawase pass: draw the whole of `src` over the whole of `dst`, sampling
/// `src` (or just its `from` rectangle) under `transform` (`Normal` except for
/// the final, upright-ing pass), through `program` or the default shader. The half-pixel is always one of `src`'s own.
fn blit(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    program: Option<&GlesTexProgram>,
    offset: f32,
    transform: Transform,
    from: Option<Rectangle<f64, BufferCoords>>,
) -> Result<(), GlesError> {
    let src_size = src.size();
    let dst_size = dst.size();
    let dest = Rectangle::<i32, Physical>::from_size((dst_size.w, dst_size.h).into());
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
        &[dest],
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

    /// COMP-02 §3: the glass refraction widens the invalidation reach by its
    /// displacement (with the dispersion margin), scaled to physical px;
    /// flat glass adds nothing.
    #[test]
    fn glass_refraction_widens_the_invalidation_reach() {
        use crate::effects::glass_reach;
        assert_eq!(glass_reach(0, 1.5), 0);
        assert_eq!(glass_reach(12, 1.0), 13);
        assert_eq!(glass_reach(12, 2.0), 26);
        let region = r(100, 100, 200, 200);
        let k = kernel_radius(&blur(8, 2));
        let just_outside = [r(100 - k - 5, 150, 2, 2)];
        assert!(!invalidates(region, k, &just_outside));
        assert!(invalidates(region, k + glass_reach(12, 1.0), &just_outside));
    }

    /// An opaque window's bezel: the strips cover the ring and its corners,
    /// never overlap, and leave the window's middle out.
    #[test]
    fn ring_strips_cover_only_the_bezel() {
        // A 300x200 window inside a 6 px bezel, corner radius 9.
        let size = Size::<i32, Physical>::from((312, 212));
        let strips = ring_strips(size, 6, 9);
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

    #[test]
    fn ring_strips_clamp_to_a_tiny_region() {
        for s in ring_strips(Size::from((10, 6)), 6, 9) {
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
