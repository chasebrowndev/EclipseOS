// SPDX-License-Identifier: AGPL-3.0-only
//! The redaction cover a capture shows where a `secret` surface was (ADR 0106).
//! An opaque, warm-black slab the size of what it covers, one calm hatched
//! band across its middle, a hairline rim and a small "Private" pill. No
//! title and no app icon: the cover is the same for every window, so it says
//! that something is hidden and nothing about what.
//!
//! State-free. The caller (`render/capture.rs`, which decides *what* is
//! redacted) passes a rectangle and gets render elements back. Two small
//! textures, the hatch tile and the label, are painted once per renderer and
//! raster scale and kept in [`Textures`]; nothing is repainted per frame and
//! nothing grows with the size of the window.
//!
//! **Nothing leaks, by construction.** The cover never samples, blurs or
//! blends the pixels beneath it: its base is a solid colour of alpha 1.0
//! covering the whole rectangle (a blur or a translucent slab would carry the
//! hidden window's shapes through, so there is none). Every other element
//! (hatch, rim, pill) is drawn *above* that base and only ever adds ink to
//! it. The base is pinned by tests: alpha 1.0, and a render element whose
//! opaque region is the entire rectangle.

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{GlesRenderer, GlesTexture},
    },
    utils::{Logical, Point, Rectangle, Scale, Size},
};

use crate::{
    hud::{self, Paint},
    hud_font::{raster_scale, Face},
    text::{Raster, Rgba},
    AbyssRenderElement,
};

/// The label. Fixed, ASCII, and the same for every card.
pub const LABEL: &str = "Private";

/// Hatch period along the diagonal, logical px.
pub const PERIOD: i32 = 28;
/// The hatch tile's side, logical px. A whole number of periods, so tiles
/// meet without a seam.
pub const TILE: i32 = PERIOD * 12;
/// Rim width, logical px.
pub const RIM: i32 = 1;
/// The hatched band's height, logical px: two periods, calm and narrow.
pub const BAND: i32 = PERIOD * 2;
/// Room above and below the band a card needs before it gets one.
const BAND_MARGIN: i32 = 24;
/// Room a label needs around itself before it is drawn at all.
const LABEL_MARGIN: (i32, i32) = (16, 8);

/// The slab: warm near-black (the style's `#0b0906` family) and opaque,
/// straight alpha. Never below 1.0 (see the module doc and
/// `the_fill_is_opaque`).
const FILL: Rgba = [0.055, 0.047, 0.036, 1.0];
/// The hatch: a faint warm white, never the accent and never a glow.
const HATCH: Rgba = [0.96, 0.92, 0.84, 0.11];
/// The hairlines above and below the band.
const BAND_EDGE: Rgba = [1.0, 1.0, 1.0, 0.12];
const RIM_COLOR: Rgba = [1.0, 1.0, 1.0, 0.14];
const LABEL_FILL: Rgba = [0.09, 0.08, 0.06, 1.0];
const LABEL_INK: Rgba = [0.96, 0.94, 0.90, 1.0];
const LABEL_KEYLINE: Rgba = [0.0, 0.0, 0.0, 0.25];

type Rect = Rectangle<i32, Logical>;

/// The hatched band: full width, [`BAND`] tall, centred vertically. `None`
/// for a card too short to carry one with air above and below.
pub fn band(size: Size<i32, Logical>) -> Option<Rect> {
    if size.w < PERIOD * 2 || size.h < BAND + 2 * BAND_MARGIN {
        return None;
    }
    Some(Rect::new(
        ((0), (size.h - BAND) / 2).into(),
        (size.w, BAND).into(),
    ))
}

/// The repeats of the hatch tile across `band`, relative to the card's
/// corner. Every repeat starts on the band's top edge and at a multiple of
/// [`TILE`], so the pattern is continuous from one to the next.
pub fn band_tiles(band: Rect) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut x = 0;
    while x < band.size.w {
        let w = TILE.min(band.size.w - x);
        out.push(Rect::new((x, band.loc.y).into(), (w, band.size.h).into()));
        x += TILE;
    }
    out
}

