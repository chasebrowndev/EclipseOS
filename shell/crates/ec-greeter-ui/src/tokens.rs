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
pub const TILE_GAP: u32 = 8;
/// Card padding as a share of the card's short side, and its bounds.
pub const PAD_FRACTION: f32 = 0.08;
pub const PAD_MIN: f32 = 12.0;
pub const PAD_MAX: f32 = 28.0;
/// The pad never exceeds this share of the card's short side.
pub const PAD_SHARE_MAX: f32 = 0.18;
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
    /// The clock numeral.
    pub const CLOCK_H: f32 = 0.56;
    pub const CLOCK_W: f32 = 0.26;
    pub const CLOCK_MIN: f32 = 24.0;
    pub const CLOCK_MAX: f32 = 180.0;
    /// The date under the numeral, as a share of the numeral.
    pub const DATE_OF_CLOCK: f32 = 0.16;
    pub const DATE_MIN: f32 = 12.0;
    pub const DATE_MAX: f32 = 28.0;
    /// The greeting line.
    pub const GREETING_H: f32 = 0.62;
    pub const GREETING_W: f32 = 0.07;
    pub const GREETING_MIN: f32 = 20.0;
    pub const GREETING_MAX: f32 = 44.0;
    /// A Glance's figure: the battery's or the link's percentage.
    pub const FIGURE_H: f32 = 0.42;
    pub const FIGURE_MIN: f32 = 18.0;
    pub const FIGURE_MAX: f32 = 96.0;
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
    /// Charge rail.
    pub const RAIL_H: f32 = 6.0;
    /// The Unlock pill beside the field: a share of the tile, clamped.
    pub const SUBMIT_W_FRAC: f32 = 0.3;
    pub const SUBMIT_MIN: f32 = 72.0;
    pub const SUBMIT_MAX: f32 = 140.0;
    /// Link quality per lit bar, percent.
    pub const WIFI_STEP_PCT: usize = 25;
    /// The inner width at which the three power pills carry their full labels.
    pub const POWER_FULL_W: f32 = 300.0;
    /// Link bars: how many, how tall (shares of the figure), and their gaps.
    pub const WIFI_BARS: usize = 4;
    pub const BAR_W: f32 = 5.0;
    pub const BAR_GAP: f32 = 4.0;
    pub const BAR_BASE: f32 = 0.35;
    pub const BAR_STEP: f32 = 0.2;
    pub const BAR_RADIUS: f32 = 2.0;
    /// A Glance drops its detail below this height.
    pub const DETAIL_MIN_H: f32 = 120.0;
    pub const DEVICES_MAX: usize = 4;
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
    assert!(scale::CLOCK_MIN < scale::CLOCK_MAX);
    assert!(scale::GREETING_MIN < scale::GREETING_MAX);
    assert!(scale::FIGURE_MIN < scale::FIGURE_MAX);
    assert!(scale::DATE_MIN < scale::DATE_MAX);
    assert!(cell::FIELD_MIN < cell::FIELD_MAX);
    assert!(cell::ROW_MIN < cell::ROW_MAX);
    assert!(cell::SUBMIT_MIN < cell::SUBMIT_MAX);
    assert!(PAD_MIN < PAD_MAX);
};

#[cfg(test)]
mod tests {
    use super::*;

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
}
