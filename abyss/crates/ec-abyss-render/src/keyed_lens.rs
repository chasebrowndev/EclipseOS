// SPDX-License-Identifier: AGPL-3.0-only
//! Keyed lens (ADR 0132): the pure arithmetic behind the lens's label and
//! distance maps. The GPU passes in `effects` write and read exactly these
//! encodings, so they are kept here, state-free, where the tests can pin them.
//!
//! A lens is drawn over the pixels a keyed strip matched (ADR 0079/0080). Per
//! texel the label pass records which key matched and whether that key is a
//! lens: the label is stored in the blue channel as `label / 4`, where `0` is
//! no lens and `i + 1` is a lens for key `i`. The distance pass stores, in the
//! red channel, the distance to the nearest texel of another label over
//! [`effects::KEYED_LENS_DIST_MAX`] px, clamped to 1 on the plateau.

use crate::blur::{self, BlurElement, BlurKey};
use crate::effects::{self, FbMap, GlassPreset, KEYED_LENS_DIST_MAX};
use crate::gpu_timer::Span;
use crate::userdata::Keys;
use ec_abyss_config::GlassBlur;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{surface::WaylandSurfaceRenderElement, Element, RenderElement},
            gles::{GlesRenderer, GlesTexProgram, GlesTexture, Uniform},
            utils::CommitCounter,
            Bind, Color32F, Frame, Offscreen, Renderer, Texture,
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform},
};

/// The lens passes' programs, compiled on first use and kept per renderer
/// (ADR 0132). A compile that failed is remembered, so a frame does not retry
/// it; the caller then draws the flat key as before.
#[derive(Default)]
pub struct Programs {
    label: Option<Result<GlesTexProgram, ()>>,
    dist: Option<Result<GlesTexProgram, ()>>,
    glass: Option<Result<GlesTexProgram, ()>>,
}

type Compile = fn(&mut GlesRenderer) -> Result<GlesTexProgram, smithay::backend::renderer::gles::GlesError>;

fn cached(
    slot: &mut Option<Result<GlesTexProgram, ()>>,
    renderer: &mut GlesRenderer,
    what: &'static str,
    compile: Compile,
) -> Option<GlesTexProgram> {
    slot.get_or_insert_with(|| {
        compile(renderer)
            .map_err(|err| tracing::warn!(?err, what, "compiling a keyed-lens shader; keys stay flat"))
    })
    .clone()
    .ok()
}

impl Programs {
    /// The label, distance and lens-glass programs, or `None` if any failed.
    pub fn get(
        &mut self,
        renderer: &mut GlesRenderer,
    ) -> Option<(GlesTexProgram, GlesTexProgram, GlesTexProgram)> {
        let label = cached(&mut self.label, renderer, "label", effects::compile_key_label)?;
        let dist = cached(
            &mut self.dist,
            renderer,
            "distance",
            effects::compile_key_distance,
        )?;
        let glass = cached(
            &mut self.glass,
            renderer,
            "lens glass",
            effects::compile_keyed_lens,
        )?;
        Some((label, dist, glass))
    }
}

/// A keyed window's lens, asked for by `window_elements` and spliced by
/// `insert_blur` above the window's backdrop (ADR 0132). The mask is built
/// from the window's own pixels, so it is ready before the backdrop exists.
pub struct LensRequest {
    pub key: BlurKey,
    /// The lens region, output-local physical: the backdrop's region exactly.
    pub region: Rectangle<i32, Physical>,
    /// The window's rounded rectangle, cutting the lens at its corners.
    pub round: Rectangle<i32, Physical>,
    pub radius: f32,
    pub fb: FbMap,
    pub keys: Keys,
    /// Distance field (red) and label (blue), the lens region's size.
    pub mask: GlesTexture,
}

