// SPDX-License-Identifier: AGPL-3.0-only
//! Settings search: instant, fuzzy, synonym-aware, fully offline.
//!
//! The index is built once per schema reply from the generated rows
//! ([`index`]) plus a handful of hand-written entries for the bespoke panes
//! that are not schema keys ([`bespoke`]). Every field is lowercased and split
//! into words up front, so a keystroke is a pass over a few hundred small
//! word lists and no allocation-heavy work happens per entry.
//!
//! Scoring ([`search`]) runs in tiers — exact, prefix, whole word, word
//! prefix, infix, abbreviation (subsequence), typo — each weighted by the
//! field it hit (label > path > extra terms > breadcrumb > doc). A query word
//! is also tried as each of its [`SYNONYMS`], at a discount, so "mouse speed"
//! finds "Pointer acceleration". Every query word must match something; the
//! words' scores add.
//!
//! Where an entry lives in the UI is the caller's business: [`index`] takes a
//! closure that places a row, so this module does not depend on how panes and
//! sub-pages are laid out.

use crate::schema::{self, Control, Row};

/// Path prefix of a hand-written entry. Its path is not a config key: it
/// names a place in a bespoke pane (`pane:display.scale`), and the prefix is
/// stripped before the path is indexed.
pub const BESPOKE: &str = "pane:";

/// A sensible default for how many results to show.
pub const LIMIT: usize = 8;

/// Where a row is shown: its section in the sidebar, the sub-page inside it,
/// and the group heading on that page. Supplied by the caller of [`index`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    pub section: String,
    pub page: Option<String>,
    pub group: Option<String>,
}

impl Place {
    /// `section › page › group`, leaving out what is absent.
    pub fn crumb(&self) -> String {
        let mut parts = vec![self.section.as_str()];
        parts.extend(self.page.as_deref());
        parts.extend(self.group.as_deref());
        parts.join(" › ")
    }
}

/// One searchable thing: a schema key or a place in a bespoke pane.
#[derive(Debug, Clone)]
pub struct SearchEntry {
    /// The dotted config path, or a [`BESPOKE`] pseudo-path.
    pub path: String,
    pub label: String,
    /// `section › sub-page › group`.
    pub breadcrumb: String,
    pub doc: String,
    /// Enum values (wire spelling and display form) and, for hand-written
    /// entries, the words people use for the thing.
    pub extra_terms: Vec<String>,
    hay: Hay,
}

impl SearchEntry {
    pub fn new(
        path: String,
        label: String,
        breadcrumb: String,
        doc: String,
        extra_terms: Vec<String>,
    ) -> Self {
        Self::build(path, label, breadcrumb, doc, extra_terms, "")
    }

    /// `more_doc` is searched at doc weight but not shown (enum blurbs).
    fn build(
        path: String,
        label: String,
        breadcrumb: String,
        doc: String,
        extra_terms: Vec<String>,
        more_doc: &str,
    ) -> Self {
        let bare = path.strip_prefix(BESPOKE).unwrap_or(&path);
        let label_f = Field::new(&label);
        let path_f = Field::new(bare);
        let mut direct = vec![(label_f.clone(), LABEL), (path_f.clone(), PATH)];
        direct.extend(extra_terms.iter().map(|t| (Field::new(t), EXTRA)));
        direct.push((Field::new(&breadcrumb), CRUMB));
        direct.push((Field::new(&format!("{doc} {more_doc}")), DOC));
        let abbrev = vec![
            (label_f, LABEL),
            (path_f, PATH),
            (Field::new(&format!("{breadcrumb} {label}")), COMBO),
        ];
        SearchEntry {
            path,
            label,
            breadcrumb,
            doc,
            extra_terms,
            hay: Hay { direct, abbrev },
        }
    }

    /// True for a hand-written entry, whose path is not a config key.
    pub fn is_bespoke(&self) -> bool {
        self.path.starts_with(BESPOKE)
    }
}

