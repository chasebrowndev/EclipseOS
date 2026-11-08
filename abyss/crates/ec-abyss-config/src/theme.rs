// SPDX-License-Identifier: AGPL-3.0-only
//! Themes (ADR 0084): `general { color-scheme … }` and the `theme "<name>" { … }`
//! blocks. A block resolves to a validated [`Seeds`] at parse time, so an
//! inherit cycle cannot exist (a block may only inherit what is already
//! defined) and a bad theme is refused whole, leaving the last good config.

use super::*;
use ec_theme::{Rgba as ThemeRgba, Scheme, Seeds, DARK_SEEDS, LIGHT_SEEDS};

/// `general.color-scheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSetting {
    Dark,
    Light,
    /// Follow the `light-from` / `dark-from` schedule, local time.
    Auto,
}

impl ColorSetting {
    pub fn name(self) -> &'static str {
        match self {
            ColorSetting::Dark => "dark",
            ColorSetting::Light => "light",
            ColorSetting::Auto => "auto",
        }
    }
}

pub const COLOR_SCHEMES: &[&str] = &["dark", "light", "auto"];
pub const DEFAULT_LIGHT_FROM: u16 = 7 * 60;
pub const DEFAULT_DARK_FROM: u16 = 19 * 60;

/// A user `theme` block, already resolved against its `inherit`.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeDef {
    pub name: String,
    pub seeds: Seeds,
}

/// The theme in force right now: the effective scheme, the theme name and its
/// seeds. Runtime state: the compositor recomputes it on reload and when the
/// `auto` schedule crosses a boundary, then hands it to
/// [`Config::apply_appearance`]. The default is the built-in dark theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Appearance {
    pub scheme: Scheme,
    pub name: String,
    pub seeds: Seeds,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            scheme: Scheme::Dark,
            name: "dark".into(),
            seeds: DARK_SEEDS,
        }
    }
}

impl Appearance {
    /// The built-in dark seeds: the compositor then draws exactly what it drew
    /// before themes existed.
    pub fn is_builtin_dark(&self) -> bool {
        self.seeds == DARK_SEEDS
    }

    /// How light the compositor's glass is drawn: 0.0 is the original dark
    /// look bit for bit, 1.0 the bright frosted light twin (ADR 0084).
    pub fn glass_light(&self) -> f32 {
        match self.scheme {
            Scheme::Dark => 0.0,
            Scheme::Light => 1.0,
        }
    }

    /// Window-shadow colour, straight RGB: black on dark, STYLE.md's warm
    /// brown on light (no black halo on a light window).
    pub fn shadow_rgb(&self) -> [f32; 3] {
        match self.scheme {
            Scheme::Dark => [0.0, 0.0, 0.0],
            Scheme::Light => [60.0 / 255.0, 45.0 / 255.0, 20.0 / 255.0],
        }
    }
}

/// What the compositor draws from a theme: the values behind
/// `blur { frost { tint } }`, `blur { glass { rim } }` and `annotations { }`.
pub(crate) struct Look {
    pub frost_tint: [f32; 4],
    pub rim: f32,
    pub annotations: AnnotationColors,
}

/// The seed `rim` is on the scale where the built-in dark theme is 0.20, and
/// the compositor's `glass.rim` default is 0.5.
const RIM_SCALE: f32 = 2.5;

pub(crate) fn look(a: &Appearance) -> Look {
    if a.is_builtin_dark() {
        return Look {
            frost_tint: FROST_TINT,
            rim: GlassBlur::default().rim,
            annotations: AnnotationColors::DEFAULT,
        };
    }
    let p = ec_theme::derive(&a.seeds);
    let mix = match a.scheme {
        Scheme::Dark => FROST_TINT[3],
        Scheme::Light => 0.35,
    };
    let f = a.seeds.frost_tint.0;
    let base = p.base.0;
    let mut annotations = AnnotationColors {
        accent: p.accent.0,
        accent_text: p.accent_text.0,
        text: p.text.0,
        panel_tint: p.menu_ground.0,
        panel_rim: p.border.0,
        hairline: p.hairline.0,
        region_outline: p.border_strong.0,
        leader: p.border_strong.0,
        selection_dim: [base[0], base[1], base[2], 0.40],
        danger: p.danger.0,
    };
    annotations.panel_tint[3] = annotations.panel_tint[3].max(PANEL_TINT_MIN_ALPHA);
    Look {
        frost_tint: [f[0], f[1], f[2], mix],
        rim: (a.seeds.rim * RIM_SCALE).min(1.0),
        annotations,
    }
}

/// `[r, g, b, a]` as `#rrggbb` (alpha dropped).
pub fn hex_rgb(c: [f32; 4]) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// `"HH:MM"` as minutes since midnight.
pub fn parse_hhmm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    if h.len() != 2 || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Minutes since midnight as `"HH:MM"`.
pub fn fmt_hhmm(m: u16) -> String {
    format!("{:02}:{:02}", m / 60 % 24, m % 60)
}

