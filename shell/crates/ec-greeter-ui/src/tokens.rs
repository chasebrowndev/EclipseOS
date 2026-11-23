// SPDX-License-Identifier: AGPL-3.0-only
//! The greeter's tokens (docs/STYLE.md, ADR 0126): every literal size, radius,
//! spacing and motion number the login screen uses, in one place.
//!
//! Colour is not transcribed here. It comes from the shared palette
//! (`ec_theme::derive`, ADR 0084), the table the desktop reads too, so the
//! login screen and the session agree bit for bit. Type scale and spacing come
//! from `ec_style_tokens`, the vocabulary `ec-ui` re-exports. What is left is
//! what the greeter alone needs: pane radii, the sizes a tile scales to, and
//! the spring.

pub use ec_style_tokens::{size, space};

use ec_theme::Rgba;
use iced::{Color, Font};

/// A palette colour as iced's, channel for channel.
pub fn to_iced(c: Rgba) -> Color {
    Color {
        r: c.0[0],
        g: c.0[1],
        b: c.0[2],
        a: c.0[3],
    }
}

/// Light glass: the ink alphas the greeter's text uses (linear-light, as the
/// palette's are). The shared table's secondary and tertiary steps (.62, .42)
/// are fine on the settings column but fall under 4.5:1 on the frost over a
/// colourful wallpaper, so the greeter, which sits straight on the wallpaper,
/// steps them up. Hierarchy then comes from size, weight and the mono face.
pub mod light {
    pub const TEXT_SECONDARY: f32 = 0.92;
    pub const TEXT_TERTIARY: f32 = 0.90;
    /// Danger text on light glass: a deep brick red, as #bd3a26 is 2.5:1 there.
    pub const DANGER: u32 = 0x701609;
    /// How far the typed marks' accent text steps toward the ink.
    pub const MARK_INK: f32 = 0.4;
    /// A disabled control's ink, as a share (dark uses `backdrop::DISABLED`).
    pub const DISABLED_INK: f32 = 0.8;
    /// The luminance of the darkest glass ground text may land on: measured
    /// 0.49 over the colourful, pale, warm and shipped wallpapers, less a hover
    /// darkening and a margin.
    pub const WORST_GROUND_LUMINANCE: f32 = 0.40;
}

/// The luminance of the brightest dark glass ground a gauge may land on (the
/// smoked blur over the saturated test wallpapers measures 0.01 to 0.04).
pub const DARK_WORST_GROUND_LUMINANCE: f32 = 0.04;

/// The worst glass ground for `scheme`.
pub fn worst_ground(scheme: ec_theme::Scheme) -> f32 {
    match scheme {
        ec_theme::Scheme::Dark => DARK_WORST_GROUND_LUMINANCE,
        ec_theme::Scheme::Light => light::WORST_GROUND_LUMINANCE,
    }
}

/// The greeter's palette for `seeds`: the shared derivation, then (light only)
/// the greeter's own legibility steps.
pub fn palette(seeds: &ec_theme::Seeds, scheme: ec_theme::Scheme) -> ec_theme::Palette {
    let mut p = ec_theme::derive(seeds);
    if scheme == ec_theme::Scheme::Light {
        p.text_secondary = p.text.with_alpha(light::TEXT_SECONDARY);
        p.text_tertiary = p.text.with_alpha(light::TEXT_TERTIARY);
        p.danger = Rgba::rgb(light::DANGER);
    }
    p
}

/// A disabled control's ink as a share of its enabled ink.
pub fn disabled_ink(scheme: ec_theme::Scheme) -> f32 {
    match scheme {
        ec_theme::Scheme::Dark => backdrop::DISABLED,
        ec_theme::Scheme::Light => light::DISABLED_INK,
    }
}

