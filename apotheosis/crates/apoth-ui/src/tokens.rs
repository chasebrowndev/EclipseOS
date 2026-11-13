// SPDX-License-Identifier: AGPL-3.0-only
//! The only module in `apoth-ui` that may hold a literal colour, radius, spacing or
//! duration (a lint test enforces this).
//!
//! Colours come from `ec_theme::derive` (the X2 token crate, ADR 0084); radii, spacing and
//! sizes mirror `shell/crates/ec-ui/src/tokens.rs` and STYLE.md.

/// Straight sRGB, each channel `0..=1`: `[r, g, b, a]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    /// Fully transparent: a rim-only rect uses it as its fill.
    pub const CLEAR: Rgba = Rgba([0.0; 4]);

    /// `#rrggbb` at full opacity.
    pub const fn hex(v: u32) -> Rgba {
        Rgba([
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
            1.0,
        ])
    }

    /// The same colour at alpha `a`.
    pub const fn alpha(self, a: f32) -> Rgba {
        Rgba([self.0[0], self.0[1], self.0[2], a])
    }

    /// Alpha multiplied by `k` (fades).
    pub fn scaled(self, k: f32) -> Rgba {
        Rgba([self.0[0], self.0[1], self.0[2], self.0[3] * k])
    }

    pub fn a(self) -> f32 {
        self.0[3]
    }
}

/// Brightness class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

/// The slice of `ec_theme::Palette` the terminal's interface uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub scheme: Scheme,
    pub base: Rgba,
    pub bar_tint: Rgba,
    pub sheet_tint: Rgba,
    pub cell: Rgba,
    pub cell_hover: Rgba,
    pub cell_press: Rgba,
    pub cell_rim: Rgba,
    pub cell_rim_hover: Rgba,
    pub cell_focus: Rgba,
    pub cell_focus_hover: Rgba,
    pub cell_focus_press: Rgba,
    pub cell_focus_rim: Rgba,
    pub cell_focus_rim_hover: Rgba,
    pub hairline: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub accent_fill: Rgba,
    pub accent_border: Rgba,
    pub on_accent: Rgba,
    pub text: Rgba,
    pub text_secondary: Rgba,
    pub text_tertiary: Rgba,
    pub track: Rgba,
    pub scroller: Rgba,
    pub scroller_hover: Rgba,
    pub danger: Rgba,
    pub danger_fill: Rgba,
    pub ok: Rgba,
    /// Inactive-pane dim colour (alpha is set per use from `panes.inactive-dim`).
    pub dim: Rgba,
    /// Soft shadow colour (blur-off fallback only).
    pub shadow: Rgba,
}

impl From<ec_theme::Rgba> for Rgba {
    fn from(c: ec_theme::Rgba) -> Rgba {
        Rgba(c.0)
    }
}

impl Palette {
    /// The terminal's slice of a derived desktop palette (ADR 0084, T-08 §1). Nothing is
    /// copied by hand: every colour is an `ec_theme::derive` token.
    pub fn from_theme(p: &ec_theme::Palette) -> Palette {
        Palette {
            scheme: match p.scheme {
                ec_theme::Scheme::Dark => Scheme::Dark,
                ec_theme::Scheme::Light => Scheme::Light,
            },
            base: p.base.into(),
            bar_tint: p.bar_tint.into(),
            sheet_tint: p.sheet_tint.into(),
            cell: p.cell.into(),
            cell_hover: p.cell_hover.into(),
            cell_press: p.cell_press.into(),
            cell_rim: p.cell_rim.into(),
            cell_rim_hover: p.cell_rim_hover.into(),
            cell_focus: p.cell_focus.into(),
            cell_focus_hover: p.cell_focus_hover.into(),
            cell_focus_press: p.cell_focus_press.into(),
            cell_focus_rim: p.cell_focus_rim.into(),
            cell_focus_rim_hover: p.cell_focus_rim_hover.into(),
            hairline: p.hairline.into(),
            accent: p.accent.into(),
            accent_text: p.accent_text.into(),
            accent_fill: p.accent_fill.into(),
            accent_border: p.accent_border.into(),
            on_accent: p.on_accent.into(),
            text: p.text.into(),
            text_secondary: p.text_secondary.into(),
            text_tertiary: p.text_tertiary.into(),
            track: p.track.into(),
            scroller: p.scroller.into(),
            scroller_hover: p.scroller_hover.into(),
            danger: p.danger.into(),
            danger_fill: p.danger_fill.into(),
            ok: p.ok.into(),
            dim: p.base.into(),
            shadow: p.cell_shadow.into(),
        }
    }

