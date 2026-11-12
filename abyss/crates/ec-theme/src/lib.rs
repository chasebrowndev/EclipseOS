// SPDX-License-Identifier: AGPL-3.0-only
//! Seed colours to a full desktop palette (ADR 0084).
//!
//! A theme is a handful of [`Seeds`]; [`derive`] maps them to every token the
//! DE uses ([`Palette`]). Two seed sets are built in, [`DARK_SEEDS`] and
//! [`LIGHT_SEEDS`]. `derive(&DARK_SEEDS)` reproduces the historical dark tokens
//! exactly (the `dark_golden` test pins every value); the light seeds produce
//! `docs/STYLE.md` "Light".
//!
//! Colour convention: [`Rgba`] is *straight* (non-premultiplied) sRGB numbers in
//! `0..=1`, exactly what `iced::Color` carries. The alphas in the tables below
//! are the values the DE hands to iced, which blends in linear light, so the
//! dark ones are STYLE.md's sRGB intent brought back to linear (see `tokens.rs`
//! history); they are not the CSS figures in STYLE.md.

pub mod bar_mark;

use std::fmt;

/// Straight sRGB, each channel `0..=1`: `[r, g, b, a]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    /// `#rrggbb` at full opacity. Same arithmetic the DE's old `rgb()` used.
    pub const fn rgb(hex: u32) -> Self {
        Rgba([
            ((hex >> 16) & 0xff) as f32 / 255.0,
            ((hex >> 8) & 0xff) as f32 / 255.0,
            (hex & 0xff) as f32 / 255.0,
            1.0,
        ])
    }

    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Rgba([r, g, b, a])
    }

    pub const fn r(self) -> f32 {
        self.0[0]
    }
    pub const fn g(self) -> f32 {
        self.0[1]
    }
    pub const fn b(self) -> f32 {
        self.0[2]
    }
    pub const fn a(self) -> f32 {
        self.0[3]
    }

    /// The same colour at alpha `a` (replaces, does not multiply).
    pub const fn with_alpha(self, a: f32) -> Self {
        Rgba([self.0[0], self.0[1], self.0[2], a])
    }

    /// `self` composited over an opaque `bg` (Porter-Duff "over"); result is
    /// opaque. Plain sRGB-number arithmetic, matching the DE's `over_opaque`.
    pub const fn over_opaque(self, bg: Rgba) -> Rgba {
        let a = self.0[3];
        let keep = 1.0 - a;
        Rgba([
            self.0[0] * a + bg.0[0] * keep,
            self.0[1] * a + bg.0[1] * keep,
            self.0[2] * a + bg.0[2] * keep,
            1.0,
        ])
    }

    /// WCAG relative luminance of the colour's RGB (alpha ignored).
    pub fn luminance(self) -> f32 {
        0.2126 * srgb_to_linear(self.0[0])
            + 0.7152 * srgb_to_linear(self.0[1])
            + 0.0722 * srgb_to_linear(self.0[2])
    }

    fn finite_unit(self) -> bool {
        self.0.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c))
    }
}

/// sRGB-encoded channel to linear light.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear-light channel to sRGB encoding.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// WCAG contrast ratio (`1..=21`) of `fg` over the opaque `bg`. A translucent
/// `fg` is first composited over `bg` the way iced draws it, in linear light
/// (see [`linear_alpha`]), so the figure is what is on screen.
pub fn contrast(fg: Rgba, bg: Rgba) -> f32 {
    let a = fg.0[3];
    let mix = |c: usize| a * srgb_to_linear(fg.0[c]) + (1.0 - a) * srgb_to_linear(bg.0[c]);
    let l1 = 0.2126 * mix(0) + 0.7152 * mix(1) + 0.0722 * mix(2);
    let l2 = bg.luminance();
    let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// The theme's inputs. See ADR 0084.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seeds {
    /// Window and desktop base, opaque. Decides the [`Scheme`].
    pub base: Rgba,
    /// Ink: text, hairlines, borders, tracks. Opaque; tokens are ink at alpha.
    pub ink: Rgba,
    /// The single accent (fills, toggles, marks).
    pub accent: Rgba,
    /// Accent used as text. `None` falls back to `accent` (and `validate`
    /// then demands it clear 4.5:1 on its own).
    pub accent_text: Option<Rgba>,
    /// The tint laid over the compositor's blur (sheets, bar, menus); opaque
    /// colour, alpha comes from the token table.
    pub frost_tint: Rgba,
    /// Strength of the compositor's rim light, `0..=1`. Carried for the
    /// compositor side of a theme; no palette token is derived from it.
    pub rim: f32,
}