/// The distance field of a lens region: its keyed pixels labelled by
/// `KEY_LABEL_SRC` from the window's own surfaces, then the distance to the
/// nearest other label by `KEY_DISTANCE_SRC`. `origin` is the region's
/// output-local top-left, so each surface lands in the target's own space.
pub fn mask(
    renderer: &mut GlesRenderer,
    programs: &(GlesTexProgram, GlesTexProgram, GlesTexProgram),
    surfaces: &[WaylandSurfaceRenderElement<GlesRenderer>],
    origin: Point<i32, Physical>,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    keys: &Keys,
) -> Option<GlesTexture> {
    let (label_prog, dist_prog, _) = programs;
    let (w, h) = (size.w.max(1), size.h.max(1));
    let size = Size::<i32, Physical>::from((w, h));
    let buf = Size::<i32, Buffer>::from((w, h));
    let full = Rectangle::from_size(size);
    let label = timed(renderer, Span::LensLabel, |renderer| {
        let mut label = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Xbgr8888, buf).ok()?;
        {
            let mut target = Bind::bind(renderer, &mut label).ok()?;
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[full]).ok()?;
            let uniforms = effects::key_label_uniforms(EVERYWHERE, keys);
            for s in surfaces {
                let geo = s.geometry(scale);
                let dst = Rectangle::new(geo.loc - origin, geo.size);
                frame.override_default_tex_program(label_prog.clone(), uniforms.clone());
                let res = RenderElement::<GlesRenderer>::draw(
                    s,
                    &mut frame,
                    s.src(),
                    dst,
                    &[Rectangle::from_size(dst.size)],
                    &[],
                );
                frame.clear_tex_program_override();
                res.ok()?;
            }
            let _ = frame.finish().ok()?;
        }
        Some(label)
    })?;
    timed(renderer, Span::LensDistance, |renderer| {
        let mut dist = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Xbgr8888, buf).ok()?;
        {
            let mut target = Bind::bind(renderer, &mut dist).ok()?;
            let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
            let texel = [1.0 / w as f32, 1.0 / h as f32];
            let uniforms = effects::key_distance_uniforms(texel, KEYED_LENS_DIST_MAX);
            frame
                .render_texture_from_to(
                    &label,
                    Rectangle::from_size((w as f64, h as f64).into()),
                    full,
                    &[full],
                    &[],
                    Transform::Normal,
                    1.0,
                    Some(dist_prog),
                    &uniforms,
                )
                .ok()?;
            let _ = frame.finish().ok()?;
        }
        Some(dist)
    })
}

/// Runs `f` between a GPU timer span of `kind` (a no-op unless
/// `EC_GLASS_PROFILE` is set; see [`crate::gpu_timer`]).
fn timed<R>(renderer: &mut GlesRenderer, kind: Span, f: impl FnOnce(&mut GlesRenderer) -> R) -> R {
    crate::gpu_timer::begin(renderer, kind);
    let out = f(renderer);
    crate::gpu_timer::end(renderer);
    out
}

/// The blurred backdrop, its lightly blurred twin and `mask` side by side in a
/// `3w` x `h` texture: the packed layout a keyed lens samples. `blurred` and
/// `light` are the same region of the backdrop's two halves, in its own texels.
fn pack(
    renderer: &mut GlesRenderer,
    backdrop: &GlesTexture,
    (blurred, light): (Rectangle<f64, Buffer>, Rectangle<f64, Buffer>),
    mask: &GlesTexture,
    size: Size<i32, Physical>,
) -> Option<GlesTexture> {
    timed(renderer, Span::LensPack, |renderer| {
        pack_passes(renderer, backdrop, (blurred, light), mask, size)
    })
}

