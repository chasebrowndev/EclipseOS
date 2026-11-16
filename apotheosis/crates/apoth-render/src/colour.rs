// SPDX-License-Identifier: AGPL-3.0-only
//! Colour resolution, premultiplication, text blending and minimum contrast (T-07 §3/§4,
//! T-20 §6; RENDERING-19, -24, -25, -64).

use apoth_grid::Color;

/// An opaque sRGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Packs as little-endian RGBA8 with the given alpha, premultiplied.
    #[must_use]
    pub fn premul(self, a: u8) -> [u8; 4] {
        let m = |c: u8| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
        [m(self.0), m(self.1), m(self.2), a]
    }
}

/// The colour table the renderer resolves cells against.
#[derive(Clone, Debug)]
pub struct Palette {
    /// Default foreground.
    pub fg: Rgb,
    /// Default background.
    pub bg: Rgb,
    /// The 256 indexed colours.
    pub indexed: [Rgb; 256],
    /// Selection background.
    pub selection_bg: Rgb,
    /// Selection foreground; `None` keeps the cell's own (after inverse).
    pub selection_fg: Option<Rgb>,
    /// Cursor colour; `None` uses the cell's foreground.
    pub cursor: Option<Rgb>,
    /// Colour of the glyph under a block cursor (`cursor.text-color`); `None` uses the cell
    /// background.
    pub cursor_text: Option<Rgb>,
}

impl Palette {
    /// The xterm palette with a light-on-dark default pair.
    #[must_use]
    pub fn xterm() -> Self {
        const BASE: [(u8, u8, u8); 16] = [
            (0, 0, 0),
            (205, 0, 0),
            (0, 205, 0),
            (205, 205, 0),
            (0, 0, 238),
            (205, 0, 205),
            (0, 205, 205),
            (229, 229, 229),
            (127, 127, 127),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (92, 92, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ];
        let mut indexed = [Rgb(0, 0, 0); 256];
        for (i, c) in BASE.iter().enumerate() {
            indexed[i] = Rgb(c.0, c.1, c.2);
        }
        let level = |v: usize| if v == 0 { 0 } else { (55 + 40 * v) as u8 };
        for i in 0..216 {
            indexed[16 + i] = Rgb(level(i / 36), level((i / 6) % 6), level(i % 6));
        }
        for i in 0..24 {
            let g = (8 + 10 * i) as u8;
            indexed[232 + i] = Rgb(g, g, g);
        }
        Self {
            fg: Rgb(229, 229, 229),
            bg: Rgb(0, 0, 0),
            indexed,
            selection_bg: Rgb(68, 68, 120),
            selection_fg: None,
            cursor: None,
            cursor_text: None,
        }
    }

    /// Resolves a foreground colour; `bold_bright` maps indices 0..8 to 8..16.
    #[must_use]
    pub fn resolve_fg(&self, c: Color, bold_bright: bool) -> Rgb {
        match c {
            Color::Default => self.fg,
            Color::Palette(i) => {
                let i = if bold_bright && i < 8 { i + 8 } else { i };
                self.indexed[usize::from(i)]
            }
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        }
    }

    /// Resolves a background colour.
    #[must_use]
    pub fn resolve_bg(&self, c: Color) -> Rgb {
        match c {
            Color::Default => self.bg,
            Color::Palette(i) => self.indexed[usize::from(i)],
            Color::Rgb(r, g, b) => Rgb(r, g, b),
        }
    }
}

/// `render.text-blending`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextBlending {
    /// Linear-light blend with coverage corrected for perceived weight (default).
    #[default]
    LinearCorrected,
    /// Plain linear-light blend.
    Linear,
    /// Blend in encoded sRGB space.
    Native,
}

/// sRGB byte to linear light.
#[must_use]
pub fn to_linear(c: u8) -> f32 {
    let v = f32::from(c) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to an sRGB byte.
#[must_use]
pub fn to_srgb(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let v = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0 + 0.5) as u8
}