/// A `general.theme-dark` / `theme-light` value waiting for the end of the
/// load, when every file's `theme` blocks are known.
#[derive(Debug, Clone)]
pub(crate) struct PendingThemeRef {
    pub(crate) light: bool,
    pub(crate) name: String,
    pub(crate) offset: usize,
    pub(crate) len: usize,
    pub(crate) src: Source,
    pub(crate) text: String,
}

fn to_seeds_color(c: [f32; 4]) -> ThemeRgba {
    ThemeRgba(c)
}

impl Config {
    /// Seeds of a built-in or user theme.
    pub fn theme_seeds(&self, name: &str) -> Option<Seeds> {
        match name {
            "dark" => Some(DARK_SEEDS),
            "light" => Some(LIGHT_SEEDS),
            _ => self.themes.iter().find(|t| t.name == name).map(|t| t.seeds),
        }
    }

    /// The scheme `general.color-scheme` yields at `minute` (minutes since
    /// local midnight). Only `auto` looks at the time.
    pub fn scheme_at(&self, minute: u16) -> Scheme {
        let g = &self.general;
        match g.color_scheme {
            ColorSetting::Dark => Scheme::Dark,
            ColorSetting::Light => Scheme::Light,
            ColorSetting::Auto => {
                let light = if g.light_from <= g.dark_from {
                    (g.light_from..g.dark_from).contains(&minute)
                } else {
                    minute >= g.light_from || minute < g.dark_from
                };
                if light {
                    Scheme::Light
                } else {
                    Scheme::Dark
                }
            }
        }
    }

    /// The appearance for `scheme`: the theme `theme-dark` / `theme-light`
    /// names. An unresolvable name cannot survive validation; if one reaches
    /// here anyway it falls back to the built-in, never to a half theme.
    pub fn appearance_for(&self, scheme: Scheme) -> Appearance {
        let mut a = self.themed_appearance(scheme);
        // The shared accent replaces the theme's seed; its text variant is
        // re-derived, so the override clears the contrast rule by construction.
        if let Some(c) = self.general.accent {
            let seeds = a.seeds.with_accent(to_seeds_color(c));
            if ec_theme::validate(&seeds).is_ok() {
                a.seeds = seeds;
            }
        }
        a
    }

    fn themed_appearance(&self, scheme: Scheme) -> Appearance {
        let (name, builtin) = match scheme {
            Scheme::Dark => (&self.general.theme_dark, DARK_SEEDS),
            Scheme::Light => (&self.general.theme_light, LIGHT_SEEDS),
        };
        match self.theme_seeds(name) {
            Some(seeds) if seeds.scheme() == scheme => Appearance {
                scheme,
                name: name.clone(),
                seeds,
            },
            _ => Appearance {
                scheme,
                name: if scheme == Scheme::Dark { "dark" } else { "light" }.into(),
                seeds: builtin,
            },
        }
    }

    /// Make `a` the theme in force and fill every themed value `abyss.kdl` did
    /// not set itself: an explicit `frost { tint }`, `glass { rim }` or
    /// `annotations { … }` key always wins over the theme. Idempotent.
    pub fn apply_appearance(&mut self, a: Appearance) {
        let l = look(&a);
        if !self.look_set.frost_tint {
            self.decoration.blur.frost.tint = l.frost_tint;
        }
        if !self.look_set.glass_rim {
            self.decoration.blur.glass.rim = l.rim;
        }
        let set = self.look_set.annotations;
        let slots = ANNOTATION_KEYS
            .iter()
            .enumerate()
            .filter(|(i, _)| set & (1 << i) == 0);
        for (_, key) in slots {
            if let (Some(dst), Some(src)) = (self.annotations.slot_mut(key), l.annotations.slot(key)) {
                *dst = *src;
            }
        }
        self.appearance = a;
    }

    pub(crate) fn apply_theme_block(&mut self, node: &KdlNode) {
        let Some(name) = arg(node).and_then(KdlValue::as_string).map(str::to_owned) else {
            return self.reject(node, "theme expects a name: theme \"<name>\" { … }");
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return self.reject(node, "theme name must be non-empty and have no spaces");
        }
        if name == "dark" || name == "light" {
            return self.reject(
                node,
                format!("theme {name:?} is built in and cannot be redefined"),
            );
        }
        if self.themes.iter().any(|t| t.name == name) {
            return self.reject(node, format!("theme {name:?} is defined twice"));
        }
        let mut inherit: Option<(String, &KdlNode)> = None;
        let mut sets: Vec<(&str, &KdlNode)> = Vec::new();
        for n in node.children().map(|c| c.nodes()).unwrap_or_default() {
            match n.name().value() {
                "inherit" => match arg(n).and_then(KdlValue::as_string) {
                    Some(s) => inherit = Some((s.to_owned(), n)),
                    None => return self.reject(n, "theme inherit expects a theme name"),
                },
                k @ ("base" | "ink" | "accent" | "accent-text" | "frost-tint" | "rim") => sets.push((k, n)),
                _ => return self.unknown_key(n, "theme", "theme key"),
            }
        }
        let mut seeds = match &inherit {
            None => DARK_SEEDS,
            Some((from, n)) => {
                if *from == name {
                    return self.reject(n, format!("theme {name:?} inherits itself (inherit cycle)"));
                }
                match self.theme_seeds(from) {
                    Some(s) => s,
                    None => {
                        let msg = format!(
                            "theme {name:?} inherits unknown theme {from:?}; a theme must be defined before one inherits it"
                        );
                        return self.reject(n, msg);
                    }
                }
            }
        };
        for (key, n) in sets {
            if key == "rim" {
                match arg(n).and_then(as_f64) {
                    Some(v) if (0.0..=1.0).contains(&v) => seeds.rim = v as f32,
                    _ => return self.reject(n, "theme rim must be 0.0..=1.0"),
                }
                continue;
            }
            let Some(c) = arg(n).and_then(KdlValue::as_string).and_then(parse_color) else {
                return self.reject(n, format!("bad color for \"theme {key}\"; expected \"#rrggbb\""));
            };
            let c = to_seeds_color(c);
            match key {
                "base" => seeds.base = c,
                "ink" => seeds.ink = c,
                "accent" => seeds.accent = c,
                "accent-text" => seeds.accent_text = Some(c),
                _ => seeds.frost_tint = c,
            }
        }
        if let Err(e) = ec_theme::validate(&seeds) {
            return self.reject(node, format!("theme {name:?} refused: {e}"));
        }
        self.themes.push(ThemeDef { name, seeds });
    }