fn pack_passes(
    renderer: &mut GlesRenderer,
    backdrop: &GlesTexture,
    (blurred, light): (Rectangle<f64, Buffer>, Rectangle<f64, Buffer>),
    mask: &GlesTexture,
    size: Size<i32, Physical>,
) -> Option<GlesTexture> {
    let (w, h) = (size.w, size.h);
    let wide = Size::<i32, Physical>::from((3 * w, h));
    let mut packed =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Xbgr8888, (3 * w, h).into()).ok()?;
    {
        let mut target = Bind::bind(renderer, &mut packed).ok()?;
        let mut frame = renderer.render(&mut target, wide, Transform::Normal).ok()?;
        let left = Rectangle::new((0, 0).into(), size);
        let middle = Rectangle::new((w, 0).into(), size);
        let right = Rectangle::new((2 * w, 0).into(), size);
        let whole = Rectangle::from_size((w as f64, h as f64).into());
        for (src, dst) in [(blurred, left), (light, middle)] {
            frame
                .render_texture_from_to(
                    backdrop,
                    src,
                    dst,
                    &[Rectangle::from_size(size)],
                    &[],
                    Transform::Normal,
                    1.0,
                    None,
                    &[],
                )
                .ok()?;
        }
        frame
            .render_texture_from_to(
                mask,
                whole,
                right,
                &[Rectangle::from_size(size)],
                &[],
                Transform::Normal,
                1.0,
                None,
                &[],
            )
            .ok()?;
        let _ = frame.finish().ok()?;
    }
    Some(packed)
}

/// The lens glass element over `backdrop`'s region: the backdrop's blurred
/// texture packed beside the mask, drawn with the lens program (ADR 0132).
/// `None` when the region is not the backdrop's own, or a pass fails.
#[allow(clippy::too_many_arguments)]
pub fn element(
    renderer: &mut GlesRenderer,
    glass: &GlesTexProgram,
    req: &LensRequest,
    backdrop: &BlurElement,
    scale: Scale<f64>,
    cfg: &GlassBlur,
    light: f32,
    adaptive: f32,
    upscale: f64,
    fb_size: (i32, i32),
) -> Option<BlurElement> {
    let outer = backdrop.region();
    // The rim reads the lightly blurred level, which only a packed backdrop
    // (glass mode) has; otherwise the key stays flat.
    if !backdrop.packed() || !within(req.region, outer) {
        return None;
    }
    // The lens covers a part of the backdrop (a strip of its window): take the
    // part of the backdrop's source that lies under it.
    let r = req.region;
    let (off, uv) = sub_rect_uv(
        (r.loc.x, r.loc.y, r.size.w, r.size.h),
        (outer.loc.x, outer.loc.y, outer.size.w, outer.size.h),
    )?;
    let src = backdrop.src();
    let part = Rectangle::new(
        (
            src.loc.x + off[0] as f64 * src.size.w,
            src.loc.y + off[1] as f64 * src.size.h,
        )
            .into(),
        (src.size.w * uv[0] as f64, src.size.h * uv[1] as f64).into(),
    );
    // The lightly blurred level is the same rectangle one half-texture to the right.
    let half = Texture::width(backdrop.texture()) as f64 / 2.0;
    let twin = Rectangle::new((part.loc.x + half, part.loc.y).into(), part.size);
    let packed = pack(renderer, backdrop.texture(), (part, twin), &req.mask, r.size)?;
    let mut uniforms = Vec::new();
    lens_uniforms(&mut uniforms, req, cfg, light, adaptive, upscale, fb_size);
    tracing::debug!(region = ?r, "keyed lens built over its backdrop");
    let mut commit = CommitCounter::default();
    commit.increment();
    Some(blur::keyed_lens(
        backdrop.context(),
        packed,
        req.region,
        scale,
        glass.clone(),
        uniforms,
        commit,
    ))
}

/// The lens glass's uniforms: the Lens preset's glass as the window glass sets
/// it (strength, bevel, dispersion and rim from the compositor's settings),
/// packed, plus the keyed lens's own (ADR 0132).
fn lens_uniforms(
    out: &mut Vec<Uniform<'static>>,
    req: &LensRequest,
    cfg: &GlassBlur,
    light: f32,
    adaptive: f32,
    upscale: f64,
    fb_size: (i32, i32),
) {
    let look = effects::preset_look(GlassPreset::Lens, cfg, upscale);
    let mut tuned = cfg.clone();
    tuned.dispersion = look.dispersion;
    tuned.rim = look.rim;
    effects::glass_uniforms_lifted_into(
        out,
        GlassPreset::Lens,
        req.region,
        req.fb,
        req.radius,
        fb_size,
        look.strength,
        look.bevel,
        &tuned,
        None,
        0.0,
        0.0,
        1.0,
        light,
        adaptive,
        1.0,
    );
    effects::set_shade_ref(out, upscale);
    effects::set_packed(out, true);
    effects::set_px_uv(out, (req.region.size.w * 3, req.region.size.h));
    out.extend(effects::keyed_lens_uniforms(
        effects::fb_rect(req.round, req.fb),
        req.radius,
        &req.keys,
        KEYED_LENS_DIST_MAX,
    ));
}

