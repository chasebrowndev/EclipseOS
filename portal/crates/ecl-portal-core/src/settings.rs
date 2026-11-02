// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Settings` content: the `org.freedesktop.appearance`
//! namespace derived from Eclipse's tokens and an optional keys file.

use crate::val::Val;

pub const APPEARANCE_NS: &str = "org.freedesktop.appearance";

/// Gold accent `#f2c33c` (ec-ui token), as sRGB components in 0..=1.
pub const GOLD: (f64, f64, f64) = (242.0 / 255.0, 195.0 / 255.0, 60.0 / 255.0);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appearance {
    /// 0 no preference, 1 prefer dark, 2 prefer light. Eclipse is dark.
    pub color_scheme: u32,
    pub accent: (f64, f64, f64),
    /// 0 normal, 1 high.
    pub contrast: u32,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            color_scheme: 1,
            accent: GOLD,
            contrast: 0,
        }
    }
}

/// Parse the optional `~/.config/eclipse/appearance.conf`:
///
/// ```text
/// # comments and blank lines ignored
/// color-scheme = dark | light | default
/// accent-color = #rrggbb
/// contrast     = normal | high
/// ```
///
/// Unknown keys and malformed values are ignored (the default stays).
pub fn parse_appearance(text: &str) -> Appearance {
    let mut a = Appearance::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "color-scheme" => match v {
                "dark" => a.color_scheme = 1,
                "light" => a.color_scheme = 2,
                "default" => a.color_scheme = 0,
                _ => {}
            },
            "accent-color" => {
                if let Some(c) = parse_hex(v) {
                    a.accent = c;
                }
            }
            "contrast" => match v {
                "normal" => a.contrast = 0,
                "high" => a.contrast = 1,
                _ => {}
            },
            _ => {}
        }
    }
    a
}

pub fn parse_hex(s: &str) -> Option<(f64, f64, f64)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(|v| f64::from(v) / 255.0);
    Some((c(0).ok()?, c(2).ok()?, c(4).ok()?))
}

impl Appearance {
    fn entries(&self) -> [(&'static str, Val); 3] {
        [
            ("color-scheme", Val::U32(self.color_scheme)),
            (
                "accent-color",
                Val::Rgb(self.accent.0, self.accent.1, self.accent.2),
            ),
            ("contrast", Val::U32(self.contrast)),
        ]
    }

    pub fn get(&self, ns: &str, key: &str) -> Option<Val> {
        if ns != APPEARANCE_NS {
            return None;
        }
        self.entries()
            .into_iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

/// Whether `pattern` (an entry of ReadAll's `namespaces`) selects `ns`: empty
/// matches everything, a trailing `.*` matches the prefix, else exact.
pub fn namespace_matches(pattern: &str, ns: &str) -> bool {
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    match pattern.strip_suffix(".*") {
        Some(prefix) => ns.strip_prefix(prefix).is_some_and(|r| r.starts_with('.')),
        None => pattern == ns,
    }
}

/// ReadAll: `(namespace, [(key, value)])` for every matching namespace that
/// has content.
pub fn read_all(a: &Appearance, patterns: &[String]) -> Vec<(String, Vec<(String, Val)>)> {
    let wanted =
        patterns.is_empty() || patterns.iter().any(|p| namespace_matches(p, APPEARANCE_NS));
    if !wanted {
        return Vec::new();
    }
    vec![(
        APPEARANCE_NS.to_owned(),
        a.entries()
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )]
}

/// Keys whose value differs, as `(namespace, key, new value)` for
/// `SettingChanged`.
pub fn changes(old: &Appearance, new: &Appearance) -> Vec<(String, String, Val)> {
    old.entries()
        .into_iter()
        .zip(new.entries())
        .filter(|((_, a), (_, b))| a != b)
        .map(|(_, (k, v))| (APPEARANCE_NS.to_owned(), k.to_owned(), v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_dark_gold() {
        let a = Appearance::default();
        assert_eq!(a.color_scheme, 1);
        assert_eq!(a.get(APPEARANCE_NS, "color-scheme"), Some(Val::U32(1)));
        assert_eq!(parse_hex("#f2c33c"), Some(GOLD));
        assert_eq!(a.get("other", "color-scheme"), None);
        assert_eq!(a.get(APPEARANCE_NS, "nope"), None);
    }

    #[test]
    fn file_parsing() {
        let a = parse_appearance(
            "# x\ncolor-scheme = light\naccent-color=#000000\ncontrast=high\nbogus=1\n",
        );
        assert_eq!(a.color_scheme, 2);
        assert_eq!(a.accent, (0.0, 0.0, 0.0));
        assert_eq!(a.contrast, 1);
        let b = parse_appearance("color-scheme = purple\naccent-color=red\n");
        assert_eq!(b, Appearance::default());
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#12345g"), None);
    }

    #[test]
    fn namespaces() {
        assert!(namespace_matches("", APPEARANCE_NS));
        assert!(namespace_matches("org.freedesktop.*", APPEARANCE_NS));
        assert!(namespace_matches(APPEARANCE_NS, APPEARANCE_NS));
        assert!(!namespace_matches("org.gnome.*", APPEARANCE_NS));
        assert!(!namespace_matches(
            "org.freedesktop.appearanc",
            APPEARANCE_NS
        ));
        assert!(!namespace_matches(
            "org.freedesktop.appearance.*",
            APPEARANCE_NS
        ));
    }

    #[test]
    fn read_all_filters() {
        let a = Appearance::default();
        assert_eq!(read_all(&a, &[]).len(), 1);
        assert_eq!(read_all(&a, &["org.freedesktop.*".into()])[0].1.len(), 3);
        assert!(read_all(&a, &["org.gnome.desktop.interface".into()]).is_empty());
    }

    #[test]
    fn change_diff() {
        let a = Appearance::default();
        assert!(changes(&a, &a).is_empty());
        let b = Appearance {
            color_scheme: 2,
            ..a
        };
        let c = changes(&a, &b);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1, "color-scheme");
        assert_eq!(c[0].2, Val::U32(2));
    }
}
