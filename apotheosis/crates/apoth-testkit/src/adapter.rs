// SPDX-License-Identifier: AGPL-3.0-only
//! The ONE module that touches `apoth-term` / `apoth-grid` types.
//!
//! `apoth-term` was being written concurrently with this crate, so every call below is
//! written against the names in T-03 §1 and T-02 §5 and marked `RECONCILE`: the build
//! phase aligns this file with the real API and nothing else in the crate changes.
//!
//! Assumed `apoth-term` surface:
//! * `TermConfig::new(rows, cols)` + `.app_version(major, minor, patch)`; `Term::new(cfg)`
//! * `Term::feed(&mut self, &[u8], Instant) -> Wake`
//! * `Term::resize(&mut self, rows, cols)`
//! * `Term::take_replies(&mut self) -> Vec<u8>`, `Term::take_events(&mut self) -> Vec<Event>`
//! * `Term::screen(&self) -> &apoth_grid::Screen` (active screen),
//!   `Term::primary(&self) -> &apoth_grid::Screen` (for history)
//! * `Term::cursor(&self) -> CursorState { row, col, visible, shape }`
//! * `Term::is_alt_screen`, `Term::sync_hold`
//! * `Screen::link_at(y, x) -> Option<(String, String)>` (id, uri) in `apoth-grid`
//! * `Event` variants named in [`event_text`]; text forms are listed in `fixture.rs`.

use std::time::{Duration, Instant};

use apoth_grid::{Attrs, Color, SnapRow, Underline, Wide};
use apoth_term::{Term, TermConfig};

use crate::runner::Driver;
use crate::snapshot::{CellInfo, CursorInfo, Snapshot, WideKind, row_text};

/// The real terminal under test.
#[derive(Debug)]
pub struct TermDriver {
    term: Term,
    epoch: Instant,
}

