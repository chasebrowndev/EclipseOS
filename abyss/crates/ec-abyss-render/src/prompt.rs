// SPDX-License-Identifier: AGPL-3.0-only
//! The glass of a compositor-drawn trusted prompt (ADR 0128, COMP-10).
//!
//! A prompt is drawn by the compositor so a client cannot imitate one. A
//! translucent panel samples what a client drew behind it, so the sampling has
//! to be built so that nothing a client draws is legible through its body, and
//! so that no configuration, theme, window rule or client can change that. This
//! module is that sampling and nothing else:
//!
//! * One entry point, [`prompt_backdrop`]. It takes NO blur configuration: the
//!   blur and the lens are fixed here ([`PROMPT_BLUR_SIZE`], [`PROMPT_PASSES`],
//!   [`PROMPT_REFRACTION`] ...), not read from `decoration.blur.*`.
//! * The panel and its buttons are lenses (the launcher's and bar's lens
//!   preset: bevel bend, dispersion, lit rim, edge darkening). The interior,
//!   everything beyond the bevel band, is the plain blur of at least
//!   [`PROMPT_MIN_PASSES`] passes under the caller's fixed tint; only the
//!   bevel band (a few tens of px at the edge) reads the lightly blurred half
//!   of the packed texture. No adaptive tint (that would read the backdrop's
//!   luma, i.e. client pixels).
//! * Private: its own [`BlurStore`] per output, one chain shared by the panel
//!   and its buttons, never a tier shared with windows and never a persistent
//!   tier-0 member.
//! * Fail closed: any failure returns `None` and leaves `out` empty, so the
//!   backend draws its opaque panel. There is no half-built set.
//!
//! Capture: the elements are ordinary [`BlurElement`]s with no capture-pass
//! handle. The backend splices them with the panel texture, in front of
//! everything and never into a capture target (COMP-02 §4); nothing here
//! registers with a capture pass.

use smithay::{
    backend::renderer::{
        element::{Element, RenderElement},
        gles::{GlesRenderer, GlesTexture},
        Renderer,
    },
    output::Output,
    utils::{Physical, Rectangle, Scale},
};

use ec_abyss_config::{AdaptiveBlur, Blur, BlurMode, FrostBlur, GlassBlur, LensGlass};

use crate::{
    blur::{BlurElement, BlurKey},
    effects, BorderStore,
};

/// Blur passes a prompt backdrop's interior is always built with. A floor:
/// nothing may lower it (a test pins [`PROMPT_PASSES`] to it).
pub const PROMPT_MIN_PASSES: i32 = 6;
/// The passes actually used. Not configurable.
pub const PROMPT_PASSES: i32 = PROMPT_MIN_PASSES;
/// The blur size, fixed at the configuration's default (8). Not configurable.
pub const PROMPT_BLUR_SIZE: i32 = 8;
/// The least tint alpha over the blur, 0.0..=1.0. The caller's tint is raised
/// to it; a lower one cannot be asked for. Chosen against the leak bound in
/// this module's tests (see `docs/GLASS_PROFILING.md`).
pub const PROMPT_MIN_TINT_ALPHA: f32 = 0.25;
/// The most members (buttons) one prompt draws; more is a failure.
pub const PROMPT_MAX_MEMBERS: usize = 8;

/// The lens of a prompt, fixed (not read from `decoration.blur.glass.lens`):
/// rim displacement in logical px, widest bevel band in logical px, chromatic
/// spread 0..1, rim strength 0..1. They are the lens preset's shipped defaults,
/// pinned here so that a change to those defaults cannot move the prompt.
pub const PROMPT_REFRACTION: f32 = 28.0;
pub const PROMPT_BEVEL: i32 = 40;
pub const PROMPT_DISPERSION: f32 = 0.7;
pub const PROMPT_RIM: f32 = 0.7;
/// The most of the lightly blurred half the bevel band may show, 0..1 (the lens
/// of a window shows all of it, share 1). The band is where a client's fine
/// detail could otherwise come through, so a prompt holds most of it back; the
/// leak tests bound the result. A ceiling: nothing may raise it.
pub const PROMPT_SHARP_SHARE: f32 = 0.07;

/// One lens a prompt draws: the panel first, then each button.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PromptMember {
    /// Output-local physical px.
    pub rect: Rectangle<i32, Physical>,
    /// Corner radius in physical px (clamped to half the shorter side).
    pub radius: f32,
}

/// The fixed blur of a prompt. Built from constants, never from the live
/// configuration; `adaptive` is off so the tint never follows the backdrop.
/// Glass mode with a non-zero dispersion is what stores the result packed (the
/// blurred interior beside the lightly blurred level the bevel band reads).
fn prompt_blur() -> Blur {
    Blur {
        mode: BlurMode::Glass,
        size: PROMPT_BLUR_SIZE,
        passes: PROMPT_PASSES.max(PROMPT_MIN_PASSES),
        glass: prompt_glass(),
        frost: FrostBlur::default(),
        adaptive: AdaptiveBlur {
            enabled: false,
            strength: 0.0,
        },
    }
}