/// Index the schema rows, then the bespoke panes. A row the closure does not
/// place is left out: it is not reachable, so there is nowhere to take the
/// user.
pub fn index<F>(rows: &[Row], mut place: F) -> Vec<SearchEntry>
where
    F: FnMut(&Row) -> Option<Place>,
{
    let mut out: Vec<SearchEntry> = rows
        .iter()
        .filter_map(|row| Some(entry_for(row, &place(row)?)))
        .collect();
    out.extend(bespoke());
    out
}

fn entry_for(row: &Row, place: &Place) -> SearchEntry {
    let mut extra = Vec::new();
    let mut more = String::new();
    if let Control::Segmented(values) | Control::Dropdown(values) = &row.control {
        let blur_mode = format!("{}.mode", schema::BLUR);
        for v in values {
            extra.push(v.clone());
            let shown = schema::value_label(v);
            if shown != v.as_str() {
                extra.push(shown.to_owned());
            }
            let blurb = if row.path == "mode" {
                schema::mode_blurb(v)
            } else if row.path == blur_mode {
                schema::blur_blurb(v)
            } else {
                None
            };
            if let Some(b) = blurb {
                more.push(' ');
                more.push_str(b);
            }
        }
    }
    SearchEntry::build(
        row.path.clone(),
        row.label().into_owned(),
        place.crumb(),
        row.doc.clone(),
        extra,
        &more,
    )
}

/// The bespoke panes' contents, which are not schema keys and so would
/// otherwise be unfindable. Paths are [`BESPOKE`] pseudo-paths.
pub fn bespoke() -> Vec<SearchEntry> {
    const HAND: &[(&str, &str, &str, &str, &[&str])] = &[
        (
            "display.enable",
            "Enable output",
            "Display",
            "Turn a connected monitor on or off.",
            &["disable", "turn off", "monitor on", "outputs", "screen"],
        ),
        (
            "display.mode",
            "Resolution and refresh rate",
            "Display",
            "The mode an output is driven at.",
            &["resolution", "refresh", "hz", "mode", "1080p", "4k"],
        ),
        (
            "display.scale",
            "Scale",
            "Display",
            "How large everything is drawn on an output.",
            &[
                "zoom",
                "dpi",
                "hidpi",
                "scaling",
                "text size",
                "bigger",
                "smaller",
            ],
        ),
        (
            "display.rotation",
            "Rotation",
            "Display",
            "Turn an output's picture to match how the monitor stands.",
            &[
                "rotate",
                "orientation",
                "portrait",
                "landscape",
                "transform",
                "flip",
            ],
        ),
        (
            "display.overscan",
            "Overscan",
            "Display",
            "Pull the picture in from edges a TV cuts off.",
            &[
                "calibrate",
                "calibration",
                "underscan",
                "cut off",
                "edges",
                "inset",
                "tv",
            ],
        ),
        (
            "network.wifi",
            "Wi-Fi",
            "Network",
            "The wireless link: scan, connect and disconnect.",
            &[
                "wifi", "wireless", "wlan", "ssid", "connect", "internet", "hotspot",
            ],
        ),
        (
            "network.saved",
            "Saved networks",
            "Network",
            "Networks this machine remembers, and forgetting them.",
            &["known networks", "forget", "profiles", "passwords", "remembered"],
        ),
        (
            "network.bluetooth",
            "Bluetooth devices",
            "Network",
            "Paired devices, and forgetting them.",
            &[
                "bluetooth",
                "paired",
                "pair",
                "headphones",
                "speaker",
                "keyboard",
                "forget",
            ],
        ),
        (
            "addons",
            "Add-ons",
            "Add-ons",
            "Optional packages, and the hooks they switch on.",
            &["addons", "extensions", "plugins", "packages", "hooks", "optional"],
        ),
        (
            "taskbar.widgets",
            "Widget editor",
            "Taskbar › Widgets",
            "Add, edit and order the bar's widgets, including your own commands.",
            &[
                "widgets",
                "custom widget",
                "add widget",
                "modules",
                "exec",
                "script",
                "order",
            ],
        ),
    ];
    HAND.iter()
        .map(|(path, label, crumb, doc, extra)| {
            SearchEntry::new(
                format!("{BESPOKE}{path}"),
                (*label).to_owned(),
                (*crumb).to_owned(),
                (*doc).to_owned(),
                extra.iter().map(|t| (*t).to_owned()).collect(),
            )
        })
        .collect()
}

