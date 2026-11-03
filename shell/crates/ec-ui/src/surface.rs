// SPDX-License-Identifier: AGPL-3.0-only
//! The live surface values: `decoration.surface.{tint, card-fill, inset-fill}`.
//!
//! Three knobs the compositor owns and every Settings-style window reads. The
//! values are sRGB intent, as STYLE.md states them. The card and inset fills are converted
//! here to the linear-light alpha iced blends with, so each one reads on screen
//! as its sRGB figure. The tint's alpha is the one exception: the compositor
//! blends it, in sRGB, so it is written as is ([`tint_alpha`]).
//!
//! The values are process-wide rather than fields of each app. The style
//! functions that read them are plain `fn`s (`theme::inset` is handed to
//! `.style` in many places), and threading a value through each call site
//! would only lengthen the same value. Writers are the Config reload
//! handlers ([`crate::ipc::sync_surface`]); a reader sees the new value on its
//! next draw.

use std::sync::atomic::{AtomicU32, Ordering};

use iced::Color;

use crate::tokens::color::base;

/// The three surface values, sRGB intent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    /// Window tint opacity over the glass (`decoration.surface.tint`).
    pub tint: f32,
    /// A hairline card's white fill (`decoration.surface.card-fill`).
    pub card_fill: f32,
    /// An inset group's white fill (`decoration.surface.inset-fill`).
    pub inset_fill: f32,
}

/// The compositor's defaults, from `ec-abyss-config`'s schema.
pub const DEFAULT: Values = Values {
    tint: 0.70,
    card_fill: 0.035,
    inset_fill: 0.03,
};

/// A rim runs this many times its fill: the spec's `.12` border over a `.035`
/// card. Scaling by it keeps the rim in proportion when the fill is tuned.
pub const RIM_PER_FILL: f32 = 0.12 / 0.035;

static TINT: AtomicU32 = AtomicU32::new(DEFAULT.tint.to_bits());
static CARD_FILL: AtomicU32 = AtomicU32::new(DEFAULT.card_fill.to_bits());
static INSET_FILL: AtomicU32 = AtomicU32::new(DEFAULT.inset_fill.to_bits());

/// The values as last set; [`DEFAULT`] until something sets them.
pub fn current() -> Values {
    Values {
        tint: f32::from_bits(TINT.load(Ordering::Relaxed)),
        card_fill: f32::from_bits(CARD_FILL.load(Ordering::Relaxed)),
        inset_fill: f32::from_bits(INSET_FILL.load(Ordering::Relaxed)),
    }
}

/// Store new values. Each is clamped to its schema range, so a bad reply can
/// never paint a fill outside what the compositor accepts.
pub fn set(v: Values) {
    TINT.store(v.tint.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    CARD_FILL.store(v.card_fill.clamp(0.0, 0.30).to_bits(), Ordering::Relaxed);
    INSET_FILL.store(v.inset_fill.clamp(0.0, 0.30).to_bits(), Ordering::Relaxed);
}

/// White at the linear-light alpha that reads on screen as sRGB `intent` over
/// [`base`]. Solved per channel and averaged, since BASE is not neutral.
pub fn white_fill(intent: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: linear_alpha(intent),
    }
}

fn is_light() -> bool {
    crate::tokens::color::palette().scheme == ec_theme::Scheme::Light
}

/// A card's or inset's ground for the setting `intent`. Dark: white at that
/// alpha ([`white_fill`]). Light: a step darker than the glass beneath, in the
/// ink at the palette's own alpha for that step (`step`: `lift_soft` for a
/// card, `cell` for an inset), scaled by the setting's proportion of its
/// dark default `default`. White on the light column would lift a card above
/// the glass it sits on; glass stacked on glass moves away from the backdrop
/// (STYLE.md "Light"), so a light card or inset darkens, and an inset darkens
/// past its card. `card-fill` and `inset-fill` still tune both.
pub fn group_fill(intent: f32, step: ec_theme::Rgba, default: f32) -> Color {
    if !is_light() {
        return white_fill(intent);
    }
    let ink = crate::tokens::color::to_iced(step);
    let k = (intent / default).clamp(0.0, 3.0);
    Color {
        a: (ink.a * k).clamp(0.0, 1.0),
        ..ink
    }
}

