// SPDX-License-Identifier: AGPL-3.0-only
//! The command palette's model (T-14 §9): modes, sources, fuzzy scoring and selection. The
//! sheet itself is `apoth_ui::palette`; this decides what it lists.
//!
//! - `actions` lists every registry action with its title and current binding. An action
//!   with no required argument runs on Return; one that needs arguments fills the query
//!   with its id, and a query that parses as a full call (`pane.split down`) is offered as
//!   its own first row.
//! - `tabs` lists the window's tabs.
//! - A query starting with `>` lists the modes; picking one switches to it.
//!
//! Scoring is a local-alignment matcher in the fzf-v2 class written from its published
//! description: every query character must match in order; matches score, gaps cost (an
//! opening gap more than an extension), matches at word boundaries and runs of consecutive
//! matches earn bonuses, and the first query character's bonus counts double. Case-insensitive
//! for ASCII. The query is user text: bounded, drawn, never logged.

use apoth_action::{Action, REGISTRY};

use crate::binds::Keymap;

/// Longest query, in bytes (T-18 §5: every string sink has a budget).
pub(crate) const MAX_QUERY: usize = 256;
/// Longest candidate scored, in bytes; longer titles are scored on their prefix.
const MAX_TEXT: usize = 256;

const SCORE_MATCH: i32 = 16;
const GAP_START: i32 = -3;
const GAP_EXTEND: i32 = -1;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSECUTIVE: i32 = 4;
const FIRST_CHAR_MULT: i32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Actions,
    Tabs,
}

impl Mode {
    const ALL: [Mode; 2] = [Mode::Actions, Mode::Tabs];

    fn name(self) -> &'static str {
        match self {
            Mode::Actions => "actions",
            Mode::Tabs => "tabs",
        }
    }

    pub(crate) fn placeholder(self) -> &'static str {
        match self {
            Mode::Actions => "Run an action (> for modes)",
            Mode::Tabs => "Go to tab (> for modes)",
        }
    }
}

/// What picking a row does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Pick {
    Run(Action),
    /// Replace the query (an action that needs arguments).
    Fill(String),
    /// Show tab `index` (0-based) of the window.
    Tab(usize),
    Mode(Mode),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) title: String,
    pub(crate) hint: Option<String>,
    pub(crate) pick: Pick,
}

/// A tab as the palette lists it.
#[derive(Clone, Debug)]
pub(crate) struct TabItem {
    pub(crate) title: String,
    pub(crate) active: bool,
}

#[derive(Debug)]
pub(crate) struct Palette {
    pub(crate) mode: Mode,
    pub(crate) query: String,
    /// Filtered rows, best first.
    pub(crate) rows: Vec<Row>,
    pub(crate) selected: usize,
    /// First row in view.
    pub(crate) scroll: usize,
}

fn lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

fn class(b: u8) -> u8 {
    match b {
        b'a'..=b'z' => 1,
        b'A'..=b'Z' => 2,
        b'0'..=b'9' => 3,
        _ if b >= 0x80 => 1,
        _ => 0,
    }
}

/// Bonus for a match at `i` given the byte before it.
fn bonus(text: &[u8], i: usize) -> i32 {
    let prev = if i == 0 { 0 } else { class(text[i - 1]) };
    let cur = class(text[i]);
    if cur == 0 {
        return 0;
    }
    match (prev, cur) {
        (0, _) => BONUS_BOUNDARY,
        (1, 2) | (3, 1) | (3, 2) => BONUS_CAMEL,
        _ => 0,
    }
}