/// Words people search with, and the words the settings use. A query word
/// matching a term on one side is also tried as every term on the other, so
/// the table reads in both directions. Terms are lowercase words joined by
/// single spaces — the same normal form a query is split into (`see through`
/// matches "see-through").
pub const SYNONYMS: &[(&[&str], &[&str])] = &[
    (&["mouse", "cursor"], &["pointer"]),
    (&["trackpad", "touch pad"], &["touchpad"]),
    (
        &[
            "transparent",
            "transparency",
            "translucent",
            "translucency",
            "dark",
            "frosted",
            "see through",
            "alpha",
        ],
        &["opacity", "blur", "glass"],
    ),
    (
        &[
            "sleep",
            "screensaver",
            "screen saver",
            "suspend",
            "away",
            "inactivity",
            "afk",
        ],
        &["lock", "idle", "screen off"],
    ),
    (
        &["bar", "panel", "dock", "menubar", "menu bar", "status bar"],
        &["taskbar"],
    ),
    (
        &[
            "start menu",
            "app menu",
            "apps menu",
            "search apps",
            "app search",
            "app launcher",
            "spotlight",
            "rofi",
            "dmenu",
        ],
        &["launcher"],
    ),
    (
        &[
            "wifi", "wi fi", "internet", "wireless", "wlan", "ethernet", "online",
        ],
        &["network"],
    ),
    (
        &[
            "hotkey",
            "hotkeys",
            "shortcut",
            "shortcuts",
            "keybind",
            "keybinds",
            "keybinding",
            "keybindings",
        ],
        &["bind", "binds"],
    ),
    (
        &[
            "resolution",
            "monitor",
            "monitors",
            "screen",
            "screens",
            "refresh rate",
        ],
        &["display", "output"],
    ),
    (
        &["corner", "corners", "radius", "round", "rounded"],
        &["rounding"],
    ),
    (
        &["speed", "fast", "faster", "slow", "slower", "sensitivity"],
        &["rate", "acceleration", "repeat"],
    ),
    (
        &["background", "backdrop", "desktop image", "desktop picture"],
        &["wallpaper"],
    ),
    (
        &["hints", "hint", "legend", "cheatsheet", "cheat sheet"],
        &["show controls", "key hints"],
    ),
    (&["kbd", "keys", "typing"], &["keyboard", "key"]),
    (&["colour", "colours"], &["color", "colors"]),
    (
        &["spacing", "padding", "margin", "margins", "space between"],
        &["gaps"],
    ),
    (&["tiling", "tile", "tiler"], &["layout"]),
    (
        &["transition", "transitions", "movement", "motion"],
        &["animations"],
    ),
    (&["outline", "frame"], &["border"]),
    (&["highlight", "halo"], &["glow"]),
    (&["darken", "fade", "faded"], &["dim"]),
    (
        &["reverse scroll", "invert scroll", "scroll direction", "inverted"],
        &["natural scroll"],
    ),
    (&["tap", "tapping"], &["tap to click"]),
    (&["palm", "palm rejection"], &["disable while typing", "dwt"]),
    (&["language", "keymap", "xkb"], &["keyboard layout", "kb layout"]),
    (
        &[
            "screenshot",
            "screenshots",
            "screen recording",
            "recording",
            "screen share",
            "screenshare",
        ],
        &["capture"],
    ),
    (&["copy paste", "paste", "clipboard manager"], &["clipboard"]),
    (&["x11", "xorg", "x server", "legacy apps"], &["xwayland"]),
    (&["gpu", "graphics card", "video card"], &["render device"]),
    (&["time", "date", "24 hour", "12 hour", "am pm"], &["clock"]),
    (
        &["system tray", "status icons", "applets", "indicators"],
        &["tray"],
    ),
    (&["music", "song", "player", "spotify"], &["now playing", "media"]),
    (&["sound", "audio", "speaker"], &["volume"]),
    (&["cpu", "ram", "memory", "resources"], &["system usage"]),
    (&["toasts", "alerts", "popups"], &["notifications"]),
    (&["quick settings", "action center"], &["control center"]),
    (&["hide", "autohide", "auto hide", "collapse"], &["fold"]),
    (&["extensions", "plugins", "addon"], &["add ons", "addons"]),
    (&["bt", "headphones", "earbuds", "pair"], &["bluetooth"]),
    (&["zoom", "dpi", "hidpi", "bigger"], &["scale"]),
    (&["rotate", "orientation", "portrait"], &["rotation"]),
    (&["calibrate", "underscan", "cut off"], &["overscan"]),
    (&["hover focus", "sloppy focus"], &["focus follows mouse"]),
    (&["window manager"], &["wm"]),
    (&["console", "shell", "terminal emulator"], &["terminal"]),
];

