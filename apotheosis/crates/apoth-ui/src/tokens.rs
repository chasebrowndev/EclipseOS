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
    /// A toggle's track when off, at rest and hovered, and its knob.
    pub control_off: Rgba,
    pub control_off_hover: Rgba,
    pub knob_off: Rgba,
    pub scroller: Rgba,
    pub scroller_hover: Rgba,
    pub danger: Rgba,
    pub danger_fill: Rgba,
    pub ok: Rgba,
    /// Inactive-pane dim colour (alpha is set per use from `panes.inactive-dim`).
    pub dim: Rgba,
    /// Soft shadow colour (blur-off fallback only).
    pub shadow: Rgba,
    /// Alpha of a floating pane's contact shadow.
    pub float_shadow: f32,
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// The desktop's palette alphas are linear-light (iced blends in linear light). The chrome
/// pass blends encoded sRGB, so the same token drawn as is reads far fainter. This is the
/// alpha that puts the same colour on screen over `ground` when blended in sRGB (the inverse
/// of `ec_theme::linear_alpha`: per channel the linear-light mix is the target, the sRGB
/// alpha that reaches it is solved, averaged by how far the colour is from the ground).
pub fn srgb_blend(c: Rgba, ground: Rgba) -> Rgba {
    let a = c.0[3];
    let (mut num, mut den) = (0.0, 0.0);
    for ch in 0..3 {
        let (s, g) = (c.0[ch], ground.0[ch]);
        let mix = linear_to_srgb(srgb_to_linear(g) * (1.0 - a) + srgb_to_linear(s) * a);
        num += (mix - g) * (s - g);
        den += (s - g) * (s - g);
    }
    if den < 1e-6 {
        c
    } else {
        c.alpha((num / den).clamp(0.0, 1.0))
    }
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
        let base: Rgba = p.base.into();
        // Cell fills and rims are the translucent family the chrome draws; each is brought
        // to the alpha this pass needs (see [`srgb_blend`]).
        let cell = |c: ec_theme::Rgba| srgb_blend(c.into(), base);
        Palette {
            scheme: match p.scheme {
                ec_theme::Scheme::Dark => Scheme::Dark,
                ec_theme::Scheme::Light => Scheme::Light,
            },
            base: p.base.into(),
            bar_tint: p.bar_tint.into(),
            sheet_tint: p.sheet_tint.into(),
            cell: cell(p.cell),
            cell_hover: cell(p.cell_hover),
            cell_press: cell(p.cell_press),
            cell_rim: cell(p.cell_rim),
            cell_rim_hover: cell(p.cell_rim_hover),
            cell_focus: cell(p.cell_focus),
            cell_focus_hover: cell(p.cell_focus_hover),
            cell_focus_press: cell(p.cell_focus_press),
            cell_focus_rim: cell(p.cell_focus_rim),
            cell_focus_rim_hover: cell(p.cell_focus_rim_hover),
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
            control_off: p.control_off.into(),
            control_off_hover: p.control_off_hover.into(),
            knob_off: p.knob_off.into(),
            scroller: p.scroller.into(),
            scroller_hover: p.scroller_hover.into(),
            danger: p.danger.into(),
            danger_fill: p.danger_fill.into(),
            ok: p.ok.into(),
            dim: p.base.into(),
            shadow: p.cell_shadow.into(),
            float_shadow: match p.scheme {
                ec_theme::Scheme::Dark => alpha::FLOAT_SHADOW_DARK,
                ec_theme::Scheme::Light => alpha::FLOAT_SHADOW_LIGHT,
            },
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
    pub const PALETTE_W: f32 = 560.0;
    pub const PALETTE_FIELD_H: f32 = 36.0;
    pub const PALETTE_ROW_H: f32 = 30.0;
    pub const PALETTE_MAX_ROWS: usize = 10;
    /// Open menu anchored at the pointer: its width, and the gap kept to the window edge.
    pub const MENU_W: f32 = 400.0;
    /// Open menu header: its height, the type tile's width and height.
    pub const MENU_HEADER_H: f32 = 52.0;
    pub const TILE_W: f32 = 40.0;
    pub const TILE_H: f32 = 32.0;
    /// A switch: track and knob.
    pub const TOGGLE_W: f32 = 30.0;
    pub const TOGGLE_H: f32 = 18.0;
    pub const TOGGLE_KNOB_INSET: f32 = 2.0;
    /// A media window's hover close button.
    pub const MEDIA_CLOSE: f32 = 28.0;
    /// The "default" tag on a picker row.
    pub const TAG_H: f32 = 16.0;
    pub const CARET_W: f32 = 1.5;
    /// Search sheet: widest it grows, and the height of its row.
    pub const SEARCH_W: f32 = 540.0;
    pub const SEARCH_H: f32 = 44.0;
    pub const SEARCH_FIELD_H: f32 = 28.0;
    pub const FIELD_MIN_W: f32 = 96.0;
    pub const CHIP_H: f32 = 24.0;
    pub const CHIP_MIN_W: f32 = 30.0;
    pub const ICON_BUTTON: f32 = 24.0;
    /// Confirm-once sheet: widest it grows, lines its monospace box shows, line pitch.
    pub const SHEET_W: f32 = 560.0;
    pub const SHEET_BODY_LINES: usize = 6;
    pub const SHEET_LINE_H: f32 = 17.0;
    /// Program-text well under a bar headline.
    pub const DETAIL_H: f32 = 15.0;
    pub const DETAIL_GAP: f32 = 2.0;
    pub const TOOLTIP_MAX_W: f32 = 480.0;
    pub const TOOLTIP_H: f32 = 24.0;
    pub const TOOLTIP_OFFSET: f32 = 14.0;
    /// Widest the profile pill on a tab grows, and its height.
    pub const PROFILE_MAX_W: f32 = 84.0;
    pub const PROFILE_H: f32 = 16.0;
    /// Tab progress line, inset from the tab's bottom edge.
    pub const PROGRESS_H: f32 = 2.0;
    pub const PROGRESS_INSET: f32 = 3.0;
    /// How far a floating pane's contact shadow sits below it.
    pub const FLOAT_DROP: f32 = 3.0;
    /// Bell rim around a pane while the visual bell shows.
    pub const BELL_RIM_W: f32 = 2.0;
}

/// Alpha steps that are not palette tokens.
pub mod alpha {
    /// Frost laid under a floating sheet so the grid beneath cannot ghost through its text.
    pub const BACKING: f32 = 0.72;
    /// Extra backing under a float so the pane beneath never reads through its text.
    pub const FLOAT_BACKING: f32 = 0.6;
    /// A floating pane's contact shadow at its edge, on dark and on light (black either way,
    /// so light stays softer).
    pub const FLOAT_SHADOW_DARK: f32 = 0.50;
    pub const FLOAT_SHADOW_LIGHT: f32 = 0.20;
    /// Dim over the window behind a modal sheet.
    pub const SCRIM: f32 = 0.28;
    /// A progress fill with no known value.
    pub const BUSY: f32 = 0.5;
    /// The visual bell's rim, against the pane's own.
    pub const BELL: f32 = 0.85;
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
