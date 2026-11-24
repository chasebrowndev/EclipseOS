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

/// UI type: Onest, one family for everything the shell draws. Three weights,
/// because the spec distinguishes 400/500/600 and a variable font handed to
/// iced whole would flatten all three to 400. Digits are tabular in the
/// vendored statics (`assets/fonts/gen_onest.py` makes `tnum` the default
/// figures), since iced cannot switch OpenType features on.
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
    pub const UI_SEMIBOLD: Font = onest(Weight::Semibold);
    /// All data: units, paths, timestamps, device identifiers. Same family as
    /// the UI; the tabular digits are what keep columns aligned.
    pub const DATA: Font = onest(Weight::Normal);
    pub const DATA_MEDIUM: Font = onest(Weight::Medium);

    /// The bytes, for `iced::Settings::fonts`. Loading is the application's
    /// job — a library that registered fonts as a side effect would do it
    /// once per process and be impossible to opt out of.
    pub const BYTES: &[&[u8]] = &[
        include_bytes!("../../../../assets/fonts/Onest-Regular.ttf"),
        include_bytes!("../../../../assets/fonts/Onest-Medium.ttf"),
        include_bytes!("../../../../assets/fonts/Onest-SemiBold.ttf"),
    ];

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Name ID 1 of an sfnt, from its `name` table (platform 3, UTF-16BE).
        fn family_of(ttf: &[u8]) -> String {
            let be16 = |o: usize| u16::from_be_bytes([ttf[o], ttf[o + 1]]) as usize;
            let be32 = |o: usize| u32::from_be_bytes([ttf[o], ttf[o + 1], ttf[o + 2], ttf[o + 3]]) as usize;
            let tables = be16(4);
            let name = (0..tables)
                .map(|i| 12 + i * 16)
                .find(|&r| &ttf[r..r + 4] == b"name")
                .map(|r| be32(r + 8))
                .expect("name table");
            let (count, strings) = (be16(name + 2), name + be16(name + 4));
            for i in 0..count {
                let r = name + 6 + i * 12;
                if be16(r) == 3 && be16(r + 6) == 1 {
                    let (len, off) = (be16(r + 8), be16(r + 10));
                    let units: Vec<u16> = ttf[strings + off..strings + off + len]
                        .chunks(2)
                        .map(|c| u16::from_be_bytes([c[0], c[1]]))
                        .collect();
                    return String::from_utf16(&units).unwrap();
                }
            }
            panic!("no family name");
        }

        #[test]
        fn every_embedded_face_is_the_family_the_tokens_ask_for() {
            for ttf in BYTES {
                assert_eq!(family_of(ttf), FAMILY);
            }
            for f in [UI, UI_MEDIUM, UI_SEMIBOLD, DATA, DATA_MEDIUM] {
                assert_eq!(f.family, Family::Name(FAMILY));
            }
        }

        #[test]
        fn the_weights_are_distinct_faces() {
            assert_ne!(UI.weight, UI_MEDIUM.weight);
            assert_ne!(UI_MEDIUM.weight, UI_SEMIBOLD.weight);
            assert_eq!(BYTES.len(), 3);
        }
    }
}

pub mod layout_edit {
    //! The login layout editor (Settings > Login screen): a phone-lock-style
    //! canvas of glass tiles. Sizes are px at the editor's own scale; the
    //! alphas are of the previewed theme's ink over its picture.

