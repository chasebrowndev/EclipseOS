// SPDX-License-Identifier: AGPL-3.0-only

//! Every colour and size fog-ui draws with, in one place so M2 can replace
//! this module with the EclipseOS theme (FOG §Visual design) without touching
//! the views. Values follow the house style: warm near-black, white text at
//! three alphas, one gold accent that marks only the selected row, hard edges.

use iced::Color;

/// `#rrggbb` at full opacity.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

/// White at `a`.
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

    /// Window base. Warm, never blue-grey.
    pub const BASE: Color = rgb(0x0b, 0x09, 0x06);
    /// Path bar and status line ground: one step above the list.
    pub const CHROME: Color = rgb(0x12, 0x10, 0x0b);
    /// Rule between the chrome rows and the list.
    pub const RULE: Color = white(0.10);

    pub const TEXT: Color = white(1.0);
    pub const TEXT_SECONDARY: Color = white(0.64);
    pub const TEXT_TERTIARY: Color = white(0.40);

    /// The single accent. In Fog it marks the selected row and nothing else.
    pub const ACCENT: Color = rgb(0xf2, 0xc3, 0x3c);
    /// Accent as text, lifted for small sizes.
    pub const ACCENT_TEXT: Color = rgb(0xf5, 0xcf, 0x5c);
    /// Ground under the selected row.
    pub const ACCENT_FILL: Color = Color { a: 0.05, ..ACCENT };

    /// Errors and "fogd not running". Deliberately unlike the accent.
    pub const DANGER: Color = rgb(0xe0, 0x55, 0x3f);
    /// Warm neutral: the palette's "primary", which iced paints on the
    /// hovered or dragged scrollbar. Not the accent — the selection keeps
    /// the only gold.
    pub const NEUTRAL: Color = rgb(0x96, 0x91, 0x8a);
    pub const OK: Color = rgb(0x7f, 0xae, 0x5e);
}

/// The iced theme, for the widgets fog-ui does not style itself (the list's
/// scrollbar): generated from the colours above.
pub fn iced_theme() -> iced::Theme {
    iced::Theme::custom(
        "Fog",
        iced::theme::Palette {
            background: color::BASE,
            text: color::TEXT,
            primary: color::NEUTRAL,
            success: color::OK,
            warning: color::DANGER,
            danger: color::DANGER,
        },
    )
}

pub mod size {
    /// Row height of the file list, in logical pixels.
    pub const ROW_H: f32 = 24.0;
    /// Body and data text.
    pub const TEXT: f32 = 13.0;
    /// The status line and the kind tags: smaller, quieter.
    pub const TEXT_SMALL: f32 = 11.5;
    /// Horizontal padding of every row and chrome line.
    pub const PAD_X: f32 = 14.0;
    /// Vertical padding of the path bar and status line.
    pub const CHROME_Y: f32 = 9.0;
    /// The accent bar at the left edge of the selected row.
    pub const BAR_W: f32 = 3.0;
    /// One device-independent pixel: rules.
    pub const HAIRLINE: f32 = 1.0;
    /// Width of the right-aligned kind tag column.
    pub const TAG_W: f32 = 48.0;
    /// Advance of one monospace glyph, in ems: turns a width into a
    /// character budget for eliding the path bar. Slightly generous, so an
    /// estimate never clips the current folder.
    pub const MONO_ADVANCE: f32 = 0.62;
    /// Gap between status line segments.
    pub const GAP: f32 = 18.0;
    /// Initial window size.
    pub const WINDOW_W: f32 = 960.0;
    pub const WINDOW_H: f32 = 640.0;
}