/// WCAG relative luminance.
#[must_use]
pub fn luminance(c: Rgb) -> f32 {
    0.2126 * to_linear(c.0) + 0.7152 * to_linear(c.1) + 0.0722 * to_linear(c.2)
}

/// WCAG contrast ratio, 1.0..=21.0.
#[must_use]
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Luminance differences below this skip the linear-corrected adjustment (RENDERING-19).
pub const CORRECTION_EPSILON: f32 = 0.001;

/// Strength of the weight correction. T-07 gives no formula; this constant is the reference
/// value the WGSL port must match.
pub const CORRECTION_GAIN: f32 = 0.5;

/// Blends `fg` over `bg` with glyph coverage `cov` (0..=1) per `mode`. This is the CPU
/// reference of the glyph shader's blend.
#[must_use]
pub fn blend_coverage(mode: TextBlending, fg: Rgb, bg: Rgb, cov: f32) -> Rgb {
    let cov = cov.clamp(0.0, 1.0);
    match mode {
        TextBlending::Native => {
            let m = |f: u8, b: u8| (f32::from(f) * cov + f32::from(b) * (1.0 - cov) + 0.5) as u8;
            Rgb(m(fg.0, bg.0), m(fg.1, bg.1), m(fg.2, bg.2))
        }
        TextBlending::Linear | TextBlending::LinearCorrected => {
            let mut a = cov;
            if mode == TextBlending::LinearCorrected {
                let d = luminance(bg) - luminance(fg);
                if d.abs() >= CORRECTION_EPSILON {
                    a = (a + a * (1.0 - a) * d * CORRECTION_GAIN).clamp(0.0, 1.0);
                }
            }
            let m = |f: u8, b: u8| to_srgb(to_linear(f) * a + to_linear(b) * (1.0 - a));
            Rgb(m(fg.0, bg.0), m(fg.1, bg.1), m(fg.2, bg.2))
        }
    }
}

/// `colors.minimum-contrast`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum MinContrast {
    /// Off (ratio 1.0).
    #[default]
    Off,
    /// A ratio in 1.0..=21.0.
    Ratio(f32),
    /// Raises only indistinguishable pairs.
    Automatic,
}

/// Ratio `automatic` enforces. T-20 §6 says "indistinguishable pairs" without a number.
pub const AUTOMATIC_RATIO: f32 = 1.5;

impl MinContrast {
    /// The ratio to enforce, or `None` when off.
    #[must_use]
    pub fn target(self) -> Option<f32> {
        match self {
            MinContrast::Off => None,
            MinContrast::Ratio(r) if r <= 1.0 => None,
            MinContrast::Ratio(r) => Some(r.min(21.0)),
            MinContrast::Automatic => Some(AUTOMATIC_RATIO),
        }
    }
}