/// Rank `entries` against `query`: `(score, index into entries)`, best
/// first, at most `limit`. An empty query matches nothing.
pub fn search(entries: &[SearchEntry], query: &str, limit: usize) -> Vec<(f32, usize)> {
    let units = expand(&words(query));
    if units.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut hits: Vec<(f32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let mut total = 0.0;
            for unit in &units {
                let s = unit
                    .iter()
                    .map(|alt| alt.factor * term_score(alt, &e.hay))
                    .fold(0.0, f32::max);
                if s <= 0.0 {
                    return None;
                }
                total += s;
            }
            Some((total, i))
        })
        .collect();
    hits.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| entries[a.1].label.len().cmp(&entries[b.1].label.len()))
            .then(a.1.cmp(&b.1))
    });
    // A strong hit makes the stragglers noise: "wifi" finds Wi-Fi, not
    // "WIndows › Focus & guIdes" as a scattered abbreviation.
    let floor = hits.first().map_or(0.0, |h| h.0 * RELATIVE_FLOOR);
    hits.retain(|h| h.0 >= floor);
    hits.truncate(limit);
    hits
}

/// The share of the best hit's score a result needs to be listed at all.
const RELATIVE_FLOOR: f32 = 0.25;

// Field weights.
const LABEL: f32 = 1.0;
const PATH: f32 = 0.85;
const EXTRA: f32 = 0.7;
const CRUMB: f32 = 0.55;
const DOC: f32 = 0.4;
/// Breadcrumb and label run together, for abbreviations only ("kbrep" →
/// "Keyboard › Key repeat rate").
const COMBO: f32 = 0.6;

// Tiers, best first.
const EXACT: f32 = 100.0;
const PREFIX: f32 = 85.0;
const WORD: f32 = 75.0;
const WORD_PREFIX: f32 = 60.0;
const INFIX: f32 = 45.0;
const ABBREV: f32 = 40.0;
const TYPO1: f32 = 32.0;
const TYPO2: f32 = 24.0;

// Synonym discounts: an exact table hit, and a loose one (the word is still
// being typed, a plural, or a typo of the table term).
const SYN: f32 = 0.8;
const SYN_LOOSE: f32 = 0.7;
/// Each later term on a synonym's side costs this much more, so the table's
/// order says which reading wins a tie.
const SYN_STEP: f32 = 0.05;
const SYN_FLOOR: f32 = 0.15;

/// Longest word the typo tier compares, so the edit distance runs on the
/// stack.
const MAX_WORD: usize = 32;

/// A field, lowercased and split once at index time.
#[derive(Debug, Clone)]
struct Field {
    /// Words joined by single spaces.
    text: String,
    /// `text` with the spaces removed, so "wifi" finds "Wi-Fi".
    squashed: String,
    words: Vec<String>,
}

