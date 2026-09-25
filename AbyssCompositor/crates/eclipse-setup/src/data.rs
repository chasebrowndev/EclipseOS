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

/// tzdata's country names, `US<TAB>United States`.
pub const ISO_TAB: &str = "/usr/share/zoneinfo/iso3166.tab";

/// A place the time-zone search can find: the zone, how to say it, and every
/// word that should lead to it, each with a weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneEntry {
    pub zone: String,
    /// `America/Argentina/Buenos_Aires` -> `Buenos Aires`.
    pub place: String,
    /// The country the zone is in, as tzdata names it; empty when unknown.
    pub country: String,
    words: Vec<(String, u8)>,
}

const W_OTHER: u8 = 1;
const W_PLACE: u8 = 3;
const W_ALIAS: u8 = 4;
/// The most results a search returns; a longer list is a wall, not an answer.
pub const MAX_ZONE_RESULTS: usize = 40;

/// The phrases people actually type, for zones whose path does not say them.
/// Only zones that exist on the machine are given the words.
const ZONE_ALIASES: &[(&str, &str)] = &[
    ("us usa eastern est edt", "America/New_York"),
    ("us usa central cst cdt", "America/Chicago"),
    ("us usa mountain mst mdt", "America/Denver"),
    ("us usa mountain mst arizona", "America/Phoenix"),
    ("us usa pacific pst pdt", "America/Los_Angeles"),
    ("us usa alaska akst akdt", "America/Anchorage"),
    ("us usa hawaii hst", "Pacific/Honolulu"),
    ("eastern est edt canada", "America/Toronto"),
    ("pacific pst pdt canada", "America/Vancouver"),
    ("gmt bst uk britain british england greenwich", "Europe/London"),
    ("gmt utc zulu universal", "UTC"),
    ("cet cest central european", "Europe/Berlin"),
    ("cet cest central european", "Europe/Paris"),
    ("cet cest central european", "Europe/Madrid"),
    ("cet cest central european", "Europe/Rome"),
    ("cet cest central european", "Europe/Amsterdam"),
    ("cet cest central european", "Europe/Stockholm"),
    ("cet cest central european", "Europe/Warsaw"),
    ("cet cest central european", "Europe/Zurich"),
    ("eet eest eastern european", "Europe/Athens"),
    ("eet eest eastern european", "Europe/Kyiv"),
    ("eet eest eastern european", "Europe/Helsinki"),
    ("wet west western european", "Europe/Lisbon"),
    ("msk", "Europe/Moscow"),
    ("ist india", "Asia/Kolkata"),
    ("jst", "Asia/Tokyo"),
    ("kst", "Asia/Seoul"),
    ("cst china beijing", "Asia/Shanghai"),
    ("aest aedt eastern", "Australia/Sydney"),
];

fn words_of(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// `Buenos_Aires` -> `Buenos Aires`; the last path segment, said as a name.
pub fn place_name(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// Country codes to names from `iso3166.tab`.
pub fn parse_iso_tab(text: &str) -> std::collections::HashMap<String, String> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let (code, name) = l.split_once('\t')?;
            Some((code.trim().to_owned(), name.trim().to_owned()))
        })
        .collect()
}

/// The search index over `zones`. `zone_tab` supplies each zone's countries and
/// comment, `iso_tab` the country names; either may be empty, and the zone's own
/// path words and the alias table still work.
pub fn build_index(zones: &[String], zone_tab: &str, iso_tab: &str) -> Vec<ZoneEntry> {
    let iso = parse_iso_tab(iso_tab);
    let mut tab: std::collections::HashMap<&str, (&str, &str)> = std::collections::HashMap::new();
    for l in zone_tab.lines().filter(|l| !l.starts_with('#')) {
        let mut c = l.split('\t');
        if let (Some(codes), Some(_), Some(zone)) = (c.next(), c.next(), c.next()) {
            tab.insert(zone, (codes, c.next().unwrap_or("")));
        }
    }
    zones
        .iter()
        .map(|zone| {
            let mut words: Vec<(String, u8)> = Vec::new();
            for w in words_of(zone) {
                words.push((w, W_PLACE));
            }
            for (phrase, z) in ZONE_ALIASES {
                if z == zone {
                    words.extend(words_of(phrase).map(|w| (w, W_ALIAS)));
                }
            }
            let mut country = String::new();
            if let Some((codes, comment)) = tab.get(zone.as_str()) {
                for code in codes.split(',') {
                    words.push((code.to_lowercase(), W_OTHER));
                    if let Some(name) = iso.get(code.trim()) {
                        words.extend(words_of(name).map(|w| (w, W_OTHER)));
                        if country.is_empty() {
                            country = name.clone();
                        }
                    }
                }
                words.extend(words_of(comment).map(|w| (w, W_OTHER)));
            }
            ZoneEntry {
                zone: zone.clone(),
                place: place_name(zone),
                country,
                words,
            }
        })
        .collect()
}

