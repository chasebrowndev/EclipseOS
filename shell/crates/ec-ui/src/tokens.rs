// SPDX-License-Identifier: AGPL-3.0-only
//! The style spec (`docs/STYLE.md`), transcribed once.
//!
//! Every colour, radius, size and weight the desktop uses lives here. The
//! point is not tidiness: it is that "warm near-black, never blue-grey" and
//! "one live yellow per pane" are design rules that only hold if there is a
//! single place they can be checked against.

pub use ec_style_tokens::*;

use iced::{Color, Font};

pub mod color {
    //! The colour tokens, as a runtime palette (ADR 0084).
    //!
    //! Each token is a function reading the process-wide current palette,
    //! which defaults to `derive(DARK_SEEDS)` -- the historical dark values,
    //! bit for bit. Call [`set_palette`] / [`set_seeds`] to switch theme; the
    //! next view build reads the new values. Semantics of each token are
    //! documented on the matching field of [`ec_theme::Palette`] and in
    //! `docs/STYLE.md`; the alphas are linear-light (iced blends in linear).

    use super::Color;
    use ec_theme::{Palette, Rgba, Seeds, DARK_SEEDS};
    use std::sync::{OnceLock, RwLock};

    pub use ec_theme::VEIL_STRENGTH;

    fn slot() -> &'static RwLock<Palette> {
        static CURRENT: OnceLock<RwLock<Palette>> = OnceLock::new();
        CURRENT.get_or_init(|| RwLock::new(ec_theme::derive(&DARK_SEEDS)))
    }

    /// The current palette (a copy).
    pub fn palette() -> Palette {
        *slot().read().unwrap_or_else(|e| e.into_inner())
    }

    /// Replace the current palette.
    pub fn set_palette(p: Palette) {
        *slot().write().unwrap_or_else(|e| e.into_inner()) = p;
    }

    /// Derive a palette from `seeds` and make it current. Does not validate;
    /// call [`ec_theme::validate`] first on user input.
    pub fn set_seeds(seeds: &Seeds) {
        set_palette(ec_theme::derive(seeds));
    }

    /// Straight sRGB `Rgba` to the iced colour, channel for channel.
    pub fn to_iced(c: Rgba) -> Color {
        Color {
            r: c.0[0],
            g: c.0[1],
            b: c.0[2],
            a: c.0[3],
        }
    }

    macro_rules! tokens {
        ($($name:ident),* $(,)?) => {
            $(
                #[doc = concat!("The `", stringify!($name), "` token of the current palette.")]
                pub fn $name() -> Color {
                    to_iced(palette().$name)
                }
            )*
        };
    }

    tokens! {
        base,
        surface_0,
        surface_1,
        surface_2,
        glass,
        glass_strong,
        glass_deep,
        glass_deep_backed,
        sheet_tint,
        bar_tint,
        cell,
        cell_hover,
        cell_press,
        cell_rim,
        cell_rim_hover,
        cell_rim_away,
        cell_shadow,
        cell_focus,
        cell_focus_hover,
        cell_focus_press,
        cell_focus_rim,
        cell_focus_rim_hover,
        menu_ground,
        border,
        border_strong,
        hairline,
        highlight,
        highlight_soft,
        highlight_strong,
        lift,
        lift_strong,
        lift_soft,
        veil,
        sidebar,
        sidebar_backed,
        content,
        content_backed,
        accent,
        accent_text,
        accent_fill,
        accent_border,
        accent_wash,
        text,
        text_secondary,
        text_tertiary,
        neutral,
        eye_iris,
        control_off,
        track,
        danger,
        danger_fill,
        connected,
        ok,
        on_accent,
        nav_active,
        nav_hover,
        pill,
        pill_hover,
        input_fill,
        scroller,
        scroller_hover,
        chart_history,
        control_off_hover,
        knob_off,
        highlight_rim,
    }

    /// [`veil`] on a translucent column ([`content`] over the compositor's
    /// blur): only the column's own tint, at the veil's strength, fades a row
    /// without drawing a slab.
    pub fn veil_glass() -> [Color; 1] {
        [to_iced(palette().veil_glass)]
    }

    /// How far a minimized window's chip -- label and icon -- fades. The
    /// spec's tertiary text value.
    pub const DIM: f32 = 0.40;

    /// A colour that arrives as data -- a calendar's own `0xRRGGBB` -- rather
    /// than a choice of ours. A provider's palette is never mistaken for a token.
    pub const fn data(hex: u32) -> Color {
        let c = Rgba::rgb(hex);
        Color {
            r: c.0[0],
            g: c.0[1],
            b: c.0[2],
            a: c.0[3],
        }
    }
}