    /// `general { theme-dark "x" }` and `theme-light`.
    pub(crate) fn apply_theme_ref(&mut self, n: &KdlNode, light: bool) {
        let key = if light { "theme-light" } else { "theme-dark" };
        let Some(v) = arg(n).and_then(KdlValue::as_string) else {
            return self.reject(n, format!("{key} expects a theme name"));
        };
        if light {
            self.general.theme_light = v.to_owned();
        } else {
            self.general.theme_dark = v.to_owned();
        }
        self.pending_themes.retain(|p| p.light != light);
        if let Some((src, text)) = self.cur.clone() {
            let (offset, len) = match arg_entry(n) {
                Some(e) => (e.span().offset(), e.span().len()),
                None => (n.span().offset(), n.name().value().len()),
            };
            self.pending_themes.push(PendingThemeRef {
                light,
                name: v.to_owned(),
                offset,
                len,
                src,
                text,
            });
        }
    }

    /// `general { light-from "07:00" }` and `dark-from`.
    pub(crate) fn apply_schedule(&mut self, n: &KdlNode, light: bool) {
        let key = if light { "light-from" } else { "dark-from" };
        match arg(n).and_then(KdlValue::as_string).and_then(parse_hhmm) {
            Some(m) if light => self.general.light_from = m,
            Some(m) => self.general.dark_from = m,
            None => self.reject(
                n,
                format!("{key} expects a local time \"HH:MM\" (00:00 to 23:59)"),
            ),
        }
    }

    /// End of a load: every `theme-dark` / `theme-light` must name a known
    /// theme of the matching brightness, and the schedule must have two
    /// distinct times.
    pub(crate) fn finish_themes(&mut self) {
        let keep_cur = self.cur.take();
        for p in std::mem::take(&mut self.pending_themes) {
            let want = if p.light { Scheme::Light } else { Scheme::Dark };
            let key = if p.light { "theme-light" } else { "theme-dark" };
            let problem = match self.theme_seeds(&p.name) {
                None => Some(format!("{key}: unknown theme {:?}", p.name)),
                Some(s) if s.scheme() != want => Some(format!(
                    "{key}: theme {:?} is {} but {key} needs a {} theme",
                    p.name,
                    if want == Scheme::Dark { "light" } else { "dark" },
                    if want == Scheme::Dark { "dark" } else { "light" },
                )),
                Some(_) => None,
            };
            if let Some(msg) = problem {
                self.cur = Some((p.src, p.text));
                self.reject_at(p.offset, p.len, msg);
                self.cur = None;
                let default = if p.light { "light" } else { "dark" };
                if p.light {
                    self.general.theme_light = default.into();
                } else {
                    self.general.theme_dark = default.into();
                }
            }
        }
        self.cur = keep_cur;
        if self.general.light_from == self.general.dark_from {
            self.reject_at(0, 0, "light-from and dark-from must be different times");
            self.general.light_from = DEFAULT_LIGHT_FROM;
            self.general.dark_from = DEFAULT_DARK_FROM;
        }
    }
}

/// Names of the `annotations { }` keys, in bit order of
/// `ExplicitLook::annotations`.
pub(crate) const ANNOTATION_KEYS: [&str; 10] = [
    "accent",
    "accent-text",
    "text",
    "panel-tint",
    "panel-rim",
    "hairline",
    "region-outline",
    "leader",
    "selection-dim",
    "danger",
];

/// Which themed values `abyss.kdl` set itself; those the theme never touches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ExplicitLook {
    pub(crate) frost_tint: bool,
    pub(crate) glass_rim: bool,
    pub(crate) annotations: u16,
}

fn arg_entry(node: &KdlNode) -> Option<&KdlEntry> {
    node.entries().iter().find(|e| e.name().is_none())
}
