// SPDX-License-Identifier: AGPL-3.0-only
//! The redaction card a capture shows where a `secret` surface was (ADR 0106):
//! "glass police tape". An opaque dark glass card the size of what it covers,
//! diagonal gold stripes, a hairline rim and one centred label. No title and
//! no app icon: the card is the same for every window, so it says that
//! something is hidden and nothing about what.
//!
//! State-free. The caller (`render/capture.rs`, which decides *what* is
//! redacted) passes a rectangle and gets render elements back. Two small
//! textures, the stripe tile and the label, are painted once per renderer and
//! raster scale and kept in [`Textures`]; nothing is repainted per frame and
//! nothing grows with the size of the window. A window is covered by a few
//! repeats of the stripe tile.
//!
//! The fill is *opaque*. A node redaction draws the card over the surface's
//! own elements, so any translucency would let the hidden pixels through.

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
pub const LABEL: &str = "Private: Redacted";

/// Stripe period along the diagonal, logical px.
pub const PERIOD: i32 = 28;
/// The stripe tile's side, logical px. A whole number of periods, so tiles
/// meet without a seam.
pub const TILE: i32 = PERIOD * 12;
/// Rim width, logical px.
pub const RIM: i32 = 1;
/// Room a label needs around itself before it is drawn at all.
const LABEL_MARGIN: (i32, i32) = (16, 8);

/// The card's glass: dark and opaque, straight alpha. Never below 1.0 (see
/// the module doc and `the_fill_is_opaque`).
const FILL: Rgba = [0.07, 0.07, 0.09, 1.0];
/// The tape: gold, never a glow.
const TAPE: Rgba = [0.93, 0.74, 0.28, 0.34];
const RIM_COLOR: Rgba = [1.0, 1.0, 1.0, 0.14];
const LABEL_FILL: Rgba = [0.06, 0.06, 0.08, 0.94];
const LABEL_INK: Rgba = [0.96, 0.96, 0.97, 1.0];
const LABEL_KEYLINE: Rgba = [0.0, 0.0, 0.0, 0.25];

type Rect = Rectangle<i32, Logical>;

/// The repeats of the stripe tile that cover `size`, relative to the card's
/// corner. Edge repeats are cut short; none overlap.
pub fn tiles(size: Size<i32, Logical>) -> Vec<Rect> {
    let mut out = Vec::new();
    let mut y = 0;
    while y < size.h {
        let h = TILE.min(size.h - y);
        let mut x = 0;
        while x < size.w {
            let w = TILE.min(size.w - x);
            out.push(Rect::new((x, y).into(), (w, h).into()));
            x += TILE;
        }
        y += TILE;
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

fn stripe_tile(dev: usize) -> Raster {
    let side = TILE as usize * dev;
    let mut p = Paint::new(side, side);
    let d = dev as f32;
    p.shade((0.0, 0.0, side as f32, side as f32), TAPE, |x, y| {
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

/// Push the card for `rect` (global logical) into `out`, front to back: label,
/// rim, stripes, glass. `output_loc` is the output's corner in the same
/// space. A texture that fails to upload is skipped; the glass fill is
/// always there, so a failure only costs the pattern, never the redaction.
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
            (7, 14),
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
    for band in rim(rect.size) {
        out.push(solid(RIM_COLOR, band, at, scale));
    }

    if textures.stripes.as_ref().is_none_or(|(d, _)| *d != dev) {
        let raster = stripe_tile(dev);
        textures.stripes = crate::annotation::upload(renderer, &raster, dev).map(|b| (dev, b));
    }
    if let Some((_, buffer)) = &textures.stripes {
        for t in tiles(rect.size) {
            out.push(texture(buffer, at + t.loc, t.size, scale));
        }
    }

    out.push(solid(FILL, local, at, scale));
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
    fn tiles_cover_the_card_exactly_without_overlap() {
        for (w, h) in [
            (1, 1),
            (TILE, TILE),
            (TILE + 1, 2 * TILE - 3),
            (1920, 1080),
            (5, 700),
        ] {
            let ts = tiles(size(w, h));
            let area: i64 = ts.iter().map(|t| t.size.w as i64 * t.size.h as i64).sum();
            assert_eq!(area, w as i64 * h as i64, "{w}x{h}");
            let card = Rect::from_size(size(w, h));
            for (i, a) in ts.iter().enumerate() {
                assert!(card.contains_rect(*a));
                assert!(a.size.w <= TILE && a.size.h <= TILE);
                for b in &ts[i + 1..] {
                    assert!(a.intersection(*b).is_none(), "{a:?} overlaps {b:?}");
                }
            }
        }
        assert!(tiles(size(0, 10)).is_empty());
    }

    #[test]
    fn a_fullscreen_card_is_under_a_hundred_tiles_not_a_texture_the_size_of_the_screen() {
        assert!(tiles(size(3840, 2160)).len() <= 100);
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
    fn the_stripe_tile_is_seamless_across_its_own_edges() {
        // The pattern is a function of x + y with a period that divides the
        // tile, so shifting by a period and wrapping changes nothing.
        let t = stripe_tile(1);
        let side = TILE as usize;
        let at = |x: usize, y: usize| t.px[(y * side + x) * 4 + 3];
        for (x, y) in [(0, 0), (5, 9), (100, 3), (side - 1, side - 1), (side - 2, 40)] {
            assert_eq!(at(x, y), at((x + PERIOD as usize) % side, y));
            assert_eq!(at(x, y), at(x, (y + PERIOD as usize) % side));
        }
        assert!(t.px.chunks(4).any(|p| p[3] > 0) && t.px.chunks(4).any(|p| p[3] == 0));
    }

    #[test]
    fn the_label_is_plain_ascii_the_atlas_draws() {
        assert!(LABEL.chars().all(crate::hud_font::covers));
    }
}