impl Field {
    fn new(s: &str) -> Self {
        let words = words(s);
        let text = words.join(" ");
        Field {
            squashed: words.concat(),
            text,
            words,
        }
    }
}

#[derive(Debug, Clone)]
struct Hay {
    /// Fields for the exact-to-infix tiers and typos, with their weights.
    direct: Vec<(Field, f32)>,
    /// Fields an abbreviation may be read from.
    abbrev: Vec<(Field, f32)>,
}

/// One way to read one query unit.
#[derive(Debug, Clone)]
struct Alt {
    term: String,
    factor: f32,
    /// Abbreviation and typo tiers apply: only to what the user typed, never
    /// to a synonym, which is already a real word.
    fuzzy: bool,
}

/// Lowercase alphanumeric runs.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn table_has(term: &str) -> bool {
    SYNONYMS
        .iter()
        .any(|(l, r)| l.iter().chain(r.iter()).any(|t| *t == term))
}

/// Group query words into units — a table phrase ("start menu") is one unit —
/// and give each its alternative readings.
fn expand(words: &[String]) -> Vec<Vec<Alt>> {
    let mut units = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let longest = (2..=3.min(words.len() - i))
            .rev()
            .map(|n| (n, words[i..i + n].join(" ")))
            .find(|(_, p)| table_has(p));
        let (n, term) = longest.unwrap_or_else(|| (1, words[i].clone()));
        units.push(alts_for(&term, n == 1));
        i += n;
    }
    units
}

fn alts_for(term: &str, fuzzy: bool) -> Vec<Alt> {
    let mut alts = vec![Alt {
        term: term.to_owned(),
        factor: 1.0,
        fuzzy,
    }];
    let mut add = |side: &[&str], base: f32| {
        for (k, t) in side.iter().enumerate() {
            let factor = (base - SYN_STEP * k as f32).max(base - SYN_FLOOR);
            match alts.iter_mut().find(|a| a.term == *t) {
                Some(a) => a.factor = a.factor.max(factor),
                None => alts.push(Alt {
                    term: (*t).to_owned(),
                    factor,
                    fuzzy: false,
                }),
            }
        }
    };
    // A misspelt or half-typed table term also reads as the term itself,
    // above its synonyms: "keybord" is "keyboard" before it is "typing".
    if fuzzy && term.len() >= 4 && !term.contains(' ') && !table_has(term) {
        for (lhs, rhs) in SYNONYMS {
            for t in lhs.iter().chain(rhs.iter()) {
                if *t != term && !t.contains(' ') && side_match(term, &[t]).is_some() {
                    add(&[t], SYN);
                }
            }
        }
    }
    for (lhs, rhs) in SYNONYMS {
        if let Some(m) = side_match(term, lhs) {
            add(rhs, m);
        }
        if let Some(m) = side_match(term, rhs) {
            add(lhs, m);
        }
    }
    alts
}