    /// Stock dark theme (`DARK_SEEDS`), checked by `ec_theme::validate` in the tests.
    pub fn dark() -> Palette {
        Palette::from_theme(&ec_theme::derive(&ec_theme::DARK_SEEDS))
    }

    /// Stock light theme (`LIGHT_SEEDS`).
    pub fn light() -> Palette {
        Palette::from_theme(&ec_theme::derive(&ec_theme::LIGHT_SEEDS))
    }
}

pub mod radius {
    pub const WINDOW: f32 = 16.0;
    pub const CARD: f32 = 13.0;
    pub const INSET: f32 = 10.0;
    pub const PILL: f32 = 99.0;
    pub const BADGE: f32 = 3.0;
    pub const KEYCAP: f32 = 5.0;
}

pub mod space {
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
}

pub mod size {
    pub const BODY: f32 = 13.0;
    pub const BODY_SMALL: f32 = 12.0;
    pub const MICRO: f32 = 10.0;
    pub const ICON: f32 = 14.0;
}

/// Widget metrics (logical px).
pub mod metrics {
    pub const RIM_W: f32 = 1.0;
    /// Keyboard-origin focus rim (crisp, replaces the hairline; never a halo).
    pub const FOCUS_RIM_W: f32 = 2.0;
    pub const TAB_H: f32 = 32.0;
    pub const TAB_MIN_W: f32 = 96.0;
    pub const TAB_MAX_W: f32 = 220.0;
    pub const TAB_GAP: f32 = 4.0;
    pub const TAB_CLOSE: f32 = 18.0;
    pub const NEW_TAB: f32 = 28.0;
    pub const BADGE_D: f32 = 6.0;
    pub const BAR_H: f32 = 40.0;
    pub const BAR_BUTTON_H: f32 = 26.0;
    pub const BAR_BUTTON_PAD: f32 = 12.0;
    pub const KEYCAP_W: f32 = 16.0;
    pub const SCROLLBAR_W: f32 = 6.0;
    pub const SCROLLBAR_W_ACTIVE: f32 = 8.0;
    pub const SCROLLBAR_EDGE: f32 = 3.0;
    pub const SCROLLBAR_HIT_PAD: f32 = 4.0;
    pub const THUMB_MIN: f32 = 24.0;
    pub const MARK_H: f32 = 2.0;
    pub const SPLIT_HANDLE: f32 = 6.0;
    pub const PANE_TITLE_H: f32 = 20.0;
    pub const RESIZE_MARGIN: f32 = 4.0;
}

/// Durations in milliseconds.
pub mod motion {
    pub const DURATION_MS: u64 = 220;
    pub const FADE_MS: u64 = 200;
    /// Idle time before an auto-hide scrollbar starts to fade.
    pub const SCROLLBAR_HOLD_MS: u64 = 1000;
    /// A consent bar or sheet ignores input this long after appearing (T-08 §5, T-18 §3).
    pub const CONSENT_GUARD_MS: u64 = 500;
}

/// Caps on program-supplied text shown in the interface (T-18 §6).
pub mod limits {
    pub const PROGRAM_TEXT_CHARS: usize = 256;
}