/// Brightness class of a seed set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Dark,
    Light,
}

impl Seeds {
    /// `Light` when the base's relative luminance is at least `0.179`, the
    /// WCAG crossover where black and white ink give equal contrast; `Dark`
    /// below. Picks which alpha table and fixed colours [`derive`] uses.
    pub fn scheme(&self) -> Scheme {
        if self.base.luminance() >= SCHEME_LUMINANCE {
            Scheme::Light
        } else {
            Scheme::Dark
        }
    }
}

impl Seeds {
    /// These seeds with `accent` replaced and `accent_text` re-derived from
    /// it ([`accent_text_for`]), so the result always clears the 4.5:1 text
    /// rule on its own base. The shared user accent (`general.accent`) is
    /// applied this way over whichever theme is active.
    pub fn with_accent(&self, accent: Rgba) -> Seeds {
        let accent = accent.with_alpha(1.0);
        Seeds {
            accent,
            accent_text: Some(accent_text_for(accent, self.base)),
            ..*self
        }
    }
}

/// How far HSL lightness is lifted (dark) or dropped (light) for accent text
/// before the contrast search starts: `#f2c33c` on the dark base gives
/// `#f4cd5d`, within a step of the hand-picked `#f5cf5c`.
const TEXT_LIGHTNESS_STEP: f32 = 0.0686;

/// The text variant of `accent` on `base`: same hue and saturation; lighter on
/// a dark base, darker on a light one, then nudged until it clears
/// [`MIN_TEXT_CONTRAST`]. Pure and total; the one shared definition.
pub fn accent_text_for(accent: Rgba, base: Rgba) -> Rgba {
    let dark = base.luminance() < SCHEME_LUMINANCE;
    let (h, s, l) = rgb_to_hsl(accent.0[0], accent.0[1], accent.0[2]);
    let q = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) / 255.0;
    let make = |l: f32| {
        let (r, g, b) = hsl_to_rgb(h, s, l.clamp(0.0, 1.0));
        Rgba([q(r), q(g), q(b), 1.0])
    };
    let mut l = if dark {
        (l + TEXT_LIGHTNESS_STEP).min(0.95).max(l)
    } else {
        (l - TEXT_LIGHTNESS_STEP).max(0.05).min(l)
    };
    let mut c = make(l);
    for _ in 0..64 {
        if contrast(c, base) >= MIN_TEXT_CONTRAST {
            break;
        }
        l += if dark { 0.02 } else { -0.02 };
        c = make(l);
        if !(0.0..=1.0).contains(&l) {
            break;
        }
    }
    c
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// See [`Seeds::scheme`].
pub const SCHEME_LUMINANCE: f32 = 0.179;

pub const DARK_SEEDS: Seeds = Seeds {
    base: Rgba::rgb(0x0b0906),
    ink: Rgba::rgb(0xffffff),
    accent: Rgba::rgb(0xf2c33c),
    accent_text: Some(Rgba::rgb(0xf5cf5c)),
    frost_tint: Rgba::rgb(0x17140f),
    rim: 0.20,
};

/// STYLE.md "Light".
pub const LIGHT_SEEDS: Seeds = Seeds {
    base: Rgba::rgb(0xf4f1ea),
    ink: Rgba::rgb(0x1a1610),
    accent: Rgba::rgb(0xf2c33c),
    accent_text: Some(Rgba::rgb(0x856000)),
    frost_tint: Rgba::rgb(0xf4f1ea),
    rim: 0.60,
};

