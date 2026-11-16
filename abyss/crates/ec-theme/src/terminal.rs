// SPDX-License-Identifier: AGPL-3.0-only
//! The terminal colour scheme of a theme (ADR 0084 amendment, milestone X5; T-17 §2).
//!
//! Neutrals (ANSI 0, 7, 8, 15 and the foreground) are the ink laid over the base at fixed
//! strengths, so they follow any seed set; on the light seeds they reproduce the shipped
//! `packaging/etc/foot.ini` light table. The six hues are fixed except yellow, which is the
//! accent's; each starts at a scheme lightness and is moved away from the base until it
//! reads on it ([`MIN_ANSI_CONTRAST`] for the regular row, [`MIN_ANSI_BRIGHT_CONTRAST`] for
//! the bright row). The cursor and the selection are the accent, with whichever of base or
//! ink reads better on it as their text. Every colour is opaque and on the 8-bit grid.

use crate::{contrast, hsl_to_rgb, rgb_to_hsl, Rgba, Scheme, Seeds};

/// Contrast the regular hues (ANSI 1-6) aim for on the background.
pub const MIN_ANSI_CONTRAST: f32 = 4.5;
/// Contrast the bright hues (ANSI 9-14) aim for on the background.
pub const MIN_ANSI_BRIGHT_CONTRAST: f32 = 3.0;
/// Floor `validate` holds every hue to (the search can miss its aim on unusual bases).
pub const MIN_ANSI_FLOOR: f32 = 3.0;

/// A terminal's colours: the 16 ANSI slots and the special colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TermScheme {
    /// ANSI 0-15 (black, red, green, yellow, blue, magenta, cyan, white; then bright).
    pub ansi: [Rgba; 16],
    pub foreground: Rgba,
    pub background: Rgba,
    pub cursor: Rgba,
    pub cursor_text: Rgba,
    pub selection_foreground: Rgba,
    pub selection_background: Rgba,
}

/// Hue (degrees) and saturation of red, green, (yellow: the accent), blue, magenta, cyan.
const HUES: [(f32, f32); 6] = [
    (3.0, 0.70),
    (123.0, 0.46),
    (45.0, 0.88),
    (218.0, 0.68),
    (300.0, 0.42),
    (180.0, 0.78),
];

/// Ink strength over the base for ANSI 0, 7, 8, 15 and the foreground.
struct Neutrals {
    black: f32,
    white: f32,
    bright_black: f32,
    bright_white: f32,
    foreground: f32,
}

const DARK_NEUTRALS: Neutrals = Neutrals {
    black: 0.10,
    white: 0.78,
    bright_black: 0.42,
    bright_white: 1.0,
    foreground: 0.90,
};

/// Light terminals keep "white" readable on the light ground: 7 and 15 are dark inks.
const LIGHT_NEUTRALS: Neutrals = Neutrals {
    black: 0.986,
    white: 0.70,
    bright_black: 0.78,
    bright_white: 0.92,
    foreground: 0.986,
};

/// Starting lightness of the regular and bright rows.
fn start_lightness(scheme: Scheme) -> (f32, f32) {
    match scheme {
        Scheme::Dark => (0.62, 0.72),
        Scheme::Light => (0.40, 0.50),
    }
}

fn q(v: f32) -> f32 {
    (v * 255.0).round().clamp(0.0, 255.0) / 255.0
}

fn quant(c: Rgba) -> Rgba {
    Rgba([q(c.0[0]), q(c.0[1]), q(c.0[2]), 1.0])
}

fn ink_at(seeds: &Seeds, strength: f32) -> Rgba {
    quant(seeds.ink.with_alpha(strength).over_opaque(seeds.base))
}

/// `hue`/`sat` at the first lightness from `l` (moving away from the base) that reads at
/// `min` on `base`; the last tried when none does.
fn hue_on(base: Rgba, dark: bool, hue: f32, sat: f32, mut l: f32, min: f32) -> Rgba {
    let make = |l: f32| {
        let (r, g, b) = hsl_to_rgb(hue, sat, l.clamp(0.0, 1.0));
        Rgba([q(r), q(g), q(b), 1.0])
    };
    let mut c = make(l);
    for _ in 0..64 {
        if contrast(c, base) >= min {
            break;
        }
        l += if dark { 0.01 } else { -0.01 };
        if !(0.0..=1.0).contains(&l) {
            break;
        }
        c = make(l);
    }
    c
}