/// The 1px rim of a card or inset whose fill setting is `fill`: [`RIM_PER_FILL`]
/// times it in white on dark. On light the rim is the ink, because a white edge
/// on a white card is no edge (STYLE.md "Light": ink hairlines, white rim only
/// from the compositor).
pub fn group_rim(fill: f32) -> Color {
    if !is_light() {
        return white_fill(fill * RIM_PER_FILL);
    }
    let p = crate::tokens::color::palette();
    // A light card's rim is its ink hairline: `border` at the setting's own
    // proportion (the default `.035` is the token itself).
    let k = (fill / DEFAULT.card_fill).clamp(0.0, 3.0);
    let b = crate::tokens::color::to_iced(p.border);
    Color {
        a: (b.a * k).clamp(0.0, 1.0),
        ..b
    }
}

/// [`base`] at the window tint `intent`, as the alpha the buffer carries.
/// The tint is the one fill iced does not blend itself: it lands on a
/// transparent clear, and the compositor then blends the surface's alpha over
/// the blurred backdrop in sRGB, exactly as it blends a terminal's `alpha=0.72`.
/// So the alpha is the intent, unconverted. (Solving a linear-light alpha
/// against mid-grey, as the card and inset fills below are, gave about `.89`
/// for `.70` and made Settings visibly darker than a terminal.)
pub fn tint_alpha(intent: f32) -> f32 {
    intent.clamp(0.0, 1.0)
}

/// [`base`] at the alpha of the tint `intent`.
pub fn tint(intent: f32) -> Color {
    Color {
        a: tint_alpha(intent),
        ..base()
    }
}

fn linear_alpha(intent: f32) -> f32 {
    let channels = [base().r, base().g, base().b];
    let sum: f32 = channels
        .iter()
        .map(|&b| {
            let target = b + intent * (1.0 - b);
            let bl = to_linear(b);
            (to_linear(target) - bl) / (1.0 - bl)
        })
        .sum();
    (sum / 3.0).clamp(0.0, 1.0)
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_card_fill_reads_as_its_srgb_figure() {
        // STYLE.md's .035 is about .0032 in linear light over BASE.
        let a = white_fill(0.035).a;
        assert!((a - 0.0032).abs() < 0.0005, "got {a}");
    }

    #[test]
    fn no_fill_is_no_alpha_and_the_range_is_monotonic() {
        assert_eq!(white_fill(0.0).a, 0.0);
        assert!(white_fill(0.10).a > white_fill(0.05).a);
    }

    #[test]
    fn tint_endpoints_are_exact() {
        assert_eq!(tint_alpha(0.0), 0.0);
        assert!((tint_alpha(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tint_is_monotonic() {
        let mut prev = -1.0;
        for i in 0..=20 {
            let a = tint_alpha(i as f32 / 20.0);
            assert!(a >= prev, "not monotonic at {i}: {a} < {prev}");
            prev = a;
        }
    }

    #[test]
    fn the_tint_alpha_is_the_intent_the_compositor_blends() {
        assert_eq!(tint_alpha(0.70), 0.70);
        assert_eq!(tint(0.72).a, 0.72);
    }

    #[test]
    fn set_clamps_to_the_schema_range() {
        set(Values {
            tint: 4.0,
            card_fill: -1.0,
            inset_fill: 0.9,
        });
        let v = current();
        assert_eq!(v.tint, 1.0);
        assert_eq!(v.card_fill, 0.0);
        assert_eq!(v.inset_fill, 0.30);
        set(DEFAULT);
        assert_eq!(current(), DEFAULT);
    }
}
