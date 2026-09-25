// SPDX-License-Identifier: AGPL-3.0-only
//! What the wizard offers: the lists behind steps 1, 2 and 6.
//!
//! Lists come from the machine where they exist (`xkeyboard-config`'s
//! `evdev.lst`, tzdata's `zone1970.tab`) and fall back to a short built-in one
//! so the wizard never opens onto an empty step. Everything here is only ever a
//! *suggestion*: the helper validates locale and timezone against its own lists
//! (D-07 §6), so a wrong entry here is a refusal there, never an injection.

use eclipse_setup_plan::Profile;

/// One language the wizard can install for: locale, its own name for itself,
/// the English name, and the keyboard layout people who speak it usually have.
pub struct Language {
    pub locale: &'static str,
    pub native: &'static str,
    pub english: &'static str,
    pub layout: &'static str,
}

const fn lang(
    locale: &'static str,
    native: &'static str,
    english: &'static str,
    layout: &'static str,
) -> Language {
    Language {
        locale,
        native,
        english,
        layout,
    }
}

/// Locales that exist in glibc's `SUPPORTED` list. Ordered by how likely a
/// first-time installer is to want them, English first: the welcome screen
/// greets in English longest.
pub const LANGUAGES: &[Language] = &[
    lang("en_US.UTF-8", "English", "United States", "us"),
    lang("en_GB.UTF-8", "English", "United Kingdom", "gb"),
    lang("de_DE.UTF-8", "Deutsch", "German", "de"),
    lang("fr_FR.UTF-8", "Français", "French", "fr"),
    lang("es_ES.UTF-8", "Español", "Spanish", "es"),
    lang("it_IT.UTF-8", "Italiano", "Italian", "it"),
    lang("pt_BR.UTF-8", "Português", "Portuguese (Brazil)", "br"),
    lang("nl_NL.UTF-8", "Nederlands", "Dutch", "us"),
    lang("sv_SE.UTF-8", "Svenska", "Swedish", "se"),
    lang("nb_NO.UTF-8", "Norsk bokmål", "Norwegian", "no"),
    lang("da_DK.UTF-8", "Dansk", "Danish", "dk"),
    lang("fi_FI.UTF-8", "Suomi", "Finnish", "fi"),
    lang("pl_PL.UTF-8", "Polski", "Polish", "pl"),
    lang("cs_CZ.UTF-8", "Čeština", "Czech", "cz"),
    lang("ru_RU.UTF-8", "Русский", "Russian", "ru"),
    lang("uk_UA.UTF-8", "Українська", "Ukrainian", "ua"),
    lang("tr_TR.UTF-8", "Türkçe", "Turkish", "tr"),
    lang("el_GR.UTF-8", "Ελληνικά", "Greek", "gr"),
    lang("ja_JP.UTF-8", "日本語", "Japanese", "jp"),
    lang("zh_CN.UTF-8", "简体中文", "Chinese (Simplified)", "cn"),
    lang("ko_KR.UTF-8", "한국어", "Korean", "kr"),
];

/// A keyboard layout and the variants it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub code: String,
    pub name: String,
    pub variants: Vec<Variant>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub code: String,
    pub name: String,
}

/// Where xkeyboard-config keeps its human-readable lists.
pub const EVDEV_LST: &str = "/usr/share/X11/xkb/rules/evdev.lst";

