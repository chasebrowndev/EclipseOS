// SPDX-License-Identifier: AGPL-3.0-only
//! The live surface values: `decoration.surface.{tint, card-fill, inset-fill}`.
//!
//! Three knobs the compositor owns and every Settings-style window reads. The
//! values are sRGB intent, as STYLE.md states them. All three are converted
//! here to the linear-light alpha iced blends with, so each one reads on screen
//! as its sRGB figure. The tint's colour is the ground itself, so its conversion
//! is solved against one reference backdrop, mid-grey ([`MID`]).
//!
//! The values are process-wide rather than fields of each app. The style
//! functions that read them are plain `fn`s (`theme::inset` is handed to
//! `.style` in many places), and threading a value through each call site
//! would only lengthen the same value. Writers are the Config reload
//! handlers ([`crate::ipc::sync_surface`]); a reader sees the new value on its
//! next draw.

use std::sync::atomic::{AtomicU32, Ordering};

use iced::Color;

use crate::tokens::color::BASE;

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
/// [`BASE`]. Solved per channel and averaged, since BASE is not neutral.
pub fn white_fill(intent: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: linear_alpha(intent),
    }
}

/// The reference backdrop the tint is solved against: sRGB 128, mid-grey.
pub const MID: f32 = 128.0 / 255.0;

/// The linear-light alpha of [`BASE`] over the backdrop that reads on screen as
/// sRGB `intent` over [`MID`]. Per channel, `a = (L(mid) - L(out)) / (L(mid) -
/// L(base))` with `out = intent*base + (1-intent)*mid` in sRGB and `L` the
/// sRGB-to-linear transfer, then averaged since BASE is not neutral. At `.70`
/// this is about `.89`, not `.70`. The tint is then exact over mid-grey and
/// close over other backdrops, where a single alpha cannot be exact.
pub fn tint_alpha(intent: f32) -> f32 {
    let channels = [BASE.r, BASE.g, BASE.b];
    let lin_mid = to_linear(MID);
    let sum: f32 = channels
        .iter()
        .map(|&b| {
            let out = intent * b + (1.0 - intent) * MID;
            (lin_mid - to_linear(out)) / (lin_mid - to_linear(b))
        })
        .sum();
    (sum / 3.0).clamp(0.0, 1.0)
}

/// [`BASE`] at the linear alpha that reads as the tint `intent` over mid-grey.
pub fn tint(intent: f32) -> Color {
    Color {
        a: tint_alpha(intent),
        ..BASE
    }
}

fn linear_alpha(intent: f32) -> f32 {
    let channels = [BASE.r, BASE.g, BASE.b];
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
    fn tint_over_mid_grey_matches_the_srgb_blend() {
        // Linear blend of BASE at the solved alpha over mid-grey, back to sRGB,
        // must land within 1.5 of the sRGB blend of the intent. One averaged
        // alpha cannot match all three BASE channels exactly; the residual is
        // about one code value.
        let intent = 0.70_f32;
        let a = tint_alpha(intent);
        let to_srgb = |l: f32| {
            if l <= 0.003_130_8 {
                l * 12.92
            } else {
                1.055 * l.powf(1.0 / 2.4) - 0.055
            }
        };
        for b in [BASE.r, BASE.g, BASE.b] {
            let lin = a * to_linear(b) + (1.0 - a) * to_linear(MID);
            let got = to_srgb(lin) * 255.0;
            let want = (intent * b + (1.0 - intent) * MID) * 255.0;
            assert!((got - want).abs() < 1.5, "got {got}, want {want}");
        }
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