/// The linear-light alpha at which `src` over `ground` reads on screen as the
/// sRGB figure `intent` (STYLE.md quotes sRGB; iced blends in linear light).
///
/// Per channel the sRGB-number mix `intent*src + (1-intent)*ground` is the
/// target; the alpha that reaches it in linear light is
/// `(L(out) - L(ground)) / (L(src) - L(ground))`, averaged over channels
/// weighted by how far `src` is from `ground` there. Dark ink on a light
/// ground needs far more alpha than the CSS number: `.62` of near-black over
/// off-white is `.84` in linear light. Falls back to `intent` when `src` is
/// indistinguishable from `ground`.
pub fn linear_alpha(src: Rgba, ground: Rgba, intent: f32) -> f32 {
    let (mut num, mut den) = (0.0, 0.0);
    for c in 0..3 {
        let (s, g) = (src.0[c], ground.0[c]);
        let out = intent * s + (1.0 - intent) * g;
        num += srgb_to_linear(out) - srgb_to_linear(g);
        den += srgb_to_linear(s) - srgb_to_linear(g);
    }
    if den.abs() < 1e-4 {
        intent
    } else {
        (num / den).clamp(0.0, 1.0)
    }
}

/// The alpha to write into a tint buffer so it reads as the sRGB figure
/// `intent`. iced stores the premultiplied colour sRGB-encoded and the
/// compositor reads it as plain sRGB, so a written alpha `a` weighs about
/// `enc(0.89 * a)` on screen (measured headless: written .30 landed near .74).
/// `linear_alpha` cannot help here, the frost tint is the base colour. A
/// scheme without `intent` keeps its figure as written.
pub fn premult_alpha(intent: f32, scheme_intent: bool) -> f32 {
    if scheme_intent {
        (srgb_to_linear(intent) / 0.89).clamp(0.0, 1.0)
    } else {
        intent
    }
}

/// How far a veil fades what it covers toward the ground.
pub const VEIL_STRENGTH: f32 = 0.62;

/// Every colour token. Field names are the old `tokens::color` constants,
/// lower-cased. Surface fills that are white (dark) / white (light) use the
/// "fill" colour, hairlines and text use the ink, tints use the frost tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub base: Rgba,
    pub surface_0: Rgba,
    pub surface_1: Rgba,
    pub surface_2: Rgba,
    pub glass: Rgba,
    pub glass_strong: Rgba,
    pub glass_deep: Rgba,
    pub glass_deep_backed: Rgba,
    pub sheet_tint: Rgba,
    pub bar_tint: Rgba,
    pub cell: Rgba,
    pub cell_hover: Rgba,
    pub cell_press: Rgba,
    pub cell_rim: Rgba,
    pub cell_rim_hover: Rgba,
    pub cell_rim_away: Rgba,
    pub cell_shadow: Rgba,
    pub cell_focus: Rgba,
    pub cell_focus_hover: Rgba,
    pub cell_focus_press: Rgba,
    pub cell_focus_rim: Rgba,
    pub cell_focus_rim_hover: Rgba,
    pub menu_ground: Rgba,
    pub border: Rgba,
    pub border_strong: Rgba,
    pub hairline: Rgba,
    pub highlight: Rgba,
    pub highlight_soft: Rgba,
    pub highlight_strong: Rgba,
    pub lift: Rgba,
    pub lift_strong: Rgba,
    pub lift_soft: Rgba,
    pub veil: Rgba,
    pub veil_glass: Rgba,
    pub sidebar: Rgba,
    pub sidebar_backed: Rgba,
    pub content: Rgba,
    pub content_backed: Rgba,
    pub accent: Rgba,
    pub accent_text: Rgba,
    pub accent_fill: Rgba,
    pub accent_border: Rgba,
    pub accent_wash: Rgba,
    pub text: Rgba,
    pub text_secondary: Rgba,
    pub text_tertiary: Rgba,
    pub neutral: Rgba,
    pub eye_iris: Rgba,
    pub control_off: Rgba,
    pub track: Rgba,
    pub danger: Rgba,
    pub danger_fill: Rgba,
    pub connected: Rgba,
    pub ok: Rgba,
    /// Brightness class the palette was derived for.
    pub scheme: Scheme,
    /// Ink for a mark or label laid on an accent fill: a toggle's knob, a
    /// glyph on a yellow disc. The base on dark, the ink on light, so it
    /// always contrasts with the accent (STYLE.md "Light": `#1a1610`).
    pub on_accent: Rgba,
    /// A nav item's selected / hovered fill.
    pub nav_active: Rgba,
    pub nav_hover: Rgba,
    /// A pill control at rest / hovered (selected is `accent_fill`).
    pub pill: Rgba,
    pub pill_hover: Rgba,
    /// A text field's ground.
    pub input_fill: Rgba,
    /// A scrollbar's scroller at rest / hovered.
    pub scroller: Rgba,
    pub scroller_hover: Rgba,
    /// A bar chart's history bars (the peak is the accent).
    pub chart_history: Rgba,
    /// An off toggle's track while hovered (`control_off` at rest).
    pub control_off_hover: Rgba,
    /// An off toggle's knob.
    pub knob_off: Rgba,
    /// The crisp rim of a selected neutral cell (`pick`, a lit editor, a
    /// selected arrangement tile): `highlight_strong` on dark, where a white
    /// rim is the highlight; the ink on light, where a white rim on a white
    /// fill would vanish.
    pub highlight_rim: Rgba,
}

