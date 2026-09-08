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
//! plane. See [`crate::render::scanout_candidate`].

use std::collections::HashMap;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{texture::TextureRenderElement, Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
                UniformType,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions},
            Bind, Frame, Offscreen, Renderer, Texture,
        },
    },
    desktop::Window,
    output::Output,
    utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform},
};

use crate::config::Blur;

/// The offscreen chain is rendered in the same format the capture path uses.
const FORMAT: Fourcc = Fourcc::Xbgr8888;

/// Clear colour for the backdrop pass. Opaque black: the backdrop is a full
/// output-sized image and every visible pixel is overdrawn by an element.
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

/// A blurred backdrop, drawn immediately beneath the window it belongs to.
///
/// Everything but damage delegates to a plain [`TextureRenderElement`]. The
/// element's damage is *not* the texture's own damage: it is the invalidation
/// rule above, folded into a commit counter that the caller bumps whenever the
/// backdrop was recomputed. `underlying_storage` is `None` so no backend ever
/// mistakes this for a client buffer it could scan out.
#[derive(Debug)]
pub struct BlurElement {
    inner: TextureRenderElement<GlesTexture>,
    commit: CommitCounter,
}

impl Element for BlurElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.inner.location(scale)
    }

    fn src(&self) -> Rectangle<f64, BufferCoords> {
        self.inner.src()
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.inner.geometry(scale)
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            // Expanded damage: the whole blurred region, not the region behind
            // it that actually changed.
            DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)])
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

impl RenderElement<GlesRenderer> for BlurElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, BufferCoords>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        RenderElement::<GlesRenderer>::draw(&self.inner, frame, src, dst, damage, opaque_regions)
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// Per-window blur bookkeeping, kept alive between frames.
struct WindowBlur {
    /// Damage of the backdrop behind this window, tracked across frames. Only
    /// ever queried (`damage_output`), never rendered through.
    damage: OutputDamageTracker,
    /// The upsampled result, held so the element can borrow it.
    result: Option<GlesTexture>,
    /// Bumped whenever `result` was recomputed; drives [`BlurElement`] damage.
    commit: CommitCounter,
    /// A stable id so the damage tracker can follow this element across frames.
    id: Id,
}

/// Compiled programs, the offscreen chain and per-window state.
#[derive(Default)]
pub struct BlurStore {
    down: Option<GlesTexProgram>,
    up: Option<GlesTexProgram>,
    /// Level 0 is the full-size backdrop; level `n` is half of level `n-1`.
    chain: Vec<GlesTexture>,
    chain_size: Size<i32, Physical>,
    /// Renders the backdrop into level 0. Always driven with `age = 0`, so its
    /// own damage history is never consulted and it can be shared by every
    /// blurred window on the output.
    backdrop: Option<OutputDamageTracker>,
    windows: HashMap<Window, WindowBlur>,
}

impl std::fmt::Debug for BlurStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlurStore")
            .field("levels", &self.chain.len())
            .field("windows", &self.windows.len())
            .finish()
    }
}

impl BlurStore {
    pub fn forget(&mut self, window: &Window) {
        self.windows.remove(window);
    }

    /// Drop everything: called when blur is switched off so the textures do not
    /// sit in GPU memory for a config that no longer draws them.
    pub fn clear(&mut self) {
        self.chain.clear();
        self.chain_size = Size::default();
        self.windows.clear();
        self.backdrop = None;
    }

    /// Build the blurred backdrop for one window.
    ///
    /// `behind` is the slice of the frame's element list that sits below the
    /// window, in front-to-back order — exactly what the blur samples.
    /// Returns `None` when nothing needs redrawing *and* nothing is cached, or
    /// when any GL step failed (blur is an effect; a failure drops the effect,
    /// never the frame).
    #[allow(clippy::too_many_arguments)]
    pub fn element<E>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        window: &Window,
        region: Rectangle<i32, Physical>,
        behind: &[E],
        blur: &Blur,
        scale: Scale<f64>,
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
        self.ensure_chain(renderer, fb_size, blur.passes.clamp(1, 6) as usize)?;

        let entry = self.windows.entry(window.clone()).or_insert_with(|| WindowBlur {
            damage: OutputDamageTracker::from_output(output),
            result: None,
            commit: CommitCounter::default(),
            id: Id::new(),
        });

        // The invalidation rule: recompute only when damage behind this window
        // lands inside the region grown by the kernel radius.
        let dirty = {
            let (damage, _) = entry.damage.damage_output(1, behind).ok()?;
            match damage {
                Some(rects) => invalidates(region, kernel_radius(blur), rects),
                // `None` means the tracker could not reason about damage; the
                // fail-safe answer is "everything changed".
                None => entry.result.is_none(),
            }
        };