/// UI type. Three weights, because the spec distinguishes 400/500/600 and a
/// variable font handed to iced whole would flatten all three to 400.
pub mod font {
    use super::Font;
    use iced::font::{Family, Weight};

    const fn sans(weight: Weight) -> Font {
        Font {
            family: Family::Name("Instrument Sans"),
            weight,
            ..Font::DEFAULT
        }
    }

    const fn mono(weight: Weight) -> Font {
        Font {
            family: Family::Name("JetBrains Mono"),
            weight,
            ..Font::DEFAULT
        }
    }

    pub const UI: Font = sans(Weight::Normal);
    pub const UI_MEDIUM: Font = sans(Weight::Medium);
    pub const UI_SEMIBOLD: Font = sans(Weight::Semibold);
    /// All data: units, paths, timestamps, device identifiers.
    pub const DATA: Font = mono(Weight::Normal);
    pub const DATA_MEDIUM: Font = mono(Weight::Medium);

    /// The bytes, for `iced::Settings::fonts`. Loading is the application's
    /// job — a library that registered fonts as a side effect would do it
    /// once per process and be impossible to opt out of.
    pub const BYTES: &[&[u8]] = &[
        include_bytes!("../../../../assets/fonts/InstrumentSans-Regular.ttf"),
        include_bytes!("../../../../assets/fonts/InstrumentSans-Medium.ttf"),
        include_bytes!("../../../../assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../../../../assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../../../../assets/fonts/JetBrainsMono-Medium.ttf"),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_face_is_present_and_is_a_truetype_file() {
        assert_eq!(font::BYTES.len(), 5);
        for face in font::BYTES {
            // Either a bare TrueType outline file or an OpenType wrapper; a
            // missing `include_bytes!` target would not have compiled, but a
            // wrong file very much would have.
            assert!(face.starts_with(&[0x00, 0x01, 0x00, 0x00]) || face.starts_with(b"OTTO"));
        }
    }

    #[test]
    fn the_accent_is_the_one_from_the_style_spec() {
        let c = color::accent();
        let byte = |f: f32| (f * 255.0).round() as u8;
        assert_eq!((byte(c.r), byte(c.g), byte(c.b)), (0xf2, 0xc3, 0x3c));
    }

    /// The compositor draws the capture eye from `ec_theme::bar_mark`, a copy
    /// of these tokens; this is the drift guard.
    #[test]
    fn bar_mark_matches_tokens() {
        use ec_theme::bar_mark as m;
        assert_eq!(m::PILL_H, bar::PILL_H);
        assert_eq!(m::EDGE, bar::EDGE);
        assert_eq!(m::TASK_MIN, bar::TASK_MIN);
        assert_eq!(m::TASK_H, bar::TASK_H);
        assert_eq!(m::RING, bar::RING);
        assert_eq!(m::EYE_DISC, bar::EYE_DISC);
        assert_eq!(m::EYE_PUPIL, bar::EYE_PUPIL);
        assert_eq!(m::EYE_WANDER, bar::EYE_WANDER);
        assert_eq!(m::EYE_GLANCE, bar::EYE_GLANCE);
        assert_eq!(m::EYE_RECENTRE, bar::EYE_RECENTRE);
        assert_eq!(m::EYE_RIM, bar::EYE_RIM);
        assert_eq!(m::EYE_BLEED, bar::EYE_BLEED);
        assert_eq!(m::EYE_DART_MS, bar::EYE_DART_MS);
        assert_eq!(m::EYE_RESIZE_MS, bar::EYE_RESIZE_MS);
        assert_eq!(m::EYE_HOLD_MIN_MS, bar::EYE_HOLD_MIN_MS);
        assert_eq!(m::EYE_HOLD_MAX_MS, bar::EYE_HOLD_MAX_MS);
        assert_eq!(m::EYE_FRAME_MS, bar::EYE_FRAME_MS);
    }
}
