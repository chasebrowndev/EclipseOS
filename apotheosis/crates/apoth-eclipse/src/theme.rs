// SPDX-License-Identifier: AGPL-3.0-only
//! Theme seeds to terminal colours (T-17 §2). The seed → colour mapping is
//! `ec_theme::terminal_scheme`; this module only parses the compositor's `get_theme` reply
//! and converts to 8-bit colours. It holds no mapping of its own.

use ec_theme::{Rgba, Seeds, ThemeError};
use serde_json::Value;

pub use ec_theme::{DARK_SEEDS, LIGHT_SEEDS};

/// An 8-bit sRGB colour, the terminal's colour type.
pub type Rgb8 = (u8, u8, u8);

/// The terminal colours of one theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermColors {
    /// ANSI 0-15.
    pub ansi: [Rgb8; 16],
    pub foreground: Rgb8,
    pub background: Rgb8,
    pub cursor: Rgb8,
    pub cursor_text: Rgb8,
    pub selection_foreground: Rgb8,
    pub selection_background: Rgb8,
    /// The seeds are a dark scheme (answers `CSI ? 996 n`, drives mode 2031).
    pub dark: bool,
}

fn rgb8(c: Rgba) -> Rgb8 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    (b(c.r()), b(c.g()), b(c.b()))
}

impl TermColors {
    /// The colours of `seeds`, unchecked (the built-in seeds pass `ec_theme::validate`).
    pub fn from_seeds(seeds: &Seeds) -> TermColors {
        let t = ec_theme::terminal_scheme(seeds);
        TermColors {
            ansi: t.ansi.map(rgb8),
            foreground: rgb8(t.foreground),
            background: rgb8(t.background),
            cursor: rgb8(t.cursor),
            cursor_text: rgb8(t.cursor_text),
            selection_foreground: rgb8(t.selection_foreground),
            selection_background: rgb8(t.selection_background),
            dark: seeds.scheme() == ec_theme::Scheme::Dark,
        }
    }

    /// The colours of `seeds` once `ec_theme::validate` accepts them (T-17 §2: a theme
    /// change re-validates contrast).
    pub fn checked(seeds: &Seeds) -> Result<TermColors, ThemeError> {
        ec_theme::validate(seeds)?;
        Ok(TermColors::from_seeds(seeds))
    }

    /// The stock pair (`DARK_SEEDS` / `LIGHT_SEEDS`): the colours off Abyss and the
    /// 16-colour default the settings preview shows.
    pub fn stock(dark: bool) -> TermColors {
        TermColors::from_seeds(if dark { &DARK_SEEDS } else { &LIGHT_SEEDS })
    }
}

/// `#rrggbb` or `#rrggbbaa`.
fn hex(v: &Value) -> Option<Rgba> {
    let s = v.as_str()?.strip_prefix('#')?;
    if !(s.len() == 6 || s.len() == 8) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(s, 16).ok()?;
    Some(if s.len() == 6 {
        Rgba::rgb(n)
    } else {
        Rgba::rgb(n >> 8).with_alpha((n & 0xff) as f32 / 255.0)
    })
}

/// The seeds of a `get_theme` reply or `theme` event (`{"seeds": {...}, ...}`), or `None`
/// when it does not parse as seeds. Unknown fields are ignored.
pub fn seeds_from_payload(v: &Value) -> Option<Seeds> {
    let s = v.get("seeds")?;
    let accent_text = match s.get("accent_text") {
        None | Some(Value::Null) => None,
        Some(c) => Some(hex(c)?),
    };
    let rim = s.get("rim")?.as_f64()? as f32;
    Some(Seeds {
        base: hex(s.get("base")?)?,
        ink: hex(s.get("ink")?)?,
        accent: hex(s.get("accent")?)?,
        accent_text,
        frost_tint: hex(s.get("frost_tint")?)?,
        rim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({"scheme": "dark", "setting": "dark", "theme": "dark", "seeds": {
            "base": "#0b0906", "ink": "#ffffff", "accent": "#f2c33c",
            "accent_text": "#f5cf5c", "frost_tint": "#17140fff", "rim": 0.2,
        }})
    }

    #[test]
    fn parses_the_compositor_payload() {
        let s = seeds_from_payload(&payload()).unwrap();
        assert_eq!(s.base, Rgba::rgb(0x0b0906));
        assert_eq!(s.accent_text, Some(Rgba::rgb(0xf5cf5c)));
        assert_eq!(s.frost_tint, Rgba::rgb(0x17140f));
        assert!((s.rim - 0.2).abs() < 1e-6);
        let c = TermColors::checked(&s).unwrap();
        assert_eq!(c, TermColors::stock(true));
        assert_eq!(c.background, (0x0b, 0x09, 0x06));
        assert!(c.dark);
    }

    #[test]
    fn null_accent_text_and_bad_shapes() {
        let mut p = payload();
        p["seeds"]["accent_text"] = Value::Null;
        assert_eq!(seeds_from_payload(&p).unwrap().accent_text, None);
        for (k, v) in [
            ("base", json!("0b0906")),
            ("base", json!("#0b09")),
            ("ink", json!("#gggggg")),
            ("accent", json!(7)),
            ("rim", json!("x")),
        ] {
            let mut p = payload();
            p["seeds"][k] = v;
            assert_eq!(seeds_from_payload(&p), None, "{k}");
        }
        assert_eq!(seeds_from_payload(&json!({})), None);
        assert_eq!(seeds_from_payload(&json!({"seeds": {}})), None);
    }

    #[test]
    fn light_payload_and_contrast_check() {
        let mut p = payload();
        p["seeds"]["base"] = json!("#f4f1ea");
        p["seeds"]["ink"] = json!("#1a1610");
        p["seeds"]["accent_text"] = json!("#856000");
        let c = TermColors::checked(&seeds_from_payload(&p).unwrap()).unwrap();
        assert!(!c.dark);
        assert_eq!(c, TermColors::stock(false));
        // Ink on its own base cannot be read: rejected.
        p["seeds"]["ink"] = json!("#f4f1ea");
        assert!(TermColors::checked(&seeds_from_payload(&p).unwrap()).is_err());
    }
}