/// The typed length's dots and caret: the accent, or its text step on light
/// glass (stepped toward the ink), where the gold itself would wash out.
pub fn typed_mark(p: &ec_theme::Palette) -> Rgba {
    match p.scheme {
        ec_theme::Scheme::Dark => p.accent,
        ec_theme::Scheme::Light => {
            let k = light::MARK_INK;
            let mix = |a: f32, b: f32| a * (1.0 - k) + b * k;
            Rgba::new(
                mix(p.accent_text.r(), p.text.r()),
                mix(p.accent_text.g(), p.text.g()),
                mix(p.accent_text.b(), p.text.b()),
                1.0,
            )
        }
    }
}

/// The Glances' text sizes. Their sizing rules are `ec_glance::tile`, shared with
/// the lock screen and the layout builder's preview.
pub const GLANCE_TYPE: ec_glance::tile::Type = ec_glance::tile::Type {
    micro: size::MICRO,
    body_small: size::BODY_SMALL,
    body: size::BODY,
};
/// A signal bar's corner radius.
pub const BAR_RADIUS: f32 = 2.0;

/// The window size before the compositor says otherwise, logical px.
pub const DEFAULT_WINDOW: (f32, f32) = (1920.0, 1080.0);

/// The card's corner radius (STYLE.md: 22-24px on the login screen). Glass
/// panes are clamped to half their short side, so a small card stays round.
pub const CARD_RADIUS: f32 = 22.0;
/// A cell inside a card: a user row, the password field.
pub const CELL_RADIUS: f32 = 14.0;
/// Asks for a pill: clamped to half the cell's short side.
pub const PILL_RADIUS: f32 = 999.0;
/// The gap a tile leaves on each side (`Rect::to_px`): neighbours sit 2x apart.
pub const TILE_GAP: u32 = ec_layout::intrinsic::TILE_GAP_PX as u32;
/// Card padding as a share of the card's short side, and its bounds.
/// The pad never exceeds this share of the card's short side.
/// The hairline width of a rim painted without glass.
pub const RIM: f32 = 1.0;
/// Drop shadows painted without glass. The compositor casts them with glass on.
pub const CELL_SHADOW_Y: f32 = 1.0;
pub const CELL_SHADOW_BLUR: f32 = 6.0;
pub const CARD_SHADOW_Y: f32 = 20.0;
pub const CARD_SHADOW_BLUR: f32 = 44.0;

/// The wallpaper and the frosted look of a pane painted without glass.
pub mod backdrop {
    /// The largest wallpaper file and side the greeter will decode.
    pub const MAX_BYTES: u64 = 32 * 1024 * 1024;
    pub const MAX_SIDE: u32 = 8192;
    /// The blurred copy: width divisor, floor, and softening sigma (px of the copy).
    pub const FROST_DIV: u32 = 14;
    pub const FROST_MIN: u32 = 64;
    pub const FROST_SIGMA: f32 = 3.0;
    /// The frosted look without glass is ink laid over the blurred wallpaper:
    /// (fill, rim) alphas of text ink. A card first.
    pub const CARD: (f32, f32) = (0.035, 0.18);
    /// A cell, by state: rest, hover, press.
    pub const CELL: (f32, f32) = (0.045, 0.10);
    pub const CELL_HOVER: (f32, f32) = (0.085, 0.18);
    pub const CELL_PRESS: (f32, f32) = (0.025, 0.10);
    /// The focused cell: a brighter fill and a crisp rim, never a glow.
    pub const FOCUS: (f32, f32) = (0.11, 0.50);
    pub const FOCUS_HOVER: (f32, f32) = (0.15, 0.60);
    pub const FOCUS_PRESS: (f32, f32) = (0.07, 0.50);
    /// How far a card's backing is darkened before the frost shows through.
    pub const CARD_BACK: f32 = 0.30;
    /// A control that cannot be used yet: its ground and its ink, as a share.
    pub const DISABLED: f32 = 0.5;
}

