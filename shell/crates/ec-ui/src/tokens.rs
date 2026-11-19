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
        accent_border,
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

    /// A selected pill's gold rim under the pointer: the same tint a step
    /// crisper. Scales `accent_border`, so a user accent keeps it. Glassy, not
    /// glowy: the rim stays a 1px edge and nothing lights past it.
    pub const SELECTED_HOVER_RIM: f32 = 1.6;
    /// A slider's gold: the rim at full value is `accent_border` this many
    /// steps crisper (its rim runs from the neutral inset rim at 0 to here at
    /// 1), and the filled run starts this faint at 0 and is the accent itself
    /// at 1.
    pub const SLIDER_RIM_MAX: f32 = 2.4;
    pub const SLIDER_RUN_MIN: f32 = 0.18;
    /// A cell cut from the inset material ([`crate::theme::inset_cell`]):
    /// hover and press are small steps up its fill and rim, not a jump to a
    /// pill's slab; a focused inset field is the rim alone, one step crisper.
    pub const INSET_HOVER_FILL: f32 = 1.35;
    pub const INSET_PRESS_FILL: f32 = 1.7;
    pub const INSET_HOVER_RIM: f32 = 1.3;
    pub const INSET_FOCUS_RIM: f32 = 1.8;

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

pub mod layout_edit {
    //! The login layout editor (Settings > Login screen): a phone-lock-style
    //! canvas of glass tiles. Sizes are px at the editor's own scale; the
    //! alphas are of the previewed theme's ink over its picture.

    /// Air around the preview inside the widget, so a tile's remove badge can
    /// overhang the preview's edge without being clipped.
    pub const PAD: f32 = 12.0;
    /// The widest the editor grows.
    pub const MAX_W: f32 = 760.0;
    /// Aspect of the preview (16:9).
    pub const ASPECT: f32 = 9.0 / 16.0;
    /// Gap a tile keeps from its cell edges, so neighbours never touch.
    pub const TILE_GAP: f32 = 3.0;
    pub const TILE_RADIUS: f32 = 9.0;
    /// Tile glass over the picture, rest and hover/held, and its rim.
    pub const TILE_FILL: f32 = 0.11;
    pub const TILE_FILL_HOT: f32 = 0.19;
    pub const TILE_RIM: f32 = 0.22;
    pub const TILE_RIM_HOT: f32 = 0.62;
    /// The base colour under a tile's tint (the stand-in for the blur), and the
    /// remove badge's ground and rims.
    pub const GROUND: f32 = 0.34;
    pub const BADGE_FILL: f32 = 0.86;
    pub const BADGE_RIM: f32 = 0.55;
    pub const BADGE_RIM_HOT: f32 = 0.9;
    /// The avatar disc, and the dim last signal bar, as ink alphas.
    pub const DISC: f32 = 0.22;
    pub const TRACK: f32 = 0.18;
    pub const DIM_BAR: f32 = 0.28;
    /// A scrim under tiles over a picture, so light wallpapers still read.
    pub const SCRIM: f32 = 0.30;
    /// Grid dots shown while a tile is held, and the ghost of its landing.
    pub const DOT: f32 = 2.0;
    pub const DOT_ALPHA: f32 = 0.20;
    pub const GHOST_FILL: f32 = 0.07;
    pub const GHOST_RIM: f32 = 0.55;
    pub const GHOST_RIM_W: f32 = 1.5;
    /// Resize grips: the corner bracket, and the edge bars.
    pub const GRIP_REST: f32 = 0.38;
    pub const GRIP_LEN: f32 = 12.0;
    pub const GRIP_W: f32 = 2.5;
    pub const GRIP_EDGE_LEN: f32 = 16.0;
    /// Hit zones: the corner square and the edge strips.
    pub const HIT_CORNER: f32 = 20.0;
    pub const HIT_EDGE: f32 = 10.0;
    /// The remove badge: its diameter, and how far it sits outside the corner.
    pub const BADGE: f32 = 18.0;
    pub const BADGE_OUT: f32 = 4.0;
    pub const BADGE_ALPHA: f32 = 0.78;
    /// A held tile lifts: this much bigger, with this much shadow.
    pub const LIFT: f32 = 1.03;
    pub const SHADOW: f32 = 0.32;
    pub const SHADOW_SPREAD: f32 = 7.0;
    pub const SHADOW_DROP: f32 = 5.0;
    /// The tile that arrives pops from this scale.
    pub const POP_FROM: f32 = 0.78;
    /// The tray of tiles not on the screen.
    pub const TRAY_GAP: f32 = 14.0;
    pub const CHIP_H: f32 = 30.0;
    pub const CHIP_PAD: f32 = 13.0;
    pub const CHIP_GAP: f32 = 8.0;
    pub const CHIP_SIZE: f32 = 12.0;
    /// Mean glyph width of the chip label, as a share of its size.
    pub const CHIP_GLYPH: f32 = 0.56;
    pub const LABEL_SIZE: f32 = 10.0;
    /// Motion, in ms to settle: a tile following the pointer, a tile landing
    /// (bouncy), the hover light, the ghost's hop between cells, the dots.
    pub const FOLLOW_MS: u64 = 90;
    pub const LAND_MS: u64 = 460;
    pub const HOVER_MS: u64 = 140;
    pub const GHOST_MS: u64 = 120;
    pub const DOTS_MS: u64 = 200;
    /// Mock text: fractions of a tile's height, and floors in px.
    pub const MIN_TEXT: f32 = 6.5;
    pub const MOCK_NAMES: [&str; 3] = ["robin", "sam", "ada"];
    pub const MOCK_TIME: &str = "Thursday, November 19";
    pub const MOCK_GREETING: &str = "Sign in to continue";
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