/// Per-scheme numbers. Everything that is "colour at alpha" is an alpha here;
/// the rest are fixed colours that do not come from seeds (documented below).
struct Table {
    /// The alphas below are sRGB intent, to be brought to linear-light alphas
    /// over the base with [`linear_alpha`]. Dark's are already linear (and
    /// bit-exact with the historical tokens), so it leaves them alone.
    intent: bool,
    // Opaque surfaces behind glass, as built for the scheme's own base; moved
    // with the base by `shifted` when a theme brings a different base.
    builtin_base: Rgba,
    surfaces: [Rgba; 3],
    /// Fill colour for glass / cells / highlights (white in both schemes).
    fill: Rgba,
    glass: f32,
    glass_strong: f32,
    glass_deep: f32,
    sheet_tint: f32,
    bar_tint: f32,
    cell: [f32; 3],
    cell_rim: [f32; 2],
    cell_rim_away: f32,
    cell_focus: [f32; 3],
    cell_focus_rim: [f32; 2],
    /// Shadow colour and alpha under a bar cell. Fixed per scheme: black on
    /// dark, a warm brown on light (STYLE.md: no black halo on light).
    shadow: Rgba,
    cell_shadow: f32,
    border: [f32; 2],
    hairline: f32,
    highlight: [f32; 3], // soft, normal, strong
    lift: [f32; 3],      // normal, strong, soft
    /// Sidebar rail and content column ground colours (fixed per scheme) and alphas.
    sidebar_colour: Rgba,
    sidebar: f32,
    content_colour: Rgba,
    content: f32,
    accent_fill: f32,
    accent_border: f32,
    accent_wash: f32,
    text_secondary: f32,
    text_tertiary: f32,
    control_off: f32,
    track: f32,
    // Fixed semantic colours, not seed-derived: a status colour must not move
    // with a user's accent or ink.
    neutral: Rgba,
    danger: Rgba,
    danger_fill: f32,
    connected: Rgba,
    ok: Rgba,
    // Control grounds: ink-coloured except `input_fill` and `knob_off`, which
    // use `fill`.
    nav: [f32; 2],  // active, hover
    pill: [f32; 2], // rest, hover
    input_fill: f32,
    scroller: [f32; 2], // rest, hover
    chart_history: f32,
    control_off_hover: f32,
    knob_off: f32,
    highlight_rim: f32,
}

const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);
const BLACK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);