fn prompt_glass() -> GlassBlur {
    GlassBlur {
        refraction: PROMPT_REFRACTION,
        bevel: PROMPT_BEVEL,
        dispersion: PROMPT_DISPERSION,
        rim: PROMPT_RIM,
        lens: LensGlass {
            refraction: PROMPT_REFRACTION,
            bevel: PROMPT_BEVEL,
            dispersion: PROMPT_DISPERSION,
            rim: PROMPT_RIM,
        },
    }
}

/// `tint` as the shader gets it: every channel finite and in 0..=1, alpha at
/// least [`PROMPT_MIN_TINT_ALPHA`]. `None` for a non-finite channel (a
/// malformed palette is a failure, not a clamp to something see-through).
pub fn clamp_tint(tint: [f32; 4]) -> Option<[f32; 4]> {
    if tint.iter().any(|c| !c.is_finite()) {
        return None;
    }
    Some([
        tint[0].clamp(0.0, 1.0),
        tint[1].clamp(0.0, 1.0),
        tint[2].clamp(0.0, 1.0),
        tint[3].clamp(PROMPT_MIN_TINT_ALPHA, 1.0),
    ])
}

/// The corner radius in physical px, clamped to half the rect's shorter side
/// (and to 0). `None` for a non-finite radius.
pub fn clamp_radius(radius: f32, rect: Rectangle<i32, Physical>) -> Option<f32> {
    if !radius.is_finite() {
        return None;
    }
    let half = rect.size.w.min(rect.size.h).max(0) as f32 * 0.5;
    Some(radius.clamp(0.0, half))
}

/// Whether `rect` is non-empty and wholly on an output of `fb` physical px.
pub fn rect_on_output(rect: Rectangle<i32, Physical>, fb: (i32, i32)) -> bool {
    rect.size.w > 0
        && rect.size.h > 0
        && rect.loc.x >= 0
        && rect.loc.y >= 0
        && rect.loc.x.checked_add(rect.size.w).is_some_and(|r| r <= fb.0)
        && rect.loc.y.checked_add(rect.size.h).is_some_and(|b| b <= fb.1)
}

/// The width in physical px of the bevel band of a lens of shorter side
/// `short` physical px at `upscale`: the only region where the lightly blurred
/// half of the packed texture is read. Everything deeper is the plain
/// [`PROMPT_PASSES`]-pass blur.
pub fn band_px(short: i32, upscale: f64) -> f32 {
    effects::lens_bevel((f64::from(PROMPT_BEVEL) * upscale) as f32, short.max(1) as f32)
}

/// The lens glass of a trusted prompt: the panel `panel` and its `members`
/// (buttons), each a lens over the same `behind` (everything the backend drew
/// behind the prompt: scene, scrim, cards). On success `out` holds the panel's
/// element first and then one per member, in the order given, to be drawn in
/// that order (each over the previous). On any failure `out` is left EMPTY and
/// the result is `None`: the caller draws its opaque panel instead. There is
/// never a partial set.
///
/// Rects are output-local physical px and must lie wholly on the output.
/// `light` picks the scheme's lens shading; `tint` is the palette's fixed tint
/// (straight alpha), alpha raised to [`PROMPT_MIN_TINT_ALPHA`].
///
/// The blur is built once, over the union of the rects, in a private store
/// (`BlurKey::Prompt`), and rebuilt only when damage lands under it (or a rect
/// or the output changes), so a static scene costs nothing per frame. `out` is
/// the caller's reused scratch, nothing allocates per frame once warm. Call
/// [`prompt_backdrop_release`] when the prompt closes.
#[allow(clippy::too_many_arguments)]
pub fn prompt_backdrop<E>(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
    behind: &[E],
    panel: PromptMember,
    members: &[PromptMember],
    scale: Scale<f64>,
    light: bool,
    tint: [f32; 4],
    out: &mut Vec<BlurElement<GlesTexture>>,
) -> Option<()>
where
    E: Element + RenderElement<GlesRenderer>,
{
    out.clear();
    let got = build(
        renderer, store, output, behind, panel, members, scale, light, tint, out,
    );
    if got.is_none() {
        out.clear();
    }
    got
}