/// Where a label of `label` (logical px) sits on a card of `size`, relative
/// to the card's corner: centred, or `None` when the card is too small to
/// hold it with a margin (a redacted field, not a window).
pub fn label_rect(size: Size<i32, Logical>, label: (i32, i32)) -> Option<Rect> {
    if size.w < label.0 + 2 * LABEL_MARGIN.0 || size.h < label.1 + 2 * LABEL_MARGIN.1 {
        return None;
    }
    Some(Rect::new(
        ((size.w - label.0) / 2, (size.h - label.1) / 2).into(),
        label.into(),
    ))
}

/// The four rim bands, relative to the card's corner: top and bottom span the
/// width, left and right fit between them. Empty for a card too small to
/// have an inside.
pub fn rim(size: Size<i32, Logical>) -> Vec<Rect> {
    if size.w < 4 * RIM || size.h < 4 * RIM {
        return Vec::new();
    }
    let (w, h) = (size.w, size.h);
    vec![
        Rect::new((0, 0).into(), (w, RIM).into()),
        Rect::new((0, h - RIM).into(), (w, RIM).into()),
        Rect::new((0, RIM).into(), (RIM, h - 2 * RIM).into()),
        Rect::new((w - RIM, RIM).into(), (RIM, h - 2 * RIM).into()),
    ]
}

/// Stripe coverage at logical diagonal position `u` (x + y), with `dev`
/// device pixels per logical one: a band half a period wide, one device pixel
/// of antialiasing, wrap-aware so it is continuous across tile edges.
pub fn stripe(u: f32, dev: f32) -> f32 {
    let p = PERIOD as f32;
    let r = u.rem_euclid(p);
    let d = (r - p / 4.0).abs();
    let d = d.min(p - d);
    ((p / 4.0 - d) * dev * std::f32::consts::FRAC_1_SQRT_2 + 0.5).clamp(0.0, 1.0)
}

fn hatch_tile(dev: usize) -> Raster {
    let (w, h) = (TILE as usize * dev, BAND as usize * dev);
    let mut p = Paint::new(w, h);
    let d = dev as f32;
    p.shade((0.0, 0.0, w as f32, h as f32), HATCH, |x, y| {
        stripe((x + y) / d, d)
    });
    p.into_raster()
}

/// The card's textures for one renderer: the stripe tile and the label, each
/// with the raster scale it was painted at.
#[derive(Default)]
pub struct Textures {
    stripes: Option<(usize, TextureBuffer<GlesTexture>)>,
    label: Option<(usize, TextureBuffer<GlesTexture>, (i32, i32))>,
}

fn solid(color: Rgba, rect: Rect, at: Point<i32, Logical>, scale: Scale<f64>) -> AbyssRenderElement {
    // Solid buffers are premultiplied.
    let c = [
        color[0] * color[3],
        color[1] * color[3],
        color[2] * color[3],
        color[3],
    ];
    let buffer = SolidColorBuffer::new(rect.size, c);
    AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        phys(rect.loc + at, scale),
        scale,
        1.0,
        Kind::Unspecified,
    ))
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, smithay::utils::Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

fn texture(
    buffer: &TextureBuffer<GlesTexture>,
    at: Point<i32, Logical>,
    size: Size<i32, Logical>,
    scale: Scale<f64>,
) -> AbyssRenderElement {
    AbyssRenderElement::Texture(TextureRenderElement::from_texture_buffer(
        phys(at, scale).to_f64(),
        buffer,
        None,
        Some(Rectangle::new((0.0, 0.0).into(), size.to_f64())),
        Some(size),
        Kind::Unspecified,
    ))
}

/// The slab's base element: a solid of [`FILL`] over the whole of `local`.
/// Its own function so a test can hold it to the opacity guarantee.
fn glass(local: Rect, at: Point<i32, Logical>, scale: Scale<f64>) -> SolidColorRenderElement {
    let c = [FILL[0], FILL[1], FILL[2], FILL[3]]; // alpha 1.0: premultiplied == straight
    let buffer = SolidColorBuffer::new(local.size, c);
    SolidColorRenderElement::from_buffer(
        &buffer,
        phys(local.loc + at, scale),
        scale,
        1.0,
        Kind::Unspecified,
    )
}