    /// Air around the preview inside the widget, so a tile's remove badge can
    /// overhang the preview's edge without being clipped.
    pub const PAD: f32 = 12.0;
    /// The widest the editor grows.
    pub const MAX_W: f32 = 1040.0;
    /// Aspect of the preview (16:9).
    pub const ASPECT: f32 = 9.0 / 16.0;
    /// Gap a tile keeps from its cell edges, so neighbours never touch.
    pub const TILE_GAP: f32 = 3.0;
    pub const TILE_RADIUS: f32 = 9.0;
    /// Tile glass over the picture when hovered or held, and its rim. At rest a
    /// tile is the greeter's card (`ec_glance::cells::CARD`), so it is not here.
    pub const TILE_FILL_HOT: f32 = 0.19;
    pub const TILE_RIM_HOT: f32 = 0.62;
    /// The remove badge's ground and rims.
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
    /// The caption column of the sample-data pills under the preview.
    pub const SAMPLE_LABEL_W: f32 = 120.0;
    /// The screen the preview stands in for: the greeter's reference window,
    /// and the gap its tiles leave (`ec_layout::Rect::to_px`). The shared
    /// sizing rules (`ec_glance::tile`) are asked in these px, then drawn
    /// scaled by the preview's width over `REAL_W`.
    pub const REAL_W: f32 = 1920.0;
    pub const REAL_GAP: f32 = 8.0;
    /// Motion, in ms to settle: a tile following the pointer, a tile landing
    /// (bouncy), the hover light, the ghost's hop between cells, the dots.
    pub const FOLLOW_MS: u64 = 90;
    pub const LAND_MS: u64 = 460;
    pub const HOVER_MS: u64 = 140;
    pub const GHOST_MS: u64 = 120;
    pub const DOTS_MS: u64 = 200;
    /// The smallest text a mock draws, in px. Anything that would come out
    /// smaller is hidden instead: unreadable text is noise.
    pub const MIN_TEXT: f32 = 8.0;
    /// A tile's main text (the time, the greeting) below this size gives way
    /// to a glyph, and a glyph under `GLYPH_MIN` px is not drawn.
    pub const MIN_PRIMARY: f32 = 11.0;
    pub const GLYPH_MIN: f32 = 7.0;
    /// Below these tile sizes (px, inside the padding) a mock stops being a
    /// miniature of the greeter: first it drops its secondary text, then it is
    /// only a glyph and one short label.
    pub const LEAN_H: f32 = 64.0;
    pub const GLYPH_H: f32 = 14.0;
    pub const GLYPH_W: f32 = 60.0;
    /// A glyph's size as a share of the tile's short side, and its ceiling.
    pub const GLYPH_SHARE: f32 = 0.55;
    pub const GLYPH_MAX: f32 = 26.0;
    /// Grip marks other than the south-east bracket show only while the tile
    /// is lit, at this alpha (ink).
    pub const GRIP_HOT: f32 = 0.85;
    /// The apply bar under the tray: its height, gap above, inner padding,
    /// radius, button height and side padding.
    pub const BAR_H: f32 = 48.0;
    pub const BAR_GAP: f32 = 12.0;
    /// The narrowest the bar may be and still sit beside the tray's last row.
    pub const BAR_MIN_W: f32 = 440.0;
    pub const BAR_PAD: f32 = 9.0;
    pub const BAR_RADIUS: f32 = 24.0;
    pub const BTN_H: f32 = 30.0;
    pub const BTN_PAD: f32 = 18.0;
    pub const BAR_TEXT: f32 = 13.0;
    /// How far (px) the bar rises from while it arrives, and its spring.
    pub const BAR_SLIDE: f32 = 18.0;
    pub const BAR_MS: u64 = 480;
    /// What the bar's slot says while nothing differs.
    pub const BAR_IDLE: &str = "Matches this computer";
    pub const MOCK_NAMES: [&str; 3] = ["robin", "sam", "ada"];
    pub const MOCK_TIME: &str = "Thursday, November 19";
    pub const MOCK_GREETING: &str = "Sign in to continue";
}

pub mod art {
    //! Settings' drawn art: the icon tile every category carries, the header
    //! vignette, and the previews. Hues are data (restrained, mid-chroma),
    //! one per sidebar group; every alpha below is of that hue over the
    //! window's glass, and the strokes take the palette's text so the same
    //! drawing reads in light and dark. The accent is never used here: a tile
    //! is glass with a hue in it, never a gold ground.

    use super::color::data;
    use iced::Color;

    pub const HUE_HOME: Color = data(0x96918a);
    pub const HUE_LOOK: Color = data(0x8e86d9);
    pub const HUE_DEVICES: Color = data(0x5b9bd0);
    pub const HUE_SYSTEM: Color = data(0x8794a6);
    pub const HUE_PRIVACY: Color = data(0x62ad8a);
    pub const HUE_APPS: Color = data(0xd0887a);