/// The terminal scheme of `seeds`.
pub fn terminal_scheme(seeds: &Seeds) -> TermScheme {
    let scheme = seeds.scheme();
    let dark = scheme == Scheme::Dark;
    let n = if dark { &DARK_NEUTRALS } else { &LIGHT_NEUTRALS };
    let base = quant(seeds.base);
    let accent = quant(seeds.accent);
    let (lr, lb) = start_lightness(scheme);
    let (ah, as_, _) = rgb_to_hsl(accent.0[0], accent.0[1], accent.0[2]);
    let mut ansi = [base; 16];
    ansi[0] = ink_at(seeds, n.black);
    ansi[7] = ink_at(seeds, n.white);
    ansi[8] = ink_at(seeds, n.bright_black);
    ansi[15] = ink_at(seeds, n.bright_white);
    for (i, &(h, s)) in HUES.iter().enumerate() {
        let (h, s) = if i == 2 { (ah, as_) } else { (h, s) };
        ansi[1 + i] = hue_on(base, dark, h, s, lr, MIN_ANSI_CONTRAST);
        ansi[9 + i] = hue_on(base, dark, h, s, lb, MIN_ANSI_BRIGHT_CONTRAST);
    }
    let foreground = ink_at(seeds, n.foreground);
    let ink = quant(seeds.ink);
    // Text on the accent: base or ink, whichever reads better; when neither
    // clears 4.5:1 (a mid-tone accent), black or white, one of which always does.
    let best = |a: Rgba, b: Rgba| {
        if contrast(a, accent) >= contrast(b, accent) {
            a
        } else {
            b
        }
    };
    let mut on_accent = best(base, ink);
    if contrast(on_accent, accent) < crate::MIN_TEXT_CONTRAST {
        on_accent = best(Rgba::rgb(0x000000), Rgba::rgb(0xffffff));
    }
    TermScheme {
        ansi,
        foreground,
        background: base,
        cursor: accent,
        cursor_text: on_accent,
        selection_foreground: on_accent,
        selection_background: accent,
    }
}