/// How well `term` names one of `side`'s terms: exactly, or loosely (still
/// being typed, a plural, a typo).
fn side_match(term: &str, side: &[&str]) -> Option<f32> {
    let mut best: Option<f32> = None;
    for t in side {
        let m = if *t == term {
            Some(SYN)
        } else if term.len() >= 4 && !term.contains(' ') && !t.contains(' ') {
            let typing = t.starts_with(term);
            let plural = t.len() >= 4 && term.starts_with(t) && term.len() - t.len() <= 2;
            let budget = typo_budget(term);
            let typo = budget > 0 && osa(term.as_bytes(), t.as_bytes()) <= budget;
            (typing || plural || typo).then_some(SYN_LOOSE)
        } else {
            None
        };
        best = match (best, m) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
    best
}

fn term_score(alt: &Alt, hay: &Hay) -> f32 {
    if alt.term.contains(' ') {
        phrase_score(&alt.term, hay)
    } else {
        word_score(&alt.term, hay, alt.fuzzy)
    }
}

/// A phrase scores as a whole where a field holds it, else as the mean of its
/// words, all of which must match.
fn phrase_score(phrase: &str, hay: &Hay) -> f32 {
    let whole = hay
        .direct
        .iter()
        .map(|(f, w)| direct(phrase, f) * w)
        .fold(0.0, f32::max);
    let mut sum = 0.0;
    let mut n = 0.0;
    for w in phrase.split(' ') {
        let s = word_score(w, hay, false);
        if s <= 0.0 {
            return whole;
        }
        sum += s;
        n += 1.0;
    }
    whole.max(sum / n)
}

fn word_score(term: &str, hay: &Hay, fuzzy: bool) -> f32 {
    let mut best = hay
        .direct
        .iter()
        .map(|(f, w)| direct(term, f) * w)
        .fold(0.0, f32::max);
    if !fuzzy || best >= ABBREV {
        return best;
    }
    if term.len() >= 3 {
        let a = hay
            .abbrev
            .iter()
            .map(|(f, w)| abbrev(term, f) * w)
            .fold(0.0, f32::max);
        best = best.max(a);
    }
    let budget = typo_budget(term);
    if best >= TYPO1 || budget == 0 {
        return best;
    }
    for (f, w) in &hay.direct {
        for word in &f.words {
            if word.len().abs_diff(term.len()) > budget {
                continue;
            }
            let d = osa(term.as_bytes(), word.as_bytes());
            if d <= budget {
                let tier = if d <= 1 { TYPO1 } else { TYPO2 };
                best = best.max(tier * w);
            }
        }
    }
    best
}

fn direct(term: &str, f: &Field) -> f32 {
    if f.text == term || f.squashed == term {
        EXACT
    } else if f.text.starts_with(term) || f.squashed.starts_with(term) {
        PREFIX
    } else if f.words.iter().any(|w| w == term) {
        WORD
    } else if f.words.iter().any(|w| w.starts_with(term)) {
        WORD_PREFIX
    } else if term.len() >= 3 && f.text.contains(term) {
        INFIX
    } else {
        0.0
    }
}

/// `term` as a subsequence of the field, starting at a word start. Scored up
/// to [`ABBREV`] by how many of its letters land on word starts, so "kbrep"
/// reads "Keyboard › Key repeat rate" over "Keyboard options".
fn abbrev(term: &str, f: &Field) -> f32 {
    let text = f.text.as_bytes();
    let t = term.as_bytes();
    let starts = [true, false]
        .into_iter()
        .filter_map(|prefer| subsequence(text, t, prefer))
        .max();
    match starts {
        Some(s) => ABBREV * (0.6 + 0.4 * s as f32 / t.len() as f32),
        None => 0.0,
    }
}

/// Greedy subsequence match, counting letters that land on word starts.
/// With `prefer_starts` each letter takes the next word-start occurrence when
/// there is one, which finds the better reading but can miss a match the
/// plain greedy pass finds.
fn subsequence(text: &[u8], t: &[u8], prefer_starts: bool) -> Option<usize> {
    let at_start = |i: usize| i == 0 || text[i - 1] == b' ';
    let mut pos = (0..text.len()).find(|&i| text[i] == t[0] && at_start(i))?;
    let mut starts = 0;
    for &c in t {
        let hit = if prefer_starts {
            (pos..text.len()).find(|&i| text[i] == c && at_start(i))
        } else {
            None
        };
        let at = hit.or_else(|| (pos..text.len()).find(|&i| text[i] == c))?;
        starts += usize::from(at_start(at));
        pos = at + 1;
    }
    Some(starts)
}

/// Edits a word of this length may carry and still match: none under four
/// letters, one from four, two from seven.
fn typo_budget(term: &str) -> usize {
    match term.len() {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    }
}

/// Optimal-string-alignment (restricted Damerau–Levenshtein) distance:
/// insertions, deletions, substitutions and adjacent transpositions.
fn osa(a: &[u8], b: &[u8]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n > MAX_WORD || m > MAX_WORD {
        return usize::MAX;
    }
    let mut prev2 = [0usize; MAX_WORD + 1];
    let mut prev = [0usize; MAX_WORD + 1];
    let mut cur = [0usize; MAX_WORD + 1];
    for (j, p) in prev.iter_mut().enumerate().take(m + 1) {
        *p = j;
    }
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
        }
        prev2 = prev;
        prev = cur;
    }
    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_abyss_config::schema::{Owner, Ty, TABLE};
    use serde_json::{json, Value};

    /// The compositor's real schema, as the wire would deliver it.
    fn rows() -> Vec<Row> {
        TABLE
            .iter()
            .filter_map(|k| {
                let (ty, c) = match k.ty {
                    Ty::Bool => ("bool", Value::Null),
                    Ty::Int { min, max } => ("int", json!({ "min": min, "max": max })),
                    Ty::Float { min, max } => ("float", json!({ "min": min, "max": max })),
                    Ty::Str => ("string", Value::Null),
                    Ty::Enum(values) => ("enum", json!({ "values": values })),
                    Ty::Color => ("color", Value::Null),
                    Ty::StrList => ("string-list", Value::Null),
                };
                let file = match k.owner {
                    Owner::Abyss => "abyss",
                    Owner::Policy => "policy",
                };
                Row::parse(&json!({
                    "path": k.path, "type": ty, "constraints": c, "file": file, "doc": k.doc,
                }))
            })
            .collect()
    }

    fn entries() -> Vec<SearchEntry> {
        index(&rows(), crate::search_ui::place)
    }

    fn top<'e>(entries: &'e [SearchEntry], q: &str, n: usize) -> Vec<&'e str> {
        search(entries, q, n)
            .into_iter()
            .map(|(_, i)| entries[i].path.as_str())
            .collect()
    }

    fn assert_top3(entries: &[SearchEntry], q: &str, want: &str) {
        let got = top(entries, q, 3);
        assert!(
            got.contains(&want),
            "{q:?} gave {got:?}, wanted {want} in the top 3"
        );
    }

    #[test]
    fn the_brief_queries_find_their_keys() {
        let e = entries();
        assert_top3(&e, "mouse speed", "input.accel-profile");
        assert_top3(&e, "trasnparency", "decoration.active-opacity");
        assert_top3(&e, "start menu", "launcher.style");
        assert_top3(&e, "wallpaper", "wallpaper.path");
        assert_top3(&e, "kbd repeat", "input.repeat-rate");
        assert_top3(&e, "corner radius", "decoration.rounding");
        assert_top3(&e, "wifi", "pane:network.wifi");
        assert!(
            !top(&e, "wifi", LIMIT)
                .iter()
                .any(|p| p.starts_with("general.drop")),
            "{:?}",
            top(&e, "wifi", LIMIT)
        );
        assert_top3(&e, "show controls", "ui.show-key-hints");
    }

    #[test]
    fn wallpaper_fills_the_top_with_wallpaper_keys() {
        let e = entries();
        let got = top(&e, "wallpaper", 3);
        assert!(got.iter().all(|p| p.starts_with("wallpaper.")), "{got:?}");
    }

    #[test]
    fn an_abbreviation_reads_across_the_breadcrumb() {
        let e = entries();
        assert_eq!(top(&e, "kbrep", 1), ["input.repeat-rate"]);
    }

    #[test]
    fn a_misspelt_table_term_beats_its_synonyms() {
        let e = entries();
        let hits = top(&e, "keybord", 3);
        assert!(
            hits.iter()
                .all(|p| p.starts_with("input.") && *p != "input.touchpad.disable-while-typing"),
            "{hits:?}"
        );
    }

    #[test]
    fn typos_are_forgiven_by_word_length() {
        let e = entries();
        // Seven letters or more: two edits.
        assert_top3(&e, "keybaord layout", "input.kb-layout");
        assert_top3(&e, "wallpapre", "wallpaper.path");
        // Four to six: one.
        assert_top3(&e, "gloq", "decoration.glow.active");
        // Under four: none.
        assert!(top(&e, "xqz", 3).is_empty());
    }

    #[test]
    fn exact_beats_prefix_beats_typo() {
        let e = entries();
        let score = |q: &str, path: &str| {
            let i = e.iter().position(|x| x.path == path).unwrap();
            search(&e, q, e.len())
                .into_iter()
                .find(|&(_, j)| j == i)
                .map(|(s, _)| s)
                .unwrap_or(0.0)
        };
        let exact = score("rounding", "decoration.rounding");
        let prefix = score("roun", "decoration.rounding");
        let typo = score("rounidng", "decoration.rounding");
        assert!(
            exact > prefix && prefix > typo && typo > 0.0,
            "{exact} {prefix} {typo}"
        );
    }

    #[test]
    fn every_word_must_match() {
        let e = entries();
        assert!(top(&e, "wifi zzzzzzzz", 3).is_empty());
        assert!(top(&e, "", 3).is_empty());
        assert!(top(&e, "  -- ", 3).is_empty());
    }

    #[test]
    fn synonyms_read_both_ways() {
        let e = entries();
        assert_top3(&e, "trackpad tap", "input.touchpad.tap-to-click");
        assert_top3(&e, "screensaver", "idle.lock-timeout-seconds");
        let dock = search(&e, "dock", 3);
        assert!(
            dock.len() == 3 && dock.iter().all(|&(_, i)| e[i].breadcrumb.starts_with("Taskbar")),
            "{:?}",
            dock.iter().map(|&(_, i)| &e[i].path).collect::<Vec<_>>()
        );
        assert_top3(&e, "resolution", "pane:display.mode");
        assert_top3(&e, "background color", "wallpaper.color");
        // The settings word finds the user's word too.
        assert_top3(&e, "hints", "ui.show-key-hints");
    }

    #[test]
    fn enum_values_are_searchable() {
        let e = entries();
        assert_top3(&e, "liquid glass", "decoration.blur.mode");
        assert_top3(&e, "dwindle", "general.layout");
    }

    #[test]
    fn bespoke_entries_are_indexed_once_each() {
        let e = entries();
        let hand: Vec<_> = e.iter().filter(|x| x.is_bespoke()).map(|x| &x.path).collect();
        assert_eq!(hand.len(), bespoke().len());
        assert_top3(&e, "overscan", "pane:display.overscan");
        assert_top3(&e, "bluetooth", "pane:network.bluetooth");
        assert_top3(&e, "plugins", "pane:addons");
        assert_top3(&e, "custom widget", "pane:taskbar.widgets");
    }

    #[test]
    fn synonym_terms_are_in_query_normal_form() {
        for (l, r) in SYNONYMS {
            for t in l.iter().chain(r.iter()) {
                assert_eq!(words(t).join(" "), *t, "table term {t:?} is not normalised");
            }
        }
    }

    #[test]
    fn osa_counts_a_transposition_as_one() {
        assert_eq!(osa(b"trasnparency", b"transparency"), 1);
        assert_eq!(osa(b"kitten", b"sitting"), 3);
        assert_eq!(osa(b"", b"abc"), 3);
        assert_eq!(osa(b"same", b"same"), 0);
    }

    /// Per-keystroke cost over the whole index. Generous so a debug build on
    /// a loaded machine passes; a release build runs each keystroke in well
    /// under a millisecond.
    #[test]
    fn a_keystroke_is_cheap() {
        let e = entries();
        let typed = "keybaord repeat rate";
        let start = std::time::Instant::now();
        let mut n = 0u32;
        for _ in 0..5 {
            for end in 1..=typed.len() {
                std::hint::black_box(search(&e, &typed[..end], LIMIT));
                n += 1;
            }
        }
        let per = start.elapsed() / n;
        assert!(
            per < std::time::Duration::from_millis(25),
            "{per:?} per keystroke"
        );
    }
}