/// How a tile's type scales: each size is the smaller of a height share and a
/// width share, clamped. Fixed sizes are hints; a tile never gets a size it
/// cannot hold.
pub mod scale {
    /// The greeting line.
    pub const GREETING_H: f32 = 0.62;
    pub const GREETING_W: f32 = 0.07;
    pub const GREETING_MIN: f32 = 20.0;
    pub const GREETING_MAX: f32 = 44.0;
}

/// Inside a card: the field, the user rows, the pills, the Glances' parts.
pub mod cell {
    pub const FIELD_MIN: f32 = 36.0;
    pub const FIELD_MAX: f32 = 56.0;
    /// The line under the field: what is happening.
    pub const NOTE_H: f32 = 18.0;
    pub const NOTE_GAP: f32 = 10.0;
    /// The Users header row.
    pub const HEAD_H: f32 = 16.0;
    pub const ROW_MIN: f32 = 36.0;
    pub const ROW_MAX: f32 = 64.0;
    pub const ROW_GAP: f32 = 8.0;
    pub const ROW_PAD_X: f32 = 14.0;
    /// An account's disc: a share of its row, clamped.
    pub const AVATAR_FRAC: f32 = 0.7;
    pub const AVATAR_MIN: f32 = 22.0;
    pub const AVATAR_MAX: f32 = 44.0;
    /// The typed length: dots, then a caret.
    pub const DOT: f32 = 8.0;
    pub const DOT_GAP: f32 = 7.0;
    pub const DOTS_MAX: usize = 24;
    pub const CARET_W: f32 = 2.0;
    pub const CARET_H: f32 = 20.0;
    /// The danger rim of a refused field, as a share of full ink.
    pub const FAIL_RIM: f32 = 0.45;
    /// The Unlock pill beside the field: a share of the tile, clamped.
    pub const SUBMIT_W_FRAC: f32 = 0.3;
    pub const SUBMIT_MIN: f32 = 72.0;
    pub const SUBMIT_MAX: f32 = 140.0;
    /// The inner width at which the three power pills carry their full labels.
    pub const POWER_FULL_W: f32 = 300.0;
}

/// The battery gauge (`gauge.rs`). The sizes that fix the tile's footprint live
/// in `ec_layout::intrinsic`, so the layout engine and the drawing cannot
/// disagree about how big the pane is.
pub mod gauge {
    pub use ec_layout::intrinsic::{
        CELL_H_PX, GAUGE_GAP_PX, GAUGE_PX, MAX_DEVICES, MAX_PER_ROW, NUMERAL_PX, PAD_PX, ROW_GAP_PX,
    };
    /// The ring's stroke and its corner radius, as shares of the gauge's side
    /// and of the ring's own side.
    pub const STROKE_FRAC: f32 = 0.085;
    pub const CORNER_FRAC: f32 = 0.30;
    /// The device icon's box and its stroke, as shares of the side and of the box.
    pub const ICON_FRAC: f32 = 0.40;
    pub const ICON_STROKE_FRAC: f32 = 0.085;
    /// The charging bolt: its box and its top-left, as shares of the side.
    pub const BOLT_FRAC: f32 = 0.20;
    pub const BOLT_AT: f32 = 0.2;
    /// The percentage under the gauge, px at the reference size.
    pub const NUMERAL_SIZE: f32 = 20.0;
    /// At or under this and not charging, the ring is the danger colour.
    pub const LOW_PERCENT: u8 = 20;
}

/// Motion. A pane springs to its place and rises in when the screen appears.
pub mod motion {
    /// Spring stiffness (1/s²) and damping (1/s). The period is about a third
    /// of a second with about a tenth overshoot: a bubble, not a slide.
    pub const STIFFNESS: f32 = 300.0;
    pub const DAMPING: f32 = 20.0;
    /// Integration steps per frame. Semi-implicit Euler is stable at these numbers.
    pub const SUBSTEPS: u32 = 4;
    /// A spring is at rest within these (px, px/s).
    pub const REST_POS: f32 = 0.05;
    pub const REST_VEL: f32 = 0.5;
    /// The delay between tiles on appearance, ms, in layout order.
    pub const STAGGER_MS: u64 = 70;
    /// How far a tile rises as it appears, px.
    pub const RISE: f32 = 28.0;
    /// The frame step: the first frame and the cap on a stalled one.
    pub const FIRST_DT: f32 = 1.0 / 60.0;
    pub const MAX_DT: f32 = 1.0 / 30.0;
}

