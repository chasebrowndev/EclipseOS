// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's start menu, `bar.launcher-style "menu"`: the eclipse button
//! widens into a search field and the pill grows a panel of applications
//! under (or, on a bottom bar, over) it.
//!
//! The list is `ec-launcher`'s — the same `.desktop` scan, the same
//! substring match and the same ranking — reimplemented over
//! `ec_services::apps` rather than imported, because the launcher is its
//! own package and the bar must not depend on it (ADR 0052).
//!
//! This file is the state and its pure rules; the bar surface's size, input
//! region and keyboard mode are `app.rs`'s, and the drawing is `view.rs`'s.

use std::time::Instant;

use ec_services::apps::{self, Entry};
use ec_services::frecency::{self, Store};
use ec_ui::motion::{Animated, Motion};
use ec_ui::tokens::bar;

use crate::conn::MenuConfig;

/// The search field's widget id: the menu focuses it on opening, so the
/// first key typed after the click or the keybind is part of the query.
pub const INPUT_ID: &str = "bar-menu-query";

/// One box of an input region: `(x, y, width, height)`, surface-local.
pub type Rect = (i32, i32, i32, i32);

/// Longer than any surface. A box this long is cut to the surface by the
/// compositor (the input region by the protocol, the glass shape by abyss's
/// `Shape::from_region`), so a box drawn with it holds at every height the
/// surface passes through and never has to be sent again.
pub const BIG: i32 = 1 << 15;

pub struct Menu {
    /// Where the menu is going: open or closed.
    pub open: bool,
    pub query: String,
    /// An index into `matched`.
    pub selected: usize,
    /// Every application, scanned when the menu opens.
    pub entries: Vec<Entry>,
    /// Indices into `entries` that answer `query`, best first.
    pub matched: Vec<usize>,
    /// Why the last launch did not happen, verbatim, until the query changes.
    pub problem: Option<String>,
    /// How far open, `0..=1`: the cell's width and the panel's extent both
    /// follow it, so the field and the panel arrive together.
    pub reveal: Animated,
    /// The input-region boxes last sent to the surface; the same boxes are
    /// never sent twice (see `app::sync_region`).
    pub region: Option<Vec<Rect>>,
    /// `misc.terminal-command`, read when the menu opens.
    pub term: Option<String>,
    /// `launcher.menu.max-rows`, at least one, taken when the menu opens: the
    /// panel's height follows it, and a height that changed while the panel
    /// is out would move the rows under the pointer.
    pub rows: usize,
    /// `launcher.search.*`, taken when the menu opens.
    pub search: apps::Search,
    /// What the human has launched, re-read each time the menu opens so a
    /// launch from the centred launcher shows here; empty with the flag off.
    pub usage: Store,
}

impl Menu {
    pub fn new(motion: Motion) -> Self {
        Menu {
            open: false,
            query: String::new(),
            selected: 0,
            entries: Vec::new(),
            matched: Vec::new(),
            problem: None,
            reveal: Animated::new(0.0, motion),
            region: None,
            term: None,
            rows: bar::MENU_ROWS,
            search: apps::Search::default(),
            usage: Store::default(),
        }
    }

    /// Open on a fresh scan and an empty query: every application, A to Z.
    pub fn open(&mut self, entries: Vec<Entry>, term: Option<String>, cfg: MenuConfig, now: Instant) {
        self.open = true;
        self.entries = entries;
        self.term = term;
        self.rows = cfg.rows.max(1);
        self.search = cfg.search;
        // Tests never read the human's real history.
        self.usage = if cfg.search.frecency && !cfg!(test) {
            Store::load()
        } else {
            Store::default()
        };
        self.query.clear();
        self.selected = 0;
        self.problem = None;
        self.refilter();
        self.reveal.set_target(1.0, now);
    }

    pub fn close(&mut self, now: Instant) {
        self.open = false;
        self.reveal.set_target(0.0, now);
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.selected = 0;
        self.problem = None;
        self.refilter();
    }