/// Places that match every word of `query`, best first. A word matches the start
/// of any word the place is known by: its city and region, its country, tzdata's
/// note for it, and the aliases people use (`us eastern`, `cet`). An empty query
/// matches nothing: the caller shows the current guess instead.
pub fn search_zones<'a>(index: &'a [ZoneEntry], query: &str) -> Vec<&'a ZoneEntry> {
    let tokens: Vec<String> = words_of(query).collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u32, &ZoneEntry)> = index
        .iter()
        .filter_map(|e| {
            let mut total = 0u32;
            for t in &tokens {
                let best = e
                    .words
                    .iter()
                    .filter(|(w, _)| w.starts_with(t.as_str()))
                    .map(|(w, weight)| u32::from(*weight) * if w == t { 2 } else { 1 })
                    .max()?;
                total += best;
            }
            Some((total, e))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.zone.cmp(&b.1.zone)));
    hits.into_iter().take(MAX_ZONE_RESULTS).map(|(_, e)| e).collect()
}

/// The machine's own index, or one built from the zone names alone.
pub fn load_zone_index(zones: &[String]) -> Vec<ZoneEntry> {
    let tab = std::fs::read_to_string(ZONE_TAB).unwrap_or_default();
    let iso = std::fs::read_to_string(ISO_TAB).unwrap_or_default();
    build_index(zones, &tab, &iso)
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
        Profile::Standard => &["hyperion", "eclipse-launcher", "eclipse-toasts", "eclipse-center"],
        // Not selectable in this version; kept total so the match is exhaustive.
        Profile::Minimal => &["eclipse-launcher"],
        Profile::Full => &["hyperion", "eclipse-launcher", "eclipse-toasts", "eclipse-center"],
        Profile::Agentic => &["hyperion", "eclipse-launcher", "eclipse-toasts", "eclipse-center"],
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

    /// The helper's catalog holds exactly these four ids. `foot` is a floor
    /// package and a seed key, not a candidate: sending it is refused.
    #[test]
    fn candidates_are_only_catalog_ids() {
        let want = ["hyperion", "eclipse-launcher", "eclipse-toasts", "eclipse-center"];
        for p in [Profile::Standard, Profile::Full, Profile::Agentic] {
            assert_eq!(candidates(p), want);
        }
        assert_eq!(candidates(Profile::Minimal), ["eclipse-launcher"]);
    }

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

    fn index() -> Vec<ZoneEntry> {
        let tab = "# c\nUS\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\nUS\t+421953-0830245\tAmerica/Detroit\tEastern - MI (most areas)\nUS\t+340308-1181434\tAmerica/Los_Angeles\tPacific\nJP\t+353916+1394441\tAsia/Tokyo\t\nGB,GG\t+513030-0000731\tEurope/London\t\nDE,DK\t+5230+01322\tEurope/Berlin\tmost of Germany\n";
        let iso =
            "# c\nUS\tUnited States\nJP\tJapan\nGB\tBritain (UK)\nGG\tGuernsey\nDE\tGermany\nDK\tDenmark\n";
        let zones = parse_zone_tab(tab);
        build_index(&zones, tab, iso)
    }

    fn found<'a>(idx: &'a [ZoneEntry], q: &str) -> Vec<&'a str> {
        search_zones(idx, q).iter().map(|e| e.zone.as_str()).collect()
    }

    #[test]
    fn a_city_is_found_by_its_words_in_any_case() {
        let idx = index();
        assert_eq!(found(&idx, "New York"), ["America/New_York"]);
        assert_eq!(found(&idx, "  new   YORK "), ["America/New_York"]);
        assert_eq!(found(&idx, "tokyo"), ["Asia/Tokyo"]);
        assert_eq!(found(&idx, "lon"), ["Europe/London"]);
    }

    #[test]
    fn a_country_leads_to_its_zones_and_an_alias_ranks_first() {
        let idx = index();
        assert_eq!(found(&idx, "japan"), ["Asia/Tokyo"]);
        assert_eq!(found(&idx, "germany"), ["Europe/Berlin"]);
        let east = found(&idx, "us eastern");
        assert_eq!(east.first(), Some(&"America/New_York"));
        assert!(!east.contains(&"America/Los_Angeles"));
        assert_eq!(found(&idx, "US Pacific"), ["America/Los_Angeles"]);
        assert_eq!(found(&idx, "gmt").first(), Some(&"Europe/London"));
        assert!(found(&idx, "cet").contains(&"Europe/Berlin"));
        assert!(found(&idx, "utc").contains(&"UTC"));
    }

    #[test]
    fn an_empty_or_unmatched_query_finds_nothing() {
        let idx = index();
        assert!(found(&idx, "").is_empty());
        assert!(found(&idx, "   ").is_empty());
        assert!(found(&idx, "atlantis").is_empty());
        assert!(found(&idx, "new atlantis").is_empty(), "every word has to match");
    }

    #[test]
    fn the_index_works_without_the_machines_tables() {
        let idx = build_index(&builtin_zones(), "", "");
        assert_eq!(found(&idx, "new york"), ["America/New_York"]);
        assert_eq!(found(&idx, "us eastern"), ["America/New_York"]);
        assert_eq!(
            idx.iter().find(|e| e.zone == "Asia/Tokyo").unwrap().place,
            "Tokyo"
        );
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