/// Push the cover for `rect` (global logical) into `out`, front to back:
/// label, band hairlines, hatch, rim, slab. `output_loc` is the output's
/// corner in the same space. A texture that fails to upload is skipped; the
/// slab is always there, so a failure only costs the pattern, never the
/// redaction.
pub fn card(
    textures: &mut Textures,
    renderer: &mut GlesRenderer,
    rect: Rect,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    out: &mut Vec<AbyssRenderElement>,
) {
    if rect.size.w <= 0 || rect.size.h <= 0 {
        return;
    }
    let dev = raster_scale(scale.x);
    let at = Point::from((0, 0)) - output_loc + rect.loc;

    if textures.label.as_ref().is_none_or(|(d, ..)| *d != dev) {
        let (raster, (w, h)) = hud::pill(
            LABEL,
            Face::Title,
            (5, 14),
            LABEL_FILL,
            Some(RIM_COLOR),
            LABEL_KEYLINE,
            LABEL_INK,
            dev,
        );
        textures.label = crate::annotation::upload(renderer, &raster, dev).map(|b| {
            let m = hud::PILL_MARGIN as i32;
            (dev, b, (w as i32 + 2 * m, h as i32 + 2 * m))
        });
    }
    if let Some((_, buffer, size)) = &textures.label {
        if let Some(l) = label_rect(rect.size, *size) {
            out.push(texture(buffer, at + l.loc, l.size, scale));
        }
    }

    let local = Rect::from_size(rect.size);
    if let Some(b) = band(rect.size) {
        for y in [b.loc.y, b.loc.y + b.size.h - RIM] {
            let line = Rect::new((0, y).into(), (b.size.w, RIM).into());
            out.push(solid(BAND_EDGE, line, at, scale));
        }
    }
    for edge in rim(rect.size) {
        out.push(solid(RIM_COLOR, edge, at, scale));
    }

    if textures.stripes.as_ref().is_none_or(|(d, _)| *d != dev) {
        let raster = hatch_tile(dev);
        textures.stripes = crate::annotation::upload(renderer, &raster, dev).map(|b| (dev, b));
    }
    if let (Some((_, buffer)), Some(b)) = (&textures.stripes, band(rect.size)) {
        for t in band_tiles(b) {
            out.push(texture(buffer, at + t.loc, t.size, scale));
        }
    }

    out.push(AbyssRenderElement::Solid(glass(local, at, scale)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: i32, h: i32) -> Size<i32, Logical> {
        (w, h).into()
    }

    #[test]
    fn the_fill_is_opaque() {
        // A node redaction sits over the surface it hides; the fill is the
        // only element covering the whole rect, so it must not let light through.
        assert_eq!(FILL[3], 1.0);
    }

    #[test]
    fn the_slab_covers_the_whole_rectangle_opaquely() {
        // The no-leak guarantee: the base element is a solid of alpha 1.0 whose
        // opaque region is its entire geometry, so nothing beneath shows
        // through anywhere, at any scale.
        use smithay::backend::renderer::element::Element;
        for (w, h, sc) in [(1, 1, 1.0), (640, 480, 1.0), (1921, 1079, 1.5), (3840, 2160, 2.0)] {
            let scale = Scale::from(sc);
            let e = glass(Rect::from_size(size(w, h)), (13, 7).into(), scale);
            assert_eq!(Element::alpha(&e), 1.0);
            let geo = Element::geometry(&e, scale);
            let regions = Element::opaque_regions(&e, scale);
            let opaque: i64 = regions.iter().map(|r| r.size.w as i64 * r.size.h as i64).sum();
            assert_eq!(opaque, geo.size.w as i64 * geo.size.h as i64, "{w}x{h}@{sc}");
        }
    }

    #[test]
    fn nothing_translucent_is_the_base() {
        // Ink layers may be translucent (they sit above the slab); the slab
        // and the label's ground are not.
        assert_eq!(FILL[3], 1.0);
        assert_eq!(LABEL_FILL[3], 1.0);
        for ink in [HATCH, BAND_EDGE, RIM_COLOR] {
            assert!(ink[3] < 0.2, "decor stays faint");
        }
    }

    #[test]
    fn the_band_is_centred_full_width_and_inside() {
        let card = size(800, 500);
        let b = band(card).unwrap();
        assert_eq!(b.size, (800, BAND).into());
        assert_eq!(b.loc.y, (500 - BAND) / 2);
        assert!(Rect::from_size(card).contains_rect(b));
        assert!(band(size(800, BAND + 2 * BAND_MARGIN - 1)).is_none());
        assert!(band(size(10, 500)).is_none());
    }

    #[test]
    fn band_tiles_cover_the_band_without_overlap() {
        for w in [PERIOD * 2, TILE, TILE + 1, 1920, 3840] {
            let b = band(size(w, 400)).unwrap();
            let ts = band_tiles(b);
            let area: i64 = ts.iter().map(|t| t.size.w as i64 * t.size.h as i64).sum();
            assert_eq!(area, b.size.w as i64 * b.size.h as i64, "{w}");
            for (i, a) in ts.iter().enumerate() {
                assert!(b.contains_rect(*a));
                assert_eq!(a.loc.x % TILE, 0);
                for o in &ts[i + 1..] {
                    assert!(a.intersection(*o).is_none());
                }
            }
        }
    }

    #[test]
    fn the_label_is_centred_and_inside() {
        let l = label_rect(size(400, 300), (140, 30)).unwrap();
        assert_eq!(l.loc, (130, 135).into());
        assert!(Rect::from_size(size(400, 300)).contains_rect(l));
    }

    #[test]
    fn a_card_too_small_for_the_label_has_none() {
        assert!(label_rect(size(150, 300), (140, 30)).is_none());
        assert!(label_rect(size(400, 40), (140, 30)).is_none());
        assert!(label_rect(size(172, 46), (140, 30)).is_some());
    }

    #[test]
    fn the_rim_hugs_the_edge_and_tiles_without_overlap() {
        let bands = rim(size(100, 60));
        assert_eq!(bands.len(), 4);
        let area: i32 = bands.iter().map(|b| b.size.w * b.size.h).sum();
        assert_eq!(area, 100 * 60 - 98 * 58);
        for (i, a) in bands.iter().enumerate() {
            for b in &bands[i + 1..] {
                assert!(a.intersection(*b).is_none());
            }
        }
        assert!(rim(size(3, 3)).is_empty());
    }

    #[test]
    fn stripes_alternate_and_repeat_each_period() {
        let p = PERIOD as f32;
        // The middle of a band is on, the middle of a gap is off.
        assert_eq!(stripe(p / 4.0, 1.0), 1.0);
        assert_eq!(stripe(3.0 * p / 4.0, 1.0), 0.0);
        for u in [0.0f32, 3.3, 11.0, 20.5] {
            assert!((stripe(u, 2.0) - stripe(u + p, 2.0)).abs() < 1e-4);
            assert!((stripe(u, 2.0) - stripe(u + TILE as f32, 2.0)).abs() < 1e-4);
        }
    }

    #[test]
    fn the_hatch_tile_is_seamless_across_its_horizontal_edges() {
        // The pattern is a function of x + y with a period that divides the
        // tile's width, so repeats side by side meet without a seam.
        let t = hatch_tile(1);
        let (w, h) = (TILE as usize, BAND as usize);
        assert_eq!(t.px.len(), w * h * 4);
        let at = |x: usize, y: usize| t.px[(y * w + x) * 4 + 3];
        for (x, y) in [(0, 0), (5, 9), (100, 3), (w - 1, h - 1), (w - 2, 40)] {
            assert_eq!(at(x, y), at((x + PERIOD as usize) % w, y));
        }
        assert!(t.px.chunks(4).any(|p| p[3] > 0) && t.px.chunks(4).any(|p| p[3] == 0));
    }

    #[test]
    fn the_label_is_plain_ascii_the_atlas_draws() {
        assert!(LABEL.chars().all(crate::hud_font::covers));
    }
}