    /// Move the selection by `delta` rows, stopping at either end: a list
    /// that wraps sends one arrow too many to the far end of the alphabet.
    ///
    /// A refusal is about the row that refused, so moving off it clears it —
    /// as the launcher does. With the hints off the refusal is drawn on the
    /// selected row, and one left in place would land on the next row down.
    pub fn step(&mut self, delta: i32) {
        self.problem = None;
        let last = self.matched.len().saturating_sub(1) as i64;
        self.selected = (self.selected as i64 + i64::from(delta)).clamp(0, last) as usize;
    }

    /// The entry Enter runs, if any.
    pub fn current(&self) -> Option<&Entry> {
        self.matched.get(self.selected).map(|&i| &self.entries[i])
    }

    /// Note the selected entry as launched, for the next open's ranking. Call
    /// it once the launch succeeded; a no-op with the flag off, and under test
    /// so a test never writes the human's history.
    pub fn record_launch(&self) {
        if self.search.frecency && !cfg!(test) {
            if let Some(entry) = self.current() {
                apps::record_launch(entry, &self.entries);
            }
        }
    }

    /// How far the drawn rows have scrolled: just enough to keep the
    /// selection on the last row.
    pub fn scroll(&self) -> usize {
        self.selected.saturating_sub(self.rows - 1)
    }

    /// Drawn at all: open, or still closing.
    pub fn showing(&self) -> bool {
        self.open || self.reveal.value() > 0.0
    }

    /// The panel's height fully out: `bar::panel_h` for the key-hint
    /// setting and this menu's rows.
    pub fn full(&self, hints: bool) -> f32 {
        bar::panel_h(hints, self.rows)
    }

    /// How much of the panel the surface holds right now, in whole pixels.
    /// The slide and the view both read [`Menu::full`], so the panel never
    /// rolls out past its end or stops short of it.
    pub fn extent(&self, hints: bool) -> u32 {
        (self.reveal.value().clamp(0.0, 1.0) * self.full(hints)).round() as u32
    }

    fn refilter(&mut self) {
        self.matched = apps::search_with(
            &self.entries,
            &self.query,
            &self.search,
            &self.usage,
            frecency::now(),
        );
        self.selected = self.selected.min(self.matched.len().saturating_sub(1));
    }
}