#[allow(clippy::too_many_arguments)]
fn build<E>(
    renderer: &mut GlesRenderer,
    store: &mut BorderStore,
    output: &Output,
    behind: &[E],
    panel: PromptMember,
    members: &[PromptMember],
    scale: Scale<f64>,
    light: bool,
    tint: [f32; 4],
    out: &mut Vec<BlurElement<GlesTexture>>,
) -> Option<()>
where
    E: Element + RenderElement<GlesRenderer>,
{
    if members.len() > PROMPT_MAX_MEMBERS {
        return None;
    }
    let tint = clamp_tint(tint)?;
    let mode = output.current_mode()?;
    let fb = output.current_transform().transform_size(mode.size);
    let count = 1 + members.len();
    let all = |i: usize| if i == 0 { panel } else { members[i - 1] };
    let mut union = None::<Rectangle<i32, Physical>>;
    for i in 0..count {
        let m = all(i);
        if !rect_on_output(m.rect, (fb.w, fb.h)) {
            return None;
        }
        clamp_radius(m.radius, m.rect)?;
        union = Some(union.map_or(m.rect, |u| u.merge(m.rect)));
    }
    let union = union?;
    // A rotated output keeps the unmasked backdrop in every mode: a prompt
    // does not draw glass there.
    let fb_map = effects::output_fb_map(output)?;
    let upscale = scale.x.max(scale.y);
    let short = (0..count)
        .map(|i| all(i).rect.size.w.min(all(i).rect.size.h))
        .max()?;

    store.gpu.bind(&Renderer::context_id(&*renderer));
    let gpu = &mut *store.gpu;
    let lens = effects::GlassPreset::Lens.index();
    if gpu.glass_presets[lens].is_none() {
        if gpu.compile_failed.contains(&"prompt lens") {
            return None;
        }
        match effects::compile_glass_lens(renderer) {
            Ok(program) => gpu.glass_presets[lens] = Some(program),
            Err(err) => {
                gpu.compile_failed.push("prompt lens");
                tracing::warn!(?err, "compiling the lens shader; the prompt stays opaque");
                return None;
            }
        }
    }
    let program = gpu.glass_presets[lens].clone()?;
    let blur = prompt_blur();
    let glass = prompt_glass();
    let look = effects::preset_look(effects::GlassPreset::Lens, &glass, upscale);
    let reach = effects::preset_reach_for(effects::GlassPreset::Lens, &glass, upscale, short as f32);
    let blurs = gpu.prompt.entry(output.clone()).or_default();
    // One key, every frame: this also resets the store's per-frame state.
    blurs.retain(&[BlurKey::Prompt]);
    let mut tuned = glass.clone();
    tuned.dispersion = look.dispersion;
    tuned.rim = look.rim;
    blurs.pane_elements(
        renderer,
        output,
        fb_map,
        &BlurKey::Prompt,
        union,
        behind,
        &blur,
        scale,
        reach,
        &program,
        count,
        1.0,
        |i, uniforms| {
            let m = all(i);
            let radius = clamp_radius(m.radius, m.rect)?;
            effects::glass_uniforms_lifted_into(
                uniforms,
                effects::GlassPreset::Lens,
                m.rect,
                fb_map,
                radius,
                (fb.w, fb.h),
                look.strength,
                look.bevel,
                &tuned,
                None,
                0.0,
                0.0,
                1.0,
                if light { 1.0 } else { 0.0 },
                0.0,
                1.0,
            );
            effects::set_body_tint(uniforms, tint);
            effects::set_sharp_damp(uniforms, 1.0 - PROMPT_SHARP_SHARE);
            Some(m.rect)
        },
        out,
        // The chain is the frost one (the configured pass count, packed
        // because glass mode has a dispersion); the lens is the program.
        effects::GlassPreset::Frost,
    )?;
    (out.len() == count).then_some(())
}

/// Drop the prompt backdrop of `output`: its blur chain and cached result. Call
/// when the prompt closes (or its output goes away); a prompt that never
/// closes keeps one small private chain, nothing grows per frame.
pub fn prompt_backdrop_release(store: &mut BorderStore, output: &Output) {
    store.gpu.each_mut(|gpu| {
        gpu.prompt.remove(output);
    });
}