/// Nudges `fg` lightness until `contrast_ratio(fg, bg) >= target`, moving towards the
/// extreme (white or black) that can reach it. Returns `fg` unchanged when it already
/// passes or no direction can reach the target.
#[must_use]
pub fn apply_min_contrast(fg: Rgb, bg: Rgb, target: f32) -> Rgb {
    if contrast_ratio(fg, bg) >= target {
        return fg;
    }
    let white = Rgb(255, 255, 255);
    let black = Rgb(0, 0, 0);
    let toward = if luminance(bg) > 0.179 { black } else { white };
    let other = if toward == black { white } else { black };
    let end = if contrast_ratio(toward, bg) >= target {
        toward
    } else {
        other
    };
    if contrast_ratio(end, bg) < target {
        return fg;
    }
    // Binary search on the mix amount in linear light.
    let mix = |t: f32| {
        let m = |f: u8, e: u8| to_srgb(to_linear(f) * (1.0 - t) + to_linear(e) * t);
        Rgb(m(fg.0, end.0), m(fg.1, end.1), m(fg.2, end.2))
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..12 {
        let mid = (lo + hi) * 0.5;
        if contrast_ratio(mix(mid), bg) >= target {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix(hi)
}

/// Halves a foreground towards the background for SGR 2 (faint).
#[must_use]
pub fn dim(fg: Rgb, bg: Rgb) -> Rgb {
    let m = |f: u8, b: u8| (u16::from(f) + u16::from(b)).div_ceil(2) as u8;
    Rgb(m(fg.0, bg.0), m(fg.1, bg.1), m(fg.2, bg.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_cube_and_gray() {
        let p = Palette::xterm();
        assert_eq!(p.indexed[16], Rgb(0, 0, 0));
        assert_eq!(p.indexed[196], Rgb(255, 0, 0));
        assert_eq!(p.indexed[231], Rgb(255, 255, 255));
        assert_eq!(p.indexed[232], Rgb(8, 8, 8));
        assert_eq!(p.indexed[255], Rgb(238, 238, 238));
    }

    #[test]
    fn bold_bright_only_low_indices() {
        let p = Palette::xterm();
        assert_eq!(p.resolve_fg(Color::Palette(1), true), p.indexed[9]);
        assert_eq!(p.resolve_fg(Color::Palette(9), true), p.indexed[9]);
        assert_eq!(p.resolve_fg(Color::Palette(1), false), p.indexed[1]);
    }

    #[test]
    fn premul_is_premultiplied() {
        assert_eq!(Rgb(255, 128, 0).premul(128), [128, 64, 0, 128]);
        assert_eq!(Rgb(10, 20, 30).premul(255), [10, 20, 30, 255]);
        assert_eq!(Rgb(10, 20, 30).premul(0), [0, 0, 0, 0]);
    }

    #[test]
    fn srgb_roundtrip() {
        for c in 0..=255u8 {
            assert_eq!(to_srgb(to_linear(c)), c);
        }
    }

    #[test]
    fn contrast_extremes() {
        let r = contrast_ratio(Rgb(0, 0, 0), Rgb(255, 255, 255));
        assert!((r - 21.0).abs() < 0.01, "{r}");
        assert!((contrast_ratio(Rgb(9, 9, 9), Rgb(9, 9, 9)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn min_contrast_raises_and_leaves_good_pairs() {
        let bg = Rgb(30, 30, 30);
        let fg = Rgb(40, 40, 40);
        let out = apply_min_contrast(fg, bg, 4.5);
        assert!(contrast_ratio(out, bg) >= 4.5 - 0.01);
        let good = Rgb(250, 250, 250);
        assert_eq!(apply_min_contrast(good, bg, 4.5), good);
    }

    #[test]
    fn min_contrast_targets() {
        assert_eq!(MinContrast::Off.target(), None);
        assert_eq!(MinContrast::Ratio(1.0).target(), None);
        assert_eq!(MinContrast::Ratio(30.0).target(), Some(21.0));
        assert_eq!(MinContrast::Automatic.target(), Some(AUTOMATIC_RATIO));
    }

    #[test]
    fn blend_endpoints_and_correction_skip() {
        let (f, b) = (Rgb(255, 255, 255), Rgb(0, 0, 0));
        for m in [
            TextBlending::Native,
            TextBlending::Linear,
            TextBlending::LinearCorrected,
        ] {
            assert_eq!(blend_coverage(m, f, b, 0.0), b);
            assert_eq!(blend_coverage(m, f, b, 1.0), f);
        }
        // Equal luminance: corrected == linear.
        let g = Rgb(100, 100, 100);
        assert_eq!(
            blend_coverage(TextBlending::LinearCorrected, g, g, 0.5),
            blend_coverage(TextBlending::Linear, g, g, 0.5)
        );
    }

    #[test]
    fn corrected_boosts_dark_on_light() {
        let (f, b) = (Rgb(0, 0, 0), Rgb(255, 255, 255));
        let lin = blend_coverage(TextBlending::Linear, f, b, 0.5);
        let cor = blend_coverage(TextBlending::LinearCorrected, f, b, 0.5);
        assert!(
            cor.0 < lin.0,
            "dark text must get heavier: {cor:?} vs {lin:?}"
        );
    }
}
