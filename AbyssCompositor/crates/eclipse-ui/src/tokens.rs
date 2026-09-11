// SPDX-License-Identifier: AGPL-3.0-only
//! The style spec (`docs/STYLE.md`), transcribed once.
//!
//! Every colour, radius, size and weight the desktop uses lives here. The
//! point is not tidiness: it is that "warm near-black, never blue-grey" and
//! "one live yellow per pane" are design rules that only hold if there is a
//! single place they can be checked against.

use iced::{Color, Font};

/// `#rrggbb` at full opacity.
const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// White at `a`. The spec expresses nearly every surface and text colour this
/// way, because they all sit over the same warm base and over live wallpaper.
const fn white(a: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a,
    }
}

pub mod color {
    use super::{rgb, white, Color};

    /// Window and desktop base. Warm, never blue-grey.
    pub const BASE: Color = rgb(0x0b0906);
    /// Opaque surfaces behind glass, darkest to lightest.
    pub const SURFACE_0: Color = rgb(0x12100b);
    pub const SURFACE_1: Color = rgb(0x1a1712);
    pub const SURFACE_2: Color = rgb(0x1c1913);

    /// The glass fill. Thin on purpose — the depth comes from the
    /// compositor's blur underneath, not from this layer's opacity.
    pub const GLASS: Color = white(0.045);
    pub const GLASS_STRONG: Color = white(0.06);
    pub const BORDER: Color = white(0.10);
    pub const BORDER_STRONG: Color = white(0.16);
    /// Hairline between rows in an inset list.
    pub const HAIRLINE: Color = white(0.055);
    /// Top edge highlight, the one thing that reads as a light source.
    pub const HIGHLIGHT: Color = white(0.12);
    /// Sidebar ground: darker than the panes it sits beside.
    pub const SIDEBAR: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.4,
    };

    /// The single accent. See `accent discipline` in the spec: it marks state
    /// and one live value per screen, and is never decorative.
    pub const ACCENT: Color = rgb(0xf2c33c);
    /// Accent as text — lifted, because #f2c33c on near-black is heavy at
    /// 12px.
    pub const ACCENT_TEXT: Color = rgb(0xf5cf5c);
    pub const ACCENT_FILL: Color = Color { a: 0.09, ..ACCENT };
    pub const ACCENT_BORDER: Color = Color { a: 0.28, ..ACCENT };

    pub const TEXT: Color = white(1.0);
    pub const TEXT_SECONDARY: Color = white(0.64);
    pub const TEXT_TERTIARY: Color = white(0.40);
    /// Warm neutral for swatches and inert indicators.
    pub const NEUTRAL: Color = rgb(0x96918a);

    /// Off state of a toggle, and the inert bars of a chart.
    pub const CONTROL_OFF: Color = white(0.13);
    pub const TRACK: Color = white(0.12);

    /// Status. Deliberately not red/green pairs from a generic palette — a
    /// desktop that only ever goes yellow needs its alarms to look unlike it.
    pub const DANGER: Color = rgb(0xe0553f);
    pub const OK: Color = rgb(0x7fae5e);
}

pub mod radius {
    pub const WINDOW: f32 = 16.0;
    pub const CARD: f32 = 13.0;
    pub const INSET: f32 = 10.0;
    /// Anything fully round: pills, toggles, knobs.
    pub const PILL: f32 = 99.0;
    /// Bar-chart caps.
    pub const BAR: f32 = 2.5;
}

pub mod space {
    /// Content column padding, per the spec's 26/30.
    pub const PANE_X: f32 = 30.0;
    pub const PANE_Y: f32 = 26.0;
    /// Between blocks in the content column.
    pub const BLOCK: f32 = 18.0;
    pub const CARD: f32 = 16.0;
    pub const ROW_Y: f32 = 11.0;
    pub const SIDEBAR_W: f32 = 214.0;
}

pub mod size {
    pub const PANE_TITLE: f32 = 24.0;
    pub const CARD_TITLE: f32 = 13.0;
    pub const BODY: f32 = 13.0;
    pub const BODY_SMALL: f32 = 12.0;
    pub const MONO: f32 = 11.5;
    /// Small uppercase section labels — mono, wide tracking.
    pub const MICRO: f32 = 10.0;
    pub const BIG_NUMBER: f32 = 32.0;
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
        include_bytes!("../../../assets/fonts/InstrumentSans-Regular.ttf"),
        include_bytes!("../../../assets/fonts/InstrumentSans-Medium.ttf"),
        include_bytes!("../../../assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../../../assets/fonts/JetBrainsMono-Medium.ttf"),
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
        let c = color::ACCENT;
        let byte = |f: f32| (f * 255.0).round() as u8;
        assert_eq!((byte(c.r), byte(c.g), byte(c.b)), (0xf2, 0xc3, 0x3c));
    }
}