// Dark alphas are linear-light equivalents of the STYLE.md sRGB intent.
const DARK: Table = Table {
    intent: false,
    builtin_base: DARK_SEEDS.base,
    surfaces: [Rgba::rgb(0x12100b), Rgba::rgb(0x1a1712), Rgba::rgb(0x1c1913)],
    fill: WHITE,
    glass: 0.045,
    glass_strong: 0.06,
    glass_deep: 0.62,
    sheet_tint: 0.34,
    bar_tint: 0.22,
    cell: [0.006, 0.012, 0.02],
    cell_rim: [0.016, 0.026],
    cell_rim_away: 0.006,
    cell_focus: [0.014, 0.02, 0.028],
    cell_focus_rim: [0.04, 0.055],
    shadow: BLACK,
    cell_shadow: 0.30,
    border: [0.10, 0.16],
    hairline: 0.055,
    highlight: [0.07, 0.12, 0.20],
    lift: [0.08, 0.13, 0.035],
    sidebar_colour: BLACK,
    sidebar: 0.4,
    content_colour: BLACK,
    content: 0.28,
    accent_fill: 0.09,
    accent_border: 0.28,
    accent_wash: 0.035,
    text_secondary: 0.64,
    text_tertiary: 0.40,
    control_off: 0.13,
    track: 0.12,
    neutral: Rgba::rgb(0x96918a),
    danger: Rgba::rgb(0xe0553f),
    danger_fill: 0.14,
    connected: Rgba::rgb(0x4f9fe0),
    ok: Rgba::rgb(0x7fae5e),
    nav: [0.07, 0.04],
    pill: [0.035, 0.07],
    input_fill: 0.04,
    scroller: [0.13, 0.22],
    chart_history: 0.13,
    control_off_hover: 0.18,
    knob_off: 0.62,
    highlight_rim: 0.20,
};

// Light: STYLE.md "Light" figures as sRGB-intent alphas (`intent: true`),
// brought to linear-light alphas over the base by `derive`. Tuned in the
// per-pane visual pass; the status colours are darkened to clear 4.5:1 on the
// base where they carry text. Surfaces from STYLE.md.
const LIGHT: Table = Table {
    intent: true,
    builtin_base: LIGHT_SEEDS.base,
    surfaces: [Rgba::rgb(0xebe6dc), Rgba::rgb(0xe3ddd1), Rgba::rgb(0xddd6c9)],
    fill: WHITE,
    glass: 0.22,
    glass_strong: 0.32,
    glass_deep: 0.62,
    sheet_tint: 0.36,
    bar_tint: 0.30,
    cell: [0.07, 0.11, 0.15], // ink at every state: a cell is darker than the frost it sits on, and each step darker still
    cell_rim: [0.12, 0.18],
    cell_rim_away: 0.04,
    cell_focus: [0.70, 0.58, 0.46], // less white under the pointer
    cell_focus_rim: [0.18, 0.24],
    shadow: Rgba::rgb(0x3c2d14),
    cell_shadow: 0.28,
    border: [0.10, 0.16],
    hairline: 0.07,
    highlight: [0.40, 0.60, 0.80],
    lift: [0.06, 0.10, 0.04],
    sidebar_colour: Rgba::rgb(0x1a1610),
    sidebar: 0.06,
    content_colour: WHITE,
    content: 0.12,
    accent_fill: 0.18,
    accent_border: 0.45,
    accent_wash: 0.07,
    text_secondary: 0.62,
    text_tertiary: 0.42,
    control_off: 0.14,
    track: 0.12,
    neutral: Rgba::rgb(0x6f6a62),
    danger: Rgba::rgb(0xbd3a26),
    danger_fill: 0.14,
    connected: Rgba::rgb(0x2f7fc0),
    ok: Rgba::rgb(0x5b8a3c),
    nav: [0.09, 0.05],
    pill: [0.06, 0.10],
    input_fill: 0.55,
    scroller: [0.28, 0.42],
    chart_history: 0.16,
    control_off_hover: 0.20,
    knob_off: 0.95,
    highlight_rim: 0.22,
};

/// A surface token for `base`: the table value as is when `base` is the
/// scheme's own base (bit-exact), otherwise the same per-channel offset from
/// the base, clamped.
fn shifted(base: Rgba, t: &Table, i: usize) -> Rgba {
    if base.0 == t.builtin_base.0 {
        return t.surfaces[i];
    }
    let s = t.surfaces[i].0;
    let b = t.builtin_base.0;
    let mut o = [0.0, 0.0, 0.0, 1.0];
    for c in 0..3 {
        o[c] = (base.0[c] + (s[c] - b[c])).clamp(0.0, 1.0);
    }
    Rgba(o)
}