/// Prompt backdrops currently held (live renderer context), for the leak test.
pub fn prompt_backdrops_held(store: &BorderStore) -> usize {
    store.gpu_prompt_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    // ------------------------------------------------------------ constants

    #[test]
    fn the_floors_are_what_the_adr_says() {
        const {
            assert!(PROMPT_MIN_PASSES == 6);
            assert!(PROMPT_PASSES >= PROMPT_MIN_PASSES);
            assert!(PROMPT_BLUR_SIZE == 8);
            assert!(PROMPT_MIN_TINT_ALPHA >= 0.25);
        }
    }

    /// The blur is built from constants: whatever the live configuration says,
    /// a prompt's blur is the same, frost-style, adaptive off.
    #[test]
    fn prompt_glass_ignores_config() {
        let b = prompt_blur();
        assert_eq!(b.mode, BlurMode::Glass);
        assert_eq!(b.size, PROMPT_BLUR_SIZE);
        assert_eq!(b.passes, PROMPT_PASSES);
        assert!(!b.adaptive.enabled && b.adaptive.strength == 0.0);
        // Packed: the bevel band reads the lightly blurred half, the interior
        // the PROMPT_PASSES-pass blur, whatever the live glass settings.
        assert!(crate::blur::packs(&b, effects::GlassPreset::Frost));
        assert_eq!(
            crate::blur::tuned(&b, effects::GlassPreset::Frost).passes,
            PROMPT_PASSES
        );
        let g = prompt_glass();
        assert_eq!(
            (g.lens.refraction, g.lens.bevel, g.lens.dispersion, g.lens.rim),
            (PROMPT_REFRACTION, PROMPT_BEVEL, PROMPT_DISPERSION, PROMPT_RIM)
        );
        // `prompt_backdrop` has no configuration parameter at all; a hostile
        // `Blur` has nowhere to go. The constructor is the only source.
        let again = prompt_blur();
        assert_eq!(
            (b.mode, b.size, b.passes, b.adaptive),
            (again.mode, again.size, again.passes, again.adaptive)
        );
    }

    // ------------------------------------------------------- failure paths

    #[test]
    fn a_tint_alpha_is_raised_to_the_floor_never_lowered() {
        assert_eq!(
            clamp_tint([0.1, 0.2, 0.3, 0.0]).unwrap()[3],
            PROMPT_MIN_TINT_ALPHA
        );
        assert_eq!(
            clamp_tint([0.1, 0.2, 0.3, PROMPT_MIN_TINT_ALPHA - 0.01]).unwrap()[3],
            PROMPT_MIN_TINT_ALPHA
        );
        assert_eq!(clamp_tint([0.1, 0.2, 0.3, 0.3]).unwrap()[3], 0.3);
        assert_eq!(clamp_tint([0.1, 0.2, 0.3, 0.9]).unwrap()[3], 0.9);
        assert_eq!(clamp_tint([0.1, 0.2, 0.3, 7.0]).unwrap()[3], 1.0);
        assert_eq!(clamp_tint([2.0, -1.0, 0.5, 1.0]).unwrap()[..3], [1.0, 0.0, 0.5]);
    }

    #[test]
    fn a_malformed_tint_is_a_failure() {
        assert_eq!(clamp_tint([f32::NAN, 0.0, 0.0, 1.0]), None);
        assert_eq!(clamp_tint([0.0, f32::INFINITY, 0.0, 1.0]), None);
        assert_eq!(clamp_tint([0.0, 0.0, 0.0, f32::NAN]), None);
    }

    #[test]
    fn the_radius_is_clamped_to_half_the_shorter_side() {
        let rect = r(10, 10, 400, 100);
        assert_eq!(clamp_radius(20.0, rect), Some(20.0));
        assert_eq!(clamp_radius(500.0, rect), Some(50.0));
        assert_eq!(clamp_radius(-3.0, rect), Some(0.0));
        assert_eq!(clamp_radius(f32::NAN, rect), None);
        assert_eq!(clamp_radius(f32::INFINITY, rect), None);
    }

    #[test]
    fn an_empty_or_off_output_rect_is_a_failure() {
        let fb = (1920, 1080);
        assert!(rect_on_output(r(100, 100, 400, 300), fb));
        assert!(rect_on_output(r(0, 0, 1920, 1080), fb));
        assert!(!rect_on_output(r(100, 100, 0, 300), fb));
        assert!(!rect_on_output(r(100, 100, 0, 0), fb));
        assert!(!rect_on_output(r(-1, 0, 400, 300), fb));
        assert!(!rect_on_output(r(1800, 100, 400, 300), fb));
        assert!(!rect_on_output(r(100, 900, 400, 300), fb));
        assert!(!rect_on_output(r(i32::MAX, 0, 10, 10), fb));
    }

    // ------------------------------------------------------------ leak bound

    /// A single-channel image in 0..=255.
    #[derive(Clone)]
    struct Grid {
        w: i32,
        h: i32,
        v: Vec<f32>,
    }

    impl Grid {
        fn new(w: i32, h: i32, f: impl Fn(i32, i32) -> f32) -> Grid {
            let mut v = Vec::with_capacity((w * h) as usize);
            for y in 0..h {
                for x in 0..w {
                    v.push(f(x, y));
                }
            }
            Grid { w, h, v }
        }
        fn at(&self, x: i32, y: i32) -> f32 {
            self.v[(y.clamp(0, self.h - 1) * self.w + x.clamp(0, self.w - 1)) as usize]
        }
        /// GL_LINEAR at texel coordinates (texel `t` centred at `t + 0.5`),
        /// clamped to the edge.
        fn bilinear(&self, px: f64, py: f64) -> f32 {
            let (x, y) = (px - 0.5, py - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
            let (x0, y0) = (x0 as i32, y0 as i32);
            let top = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
            let bot = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
            top * (1.0 - fy) + bot * fy
        }
    }

    /// The shaders' taps (`partial.rs` tests): in units of `0.5 * offset`
    /// source texels, with their weights.
    const DOWN: [((f64, f64), f32); 5] = [
        ((0.0, 0.0), 4.0),
        ((-1.0, -1.0), 1.0),
        ((1.0, 1.0), 1.0),
        ((1.0, -1.0), 1.0),
        ((-1.0, 1.0), 1.0),
    ];
    const UP: [((f64, f64), f32); 8] = [
        ((-2.0, 0.0), 1.0),
        ((-1.0, 1.0), 2.0),
        ((0.0, 2.0), 1.0),
        ((1.0, 1.0), 2.0),
        ((2.0, 0.0), 1.0),
        ((1.0, -1.0), 2.0),
        ((0.0, -2.0), 1.0),
        ((-1.0, -1.0), 2.0),
    ];

    /// One pass as the GPU runs it: every destination texel centre mapped onto
    /// the source, taps at `0.5 * offset` source texels.
    fn pass(src: &Grid, dst: (i32, i32), offset: f64, taps: &[((f64, f64), f32)]) -> Grid {
        let total: f32 = taps.iter().map(|t| t.1).sum();
        let s = (
            f64::from(src.w) / f64::from(dst.0),
            f64::from(src.h) / f64::from(dst.1),
        );
        Grid::new(dst.0, dst.1, |i, j| {
            let cx = (f64::from(i) + 0.5) * s.0;
            let cy = (f64::from(j) + 0.5) * s.1;
            let mut sum = 0.0;
            for ((dx, dy), w) in taps {
                sum += src.bilinear(cx + dx * 0.5 * offset, cy + dy * 0.5 * offset) * w;
            }
            sum / total
        })
    }

    /// The dual-Kawase chain at the prompt's fixed parameters: `PROMPT_PASSES`
    /// downs (`full >> level`), as many ups back to full size, offset
    /// `size / 4` (`blur::sample_offset`).
    fn chain(src: &Grid, passes: i32, size: i32) -> Grid {
        let offset = f64::from((size as f32 / 4.0).max(1.0));
        let mut levels = vec![src.clone()];
        for l in 1..=passes {
            let (w, h) = ((src.w >> l).max(1), (src.h >> l).max(1));
            let next = pass(levels.last().unwrap(), (w, h), offset, &DOWN);
            levels.push(next);
        }
        let mut cur = levels.pop().unwrap();
        while let Some(finer) = levels.pop() {
            cur = pass(&cur, (finer.w, finer.h), offset, &UP);
        }
        cur
    }

    /// The largest max-min of `g` over every 16x16 window whose top-left lies
    /// in `margin..(size - margin - 16)`: the margin keeps the image edge
    /// (where the model clamps) out of the measurement.
    fn max_local_contrast(g: &Grid, margin: i32) -> f32 {
        let mut worst = 0.0f32;
        let mut y = margin;
        while y + 16 <= g.h - margin {
            let mut x = margin;
            while x + 16 <= g.w - margin {
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for yy in y..y + 16 {
                    for xx in x..x + 16 {
                        let v = g.at(xx, yy);
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
                worst = worst.max(hi - lo);
                x += 4;
            }
            y += 4;
        }
        worst
    }

    /// 5x7 glyph rows (bits 4..0) of a text-like set.
    const GLYPHS: [[u8; 7]; 8] = [
        [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001], // A
        [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110], // B
        [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110], // C
        [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111], // E
        [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001], // H
        [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001], // M
        [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110], // O
        [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100], // T
    ];

    /// Black text on white: glyph cells `scale` px square, 1 px gaps, wrapped.
    fn text(w: i32, h: i32, scale: i32) -> Grid {
        let (cw, ch) = (6 * scale, 9 * scale);
        Grid::new(w, h, |x, y| {
            let (col, row) = (x / cw, y / ch);
            let (gx, gy) = ((x % cw) / scale, (y % ch) / scale);
            if gx >= 5 || gy >= 7 {
                return 255.0;
            }
            let glyph = GLYPHS[((row * 7 + col) % 8) as usize];
            if glyph[gy as usize] >> (4 - gx) & 1 == 1 {
                0.0
            } else {
                255.0
            }
        })
    }

    /// Every pattern a client could hide a message in, each at full range.
    fn worst_patterns(n: i32) -> Vec<(&'static str, Grid)> {
        vec![
            (
                "1 px checker",
                Grid::new(n, n, |x, y| if (x + y) % 2 == 0 { 255.0 } else { 0.0 }),
            ),
            (
                "1 px stripes",
                Grid::new(n, n, |x, _| if x % 2 == 0 { 255.0 } else { 0.0 }),
            ),
            (
                "2 px stripes",
                Grid::new(n, n, |x, _| if (x / 2) % 2 == 0 { 255.0 } else { 0.0 }),
            ),
            (
                "2 px horizontal stripes",
                Grid::new(n, n, |_, y| if (y / 2) % 2 == 0 { 255.0 } else { 0.0 }),
            ),
            ("text 5x7 at 1x", text(n, n, 1)),
            ("text 5x7 at 2x", text(n, n, 2)),
            ("text 5x7 at 3x", text(n, n, 3)),
            ("text 5x7 at 4x", text(n, n, 4)),
            ("text 5x7 at 6x", text(n, n, 6)),
            ("text 5x7 at 8x", text(n, n, 8)),
            (
                "4 px stripes",
                Grid::new(n, n, |x, _| if (x / 4) % 2 == 0 { 255.0 } else { 0.0 }),
            ),
            (
                "8 px stripes",
                Grid::new(n, n, |x, _| if (x / 8) % 2 == 0 { 255.0 } else { 0.0 }),
            ),
            (
                "16 px checker",
                Grid::new(n, n, |x, y| if (x / 16 + y / 16) % 2 == 0 { 255.0 } else { 0.0 }),
            ),
        ]
    }

    /// The worst local contrast (0..=255 levels) of the BODY after a tint of
    /// alpha `alpha` over the blurred pattern: the tint scales every
    /// difference by `1 - alpha`.
    fn leak(pattern: &Grid, alpha: f32) -> f32 {
        let blurred = chain(pattern, PROMPT_PASSES, PROMPT_BLUR_SIZE);
        max_local_contrast(&blurred, 40) * (1.0 - alpha)
    }

    /// The fine-grain noise the frost shader adds is up to one level peak to
    /// peak (`(grain - 0.5) / 255`); it is counted into the bound.
    const GRAIN: f32 = 1.0;
    /// Documented bound: after blur and tint, no 16x16 window of the panel
    /// body differs by more than this many 8-bit levels, grain included. 6
    /// levels is below the 1 % step (2.55) that a viewer cannot reliably pick
    /// out of a flat field as structure once it carries a 1-2 px pattern, and
    /// far below the roughly 40 levels the smallest legible glyph stroke
    /// needs against its ground.
    const LEAK_BOUND: f32 = 6.0;

    #[test]
    fn nothing_behind_the_panel_is_legible() {
        for (name, pattern) in worst_patterns(192) {
            let c = leak(&pattern, PROMPT_MIN_TINT_ALPHA) + GRAIN;
            println!("{name}: {c:.2} levels at alpha {PROMPT_MIN_TINT_ALPHA}");
            assert!(
                c <= LEAK_BOUND * 0.8,
                "{name} leaks {c:.2} levels (bound {LEAK_BOUND}, 20 % margin)"
            );
        }
    }

    /// The chain below its floor leaks more, and below the tint floor too: the
    /// constants are not arbitrary (the bound fails one step under either).
    #[test]
    fn the_floors_are_not_slack() {
        let pat = text(192, 192, 3);
        let at_floor = leak(&pat, PROMPT_MIN_TINT_ALPHA);
        let fewer_passes = max_local_contrast(&chain(&pat, PROMPT_MIN_PASSES - 1, PROMPT_BLUR_SIZE), 40)
            * (1.0 - PROMPT_MIN_TINT_ALPHA);
        assert!(fewer_passes > at_floor, "{fewer_passes} vs {at_floor}");
        let lower_tint = leak(&pat, PROMPT_MIN_TINT_ALPHA - 0.2);
        assert!(lower_tint > at_floor);
    }

    // ------------------------------------------------------- the rim band

    /// The lens' bend, rim and tint at one band pixel, in the shader's own
    /// terms (`GLASS_BODY`, GLASS_LENS), for a straight left edge at `edge_x`
    /// and the interior to its right. `blurred` is the interior chain's result,
    /// `level1` the lightly blurred level the packed texture's sharp half
    /// holds. Gray model, one channel per colour channel of the dispersion;
    /// the smoke, saturation and edge darkening (which only lower contrast)
    /// are left out, so this is an upper bound.
    #[allow(clippy::too_many_arguments)]
    fn band_pixel(
        blurred: &Grid,
        level1: &Grid,
        x: i32,
        y: i32,
        edge_x: i32,
        band: f32,
        strength: f32,
        a: f32,
        light: bool,
        share: f32,
    ) -> [f32; 3] {
        let dist = (x - edge_x) as f32 + 0.5; // -d
        let t = dist / band;
        let base = blurred.at(x, y);
        // The tint colour (gray): a dark palette's and a light one's.
        let tint = if light { 242.0 } else { 26.0 };
        let mut out = [0.0f32; 3];
        let k = PROMPT_DISPERSION * crate::effects::DISPERSION_K;
        for (ch, scale) in [(0usize, 1.0 - k), (1, 1.0), (2, 1.0 + k)] {
            let mut c = base;
            let mut rb = 0.0;
            let mut u = 0.0;
            if t < 1.0 {
                u = 1.0 - t.max(0.0);
                let b = strength * u * u * u.sqrt() * scale;
                // The edge's outward normal is -x: sample toward the outside.
                let sx = f64::from(x as f32 + 0.5 - b);
                let sh = level1.bilinear(sx / 2.0, (f64::from(y) + 0.5) / 2.0);
                let shown = base + (sh - base) * share;
                c = base + (shown - base) * u;
                rb = shown / 255.0;
            }
            c = c * (1.0 - a) + tint * a;
            if t < 1.0 {
                // The rim's own colour: the lightly blurred backdrop, brightened,
                // lightens the band (lit from the top left: this edge is lit by
                // 0.707, so `dirl` is 1.06). Dark: the outer half of the band.
                // Light: a crisp 3 px edge, biased to white.
                let mut rbc = (rb * 1.7).clamp(0.0, 1.0);
                let dirl = 1.06f32;
                let rw = if light {
                    rbc = rbc + (0.95 - rbc) * 0.45;
                    let s = (dist / 3.0).clamp(0.0, 1.0);
                    let crisp = 1.0 - s * s * (3.0 - 2.0 * s);
                    crisp * dirl
                } else {
                    let fall = 1.0 - (dist / (0.5 * band)).clamp(0.0, 1.0);
                    0.8 * fall * fall * dirl
                };
                let cn = c / 255.0;
                let lit = cn.max(rbc);
                c = (cn + (lit - cn) * rw.clamp(0.0, 1.0)) * 255.0;
            }
            let _ = u;
            out[ch] = c;
        }
        out
    }

    /// The largest content-caused swing, in 8-bit levels, of the band over any
    /// 16x16 patch of it (and of any channel): the band's value minus the same
    /// band over a flat backdrop of the pattern's mean (which has the same
    /// geometric ramp and rim but no content), max minus min.
    #[allow(clippy::needless_range_loop)]
    fn band_leak(pattern: &Grid, a: f32, light: bool, band: f32, strength: f32, share: f32) -> f32 {
        let blurred = chain(pattern, PROMPT_PASSES, PROMPT_BLUR_SIZE);
        let lvl1 = pass(pattern, (pattern.w / 2, pattern.h / 2), 2.0, &DOWN);
        let mean = pattern.v.iter().sum::<f32>() / pattern.v.len() as f32;
        let flat = Grid::new(pattern.w, pattern.h, |_, _| mean);
        let flat1 = Grid::new(lvl1.w, lvl1.h, |_, _| mean);
        let edge_x = 64;
        let nb = band.ceil() as i32;
        let mut diff = vec![[0.0f32; 3]; (nb * pattern.h) as usize];
        for y in 0..pattern.h {
            for bx in 0..nb {
                let x = edge_x + bx;
                let p = band_pixel(&blurred, &lvl1, x, y, edge_x, band, strength, a, light, share);
                let f = band_pixel(&flat, &flat1, x, y, edge_x, band, strength, a, light, share);
                diff[(y * nb + bx) as usize] = [p[0] - f[0], p[1] - f[1], p[2] - f[2]];
            }
        }
        let mut worst = 0.0f32;
        let mut y = 40;
        while y + 16 <= pattern.h - 40 {
            let mut bx = 0;
            while bx < nb {
                for ch in 0..3 {
                    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                    for yy in y..y + 16 {
                        for xx in bx..(bx + 16).min(nb) {
                            let v = diff[(yy * nb + xx) as usize][ch];
                            lo = lo.min(v);
                            hi = hi.max(v);
                        }
                    }
                    worst = worst.max(hi - lo);
                }
                bx += 8;
            }
            y += 4;
        }
        worst
    }

    /// A 400 px high panel at 1x: the lens takes its whole bevel (40 px) and
    /// bends by the configured 28 px; the same at 2x scale doubles both.
    fn band_params(upscale: f64) -> (f32, f32) {
        let short = (400.0 * upscale) as i32;
        let look = effects::preset_look(effects::GlassPreset::Lens, &prompt_glass(), upscale);
        (
            band_px(short, upscale),
            effects::lens_strength(look.strength, short as f32),
        )
    }

    #[test]
    fn the_band_is_at_most_the_bevel_wide() {
        for upscale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            for short in [60, 110, 400, 900] {
                let band = band_px(short, upscale);
                assert!(band <= PROMPT_BEVEL as f32 * upscale as f32 + 1e-3, "{band}");
                assert!(band <= short as f32 * 0.4 + 1e-3, "{band} of {short}");
            }
        }
        // The sharp half is read only where t < 1, i.e. within `band` of the
        // edge: the model's `band_pixel` leaves the interior untouched.
        let (band, strength) = band_params(1.0);
        let pat = text(192, 192, 2);
        let blurred = chain(&pat, PROMPT_PASSES, PROMPT_BLUR_SIZE);
        let lvl1 = pass(&pat, (96, 96), 2.0, &DOWN);
        let nb = band.ceil() as i32;
        let inside = band_pixel(
            &blurred,
            &lvl1,
            64 + nb + 2,
            90,
            64,
            band,
            strength,
            0.25,
            false,
            PROMPT_SHARP_SHARE,
        );
        let tinted = blurred.at(64 + nb + 2, 90) * 0.75 + 26.0 * 0.25;
        assert!((inside[1] - tinted).abs() < 1e-3, "{inside:?} vs {tinted}");
    }

    /// Documented bounds of the rim band, in 8-bit levels of content-caused swing
    /// over any 16x16 patch, at the tint floor, both schemes, 1x and 2x. FINE
    /// detail (1 and 2 px stripes, 5x7 text at 1x and 2x: what a client hides a
    /// readable message in) stays under `BAND_FINE_BOUND`, a 12 level swing on a
    /// ring at most the bevel wide; COARSE structure (4 to 16 px stripes and
    /// checkers, big type) is a shape not text, under `BAND_COARSE_BOUND`.
    const BAND_FINE_BOUND: f32 = 12.0;
    const BAND_COARSE_BOUND: f32 = 30.0;

    fn is_fine(name: &str) -> bool {
        matches!(
            name,
            "1 px checker"
                | "1 px stripes"
                | "2 px stripes"
                | "2 px horizontal stripes"
                | "text 5x7 at 1x"
                | "text 5x7 at 2x"
        )
    }

    #[test]
    fn rim_band_leak_is_bounded() {
        for light in [false, true] {
            for upscale in [1.0, 2.0] {
                let (band, strength) = band_params(upscale);
                for (name, pattern) in worst_patterns(192) {
                    let c = band_leak(
                        &pattern,
                        PROMPT_MIN_TINT_ALPHA,
                        light,
                        band,
                        strength,
                        PROMPT_SHARP_SHARE,
                    );
                    println!(
                        "band {} {upscale}x {name}: {c:.1} levels (band {band:.0} px)",
                        if light { "light" } else { "dark" }
                    );
                    let bound = if is_fine(name) {
                        BAND_FINE_BOUND
                    } else {
                        BAND_COARSE_BOUND
                    };
                    assert!(
                        c <= bound,
                        "{name} ({light}, {upscale}x) leaks {c:.1} in the band, bound {bound}"
                    );
                }
            }
        }
    }

    /// The band share is not slack: double it and the fine bound fails; show
    /// all of the lightly blurred half (a window's lens) and it is far over.
    #[test]
    fn the_sharp_share_is_not_slack() {
        let (band, strength) = band_params(1.0);
        let stripes = Grid::new(192, 192, |x, _| if (x / 2) % 2 == 0 { 255.0 } else { 0.0 });
        let at = band_leak(
            &stripes,
            PROMPT_MIN_TINT_ALPHA,
            false,
            band,
            strength,
            PROMPT_SHARP_SHARE,
        );
        let twice = band_leak(
            &stripes,
            PROMPT_MIN_TINT_ALPHA,
            false,
            band,
            strength,
            2.0 * PROMPT_SHARP_SHARE,
        );
        let all = band_leak(&stripes, PROMPT_MIN_TINT_ALPHA, false, band, strength, 1.0);
        assert!(
            at <= BAND_FINE_BOUND && twice > BAND_FINE_BOUND && all > 5.0 * BAND_FINE_BOUND,
            "{at} {twice} {all}"
        );
    }

    /// A white to black step is not a message but it is the largest signal a
    /// client can put behind the panel; its blurred edge is a soft ramp. The
    /// per-window bound is looser (a ramp is large-scale), the ramp's slope is
    /// what is checked: under 1 level per px of the body.
    #[test]
    fn a_full_step_leaves_only_a_soft_ramp() {
        let step = Grid::new(256, 64, |x, _| if x < 128 { 255.0 } else { 0.0 });
        let blurred = chain(&step, PROMPT_PASSES, PROMPT_BLUR_SIZE);
        let mut slope = 0.0f32;
        for x in 8..blurred.w - 8 {
            slope = slope.max((blurred.at(x + 1, 32) - blurred.at(x, 32)).abs());
        }
        let body = slope * (1.0 - PROMPT_MIN_TINT_ALPHA);
        println!("step slope after tint: {body:.3} levels/px");
        assert!(body <= 1.0, "{body}");
    }
}