/// Score of `query` against `text`, `None` when not every query byte matches in order.
/// Bytes compare ASCII case-insensitively; a multi-byte character matches byte for byte.
pub(crate) fn score(query: &str, text: &str) -> Option<i32> {
    let q = query.as_bytes();
    let t = &text.as_bytes()[..text.len().min(MAX_TEXT)];
    if q.is_empty() {
        return Some(0);
    }
    if q.len() > t.len() {
        return None;
    }
    // Quick reject: an in-order subsequence must exist.
    let mut k = 0;
    for &b in t {
        if k < q.len() && lower(b) == lower(q[k]) {
            k += 1;
        }
    }
    if k < q.len() {
        return None;
    }
    // prev[j] / cur[j]: best score with query[..=i] aligned so that query[i] matches t[j].
    // `gap` is the best score of an earlier query byte matched at k <= j - 2, charged for the
    // j - 1 - k bytes skipped since (an opening gap, then extensions).
    const NEG: i32 = i32::MIN / 4;
    let n = t.len();
    // A consecutive chunk carries the bonus of the byte that started it (`chunk`), so "tab"
    // in "tab.new" outranks the same letters each at a word start but scattered.
    let mut prev = vec![NEG; n];
    let mut cur = vec![NEG; n];
    let mut chunk_prev = vec![0i32; n];
    let mut chunk_cur = vec![0i32; n];
    for (i, &qc) in q.iter().enumerate() {
        let qc = lower(qc);
        let mut gap = NEG;
        for j in 0..n {
            cur[j] = NEG;
            chunk_cur[j] = 0;
            if lower(t[j]) == qc {
                let b = bonus(t, j);
                if i == 0 {
                    cur[j] = SCORE_MATCH + b * FIRST_CHAR_MULT;
                    chunk_cur[j] = b;
                } else {
                    let (run, run_chunk) = if j > 0 && prev[j - 1] > NEG {
                        let c = chunk_prev[j - 1];
                        let eff = b.max(c).max(BONUS_CONSECUTIVE);
                        let c = if b >= BONUS_BOUNDARY && b > c { b } else { c };
                        (prev[j - 1] + SCORE_MATCH + eff, c)
                    } else {
                        (NEG, 0)
                    };
                    let skip = if gap > NEG {
                        gap + SCORE_MATCH + b
                    } else {
                        NEG
                    };
                    if run >= skip {
                        cur[j] = run;
                        chunk_cur[j] = run_chunk;
                    } else {
                        cur[j] = skip;
                        chunk_cur[j] = b;
                    }
                }
            }
            if i > 0 {
                let extend = if gap > NEG { gap + GAP_EXTEND } else { NEG };
                let open = if j >= 1 && prev[j - 1] > NEG {
                    prev[j - 1] + GAP_START
                } else {
                    NEG
                };
                gap = extend.max(open);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
        std::mem::swap(&mut chunk_prev, &mut chunk_cur);
    }
    prev.iter().copied().max().filter(|s| *s > NEG)
}

impl Palette {
    pub(crate) fn new(mode: Mode, query: &str) -> Palette {
        let mut p = Palette {
            mode,
            query: String::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
        };
        p.query = truncate(query, MAX_QUERY).to_owned();
        p
    }

    /// Rebuild the rows for the current query.
    pub(crate) fn refresh(&mut self, km: &Keymap, tabs: &[TabItem]) {
        let q = self.query.trim_start();
        let mut rows: Vec<(i32, usize, Row)> = Vec::new();
        if let Some(rest) = q.strip_prefix('>') {
            let rest = rest.trim();
            for (i, m) in Mode::ALL.iter().enumerate() {
                if let Some(s) = score(rest, m.name()) {
                    rows.push((
                        s,
                        i,
                        Row {
                            title: m.name().to_owned(),
                            hint: (*m == self.mode).then(|| "current".to_owned()),
                            pick: Pick::Mode(*m),
                        },
                    ));
                }
            }
        } else {
            match self.mode {
                Mode::Actions => self.action_rows(q, km, &mut rows),
                Mode::Tabs => {
                    for (i, t) in tabs.iter().enumerate() {
                        if let Some(s) = score(q, &t.title) {
                            rows.push((
                                s,
                                i,
                                Row {
                                    title: t.title.clone(),
                                    hint: Some(if t.active {
                                        format!("Tab {} · shown", i + 1)
                                    } else {
                                        format!("Tab {}", i + 1)
                                    }),
                                    pick: Pick::Tab(i),
                                },
                            ));
                        }
                    }
                }
            }
        }
        // Best score first; equal scores keep source order.
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.rows = rows.into_iter().map(|(_, _, r)| r).collect();
        self.selected = 0;
        self.scroll = 0;
    }

    fn action_rows(&self, q: &str, km: &Keymap, rows: &mut Vec<(i32, usize, Row)>) {
        // A full call with arguments is offered first.
        if q.contains(' ')
            && let Ok(a) = Action::parse(q)
        {
            rows.push((
                i32::MAX,
                0,
                Row {
                    title: format!("Run {}", q.trim()),
                    hint: km.binding_of(&a),
                    pick: Pick::Run(a),
                },
            ));
        }
        for (i, info) in REGISTRY.iter().enumerate() {
            let s = match (score(q, info.title), score(q, info.id)) {
                (Some(a), Some(b)) => a.max(b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => continue,
            };
            let needs_args = info.params.iter().any(|p| p.required);
            let (pick, hint) = if needs_args {
                (
                    Pick::Fill(format!("{} ", info.id)),
                    Some(info.id.to_owned()),
                )
            } else {
                match Action::parse(info.id) {
                    Ok(a) => {
                        let h = km.binding_of(&a);
                        (Pick::Run(a), h)
                    }
                    Err(_) => (
                        Pick::Fill(format!("{} ", info.id)),
                        Some(info.id.to_owned()),
                    ),
                }
            };
            rows.push((
                s,
                i + 1,
                Row {
                    title: info.title.to_owned(),
                    hint,
                    pick,
                },
            ));
        }
    }

    /// Type text into the query (control characters dropped, the byte budget kept).
    pub(crate) fn type_text(&mut self, text: &str) -> bool {
        let mut changed = false;
        for c in text.chars() {
            if c.is_control() || self.query.len() + c.len_utf8() > MAX_QUERY {
                continue;
            }
            self.query.push(c);
            changed = true;
        }
        changed
    }

    pub(crate) fn backspace(&mut self) -> bool {
        self.query.pop().is_some()
    }

    /// Delete the last word (and the separators after it).
    pub(crate) fn delete_word(&mut self) -> bool {
        let t = self.query.trim_end_matches([' ', '.', '-']);
        let cut = t.rfind([' ', '.', '-']).map_or(0, |i| i + 1);
        let changed = cut != self.query.len();
        self.query.truncate(cut);
        changed
    }

    pub(crate) fn set_query(&mut self, q: &str) {
        self.query = truncate(q, MAX_QUERY).to_owned();
    }

    /// Move the selection by `d` rows (clamped), keeping it in a window of `rows` rows.
    pub(crate) fn move_by(&mut self, d: i32, window: usize) {
        if self.rows.is_empty() {
            return;
        }
        let last = self.rows.len() - 1;
        let s = (self.selected as i64 + i64::from(d)).clamp(0, last as i64) as usize;
        self.select(s, window);
    }

    pub(crate) fn select(&mut self, i: usize, window: usize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = i.min(self.rows.len() - 1);
        let window = window.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + window {
            self.scroll = self.selected + 1 - window;
        }
    }

    /// The rows in view and the selected index among them.
    pub(crate) fn window(&self, window: usize) -> (&[Row], Option<usize>) {
        let end = (self.scroll + window.max(1)).min(self.rows.len());
        let start = self.scroll.min(end);
        let sel = (self.selected >= start && self.selected < end).then(|| self.selected - start);
        (&self.rows[start..end], sel)
    }

    pub(crate) fn picked(&self) -> Option<&Pick> {
        self.rows.get(self.selected).map(|r| &r.pick)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn km() -> Keymap {
        Keymap {
            root: Keymap::defaults(),
            ..Keymap::default()
        }
    }

    #[test]
    fn scoring_requires_order_and_prefers_boundaries() {
        assert!(score("nt", "New tab").is_some());
        assert!(score("tn", "New tab").is_none());
        assert!(score("", "anything") == Some(0));
        assert!(score("abcd", "abc").is_none());
        // Word starts beat letters in the middle of a word.
        let start = score("sp", "Split pane").unwrap();
        let mid = score("sp", "Wasp").unwrap();
        assert!(start > mid, "{start} <= {mid}");
        // Consecutive beats scattered.
        let run = score("tab", "tab.new").unwrap();
        let scattered = score("tab", "the a b").unwrap();
        assert!(run > scattered, "{run} <= {scattered}");
        // Case-insensitive for ASCII.
        assert_eq!(score("NEW", "new tab"), score("new", "new tab"));
    }

    #[test]
    fn scoring_is_bounded() {
        let long = "a".repeat(10_000);
        assert!(score("aaa", &long).is_some());
        let q = "b".repeat(300);
        assert!(score(&q, &long).is_none());
    }

    #[test]
    fn every_action_is_listed_with_its_binding() {
        let mut p = Palette::new(Mode::Actions, "");
        p.refresh(&km(), &[]);
        assert_eq!(p.rows.len(), REGISTRY.len());
        let new_tab = p.rows.iter().find(|r| r.title == "New tab").unwrap();
        assert_eq!(new_tab.hint.as_deref(), Some("ctrl+shift+t"));
        assert_eq!(new_tab.pick, Pick::Run(Action::parse("tab.new").unwrap()));
        // An action that needs arguments fills the query instead of running.
        let sel = p.rows.iter().find(|r| r.title == "Select tab").unwrap();
        assert_eq!(sel.pick, Pick::Fill("tab.select ".into()));
    }

    #[test]
    fn query_filters_and_ranks() {
        let mut p = Palette::new(Mode::Actions, "split");
        p.refresh(&km(), &[]);
        assert_eq!(p.rows[0].title, "Split pane");
        p.set_query("zzzzqqq");
        p.refresh(&km(), &[]);
        assert!(p.rows.is_empty());
        assert_eq!(p.picked(), None);
    }

    #[test]
    fn a_full_call_is_offered_first() {
        let mut p = Palette::new(Mode::Actions, "pane.split down");
        p.refresh(&km(), &[]);
        assert_eq!(
            p.rows[0].pick,
            Pick::Run(Action::parse("pane.split down").unwrap())
        );
        assert_eq!(p.rows[0].hint.as_deref(), Some("ctrl+shift+e"));
    }

    #[test]
    fn gt_lists_modes_and_tabs_mode_lists_tabs() {
        let tabs = [
            TabItem {
                title: "zsh".into(),
                active: true,
            },
            TabItem {
                title: "htop".into(),
                active: false,
            },
        ];
        let mut p = Palette::new(Mode::Actions, ">ta");
        p.refresh(&km(), &tabs);
        assert_eq!(p.rows.len(), 1);
        assert_eq!(p.rows[0].pick, Pick::Mode(Mode::Tabs));
        let mut p = Palette::new(Mode::Tabs, "ht");
        p.refresh(&km(), &tabs);
        assert_eq!(p.rows[0].pick, Pick::Tab(1));
        assert_eq!(p.rows[0].hint.as_deref(), Some("Tab 2"));
    }

    #[test]
    fn typing_is_bounded_and_drops_controls() {
        let mut p = Palette::new(Mode::Actions, "");
        assert!(p.type_text("a\x1bb\n"));
        assert_eq!(p.query, "ab");
        p.type_text(&"x".repeat(1000));
        assert_eq!(p.query.len(), MAX_QUERY);
        p.set_query("pane.split ");
        assert!(p.delete_word());
        assert_eq!(p.query, "pane.");
        assert!(p.delete_word());
        assert_eq!(p.query, "");
        assert!(!p.backspace());
        let long = "é".repeat(200);
        assert!(Palette::new(Mode::Actions, &long).query.len() <= MAX_QUERY);
    }

    #[test]
    fn selection_scrolls_the_window() {
        let mut p = Palette::new(Mode::Actions, "");
        p.refresh(&km(), &[]);
        p.move_by(12, 10);
        assert_eq!(p.selected, 12);
        let (rows, sel) = p.window(10);
        assert_eq!(rows.len(), 10);
        assert_eq!(sel, Some(9));
        p.move_by(-100, 10);
        assert_eq!((p.selected, p.scroll), (0, 0));
        p.move_by(100_000, 10);
        assert_eq!(p.selected, p.rows.len() - 1);
    }
}