/// Map seeds to every token. Total and pure; garbage seeds give garbage
/// colours, so run [`validate`] on anything user-supplied first.
pub fn derive(seeds: &Seeds) -> Palette {
    let t = match seeds.scheme() {
        Scheme::Dark => &DARK,
        Scheme::Light => &LIGHT,
    };
    let base = seeds.base;
    let ink = seeds.ink;
    let fill = t.fill;
    let frost = seeds.frost_tint;
    let acc = seeds.accent;
    // sRGB intent to the alpha iced needs, over the base (identity on dark).
    let ia = |src: Rgba, a: f32| if t.intent { linear_alpha(src, base, a) } else { a };

    let surface_1 = shifted(base, t, 1);
    let glass_deep = frost.with_alpha(t.glass_deep);
    let sidebar = t.sidebar_colour.with_alpha(ia(t.sidebar_colour, t.sidebar));
    let content = t.content_colour.with_alpha(ia(t.content_colour, t.content));
    let cell_focus = fill.with_alpha(ia(fill, t.cell_focus[0]));
    // A pointer brightens a dark cell and darkens a light one.
    let pointer = match seeds.scheme() {
        Scheme::Dark => fill,
        Scheme::Light => ink,
    };

    Palette {
        base,
        surface_0: shifted(base, t, 0),
        surface_1,
        surface_2: shifted(base, t, 2),
        glass: fill.with_alpha(ia(fill, t.glass)),
        glass_strong: fill.with_alpha(ia(fill, t.glass_strong)),
        glass_deep,
        glass_deep_backed: glass_deep.over_opaque(surface_1),
        sheet_tint: frost.with_alpha(premult_alpha(t.sheet_tint, t.intent)),
        bar_tint: frost.with_alpha(premult_alpha(t.bar_tint, t.intent)),
        cell: pointer.with_alpha(ia(pointer, t.cell[0])),
        cell_hover: pointer.with_alpha(ia(pointer, t.cell[1])),
        cell_press: pointer.with_alpha(ia(pointer, t.cell[2])),
        cell_rim: ink.with_alpha(ia(ink, t.cell_rim[0])),
        cell_rim_hover: ink.with_alpha(ia(ink, t.cell_rim[1])),
        cell_rim_away: ink.with_alpha(ia(ink, t.cell_rim_away)),
        cell_shadow: t.shadow.with_alpha(t.cell_shadow),
        cell_focus,
        cell_focus_hover: fill.with_alpha(ia(fill, t.cell_focus[1])),
        cell_focus_press: fill.with_alpha(ia(fill, t.cell_focus[2])),
        cell_focus_rim: ink.with_alpha(ia(ink, t.cell_focus_rim[0])),
        cell_focus_rim_hover: ink.with_alpha(ia(ink, t.cell_focus_rim[1])),
        menu_ground: frost,
        border: ink.with_alpha(ia(ink, t.border[0])),
        border_strong: ink.with_alpha(ia(ink, t.border[1])),
        hairline: ink.with_alpha(ia(ink, t.hairline)),
        highlight: fill.with_alpha(ia(fill, t.highlight[1])),
        highlight_soft: fill.with_alpha(ia(fill, t.highlight[0])),
        highlight_strong: fill.with_alpha(ia(fill, t.highlight[2])),
        lift: ink.with_alpha(ia(ink, t.lift[0])),
        lift_strong: ink.with_alpha(ia(ink, t.lift[1])),
        lift_soft: ink.with_alpha(ia(ink, t.lift[2])),
        veil: base.with_alpha(VEIL_STRENGTH),
        veil_glass: content.with_alpha(content.a() * VEIL_STRENGTH),
        sidebar,
        sidebar_backed: sidebar.over_opaque(base),
        content,
        content_backed: content.over_opaque(base),
        accent: acc,
        accent_text: seeds.accent_text.unwrap_or(acc),
        accent_fill: acc.with_alpha(ia(acc, t.accent_fill)),
        accent_border: acc.with_alpha(ia(acc, t.accent_border)),
        accent_wash: acc.with_alpha(ia(acc, t.accent_wash)),
        text: ink.with_alpha(1.0),
        text_secondary: ink.with_alpha(ia(ink, t.text_secondary)),
        text_tertiary: ink.with_alpha(ia(ink, t.text_tertiary)),
        neutral: t.neutral,
        eye_iris: cell_focus,
        control_off: ink.with_alpha(ia(ink, t.control_off)),
        track: ink.with_alpha(ia(ink, t.track)),
        danger: t.danger,
        danger_fill: t.danger.with_alpha(ia(t.danger, t.danger_fill)),
        connected: t.connected,
        ok: t.ok,
        scheme: seeds.scheme(),
        on_accent: match seeds.scheme() {
            Scheme::Dark => base,
            Scheme::Light => ink.with_alpha(1.0),
        },
        nav_active: ink.with_alpha(ia(ink, t.nav[0])),
        nav_hover: ink.with_alpha(ia(ink, t.nav[1])),
        pill: ink.with_alpha(ia(ink, t.pill[0])),
        pill_hover: ink.with_alpha(ia(ink, t.pill[1])),
        input_fill: fill.with_alpha(ia(fill, t.input_fill)),
        scroller: ink.with_alpha(ia(ink, t.scroller[0])),
        scroller_hover: ink.with_alpha(ia(ink, t.scroller[1])),
        chart_history: ink.with_alpha(ia(ink, t.chart_history)),
        control_off_hover: ink.with_alpha(ia(ink, t.control_off_hover)),
        knob_off: fill.with_alpha(ia(fill, t.knob_off)),
        highlight_rim: ink.with_alpha(ia(ink, t.highlight_rim)),
    }
}

