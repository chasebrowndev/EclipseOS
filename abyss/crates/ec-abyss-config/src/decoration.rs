// SPDX-License-Identifier: AGPL-3.0-only
//! `decoration` block: rounding, opacity, blur, shadow and glow (COMP-02 §9).

/// `decoration { ... }` (COMP-13 §1.1, COMP-02 §9). Every effect here is off by
/// default: the defaults below are the "no effect" values, so a config without a
/// `decoration` block renders exactly as it did before milestone 9b and keeps
/// direct scanout available. Opacity and `dim-inactive` are rendered today;
/// `shadow` and `glow` are one SDF pixel shader over the grown window rect and
/// `blur` a dual-Kawase chain behind translucent windows (COMP-02 §9); all draw
/// today.
/// `rounding` is drawn as a fragment-shader mask.
#[derive(Debug, Clone)]
pub struct Decoration {
    /// Corner radius in logical pixels; 0 disables. Matches ec-ui's
    /// `tokens::radius::CARD` (13px) so the compositor-drawn blur backdrop
    /// lines up with the client-drawn glass content on top of it.
    pub rounding: i32,
    /// Alpha applied to the focused window, 0.0..=1.0.
    pub active_opacity: f32,
    /// Alpha applied to every unfocused window, 0.0..=1.0.
    pub inactive_opacity: f32,
    /// Strength of the darkening overlay on unfocused windows, 0.0..=1.0.
    pub dim_inactive: f32,
    pub blur: Blur,
    pub shadow: Shadow,
    pub glow: Glow,
}

impl Default for Decoration {
    fn default() -> Self {
        Self {
            rounding: 9,
            active_opacity: 1.0,
            inactive_opacity: 1.0,
            dim_inactive: 0.0,
            blur: Blur::default(),
            shadow: Shadow::default(),
            glow: Glow::default(),
        }
    }
}

impl Decoration {
    /// Whether any per-window effect diverges from the plain path. When this is
    /// false the renderer keeps the single `space_render_elements` call, so
    /// damage tracking and direct scanout behave as they do with no config.
    pub fn any_window_effect(&self) -> bool {
        self.rounding > 0
            || self.active_opacity < 1.0
            || self.inactive_opacity < 1.0
            || self.dim_inactive > 0.0
            || self.glow.on()
    }
}

/// What is drawn behind a translucent surface (COMP-02 §9). Every mode but
/// `Off` runs the same dual-Kawase chain; `Frost` and `Glass` only swap the
/// program the blurred backdrop is finally drawn with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BlurMode {
    /// No backdrop pass at all.
    Off,
    /// The plain blurred backdrop, masked to the surface's corners.
    #[default]
    Blur,
    /// Blur with a tint mixed in and a fine grain over it.
    Frost,
    /// A saturated, smoked blur with a gentle bevel roll-off at the edge and
    /// a neutral hairline rim.
    Glass,
}

impl BlurMode {
    /// Every mode, in the order a GUI shows them.
    pub const NAMES: &'static [&'static str] = &["off", "blur", "frost", "glass"];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "off" => Self::Off,
            "blur" => Self::Blur,
            "frost" => Self::Frost,
            "glass" => Self::Glass,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Blur => "blur",
            Self::Frost => "frost",
            Self::Glass => "glass",
        }
    }
}

/// `blur { mode "blur"; size 8; passes 4; glass { … }; frost { … } }`.
/// Dual-Kawase (COMP-02 §9). The legacy `enabled #true|#false` still parses
/// (as `mode "blur"` / `mode "off"`); `ec-ctl config migrate` rewrites it.
#[derive(Debug, Clone)]
pub struct Blur {
    pub mode: BlurMode,
    pub size: i32,
    pub passes: i32,
    pub glass: GlassBlur,
    pub frost: FrostBlur,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            mode: BlurMode::Glass,
            size: 8,
            passes: 4,
            glass: GlassBlur::default(),
            frost: FrostBlur::default(),
        }
    }
}

/// `blur { glass { refraction 4; bevel 16; dispersion 0; rim 0.5 } }`.
#[derive(Debug, Clone, PartialEq)]
pub struct GlassBlur {
    /// Largest displacement of the blurred backdrop, logical px, at the
    /// outermost edge of the bevel; the flat centre is never bent.
    pub refraction: i32,
    /// Width of the edge band the backdrop rolls off in, logical px.
    pub bevel: i32,
    /// Chromatic spread in the bevel, 0.0..=1.0; off by default.
    pub dispersion: f32,
    /// Strength of the neutral two-lobe hairline rim, 0.0..=1.0.
    pub rim: f32,
}

impl Default for GlassBlur {
    fn default() -> Self {
        Self {
            refraction: 4,
            bevel: 16,
            dispersion: 0.0,
            rim: 0.5,
        }
    }
}

/// Default `frost.tint`: STYLE.md's `#1a1712` surface at 40%, straight alpha.
pub const FROST_TINT: [f32; 4] = [0.102, 0.091, 0.071, 0.4];

/// `blur { frost { tint "#1a171266" } }`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrostBlur {
    /// Mixed over the blurred backdrop; its alpha is the mix amount.
    pub tint: [f32; 4],
}

impl Default for FrostBlur {
    fn default() -> Self {
        Self { tint: FROST_TINT }
    }
}

/// `shadow { enabled #false; range 20 }`. An SDF ring outside the window, drawn
/// by a pixel shader (COMP-02 §9).
#[derive(Debug, Clone)]
pub struct Shadow {
    pub enabled: bool,
    pub range: i32,
}

impl Default for Shadow {
    fn default() -> Self {
        Self {
            enabled: true,
            range: 16,
        }
    }
}

/// `glow { enabled #false; active #true; inactive #true; strength 60 }`. The
/// shadow's SDF ring, tinted with the window's border colour (COMP-02 §9).
#[derive(Debug, Clone)]
pub struct Glow {
    pub enabled: bool,
    /// Glow on the focused window.
    pub active: bool,
    /// Glow on unfocused windows.
    pub inactive: bool,
    /// Peak intensity, percent 0..=100.
    pub strength: i32,
}

impl Default for Glow {
    fn default() -> Self {
        Self {
            enabled: false,
            active: true,
            inactive: true,
            strength: 60,
        }
    }
}

impl Glow {
    /// Whether any window can glow at all.
    pub fn on(&self) -> bool {
        self.enabled && self.strength > 0 && (self.active || self.inactive)
    }
}