/// The input region of a bar with its menu out, surface-local: the panel's
/// box, and the pill from where the panel's straight top edge still covers
/// the pill's rounded left end. Abyss draws the glass as the smooth union of
/// these boxes, so this is also the shape the blur takes.
///
/// On a top bar neither box depends on the surface's height — the panel
/// runs off the bottom and is cut there — so one region holds for the whole
/// slide. A bottom bar's pill sits at `height - PILL_H`, which moves.
pub fn region(top: bool, radius: f32, height: u32) -> Vec<Rect> {
    let rp = radius.min(bar::PILL_H / 2.0);
    let rb = radius.min(bar::PANEL_W / 2.0);
    // Far enough left that the pill's own rounded corners fall under the
    // panel's straight top, and the panel's rounded corner under the pill's.
    let x0 = (bar::PANEL_W - rb - rp).floor().max(0.0) as i32;
    let panel_w = bar::PANEL_W as i32;
    let pill_h = bar::PILL_H as i32;
    let h = height as i32;
    if top {
        vec![(0, 0, panel_w, BIG), (x0, 0, BIG, pill_h)]
    } else {
        vec![(0, 0, panel_w, h), (x0, h - pill_h, BIG, pill_h)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, name: &str) -> Entry {
        Entry {
            id: format!("{id}.desktop"),
            name: name.to_owned(),
            comment: None,
            argv: vec![id.to_owned()],
            terminal: false,
            keywords: Vec::new(),
        }
    }

    fn menu() -> Menu {
        let mut m = Menu::new(Motion::default());
        m.open(
            vec![
                entry("files", "Files"),
                entry("firefox", "Firefox"),
                entry("gimp", "GIMP"),
                entry("foot", "Foot"),
            ],
            None,
            MenuConfig::default(),
            Instant::now(),
        );
        m
    }

    fn names(m: &Menu) -> Vec<&str> {
        m.matched.iter().map(|&i| m.entries[i].name.as_str()).collect()
    }

    #[test]
    fn an_empty_query_lists_everything_a_to_z() {
        assert_eq!(names(&menu()), ["Files", "Firefox", "Foot", "GIMP"]);
    }

    #[test]
    fn an_exact_name_ranks_above_a_prefix_above_a_substring() {
        let mut m = menu();
        m.set_query("f".to_owned());
        assert_eq!(names(&m), ["Files", "Firefox", "Foot"]);
        m.set_query("foot".to_owned());
        assert_eq!(names(&m), ["Foot"]);
        m.set_query("i".to_owned());
        // No name starts with "i": the substring matches, by name.
        assert_eq!(names(&m), ["Files", "Firefox", "GIMP"]);
    }

    #[test]
    fn the_selection_stops_at_either_end_and_resets_on_typing() {
        let mut m = menu();
        m.step(-1);
        assert_eq!(m.selected, 0);
        m.step(10);
        assert_eq!(m.selected, 3);
        m.set_query("fi".to_owned());
        assert_eq!(m.selected, 0);
        assert_eq!(m.current().map(|e| e.name.as_str()), Some("Files"));
        m.set_query("zzz".to_owned());
        assert!(m.current().is_none());
        m.step(1);
        assert_eq!(m.selected, 0);
    }

    #[test]
    fn a_top_region_does_not_depend_on_the_height() {
        assert_eq!(region(true, 20.0, 40), region(true, 20.0, 400));
        // A bottom bar's pill rides the surface's far edge.
        let [_, pill] = region(false, 20.0, 400)[..] else {
            panic!("two boxes")
        };
        assert_eq!(pill.1, 400 - bar::PILL_H as i32);
    }

    #[test]
    fn the_pill_box_starts_under_the_panels_straight_top() {
        let [panel, pill] = region(true, 20.0, 400)[..] else {
            panic!("two boxes")
        };
        // The pill's left corners sit over the panel, whose own corner sits
        // under the pill's straight run.
        assert!(pill.0 + (bar::PILL_H / 2.0) as i32 <= panel.2 - 20);
    }

    #[test]
    fn the_extent_follows_the_reveal() {
        let mut m = Menu::new(Motion::default());
        assert_eq!(m.extent(true), 0);
        assert!(!m.showing());
        m.reveal.snap(1.0);
        assert_eq!(m.extent(true), bar::panel_h(true, bar::MENU_ROWS).round() as u32);
        // Hints off, the slide ends where the shorter panel does.
        assert_eq!(
            m.extent(false),
            bar::panel_h(false, bar::MENU_ROWS).round() as u32
        );
        assert!(m.extent(false) < m.extent(true));
    }

    #[test]
    fn the_panel_is_as_tall_as_its_configured_rows() {
        let mut m = Menu::new(Motion::default());
        let cfg = MenuConfig {
            rows: 4,
            ..MenuConfig::default()
        };
        m.open(Vec::new(), None, cfg, Instant::now());
        m.reveal.snap(1.0);
        assert_eq!(m.extent(false), bar::panel_h(false, 4).round() as u32);
        // Zero rows would leave nothing to select; one is the floor.
        m.open(Vec::new(), None, MenuConfig { rows: 0, ..cfg }, Instant::now());
        assert_eq!(m.rows, 1);
    }

    #[test]
    fn moving_off_a_refused_row_clears_the_refusal() {
        let mut m = menu();
        m.problem = Some("cannot start Files: refused".to_owned());
        m.step(1);
        assert!(m.problem.is_none());
    }
}
