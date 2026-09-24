// SPDX-License-Identifier: AGPL-3.0-only
//! The welcome screen's palette, transcribed from `reference/welcome.html`.
//!
//! This is the only file in the crate with colour literals. They are the
//! reference's own (gold `#f4bb3c` on `#060505`..`#1c1915`), not
//! `eclipse_ui::tokens`: the welcome is brand art shown before any pane
//! exists, its gold is the logo's gold (the O in `eclipseos-logo.png` is drawn
//! in it), and it has no dependency on the design system to stay embeddable.
//! The style rules that do carry over are kept: warm near-black, one gold, no
//! blue-grey.

use iced::Color;

const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// The stage's radial background, centre / 60% / rim.
pub const BG_CENTRE: Color = rgb(0x1c1915);
pub const BG_MID: Color = rgb(0x0b0a09);
pub const BG_RIM: Color = rgb(0x060505);

/// The sun, the corona, the keycap's border and glow.
pub const GOLD: Color = rgb(0xf4bb3c);
/// The moon, and the exit fade.
pub const BLACK: Color = rgb(0x000000);
/// The greetings.
pub const TEXT: Color = rgb(0xf3f2f2);
/// The version label.
pub const MUTED: Color = rgb(0x8d8a85);
/// The keycap's fill and its hard lower edge.
pub const WHITE: Color = rgb(0xffffff);

/// This colour at alpha `a`.
pub const fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}
