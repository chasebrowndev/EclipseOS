// SPDX-License-Identifier: AGPL-3.0-only
//! Font selection. Onest is the one face, vendored and embedded
//! (`assets/fonts`, [`BYTES`]): Latin and Cyrillic greetings in Light, the
//! version label in Regular. Onest has no CJK, so Japanese, Simplified Chinese
//! and Korean are asked of the Noto Sans CJK faces by fontconfig family name
//! (the ISO's `noto-fonts-cjk`); where one is missing the word falls back to
//! Onest and then to the text stack's per-glyph fallback. Greek, Arabic,
//! Hebrew, Devanagari and Thai are likewise not in Onest; none is among the ten
//! greetings.

use crate::timeline::Script;
use iced::font::{Family, Weight};
use iced::Font;
use std::collections::HashSet;
use std::process::Command;

/// The family name Onest's embedded files carry.
pub const ONEST: &str = "Onest";
pub const NOTO_JP: &str = "Noto Sans CJK JP";
pub const NOTO_SC: &str = "Noto Sans CJK SC";
pub const NOTO_KR: &str = "Noto Sans CJK KR";

/// The families asked of fontconfig, in the order the ISO must carry them
/// (Onest is embedded, so it is not one of them).
pub const REQUIRED_FAMILIES: [&str; 3] = [NOTO_JP, NOTO_SC, NOTO_KR];

/// The embedded faces, for `iced::Settings::fonts` and the offscreen renderer.
pub const BYTES: &[&[u8]] = &[
    include_bytes!("../../../../assets/fonts/Onest-Light.ttf"),
    include_bytes!("../../../../assets/fonts/Onest-Regular.ttf"),
];

/// The faces the screen draws with, resolved once.
#[derive(Debug, Clone, Copy)]
pub struct Faces {
    latin: Font,
    japanese: Font,
    chinese: Font,
    korean: Font,
    label: Font,
}

fn onest(weight: Weight) -> Font {
    Font {
        family: Family::Name(ONEST),
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
    /// Ask fontconfig about the CJK faces. Run once, at startup: it spawns `fc-list`.
    pub fn detect() -> Faces {
        Faces::from_installed(installed_families().as_ref())
    }

    /// Resolve against a known set of installed (lower-case) families.
    pub fn from_installed(installed: Option<&HashSet<String>>) -> Faces {
        let latin = onest(Weight::Light);
        let cjk = |name: &'static str| {
            if installed.is_none_or(|set| set.contains(&name.to_lowercase())) {
                Font {
                    family: Family::Name(name),
                    weight: Weight::Light,
                    ..Font::DEFAULT
                }
            } else {
                latin
            }
        };
        Faces {
            latin,
            japanese: cjk(NOTO_JP),
            chinese: cjk(NOTO_SC),
            korean: cjk(NOTO_KR),
            label: onest(Weight::Normal),
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
    fn latin_and_label_are_onest_whatever_is_installed() {
        for installed in [Some(set(&[])), Some(set(&REQUIRED_FAMILIES)), None] {
            let f = Faces::from_installed(installed.as_ref());
            assert_eq!(f.greeting(Script::Latin).family, Family::Name(ONEST));
            assert_eq!(f.label().family, Family::Name(ONEST));
            assert_eq!(f.greeting(Script::Latin).weight, Weight::Light);
            assert_eq!(f.label().weight, Weight::Normal);
        }
    }

    #[test]
    fn installed_cjk_faces_are_asked_for_by_name() {
        let f = Faces::from_installed(Some(&set(&REQUIRED_FAMILIES)));
        assert_eq!(f.greeting(Script::Japanese).family, Family::Name(NOTO_JP));
        assert_eq!(
            f.greeting(Script::ChineseSimplified).family,
            Family::Name(NOTO_SC)
        );
        assert_eq!(f.greeting(Script::Korean).family, Family::Name(NOTO_KR));
    }

    #[test]
    fn a_missing_cjk_face_falls_back_to_onest() {
        let f = Faces::from_installed(Some(&set(&[])));
        assert_eq!(f.greeting(Script::Japanese).family, Family::Name(ONEST));
        assert_eq!(f.greeting(Script::Korean).family, Family::Name(ONEST));
    }

    #[test]
    fn unknown_fontconfig_means_ask_by_name() {
        let f = Faces::from_installed(None);
        assert_eq!(f.greeting(Script::Korean).family, Family::Name(NOTO_KR));
    }

    #[test]
    fn the_embedded_faces_are_onest() {
        for ttf in BYTES {
            // `name` strings are UTF-16BE; "Onest" appears as such.
            let needle: Vec<u8> = "Onest".encode_utf16().flat_map(u16::to_be_bytes).collect();
            assert!(ttf.windows(needle.len()).any(|w| w == needle.as_slice()));
        }
    }
}