/// `(what, ratio, min)` of the first terminal colour that misses its floor.
pub(crate) fn check(t: &TermScheme) -> Option<(&'static str, f32, f32)> {
    let fg = contrast(t.foreground, t.background);
    if fg < crate::MIN_TEXT_CONTRAST {
        return Some(("terminal foreground", fg, crate::MIN_TEXT_CONTRAST));
    }
    for i in (1..7).chain(9..15) {
        let r = contrast(t.ansi[i], t.background);
        if r < MIN_ANSI_FLOOR {
            return Some(("terminal ANSI colour", r, MIN_ANSI_FLOOR));
        }
    }
    let sel = contrast(t.selection_foreground, t.selection_background);
    if sel < crate::MIN_TEXT_CONTRAST {
        return Some(("terminal selection", sel, crate::MIN_TEXT_CONTRAST));
    }
    let cur = contrast(t.cursor_text, t.cursor);
    if cur < crate::MIN_SECONDARY_CONTRAST {
        return Some(("terminal cursor text", cur, crate::MIN_SECONDARY_CONTRAST));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{validate, DARK_SEEDS, LIGHT_SEEDS};

    fn hex(c: Rgba) -> u32 {
        let b = |v: f32| (v * 255.0).round() as u32;
        (b(c.0[0]) << 16) | (b(c.0[1]) << 8) | b(c.0[2])
    }

    #[test]
    fn builtins_read_and_validate() {
        for seeds in [DARK_SEEDS, LIGHT_SEEDS] {
            let t = terminal_scheme(&seeds);
            assert_eq!(check(&t), None);
            assert!(validate(&seeds).is_ok());
            for i in 1..7 {
                assert!(contrast(t.ansi[i], t.background) >= MIN_ANSI_CONTRAST, "ansi {i}");
                assert!(
                    contrast(t.ansi[i + 8], t.background) >= MIN_ANSI_BRIGHT_CONTRAST,
                    "ansi {}",
                    i + 8
                );
            }
            assert!(t.ansi.iter().all(|c| c.a() == 1.0));
        }
    }

    #[test]
    fn light_neutrals_are_the_shipped_foot_table() {
        let t = terminal_scheme(&LIGHT_SEEDS);
        // packaging/etc/foot.ini [colors-light]: same lightness (the hand table is a
        // touch less warm than ink over base, so compare luminance, not channels).
        let want = [(0, 0x1d1b17), (7, 0x5b574f), (8, 0x4a463f), (15, 0x2b2822)];
        for (i, w) in want {
            let (got, w) = (t.ansi[i], Rgba::rgb(w));
            let r = contrast(got, w);
            assert!(r < 1.06, "ansi {i}: {:06x} vs {:06x} ({r})", hex(got), hex(w));
        }
        // Yellow is the accent's hue; selection and cursor are the accent.
        assert_eq!(t.selection_background, Rgba::rgb(0xf2c33c));
        assert_eq!(t.cursor, Rgba::rgb(0xf2c33c));
        assert_eq!(t.selection_foreground, Rgba::rgb(0x1a1610));
        let (h, _, _) = rgb_to_hsl(t.ansi[3].0[0], t.ansi[3].0[1], t.ansi[3].0[2]);
        assert!((h - 45.0).abs() < 4.0, "{h}");
    }

    #[test]
    fn dark_scheme_shape() {
        let t = terminal_scheme(&DARK_SEEDS);
        assert_eq!(t.background, Rgba::rgb(0x0b0906));
        assert_eq!(t.selection_foreground, Rgba::rgb(0x0b0906));
        assert_eq!(t.ansi[15], Rgba::rgb(0xffffff));
        // Black sits just above the base, bright black between it and white.
        let l = |i: usize| t.ansi[i].luminance();
        assert!(l(0) < l(8) && l(8) < l(7) && l(7) < l(15));
        for i in 1..7 {
            assert!(l(i + 8) > l(i), "bright {i} is lighter");
        }
    }

    #[test]
    fn follows_the_seeds() {
        let blue_base = Seeds {
            base: Rgba::rgb(0x0a1020),
            ..DARK_SEEDS
        };
        let t = terminal_scheme(&blue_base);
        assert_eq!(t.background, Rgba::rgb(0x0a1020));
        assert_ne!(t.ansi[0], terminal_scheme(&DARK_SEEDS).ansi[0]);
        assert_eq!(check(&t), None);
        let green = DARK_SEEDS.with_accent(Rgba::rgb(0x3cc06a));
        let t = terminal_scheme(&green);
        assert_eq!(t.cursor, Rgba::rgb(0x3cc06a));
        let (h, _, _) = rgb_to_hsl(t.ansi[3].0[0], t.ansi[3].0[1], t.ansi[3].0[2]);
        assert!((h - 141.0).abs() < 4.0, "yellow follows the accent: {h}");
    }

    #[test]
    fn a_mid_tone_accent_still_gets_readable_selection_text() {
        let paper = LIGHT_SEEDS.with_accent(Rgba::rgb(0xc06010));
        let t = terminal_scheme(&paper);
        assert!(contrast(t.selection_foreground, t.selection_background) >= 4.5);
        assert_eq!(check(&t), None);
        // Any accent: one of black or white clears 4.5:1.
        for v in (0..=255u32).step_by(5) {
            let s = DARK_SEEDS.with_accent(Rgba::rgb((v << 16) | ((255 - v) << 8) | (v / 2)));
            let t = terminal_scheme(&s);
            assert!(
                contrast(t.selection_foreground, t.selection_background) >= 4.5,
                "{v}"
            );
        }
    }

    #[test]
    fn unreadable_terminal_fails_validate() {
        let mut t = terminal_scheme(&DARK_SEEDS);
        t.ansi[4] = t.background;
        assert!(matches!(check(&t), Some(("terminal ANSI colour", _, _))));
    }
}