/// Why a seed set was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ThemeError {
    /// A seed channel is NaN or infinite.
    NonFinite(&'static str),
    /// A seed channel is outside `0..=1`, or a seed that must be opaque is not.
    OutOfRange(&'static str),
    /// A contrast floor is missed.
    LowContrast {
        what: &'static str,
        ratio: f32,
        min: f32,
    },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ThemeError::NonFinite(s) => write!(f, "seed `{s}` has a non-finite channel"),
            ThemeError::OutOfRange(s) => write!(f, "seed `{s}` is out of range (channels 0..=1, opaque)"),
            ThemeError::LowContrast { what, ratio, min } => {
                write!(f, "{what} contrast {ratio:.2}:1 is below the {min}:1 floor")
            }
        }
    }
}

impl std::error::Error for ThemeError {}

/// Minimum contrast of text and accent text on the base.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;
/// Minimum contrast of secondary text on the base.
pub const MIN_SECONDARY_CONTRAST: f32 = 3.0;

/// Accept a seed set: every value finite and in range, and the derived text
/// (4.5:1), accent text (4.5:1) and secondary text (3:1) readable on the base.
pub fn validate(seeds: &Seeds) -> Result<(), ThemeError> {
    let named: [(&'static str, Rgba); 4] = [
        ("base", seeds.base),
        ("ink", seeds.ink),
        ("accent", seeds.accent),
        ("frost-tint", seeds.frost_tint),
    ];
    for (name, c) in named {
        if c.0.iter().any(|v| !v.is_finite()) {
            return Err(ThemeError::NonFinite(name));
        }
        if !c.finite_unit() || c.a() != 1.0 {
            return Err(ThemeError::OutOfRange(name));
        }
    }
    if let Some(c) = seeds.accent_text {
        if c.0.iter().any(|v| !v.is_finite()) {
            return Err(ThemeError::NonFinite("accent-text"));
        }
        if !c.finite_unit() || c.a() != 1.0 {
            return Err(ThemeError::OutOfRange("accent-text"));
        }
    }
    if !seeds.rim.is_finite() {
        return Err(ThemeError::NonFinite("rim"));
    }
    if !(0.0..=1.0).contains(&seeds.rim) {
        return Err(ThemeError::OutOfRange("rim"));
    }
    let p = derive(seeds);
    let checks = [
        ("text", p.text, MIN_TEXT_CONTRAST),
        ("accent text", p.accent_text, MIN_TEXT_CONTRAST),
        ("secondary text", p.text_secondary, MIN_SECONDARY_CONTRAST),
    ];
    for (what, fg, min) in checks {
        let ratio = contrast(fg, p.base);
        if ratio < min {
            return Err(ThemeError::LowContrast { what, ratio, min });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