/// Type faces: Onest for everything (STYLE.md); digits are tabular in the
/// vendored statics.
pub mod font {
    use super::Font;
    use iced::font::{Family, Weight};

    /// The family name the embedded files carry (name ID 1).
    pub const FAMILY: &str = "Onest";

    const fn onest(weight: Weight) -> Font {
        Font {
            family: Family::Name(FAMILY),
            weight,
            ..Font::DEFAULT
        }
    }

    pub const UI: Font = onest(Weight::Normal);
    pub const UI_MEDIUM: Font = onest(Weight::Medium);
    pub const DATA: Font = onest(Weight::Normal);
    pub const DATA_MEDIUM: Font = onest(Weight::Medium);

    /// The faces the greeter uses, for `iced::Settings::fonts`. Loading them is
    /// the application's job (`run`).
    pub const BYTES: &[&[u8]] = &[
        include_bytes!("../../../../assets/fonts/Onest-Regular.ttf"),
        include_bytes!("../../../../assets/fonts/Onest-Medium.ttf"),
    ];
}

// Every clamp in the view takes its bounds in order; a reversed pair would
// panic at run time, so the order is checked at compile time.
const _: () = {
    assert!(scale::GREETING_MIN < scale::GREETING_MAX);
    assert!(cell::FIELD_MIN < cell::FIELD_MAX);
    assert!(cell::ROW_MIN < cell::ROW_MAX);
    assert!(cell::SUBMIT_MIN < cell::SUBMIT_MAX);
};

#[cfg(test)]
mod tests {
    use super::*;
    use ec_theme::{Scheme, DARK_SEEDS, LIGHT_SEEDS};

    /// WCAG contrast of `fg` laid on a neutral ground of luminance `ground`,
    /// blended in linear light as the renderer does.
    fn on_ground(fg: Rgba, ground: f32) -> f32 {
        let a = fg.a();
        let seen = a * fg.luminance() + (1.0 - a) * ground;
        (ground + 0.05) / (seen + 0.05)
    }