/// Whole-target clip for the label pass: every texel is inside the key region,
/// since the target is the region.
const EVERYWHERE: [f32; 4] = [-1.0e6, -1.0e6, 2.0e6, 2.0e6];

/// The label a texel holds: `None` (no match, or a key drawn flat) is `0`;
/// a lens for key `index` is `index + 1`. Mirrors `KEY_LABEL_SRC`.
pub fn label_of(matched: Option<usize>, lens: bool) -> u8 {
    match matched {
        Some(i) if lens && i < 4 => (i + 1) as u8,
        _ => 0,
    }
}

/// The blue-channel byte for a label, as the label pass writes it (`label / 4`
/// in 0..=1, quantised to 8 bits).
pub fn encode_label(label: u8) -> u8 {
    ((label as f32 / 4.0) * 255.0).round() as u8
}

/// The label a blue-channel byte reads back as (`floor(b * 4 + 0.5)`).
pub fn decode_label(blue: u8) -> u8 {
    ((blue as f32 / 255.0) * 4.0 + 0.5).floor() as u8
}

/// The red-channel byte for a distance in physical px: `dist / dist_max`
/// clamped to 0..=1, as the distance pass writes it.
pub fn encode_distance(dist: f32, dist_max: f32) -> u8 {
    ((dist / dist_max).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The distance a red-channel byte reads back as, in physical px.
pub fn decode_distance(red: u8, dist_max: f32) -> f32 {
    red as f32 / 255.0 * dist_max
}

/// A lens's strength, bevel or dispersion from the compositor's glass settings
/// (ADR 0132), clamped so a lens never reads past the distance field's reach
/// ([`KEYED_LENS_DIST_MAX`]). Non-finite input clamps to 0.
pub fn clamp_lens_px(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, KEYED_LENS_DIST_MAX)
    } else {
        0.0
    }
}

/// The offset and scale that map a lens region into a blur source rectangle:
/// `region` inside `blur` (both output-local physical) gives the fraction of
/// the blur's extent the lens covers, so the lens samples the same backdrop
/// the blur drew there. `None` when `region` is empty or outside `blur`.
pub fn sub_rect_uv(region: (i32, i32, i32, i32), blur: (i32, i32, i32, i32)) -> Option<([f32; 2], [f32; 2])> {
    let (rx, ry, rw, rh) = region;
    let (bx, by, bw, bh) = blur;
    if rw <= 0 || rh <= 0 || bw <= 0 || bh <= 0 {
        return None;
    }
    let inside = rx >= bx && ry >= by && rx + rw <= bx + bw && ry + rh <= by + bh;
    if !inside {
        return None;
    }
    let off = [(rx - bx) as f32 / bw as f32, (ry - by) as f32 / bh as f32];
    let scale = [rw as f32 / bw as f32, rh as f32 / bh as f32];
    Some((off, scale))
}