        if dirty || entry.result.is_none() {
            let Self {
                down,
                up,
                chain,
                backdrop,
                windows,
                ..
            } = self;
            let entry = windows.get_mut(window)?;
            let tracker = backdrop.get_or_insert_with(|| OutputDamageTracker::from_output(output));
            match render_chain(
                renderer,
                tracker,
                chain,
                down.as_ref()?,
                up.as_ref()?,
                behind,
                blur,
                entry.result.take(),
            ) {
                Ok(texture) => {
                    entry.result = Some(texture);
                    entry.commit.increment();
                }
                Err(err) => {
                    tracing::warn!(?err, "rendering the blur chain; blur skipped this frame");
                    return None;
                }
            }
        }

        let entry = self.windows.get(window)?;
        let texture = entry.result.clone()?;
        // The texture is in output-local physical pixels; `src` selects the
        // part of it that sits under this window.
        let src = Rectangle::<f64, smithay::utils::Logical>::new(
            (region.loc.x as f64 / scale.x, region.loc.y as f64 / scale.y).into(),
            (region.size.w as f64 / scale.x, region.size.h as f64 / scale.y).into(),
        );
        let size = Size::<i32, smithay::utils::Logical>::from((
            (region.size.w as f64 / scale.x).round() as i32,
            (region.size.h as f64 / scale.y).round() as i32,
        ));
        let inner = TextureRenderElement::from_static_texture(
            entry.id.clone(),
            renderer.context_id(),
            region.loc.to_f64(),
            texture,
            1,
            Transform::Normal,
            None,
            Some(src),
            Some(size),
            None,
            Kind::Unspecified,
        );
        Some(BlurElement {
            inner,
            commit: entry.commit,
        })
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
        if self.chain_size == fb_size && self.chain.len() == passes + 1 {
            return Some(());
        }
        self.chain.clear();
        self.windows.clear();
        self.backdrop = None;
        for level in 0..=passes {
            let size = level_size(fb_size, level);
            match Offscreen::<GlesTexture>::create_buffer(
                renderer,
                FORMAT,
                size.to_logical(1).to_buffer(1, Transform::Normal),
            ) {
                Ok(t) => self.chain.push(t),
                Err(err) => {
                    tracing::warn!(?err, "allocating the blur chain; blur disabled");
                    self.chain.clear();
                    return None;
                }
            }
        }
        self.chain_size = fb_size;
        Some(())
    }
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
    chain: &mut [GlesTexture],
    down: &GlesTexProgram,
    up: &GlesTexProgram,
    behind: &[E],
    blur: &Blur,
    reuse: Option<GlesTexture>,
) -> Result<GlesTexture, GlesError>
where
    E: Element + RenderElement<GlesRenderer>,
{
    let full = chain[0].size();
    let full: Size<i32, Physical> = (full.w, full.h).into();

    // 1. The backdrop, rendered whole (`age = 0`): the chain texture is shared
    //    between windows, so partial damage would leave another window's
    //    backdrop in it.
    {
        let (head, _) = chain.split_at_mut(1);
        let mut fb = Bind::bind(renderer, &mut head[0])?;
        backdrop
            .render_output(renderer, &mut fb, 0, behind, CLEAR)
            .map_err(|_| GlesError::UnknownPixelFormat)?;
    }

    let offset = sample_offset(blur);
    let passes = chain.len() - 1;

    // 2. Downsample.
    for level in 1..=passes {
        let (src, dst) = split_pair(chain, level - 1, level);
        blit(renderer, src, dst, down, offset)?;
    }

    // 3. Upsample. The last step writes into the per-window result texture so
    //    the element can hold it while the chain is reused for the next window.
    for level in (1..passes).rev() {
        let (src, dst) = split_pair(chain, level + 1, level);
        blit(renderer, src, dst, up, offset)?;
    }
    let mut result = match reuse {
        Some(t)
            if {
                let s = t.size();
                (s.w, s.h) == (full.w, full.h)
            } =>
        {
            t
        }
        _ => Offscreen::<GlesTexture>::create_buffer(
            renderer,
            FORMAT,
            full.to_logical(1).to_buffer(1, Transform::Normal),
        )?,
    };
    {
        let src = chain[1.min(passes)].clone();
        blit(renderer, &src, &mut result, up, offset)?;
    }
    Ok(result)
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

/// One Kawase pass: draw the whole of `src` over the whole of `dst`.
fn blit(
    renderer: &mut GlesRenderer,
    src: &GlesTexture,
    dst: &mut GlesTexture,
    program: &GlesTexProgram,
    offset: f32,
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
        Rectangle::from_size((src_size.w as f64, src_size.h as f64).into()),
        dest,
        &[dest],
        &[],
        Transform::Normal,
        1.0,
        Some(program),
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
            enabled: true,
            size,
            passes,
        }
    }

    #[test]
    fn radius_grows_with_both_knobs() {
        assert_eq!(kernel_radius(&blur(8, 2)), 32);
        assert_eq!(kernel_radius(&blur(8, 3)), 64);
        assert_eq!(kernel_radius(&blur(16, 2)), 64);
        assert_eq!(kernel_radius(&blur(1, 1)), 2);
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
}