    /// WCAG contrast either way round: dark ink on a light ground or light on dark.
    fn contrast(fg: Rgba, ground: f32) -> f32 {
        let a = fg.a();
        let seen = a * fg.luminance() + (1.0 - a) * ground;
        let (hi, lo) = if seen > ground {
            (seen, ground)
        } else {
            (ground, seen)
        };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn dark_palette_is_the_shared_derivation_untouched() {
        assert_eq!(palette(&DARK_SEEDS, Scheme::Dark), ec_theme::derive(&DARK_SEEDS));
    }

    #[test]
    fn light_text_holds_4_5_to_1_on_the_worst_glass_ground() {
        let p = palette(&LIGHT_SEEDS, Scheme::Light);
        let g = light::WORST_GROUND_LUMINANCE;
        for (name, c) in [
            ("text", p.text),
            ("text_secondary", p.text_secondary),
            ("text_tertiary", p.text_tertiary),
            ("danger", p.danger),
        ] {
            let r = on_ground(c, g);
            assert!(r >= 4.5, "{name} is {r:.2}:1 on light glass");
        }
    }

    #[test]
    fn light_typed_marks_hold_3_to_1_on_the_worst_glass_ground() {
        let p = palette(&LIGHT_SEEDS, Scheme::Light);
        let r = on_ground(typed_mark(&p), light::WORST_GROUND_LUMINANCE);
        assert!(r >= 3.0, "the typed marks are {r:.2}:1 on light glass");
        // Dark keeps the accent itself.
        let d = palette(&DARK_SEEDS, Scheme::Dark);
        assert_eq!(typed_mark(&d), d.accent);
    }

    #[test]
    fn light_ink_steps_stay_ordered_and_a_disabled_control_stays_dimmer() {
        let p = palette(&LIGHT_SEEDS, Scheme::Light);
        assert!(p.text.a() >= p.text_secondary.a() && p.text_secondary.a() > p.text_tertiary.a());
        assert!(disabled_ink(Scheme::Light) < 1.0);
        assert_eq!(disabled_ink(Scheme::Dark), backdrop::DISABLED);
    }

    #[test]
    fn every_face_is_a_truetype_or_opentype_file() {
        assert_eq!(font::BYTES.len(), 2);
        for face in font::BYTES {
            assert!(face.starts_with(&[0x00, 0x01, 0x00, 0x00]) || face.starts_with(b"OTTO"));
        }
    }

    /// The embedded faces carry the family name the tokens request.
    #[test]
    fn every_embedded_face_is_the_requested_family() {
        let needle: Vec<u8> = font::FAMILY.encode_utf16().flat_map(u16::to_be_bytes).collect();
        for face in font::BYTES {
            assert!(face.windows(needle.len()).any(|w| w == needle.as_slice()));
        }
        for f in [font::UI, font::UI_MEDIUM, font::DATA, font::DATA_MEDIUM] {
            assert_eq!(f.family, iced::font::Family::Name(font::FAMILY));
        }
    }

    #[test]
    fn the_gauge_ring_holds_3_to_1_and_its_numerals_4_5_to_1_in_both_schemes() {
        use crate::gauge::{Colours, DeviceKind, Spec};
        let spec = |percent, charging| Spec {
            kind: DeviceKind::Mouse,
            percent,
            charging,
        };
        for (scheme, seeds) in [(Scheme::Dark, &DARK_SEEDS), (Scheme::Light, &LIGHT_SEEDS)] {
            let p = palette(seeds, scheme);
            let g = worst_ground(scheme);
            for (what, s) in [
                ("charged", spec(80, false)),
                ("charging", spec(80, true)),
                ("low", spec(10, false)),
            ] {
                let c = Colours::of(&p, &s);
                let ring = contrast(c.ring, g);
                assert!(ring >= 3.0, "{scheme:?} {what} ring is {ring:.2}:1");
                let bolt = contrast(c.bolt, g);
                assert!(bolt >= 3.0, "{scheme:?} {what} bolt is {bolt:.2}:1");
                let ink = contrast(c.ink, g);
                assert!(ink >= 4.5, "{scheme:?} {what} numerals are {ink:.2}:1");
            }
        }
    }

    #[test]
    fn the_gauge_track_is_fainter_than_the_ring() {
        use crate::gauge::{Colours, DeviceKind, Spec};
        for (scheme, seeds) in [(Scheme::Dark, &DARK_SEEDS), (Scheme::Light, &LIGHT_SEEDS)] {
            let p = palette(seeds, scheme);
            let c = Colours::of(
                &p,
                &Spec {
                    kind: DeviceKind::Laptop,
                    percent: 50,
                    charging: false,
                },
            );
            let g = worst_ground(scheme);
            assert!(contrast(c.track, g) < contrast(c.ring, g), "{scheme:?}");
        }
    }

    #[test]
    fn the_gauge_sizes_come_from_the_layout_engine() {
        assert_eq!(TILE_GAP as i32, ec_layout::intrinsic::TILE_GAP_PX);
        assert_eq!(gauge::CELL_H_PX, gauge::GAUGE_PX + gauge::NUMERAL_PX);
        assert!(gauge::NUMERAL_SIZE < gauge::NUMERAL_PX as f32);
    }
}