/// Whether `inner` lies wholly inside `outer` (both output-local physical).
/// An empty `inner` is not within anything.
pub fn within(inner: Rectangle<i32, Physical>, outer: Rectangle<i32, Physical>) -> bool {
    inner.size.w > 0
        && inner.size.h > 0
        && inner.loc.x >= outer.loc.x
        && inner.loc.y >= outer.loc.y
        && inner.loc.x + inner.size.w <= outer.loc.x + outer.size.w
        && inner.loc.y + inner.size.h <= outer.loc.y + outer.size.h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strip_is_within_its_window_and_not_the_reverse() {
        let window = Rectangle::new((4, 8).into(), (400, 300).into());
        let strip = Rectangle::new((4, 8).into(), (400, 60).into());
        assert!(within(strip, window));
        assert!(!within(window, strip));
        assert!(within(window, window));
        // One px past the edge is not within.
        let over = Rectangle::new((4, 8).into(), (401, 60).into());
        assert!(!within(over, window));
        assert!(!within(Rectangle::new((4, 8).into(), (0, 5).into()), window));
    }

    #[test]
    fn the_lens_reads_only_the_lens_settings() {
        // ADR 0132: decoration.blur.glass.lens.* drives the lens; the window's
        // refraction, bevel, dispersion and rim do not reach it.
        let base = GlassBlur::default();
        let mut window_keys = base.clone();
        window_keys.refraction = 9.0;
        window_keys.bevel = 3;
        window_keys.dispersion = 1.0;
        window_keys.rim = 0.0;
        assert_eq!(
            effects::preset_look(GlassPreset::Lens, &base, 1.0),
            effects::preset_look(GlassPreset::Lens, &window_keys, 1.0)
        );
        let mut lens_keys = base.clone();
        lens_keys.lens.refraction = 5.0;
        assert_ne!(
            effects::preset_look(GlassPreset::Lens, &base, 1.0),
            effects::preset_look(GlassPreset::Lens, &lens_keys, 1.0)
        );
    }

    #[test]
    fn label_is_zero_unless_a_lens_key_matched() {
        assert_eq!(label_of(None, true), 0);
        assert_eq!(label_of(Some(0), false), 0);
        assert_eq!(label_of(Some(0), true), 1);
        assert_eq!(label_of(Some(3), true), 4);
        // A key past the four slots never labels.
        assert_eq!(label_of(Some(4), true), 0);
    }

    #[test]
    fn label_round_trips_through_its_byte() {
        for label in 0..=4u8 {
            assert_eq!(decode_label(encode_label(label)), label);
        }
    }

    #[test]
    fn distance_round_trips_within_one_byte_step() {
        let max = KEYED_LENS_DIST_MAX;
        for px in [0.0_f32, 1.0, 7.5, 16.0, 31.9] {
            let back = decode_distance(encode_distance(px, max), max);
            assert!((back - px).abs() <= max / 255.0, "{px} came back as {back}");
        }
    }

    #[test]
    fn distance_saturates_on_the_plateau_and_at_zero() {
        assert_eq!(encode_distance(-3.0, 32.0), 0);
        assert_eq!(encode_distance(32.0, 32.0), 255);
        assert_eq!(encode_distance(900.0, 32.0), 255);
    }

    #[test]
    fn lens_px_is_clamped_to_the_field_reach() {
        assert_eq!(clamp_lens_px(-1.0), 0.0);
        assert_eq!(clamp_lens_px(12.0), 12.0);
        assert_eq!(clamp_lens_px(400.0), KEYED_LENS_DIST_MAX);
        assert_eq!(clamp_lens_px(f32::NAN), 0.0);
        assert_eq!(clamp_lens_px(f32::INFINITY), 0.0);
    }

    #[test]
    fn sub_rect_maps_a_strip_into_its_blur() {
        // A 100x40 blur at (10, 20); the top 10 rows are the lens.
        let (off, scale) = sub_rect_uv((10, 20, 100, 10), (10, 20, 100, 40)).unwrap();
        assert_eq!(off, [0.0, 0.0]);
        assert_eq!(scale, [1.0, 0.25]);
        // The same strip one row down, with the blur half the size.
        let (off, scale) = sub_rect_uv((60, 40, 50, 20), (10, 20, 100, 40)).unwrap();
        assert_eq!(off, [0.5, 0.5]);
        assert_eq!(scale, [0.5, 0.5]);
    }

    #[test]
    fn sub_rect_refuses_empty_or_outside_regions() {
        assert!(sub_rect_uv((0, 0, 0, 5), (0, 0, 10, 10)).is_none());
        assert!(sub_rect_uv((5, 5, 10, 10), (0, 0, 10, 10)).is_none());
        assert!(sub_rect_uv((0, 0, 5, 5), (0, 0, 0, 10)).is_none());
    }
}
