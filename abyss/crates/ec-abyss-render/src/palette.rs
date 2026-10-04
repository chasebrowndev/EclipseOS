// SPDX-License-Identifier: AGPL-3.0-only
//! Every colour the annotation HUD and the region selector draw (ADR 0071).
//!
//! One place, so the accent ledger can be audited at a glance: the only
//! yellow anywhere in the HUD is [`HudPalette::accent`] (the pick tab, the
//! pick wash and rim, the selector band) and [`HudPalette::accent_text`]
//! (the pick letter in the panel). Nothing else here is gold.
//!
//! The owner-set colours come from `annotations { … }` in `abyss.kdl`
//! ([`AnnotationColors`]); a caller of the control socket cannot name one.
//! The rest are derived from them or are fixed by STYLE.md. Each is matched
//! to its desktop token, `ec_ui::tokens::color::*`, named in its doc (a name,
//! not a link: this crate does not depend on `ec-ui`).
//!
//! All values are straight (non-premultiplied) RGBA, as in config.

use ec_abyss_config::{AnnotationColors, PANEL_TINT_MIN_ALPHA};

use crate::text::Rgba;

const fn alpha(c: Rgba, a: f32) -> Rgba {
    [c[0], c[1], c[2], a]
}

const fn white(a: f32) -> Rgba {
    [1.0, 1.0, 1.0, a]
}

/// `#0b0906`, STYLE.md's base. Glyph ink on the solid accent tab.
/// `ec_ui::tokens::color::BASE`.
pub const INK: Rgba = [0x0b as f32 / 255.0, 0x09 as f32 / 255.0, 0x06 as f32 / 255.0, 1.0];

/// The HUD's colours for one frame. Plain `Copy` data: built from the config
/// on every call to the entry point at no cost, and compared as part of the
/// art cache key so a config reload repaints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudPalette {
    /// The pick tab, the pick wash and rim, the selector band.
    /// `ec_ui::tokens::color::ACCENT`.
    pub accent: Rgba,
    /// The pick letter inside the panel. `ec_ui::tokens::color::ACCENT_TEXT`.
    pub accent_text: Rgba,
    /// Title and body. `ec_ui::tokens::color::TEXT`.
    pub text: Rgba,
    /// `text` at .64: the selector hint. `ec_ui::tokens::color::TEXT_SECONDARY`.
    pub text_secondary: Rgba,
    /// `text` at .40: meta suffixes such as "(low confidence)".
    /// `ec_ui::tokens::color::TEXT_TERTIARY`.
    pub text_tertiary: Rgba,
    /// Fill over the blurred backdrop, alpha never below
    /// [`PANEL_TINT_MIN_ALPHA`]. `ec_ui::tokens::color::MENU_GROUND`.
    pub panel_tint: Rgba,
    /// The panel's 1px rim. `ec_ui::tokens::color::BORDER`.
    pub panel_rim: Rgba,
    /// The rule between title and body. `ec_ui::tokens::color::HAIRLINE`.
    pub hairline: Rgba,
    /// The region's rounded rim. `ec_ui::tokens::color::BORDER_STRONG`.
    pub region_outline: Rgba,
    /// Black .35 just outside the region rim (and the pills), so a white
    /// rim still reads on a white page. No desktop token: the desktop never
    /// draws over arbitrary pages.
    pub keyline: Rgba,
    /// The leader. `ec_ui::tokens::color::BORDER_STRONG`.
    pub leader: Rgba,
    /// The dot that ends the leader outside the region rim.
    /// `ec_ui::tokens::color::TEXT_SECONDARY`.
    pub leader_dot: Rgba,
    /// The dim outside a region selection. `ec_ui::tokens::color::BASE` @ .40.
    pub selection_dim: Rgba,
    /// The dot before an error's title. `ec_ui::tokens::color::DANGER`.
    pub danger: Rgba,
    /// Glyphs on the solid accent tab. `ec_ui::tokens::color::BASE`.
    pub ink: Rgba,
    /// Blur-off fill: `#12100b` @ .94. `ec_ui::tokens::color::GLASS_DEEP_BACKED`.
    pub fallback_fill: Rgba,
    /// Blur-off 1px top highlight. `ec_ui::tokens::color::HIGHLIGHT_SOFT`.
    pub fallback_highlight: Rgba,
    /// Accent @ .09 over the picked option. `ec_ui::tokens::color::ACCENT_FILL`.
    pub pick_wash: Rgba,
    /// Accent @ .28, the wash's rim. `ec_ui::tokens::color::ACCENT_BORDER`.
    pub pick_rim: Rgba,
    /// Peak alpha of the panel's drop shadow (black). `CELL_SHADOW`'s family.
    pub shadow: Rgba,
}

impl HudPalette {
    pub fn new(c: &AnnotationColors) -> HudPalette {
        // Config validation already raises a low tint alpha; do it again here
        // so the floor holds even for a palette built some other way. White
        // text must stay legible on a white page.
        let tint = alpha(c.panel_tint, c.panel_tint[3].clamp(PANEL_TINT_MIN_ALPHA, 1.0));
        HudPalette {
            accent: c.accent,
            accent_text: c.accent_text,
            text: c.text,
            text_secondary: alpha(c.text, c.text[3] * 0.64),
            text_tertiary: alpha(c.text, c.text[3] * 0.40),
            panel_tint: tint,
            panel_rim: c.panel_rim,
            hairline: c.hairline,
            region_outline: c.region_outline,
            keyline: [0.0, 0.0, 0.0, 0.35],
            leader: c.leader,
            leader_dot: white(0.64),
            selection_dim: c.selection_dim,
            danger: c.danger,
            ink: INK,
            fallback_fill: [
                0x12 as f32 / 255.0,
                0x10 as f32 / 255.0,
                0x0b as f32 / 255.0,
                0.94,
            ],
            fallback_highlight: white(0.07),
            pick_wash: alpha(c.accent, 0.09),
            pick_rim: alpha(c.accent, 0.28),
            shadow: [0.0, 0.0, 0.0, 0.70],
        }
    }
}

impl Default for HudPalette {
    fn default() -> Self {
        HudPalette::new(&AnnotationColors::DEFAULT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tint_floor_holds_whatever_the_config_says() {
        let mut c = AnnotationColors::DEFAULT;
        c.panel_tint[3] = 0.0;
        assert!(HudPalette::new(&c).panel_tint[3] >= PANEL_TINT_MIN_ALPHA);
    }

    #[test]
    fn only_the_pick_colours_are_derived_from_the_accent() {
        // The accent ledger: if a new colour starts borrowing the accent's
        // hue, this list has to grow on purpose.
        let p = HudPalette::default();
        let gold = |c: Rgba| c[0] > 0.8 && c[1] > 0.6 && c[2] < 0.5;
        let golden: Vec<Rgba> = [
            p.text,
            p.text_secondary,
            p.text_tertiary,
            p.panel_tint,
            p.panel_rim,
            p.hairline,
            p.region_outline,
            p.keyline,
            p.leader,
            p.leader_dot,
            p.selection_dim,
            p.danger,
            p.ink,
            p.fallback_fill,
            p.fallback_highlight,
            p.shadow,
        ]
        .into_iter()
        .filter(|&c| gold(c))
        .collect();
        assert!(golden.is_empty(), "{golden:?}");
        assert!(gold(p.accent) && gold(p.pick_wash) && gold(p.pick_rim));
    }
}
