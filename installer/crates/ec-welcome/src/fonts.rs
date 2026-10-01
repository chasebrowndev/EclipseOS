// SPDX-License-Identifier: AGPL-3.0-only
//! Font selection by fontconfig family name, with a serif fallback.
//!
//! Nothing here is vendored (the CJK faces are 10-25 MB each; see
//! `reference/fonts.md` and the README). The ISO ships the families; this asks
//! fontconfig once whether each is installed and, if it is not, degrades to the
//! generic serif rather than to iced's default sans, which is what an unknown
//! `Family::Name` would otherwise silently become. CJK glyphs missing from the
//! chosen face are found by the text stack's own per-glyph fallback.

use crate::timeline::Script;
use iced::font::{Family, Weight};
use iced::Font;
use std::collections::HashSet;
use std::process::Command;

pub const CORMORANT: &str = "Cormorant Garamond";
pub const LORA: &str = "Lora";
pub const NOTO_JP: &str = "Noto Serif JP";
pub const NOTO_SC: &str = "Noto Serif SC";
pub const NOTO_KR: &str = "Noto Serif KR";

/// The five families the screen asks for, in the order the ISO must carry them.
pub const REQUIRED_FAMILIES: [&str; 5] = [CORMORANT, LORA, NOTO_JP, NOTO_SC, NOTO_KR];

/// The faces the screen draws with, resolved once.
#[derive(Debug, Clone, Copy)]
pub struct Faces {
    latin: Font,
    japanese: Font,
    chinese: Font,
    korean: Font,
    label: Font,
}

/// Named serifs to try when a wanted family is missing. The generic
/// `Family::Serif` is not enough: the text stack can resolve it to a sans.
const SERIF_FALLBACKS: [&str; 5] = [
    "Noto Serif",
    "DejaVu Serif",
    "Liberation Serif",
    "Times New Roman",
    "FreeSerif",
];

/// One face: `name` if installed, else the first installed named serif, else
/// the generic serif.
fn face(installed: Option<&HashSet<String>>, name: &'static str, weight: Weight) -> Font {
    let has = |n: &str| installed.is_none_or(|set| set.contains(&n.to_lowercase()));
    let family = if has(name) {
        Family::Name(name)
    } else {
        SERIF_FALLBACKS
            .into_iter()
            .find(|n| has(n))
            .map_or(Family::Serif, Family::Name)
    };
    Font {
        family,
        weight,
        ..Font::DEFAULT
    }
}

/// Every installed family, lower-cased, per `fc-list`. `None` when fontconfig's
/// tools are not there to ask: then every face is requested by name and the
/// text stack does whatever it would have done anyway.
fn installed_families() -> Option<HashSet<String>> {
    let out = Command::new("fc-list")
        .args([":", "family"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .flat_map(|l| l.split(','))
            .map(|n| n.trim().to_lowercase())
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

impl Faces {
    /// Ask fontconfig. Run once, at startup: it spawns `fc-list`.
    pub fn detect() -> Faces {
        Faces::from_installed(installed_families().as_ref())
    }

    /// Resolve against a known set of installed (lower-case) families.
    pub fn from_installed(installed: Option<&HashSet<String>>) -> Faces {
        let light = Weight::Light;
        // The CJK words list Cormorant after their own Noto family, so a
        // missing Noto falls to Cormorant and then to the glyph fallback.
        let latin = face(installed, CORMORANT, light);
        let cjk = |name: &'static str| {
            if installed.is_none_or(|set| set.contains(&name.to_lowercase())) {
                face(installed, name, light)
            } else {
                latin
            }
        };
        Faces {
            latin,
            japanese: cjk(NOTO_JP),
            chinese: cjk(NOTO_SC),
            korean: cjk(NOTO_KR),
            label: face(installed, LORA, Weight::Normal),
        }
    }

    pub fn greeting(&self, script: Script) -> Font {
        match script {
            Script::Latin => self.latin,
            Script::Japanese => self.japanese,
            Script::ChineseSimplified => self.chinese,
            Script::Korean => self.korean,
        }
    }

    /// The version label.
    pub fn label(&self) -> Font {
        self.label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    #[test]
    fn installed_families_are_asked_for_by_name() {
        let all = set(&REQUIRED_FAMILIES);
        let f = Faces::from_installed(Some(&all));
        assert_eq!(f.greeting(Script::Latin).family, Family::Name(CORMORANT));
        assert_eq!(f.greeting(Script::Japanese).family, Family::Name(NOTO_JP));
        assert_eq!(
            f.greeting(Script::ChineseSimplified).family,
            Family::Name(NOTO_SC)
        );
        assert_eq!(f.greeting(Script::Korean).family, Family::Name(NOTO_KR));
        assert_eq!(f.label().family, Family::Name(LORA));
        assert_eq!(f.greeting(Script::Latin).weight, Weight::Light);
        assert_eq!(f.label().weight, Weight::Normal);
    }

    #[test]
    fn missing_families_degrade_to_serif_not_to_default_sans() {
        let f = Faces::from_installed(Some(&set(&[])));
        assert_eq!(f.greeting(Script::Latin).family, Family::Serif);
        assert_eq!(f.greeting(Script::Korean).family, Family::Serif);
        assert_eq!(f.label().family, Family::Serif);
    }

    #[test]
    fn a_named_serif_beats_the_generic_one() {
        let f = Faces::from_installed(Some(&set(&["DejaVu Serif", "Liberation Serif"])));
        assert_eq!(f.greeting(Script::Latin).family, Family::Name("DejaVu Serif"));
        assert_eq!(f.label().family, Family::Name("DejaVu Serif"));
    }

    #[test]
    fn a_missing_cjk_face_falls_back_to_cormorant_first() {
        let f = Faces::from_installed(Some(&set(&[CORMORANT])));
        assert_eq!(f.greeting(Script::Japanese).family, Family::Name(CORMORANT));
    }

    #[test]
    fn unknown_fontconfig_means_ask_by_name() {
        let f = Faces::from_installed(None);
        assert_eq!(f.greeting(Script::Korean).family, Family::Name(NOTO_KR));
    }
}