/// Parse the `! layout` and `! variant` sections of `evdev.lst`.
///
/// Layout lines read `  us              English (US)`; variant lines read
/// `  intl            us: English (US, intl., with dead keys)`. A variant whose
/// layout is not in the list is dropped.
pub fn parse_evdev(text: &str) -> Vec<Layout> {
    enum Section {
        Other,
        Layout,
        Variant,
    }
    let mut section = Section::Other;
    let mut layouts: Vec<Layout> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix('!') {
            section = match head.trim() {
                "layout" => Section::Layout,
                "variant" => Section::Variant,
                _ => Section::Other,
            };
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((code, rest)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let rest = rest.trim();
        match section {
            Section::Layout => layouts.push(Layout {
                code: code.to_owned(),
                name: rest.to_owned(),
                variants: Vec::new(),
            }),
            Section::Variant => {
                let Some((layout, name)) = rest.split_once(':') else {
                    continue;
                };
                if let Some(l) = layouts.iter_mut().find(|l| l.code == layout.trim()) {
                    l.variants.push(Variant {
                        code: code.to_owned(),
                        name: name.trim().to_owned(),
                    });
                }
            }
            Section::Other => {}
        }
    }
    layouts
}

/// The short list used when `evdev.lst` is missing, and by tests.
pub fn builtin_layouts() -> Vec<Layout> {
    let l = |code: &str, name: &str, variants: &[(&str, &str)]| Layout {
        code: code.into(),
        name: name.into(),
        variants: variants
            .iter()
            .map(|(c, n)| Variant {
                code: (*c).into(),
                name: (*n).into(),
            })
            .collect(),
    };
    vec![
        l(
            "us",
            "English (US)",
            &[
                ("intl", "English (US, intl., with dead keys)"),
                ("dvorak", "English (Dvorak)"),
                ("colemak", "English (Colemak)"),
            ],
        ),
        l(
            "gb",
            "English (UK)",
            &[("extd", "English (UK, extended, Windows)")],
        ),
        l("de", "German", &[("nodeadkeys", "German (no dead keys)")]),
        l("fr", "French", &[("oss", "French (alt.)")]),
        l("es", "Spanish", &[]),
        l("it", "Italian", &[]),
        l("br", "Portuguese (Brazil)", &[]),
        l("se", "Swedish", &[]),
        l("no", "Norwegian", &[]),
        l("dk", "Danish", &[]),
        l("fi", "Finnish", &[]),
        l("pl", "Polish", &[]),
        l("cz", "Czech", &[]),
        l("ru", "Russian", &[]),
        l("ua", "Ukrainian", &[]),
        l("tr", "Turkish", &[]),
        l("gr", "Greek", &[]),
        l("jp", "Japanese", &[]),
        l("cn", "Chinese", &[]),
        l("kr", "Korean", &[]),
    ]
}

/// Read the machine's layout list, or the built-in one.
pub fn load_layouts() -> Vec<Layout> {
    match std::fs::read_to_string(EVDEV_LST).map(|t| parse_evdev(&t)) {
        Ok(l) if !l.is_empty() => l,
        _ => builtin_layouts(),
    }
}

/// Where NetworkManager keeps saved connections; often unreadable without
/// root, in which case there is simply nothing to report.
pub const NM_CONNECTIONS: &str = "/etc/NetworkManager/system-connections";

/// Whether a saved connection file names a Wi-Fi network.
pub fn is_wifi_profile(text: &str) -> bool {
    text.lines()
        .any(|l| matches!(l.trim(), "type=wifi" | "type=802-11-wireless"))
}

/// A saved Wi-Fi connection on the live system (bounded, no symlinks).
pub fn has_saved_wifi() -> bool {
    let Ok(rd) = std::fs::read_dir(NM_CONNECTIONS) else {
        return false;
    };
    rd.flatten().take(64).any(|e| {
        e.file_type().is_ok_and(|t| t.is_file())
            && std::fs::read_to_string(e.path()).is_ok_and(|t| is_wifi_profile(&t))
    })
}

/// Where kbd keeps its console keymaps.
pub const KEYMAP_ROOT: &str = "/usr/share/kbd/keymaps";
/// Bounds on the keymap walk: depth below the root and names collected.
const KEYMAP_DEPTH: usize = 4;
const KEYMAP_MAX: usize = 1024;

/// Console keymaps to offer when the machine has none to list.
pub fn builtin_keymaps() -> Vec<String> {
    [
        "us",
        "uk",
        "de-latin1",
        "fr",
        "es",
        "it",
        "pt-latin1",
        "ru",
        "jp106",
        "br-abnt2",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Every `<name>.map.gz` under `root`, sorted and deduplicated by name. The walk
/// is bounded in depth and count and never follows a symlink.
pub fn scan_keymaps(root: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if out.len() >= KEYMAP_MAX {
                return;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if depth < KEYMAP_DEPTH {
                    walk(&e.path(), depth + 1, out);
                }
            } else if let Some(name) = e.file_name().to_str().and_then(|n| n.strip_suffix(".map.gz")) {
                if eclipse_setup_plan::valid_keymap(name) {
                    out.push(name.to_owned());
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

/// The machine's keymaps, or the built-in short list.
pub fn load_keymaps() -> Vec<String> {
    let k = scan_keymaps(std::path::Path::new(KEYMAP_ROOT));
    if k.is_empty() {
        builtin_keymaps()
    } else {
        k
    }
}

/// The keymaps that suit an xkb layout code: the code itself, or one that
/// starts with it (`de` and `de-latin1`), in list order.
pub fn keymaps_for<'a>(all: &'a [String], layout: &str) -> Vec<&'a String> {
    all.iter()
        .filter(|k| {
            k.as_str() == layout
                || (k.starts_with(layout) && matches!(k.as_bytes().get(layout.len()), Some(b'-' | b'_')))
        })
        .collect()
}

/// tzdata's zone table; the third column is the zone name.
pub const ZONE_TAB: &str = "/usr/share/zoneinfo/zone1970.tab";

pub fn parse_zone_tab(text: &str) -> Vec<String> {
    let mut zones: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').nth(2))
        .map(str::to_owned)
        .filter(|z| valid_zone(z))
        .collect();
    zones.push("UTC".into());
    zones.sort();
    zones.dedup();
    zones
}

/// A zone name is a relative path of ASCII words: never absolute, never `..`.
pub fn valid_zone(z: &str) -> bool {
    !z.is_empty()
        && !z.starts_with('/')
        && z.split('/').all(|p| {
            !p.is_empty()
                && p != ".."
                && p != "."
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+'))
        })
}

pub fn builtin_zones() -> Vec<String> {
    [
        "Africa/Cairo",
        "Africa/Johannesburg",
        "Africa/Lagos",
        "America/Anchorage",
        "America/Chicago",
        "America/Denver",
        "America/Halifax",
        "America/Los_Angeles",
        "America/Mexico_City",
        "America/New_York",
        "America/Sao_Paulo",
        "America/Toronto",
        "America/Vancouver",
        "Asia/Dubai",
        "Asia/Hong_Kong",
        "Asia/Kolkata",
        "Asia/Seoul",
        "Asia/Shanghai",
        "Asia/Singapore",
        "Asia/Tokyo",
        "Australia/Perth",
        "Australia/Sydney",
        "Europe/Amsterdam",
        "Europe/Athens",
        "Europe/Berlin",
        "Europe/Istanbul",
        "Europe/Kyiv",
        "Europe/Lisbon",
        "Europe/London",
        "Europe/Madrid",
        "Europe/Moscow",
        "Europe/Paris",
        "Europe/Rome",
        "Europe/Stockholm",
        "Europe/Warsaw",
        "UTC",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

pub fn load_zones() -> Vec<String> {
    match std::fs::read_to_string(ZONE_TAB).map(|t| parse_zone_tab(&t)) {
        Ok(z) if z.len() > 1 => z,
        _ => builtin_zones(),
    }
}

/// The zone the machine is already set to (`/etc/localtime` is a symlink into
/// the zoneinfo tree), if it is in `zones`.
pub fn current_zone(zones: &[String]) -> Option<String> {
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_string_lossy();
    let name = target.split("zoneinfo/").nth(1)?;
    zones.iter().find(|z| z.as_str() == name).cloned()
}

/// The part of a zone name before the first `/`; a bare name is its own region.
pub fn region_of(zone: &str) -> &str {
    zone.split('/').next().unwrap_or(zone)
}

/// One of the four setup profiles (COMP-17 §2.1) as the card shows it.
pub struct Card {
    pub profile: Profile,
    pub name: &'static str,
    pub blurb: &'static str,
    /// The mono line under the blurb: what it sets, in the file's own words.
    pub meta: &'static str,
}

pub const CARDS: [Card; 4] = [
    Card {
        profile: Profile::Minimal,
        name: "Minimal",
        blurb: "The smallest usable set. A launcher and a terminal, nothing else chosen for you.",
        meta: "mode wm",
    },
    Card {
        profile: Profile::Standard,
        name: "Standard",
        blurb: "The native EclipseOS desktop. Applications are asked, not assumed.",
        meta: "mode de",
    },
    Card {
        profile: Profile::Full,
        name: "Full",
        blurb: "Standard plus a browser, an office suite, an image editor, files and media.",
        meta: "mode de",
    },
    Card {
        profile: Profile::Agentic,
        name: "Agentic",
        blurb: "Standard plus the agent stack. Confers no authority; policy stays locked down.",
        meta: "agent stack",
    },
];

/// Profiles a user can pick in this version. The others are shown and refused:
/// their payload (mode, components, the agent stack) is not built yet.
pub fn selectable(p: Profile) -> bool {
    p == Profile::Standard
}

/// A profile that is offered but not finished, with the word the card shows.
pub fn status_word(p: Profile) -> Option<&'static str> {
    match p {
        Profile::Standard => None,
        Profile::Agentic => Some("preview"),
        Profile::Minimal | Profile::Full => Some("coming"),
    }
}

/// Catalog ids the helper resolves for a profile (D-07 §4.1, §6). Ids only:
/// the helper takes no package or unit name from this program.
pub fn candidates(p: Profile) -> Vec<String> {
    let ids: &[&str] = match p {
        Profile::Standard => &[
            "hyperion",
            "eclipse-launcher",
            "eclipse-toasts",
            "eclipse-center",
            "foot",
        ],
        // Not selectable in this version; kept total so the match is exhaustive.
        Profile::Minimal => &["eclipse-launcher", "foot"],
        Profile::Full => &[
            "hyperion",
            "eclipse-launcher",
            "eclipse-toasts",
            "eclipse-center",
            "foot",
        ],
        Profile::Agentic => &[
            "hyperion",
            "eclipse-launcher",
            "eclipse-toasts",
            "eclipse-center",
            "foot",
        ],
    };
    ids.iter().map(|s| (*s).to_owned()).collect()
}

/// What Standard seeds, in the config file's words (COMP-17 §2.1 table).
pub const STANDARD_SEEDS: [(&str, &str); 8] = [
    ("mode", "de"),
    ("components.bar", "hyperion"),
    ("components.launcher", "eclipse-launcher"),
    ("components.notifications", "eclipse-toasts"),
    ("components.control-center", "eclipse-center"),
    ("terminal", "foot"),
    ("agent stack", "off"),
    ("policy starting point", "locked down"),
];

/// A byte count in binary units, the way `lsblk` and the helper's own messages
/// say it: `512 MiB`, `931.5 GiB`, `1.82 TiB`.
pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KIB * KIB * KIB * KIB {
        format!("{:.2} TiB", b / (KIB * KIB * KIB * KIB))
    } else if b >= KIB * KIB * KIB {
        let g = b / (KIB * KIB * KIB);
        if g >= 100.0 {
            format!("{g:.0} GiB")
        } else {
            format!("{g:.1} GiB")
        }
    } else if b >= KIB * KIB {
        format!("{:.0} MiB", b / (KIB * KIB))
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_lsblk_says_them() {
        assert_eq!(format_size(512 * 1024 * 1024), "512 MiB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_size(300 * 1024 * 1024 * 1024), "300 GiB");
        assert_eq!(format_size(1_000_204_886_016), "932 GiB");
        assert_eq!(format_size(2_000_398_934_016), "1.82 TiB");
        assert_eq!(format_size(12), "12 B");
    }

    #[test]
    fn keymap_scan_finds_maps_skips_symlinks_and_other_files() {
        let d = std::env::temp_dir().join(format!("eclipse-keymaps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("i386/qwerty")).unwrap();
        std::fs::create_dir_all(d.join("elsewhere")).unwrap();
        std::fs::write(d.join("i386/qwerty/us.map.gz"), b"").unwrap();
        std::fs::write(d.join("i386/qwerty/de-latin1.map.gz"), b"").unwrap();
        std::fs::write(d.join("i386/qwerty/notes.txt"), b"").unwrap();
        std::fs::write(d.join("elsewhere/linked.map.gz"), b"").unwrap();
        std::os::unix::fs::symlink(d.join("elsewhere"), d.join("via")).unwrap();
        std::os::unix::fs::symlink(d.join("elsewhere/linked.map.gz"), d.join("i386/alias.map.gz")).unwrap();
        let k = scan_keymaps(&d);
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(k, ["de-latin1", "linked", "us"]);
        assert!(scan_keymaps(std::path::Path::new("/nonexistent-eclipse")).is_empty());
    }

    #[test]
    fn a_wifi_profile_is_recognised_by_its_type() {
        assert!(is_wifi_profile("[connection]\nid=x\ntype=wifi\n"));
        assert!(!is_wifi_profile("[connection]\ntype=ethernet\n"));
    }

    #[test]
    fn keymaps_for_a_layout_match_by_prefix_at_a_boundary() {
        let all: Vec<String> = ["de", "de-latin1", "dev", "us"].map(str::to_owned).to_vec();
        assert_eq!(keymaps_for(&all, "de"), ["de", "de-latin1"]);
        assert!(keymaps_for(&all, "fr").is_empty());
    }

    #[test]
    fn evdev_layouts_and_variants_parse() {
        let text = "! model\n  pc86 Generic\n\n! layout\n  us              English (US)\n  de              German\n\n! variant\n  intl            us: English (US, intl., with dead keys)\n  nodeadkeys      de: German (no dead keys)\n  ghost           xx: not a layout\n\n! option\n  grp   Switching\n";
        let l = parse_evdev(text);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].code, "us");
        assert_eq!(l[0].name, "English (US)");
        assert_eq!(l[0].variants[0].code, "intl");
        assert_eq!(l[0].variants[0].name, "English (US, intl., with dead keys)");
        assert_eq!(l[1].variants.len(), 1);
    }

    #[test]
    fn zone_tab_takes_the_third_column_and_adds_utc() {
        let tab = "# comment\nDE,CH\t+4723+00832\tEurope/Zurich\tGermany\nJP\t+353916+1394441\tAsia/Tokyo\n";
        let z = parse_zone_tab(tab);
        assert_eq!(z, vec!["Asia/Tokyo", "Europe/Zurich", "UTC"]);
    }

    #[test]
    fn zone_names_are_relative_words() {
        assert!(valid_zone("America/Argentina/Buenos_Aires"));
        assert!(valid_zone("Etc/GMT+5"));
        for bad in ["", "/etc/passwd", "../x", "a//b", "a/./b", "a b", "a\nb"] {
            assert!(!valid_zone(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_standard_is_selectable_and_the_others_say_why() {
        for c in &CARDS {
            assert_eq!(selectable(c.profile), c.profile == Profile::Standard);
        }
        assert_eq!(status_word(Profile::Agentic), Some("preview"));
        assert_eq!(status_word(Profile::Minimal), Some("coming"));
        assert_eq!(status_word(Profile::Full), Some("coming"));
        assert_eq!(status_word(Profile::Standard), None);
    }

    #[test]
    fn every_language_is_a_utf8_locale_with_a_plain_layout_code() {
        for l in LANGUAGES {
            assert!(l.locale.ends_with(".UTF-8"));
            assert!(l.layout.bytes().all(|c| c.is_ascii_lowercase()));
        }
    }
}