fn color_text(c: Color) -> String {
    match c {
        Color::Default => "default".into(),
        Color::Palette(n) => format!("p{n}"),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

fn underline_text(u: Underline) -> &'static str {
    match u {
        Underline::None => "none",
        Underline::Single => "single",
        Underline::Double => "double",
        Underline::Curly => "curly",
        Underline::Dotted => "dotted",
        Underline::Dashed => "dashed",
    }
}

fn attrs_text(a: Attrs) -> String {
    let mut v = Vec::new();
    for (bit, name) in [
        (Attrs::BOLD, "bold"),
        (Attrs::FAINT, "faint"),
        (Attrs::INVERSE, "inverse"),
        (Attrs::INVISIBLE, "invisible"),
        (Attrs::ITALIC, "italic"),
        (Attrs::BLINK, "blink"),
        (Attrs::OVERLINE, "overline"),
        (Attrs::STRIKE, "strike"),
    ] {
        if a.contains(bit) {
            v.push(name);
        }
    }
    v.sort_unstable();
    v.join(",")
}

/// Drain the term's event slot into the fixture vocabulary (see `fixture.rs`).
fn drain_events(term: &mut Term) -> Vec<String> {
    let ev = term.events();
    let mut v = Vec::new();
    if let Some(t) = ev.title.take() {
        v.push(format!("title:{t}"));
    }
    if let Some(c) = ev.cwd.take() {
        v.push(format!("cwd:{c}"));
    }
    if let Some(p) = ev.progress.take() {
        v.push(format!("progress:{}:{}", p.state, p.value));
    }
    for _ in 0..std::mem::take(&mut ev.bell) {
        v.push("bell".to_string());
    }
    for m in ev.marks.drain(..) {
        match m.exit {
            Some(e) => v.push(format!("mark:{};{e}", m.kind)),
            None => v.push(format!("mark:{}", m.kind)),
        }
    }
    for n in ev.notifications.drain(..) {
        v.push(format!("notify:{}", n.text));
    }
    for c in ev.clipboard.drain(..) {
        v.push(format!("clipboard-set:{}:{}", c.selection, c.data.len()));
    }
    if let Some(sel) = ev.clip_read.take() {
        v.push(format!("clipboard-read:{sel}"));
    }
    for u in ev.vars.drain(..) {
        match u.value {
            Some(val) => v.push(format!("var:{}={val}", u.name)),
            None => v.push(format!("var:{}", u.name)),
        }
    }
    for c in ev.commands.drain(..) {
        v.push(
            if c.finished {
                "command:finished"
            } else {
                "command:started"
            }
            .to_string(),
        );
    }
    ev.colors_changed = false;
    v
}

fn text_of(row: &SnapRow, c: &apoth_grid::SnapCell) -> String {
    if c.cluster_len > 0 {
        row.cluster(c)
            .iter()
            .filter_map(|&u| char::from_u32(u))
            .collect()
    } else if c.cp != 0 {
        char::from_u32(c.cp).map(String::from).unwrap_or_default()
    } else {
        String::new()
    }
}

fn wide_kind(w: Wide) -> WideKind {
    match w {
        Wide::Narrow => WideKind::Narrow,
        Wide::Wide => WideKind::Wide,
        Wide::SpacerTail => WideKind::Tail,
        Wide::SpacerHead => WideKind::Head,
    }
}

fn cells_of(screen: &apoth_grid::Screen, y: usize, scratch: &mut SnapRow) -> Vec<CellInfo> {
    screen.copy_row(y, scratch);
    scratch
        .cells
        .iter()
        .enumerate()
        .map(|(x, c)| CellInfo {
            ch: text_of(scratch, c),
            wide: wide_kind(c.wide),
            fg: color_text(c.style.fg),
            bg: color_text(c.style.bg),
            ulc: color_text(c.style.ul_color),
            ul: underline_text(c.style.underline).into(),
            attrs: attrs_text(c.style.attrs),
            link: if c.link {
                screen.link_at(y, x).map(|(_, uri)| uri)
            } else {
                None
            },
        })
        .collect()
}

impl Driver for TermDriver {
    fn create(rows: usize, cols: usize, version: Option<&str>) -> Self {
        let mut cfg = TermConfig {
            rows,
            cols,
            ..TermConfig::default()
        };
        if let Some(v) = version {
            let mut it = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
            cfg.version = (
                it.next().unwrap_or(0),
                it.next().unwrap_or(0),
                it.next().unwrap_or(0),
            );
        }
        TermDriver {
            term: Term::new(cfg),
            epoch: Instant::now(),
        }
    }

    fn feed(&mut self, bytes: &[u8], at_ms: u64) {
        let _ = self
            .term
            .feed(bytes, self.epoch + Duration::from_millis(at_ms));
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        let _ = self.term.resize(rows, cols);
    }

    fn take_replies(&mut self) -> Vec<u8> {
        self.term.replies().take_all()
    }

    fn take_events(&mut self) -> Vec<String> {
        drain_events(&mut self.term)
    }

    fn snapshot(&self) -> Snapshot {
        let screen = self.term.grid();
        let (rows, cols) = (screen.rows(), screen.cols());
        let mut scratch = SnapRow::default();
        let cells: Vec<Vec<CellInfo>> = (0..rows)
            .map(|y| cells_of(screen, y, &mut scratch))
            .collect();
        let primary = self.term.primary();
        let mut scrollback = Vec::new();
        for i in 0..primary.history_rows() {
            if primary.copy_abs_row(i, &mut scratch) {
                let text: Vec<CellInfo> = scratch
                    .cells
                    .iter()
                    .map(|c| CellInfo {
                        ch: text_of(&scratch, c),
                        wide: wide_kind(c.wide),
                        ..CellInfo::blank()
                    })
                    .collect();
                scrollback.push(row_text(&text));
            }
        }
        let c = self.term.cursor();
        Snapshot {
            rows,
            cols,
            cells,
            cursor: CursorInfo {
                row: c.y,
                col: c.x,
                visible: c.visible,
                // DECSCUSR parameter: block 1/2, underline 3/4, bar 5/6 (odd = blinking).
                shape: c.style.shape * 2 + if c.style.blink { 1 } else { 2 },
            },
            scrollback,
            images: self
                .term
                .placements()
                .iter()
                .map(|p| {
                    format!(
                        "{} row={} col={} cols={} rows={} z={}",
                        if p.sixel { "sixel" } else { "kitty" },
                        p.row,
                        p.col,
                        p.cols,
                        p.rows,
                        p.z
                    )
                })
                .collect(),
            alt: self.term.on_alternate(),
            hold: self.term.presentation_hold(),
        }
    }

    fn has_feature(&self, name: &str) -> bool {
        name == "sixel"
    }
}