    /// A tile in the sidebar and in a result row, and its corner as a share
    /// of the side (a superellipse-ish squircle, not a circle).
    pub const TILE: f32 = 22.0;
    pub const TILE_SMALL: f32 = 18.0;
    pub const TILE_R: f32 = 0.27;
    /// A tile's ground and rim, at rest, and when it is the current page.
    pub const TILE_FILL: f32 = 0.22;
    pub const TILE_FILL_LIT: f32 = 0.34;
    pub const TILE_RIM: f32 = 0.38;
    pub const TILE_RIM_LIT: f32 = 0.70;
    /// How far a drawn line moves from the hue toward the palette's text:
    /// pastel on dark, deep on light.
    pub const LINE_MIX: f32 = 0.58;
    /// A line's width as a share of the glyph's side, and its floor in px.
    pub const STROKE: f32 = 0.075;
    pub const STROKE_MIN: f32 = 1.0;
    /// The same, drawn large in a vignette, where the glass fills do the work.
    pub const STROKE_DETAIL: f32 = 0.052;
    /// A glyph's ink margin inside its square, as a share of the side.
    pub const PAD: f32 = 0.1;
    /// The header vignette: its size, its corner and its wash.
    pub const VIGNETTE_W: f32 = 92.0;
    pub const VIGNETTE_H: f32 = 64.0;
    pub const VIGNETTE_R: f32 = 16.0;
    pub const VIGNETTE_WASH: f32 = 0.16;
    pub const VIGNETTE_RING: f32 = 0.20;
    pub const VIGNETTE_GLASS: f32 = 0.16;
    /// The glyph's share of the vignette's height.
    pub const VIGNETTE_GLYPH: f32 = 0.74;
    /// The two rings behind it, as shares of the card's height; the outer one
    /// stays short of the corners so no arc pokes out of the rounded card.
    pub const RING_INNER: f32 = 0.44;
    pub const RING_OUTER: f32 = 0.66;
    /// Below this window width the sidebar is a list that replaces the page
    /// (a back stack), and the vignette goes.
    pub const NARROW_W: f32 = 640.0;
    /// The overview's quick tiles: the width of one (they wrap).
    pub const QUICK_W: f32 = 156.0;
    /// The least width of a device-card cell before the grid drops a column.
    pub const CELL_MIN_W: f32 = 200.0;
    /// The device card's tile, and its host name's size.
    pub const HERO_TILE: f32 = 44.0;
    pub const HOST_SIZE: f32 = 20.0;
    /// An empty state's tile.
    pub const EMPTY_TILE: f32 = 36.0;
    /// A preview card's height, and its picture's corner.
    pub const PREVIEW_H: f32 = 132.0;
    pub const PREVIEW_R: f32 = 12.0;
    /// The mock window inside a preview: its share of the card.
    pub const MOCK_W: f32 = 0.34;
    pub const MOCK_H: f32 = 0.56;
    /// The backdrop under a preview: its gradient's alpha, and the shapes'.
    pub const BACKDROP_A: f32 = 0.30;
    pub const SHAPE_A: f32 = 0.62;
    pub const SHAPE_LINE: f32 = 0.38;
    pub const SHAPE_LINE_W: f32 = 3.0;
    /// The glass pane in the glass preview: its share of the card, the fill it
    /// has at no tint and the span the tint adds, and what the sliders reach.
    pub const GLASS_W: f32 = 0.38;
    pub const GLASS_H: f32 = 0.74;
    pub const GLASS_FILL_MIN: f32 = 0.05;
    pub const GLASS_FILL_SPAN: f32 = 0.22;
    pub const REFRACT_MAX: f32 = 0.35;
    pub const FRINGE_MAX: f32 = 3.0;
    pub const FRINGE_A: f32 = 0.55;
    pub const BEVEL_MAX: f32 = 5.0;
    pub const BEVEL_A: f32 = 0.5;
    pub const RIM_MAX: f32 = 2.0;
    /// The window-set preview: how a gap, a border and a corner (px) are
    /// drawn, so a change of a few px shows at a card's size.
    pub const GAP_SCALE: f32 = 0.9;
    pub const BORDER_SCALE: f32 = 0.7;
    pub const BORDER_MAX: f32 = 4.0;
    pub const ROUND_SCALE: f32 = 0.6;
    pub const ROUND_MAX: f32 = 22.0;
    pub const STAGE_PAD: f32 = 10.0;
    pub const TITLE_A: f32 = 0.55;
    pub const BODY_A: f32 = 0.25;
    /// A scheme card's miniature: its height, its corner and sidebar share.
    pub const SWATCH_H: f32 = 76.0;
    pub const MOCK_R: f32 = 8.0;
    pub const MOCK_SIDE: f32 = 0.28;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_face_is_present_and_is_a_truetype_file() {
        assert_eq!(font::BYTES.len(), 3);
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
