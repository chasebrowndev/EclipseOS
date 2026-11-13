// SPDX-License-Identifier: AGPL-3.0-only
//! The render thread's view state (T-02 §5-§6, T-05 §5): which rows of the scrollback are
//! shown, the selection with its highlight spans, and the IME preedit drawn at the cursor.
//! Main never takes the term lock, so every selection and scroll command crosses to the render
//! thread as a [`ViewCmd`] and is applied here under the lock the frame already holds.
//!
//! Selection text and preedit text are user content: nothing here logs them.

use apoth_grid::{
    Attrs, ExtractOpts, FrameSnapshot, Pin, SelKind, Selection, Side, SnapCell, SnapRow, Style,
    Underline, Wide, WordChars,
};
use apoth_render::colour::Rgb;
use apoth_render::scene::Highlight;
use apoth_term::Term;
use apoth_unicode::Widths;

/// Longest preedit kept, in characters (a longer string is cut: an IME never needs more).
const MAX_PREEDIT: usize = 512;

/// Where a copy goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyTarget {
    Clipboard,
    Primary,
}

/// A scroll request. Positive counts go toward older rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollBy {
    Lines(i32),
    Pages(i32),
    /// Reserved for user binds (T-11); the built-in table does not use it.
    #[allow(dead_code)]
    HalfPages(i32),
    Top,
    Bottom,
}

/// The IME preedit string and its active segment (byte offsets into `text`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preedit {
    pub(crate) text: String,
    pub(crate) begin: i32,
    pub(crate) end: i32,
}

/// Selection keys the view needs (live on reload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewSettings {
    pub(crate) word_extra: String,
    pub(crate) trim_trailing: bool,
}

/// Commands from main. Rows are viewport rows (0 = top of what is shown), columns are cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewCmd {
    Begin {
        kind: SelKind,
        row: usize,
        col: usize,
        side: Side,
    },
    Extend {
        row: usize,
        col: usize,
        side: Side,
    },
    /// The button went up: copy to `copy` when the selection is not empty.
    Finish {
        copy: Option<CopyTarget>,
    },
    Clear,
    All,
    Scroll(ScrollBy),
    /// Copy the selection; `clear` drops it afterwards (`selection.clear-on-copy`).
    Copy {
        target: CopyTarget,
        clear: bool,
    },
    Settings(ViewSettings),
    Preedit(Option<Preedit>),
}

/// Text taken from the selection, for main to offer on the seat.
pub(crate) struct Copied {
    pub(crate) target: CopyTarget,
    pub(crate) text: String,
}

/// Which snapshot the frame draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The live snapshot with the damage `snapshot_dirty` reported.
    Live,
    /// The live snapshot, redrawn in full: the previous frame drew the composed one.
    LiveFull,
    /// The composed snapshot (scrolled back or with a preedit); `full` when it was rebuilt.
    Composed { full: bool },
}

pub(crate) struct View {
    /// Rows scrolled back from the live screen.
    offset: usize,
    /// The selection and whether it belongs to the alternate screen.
    sel: Option<(bool, Selection)>,
    wc: WordChars,
    opts: ExtractOpts,
    preedit: Option<Preedit>,
    /// The row at the top of a scrolled-back view: it keeps the same text on screen while
    /// output grows the history or evicts the oldest pages.
    anchor: Option<Pin>,
    last_alt: bool,
    /// The composed snapshot is stale.
    dirty: bool,
    /// The previous frame drew the composed snapshot.
    was_composed: bool,
    comp: FrameSnapshot,
    highlights: Vec<Highlight>,
    prev_highlights: Vec<Highlight>,
    widths: Widths,
}

impl View {
    pub(crate) fn new() -> View {
        View {
            offset: 0,
            sel: None,
            wc: WordChars::new(""),
            opts: ExtractOpts::default(),
            preedit: None,
            anchor: None,
            last_alt: false,
            dirty: true,
            was_composed: false,
            comp: FrameSnapshot::new(),
            highlights: Vec::new(),
            prev_highlights: Vec::new(),
            widths: Widths::default(),
        }
    }

    pub(crate) fn offset(&self) -> usize {
        self.offset
    }

    #[cfg(test)]
    pub(crate) fn has_selection(&self) -> bool {
        self.sel.is_some()
    }

    /// The composed snapshot (valid after `sync` returned `Source::Composed`).
    pub(crate) fn composed(&self) -> &FrameSnapshot {
        &self.comp
    }

    /// Highlight spans of the current frame, viewport rows.
    pub(crate) fn highlights(&self) -> &[Highlight] {
        &self.highlights
    }

    /// Rows whose baked highlight differs from the previous frame (the caller invalidates them).
    pub(crate) fn changed_rows(&self) -> Vec<u32> {
        let mut rows: Vec<u32> = Vec::new();
        for h in &self.prev_highlights {
            if !self.highlights.contains(h) {
                rows.push(h.row as u32);
            }
        }
        for h in &self.highlights {
            if !self.prev_highlights.contains(h) {
                rows.push(h.row as u32);
            }
        }
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// Every row that carries a highlight now or did in the previous frame.
    pub(crate) fn highlight_rows(&self) -> Vec<u32> {
        let mut rows: Vec<u32> = self
            .highlights
            .iter()
            .chain(&self.prev_highlights)
            .map(|h| h.row as u32)
            .collect();
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    fn top_abs(&self, t: &Term) -> usize {
        t.grid().history_rows().saturating_sub(self.offset)
    }

    fn drop_selection(&mut self, t: &mut Term) {
        if let Some((alt, s)) = self.sel.take()
            && alt == t.on_alternate()
        {
            t.grid_mut().selection_clear(s);
        }
    }

    fn text(&self, t: &Term) -> Option<String> {
        let (alt, s) = self.sel.as_ref()?;
        if *alt != t.on_alternate() {
            return None;
        }
        let mut out = String::new();
        if t.grid().selection_text(s, &self.wc, &self.opts, &mut out) && !out.is_empty() {
            Some(out)
        } else {
            None
        }
    }

    /// Apply one command under the term lock. Returns text to offer on the seat.
    pub(crate) fn apply(&mut self, t: &mut Term, cmd: ViewCmd) -> Option<Copied> {
        match cmd {
            ViewCmd::Begin {
                kind,
                row,
                col,
                side,
            } => {
                self.drop_selection(t);
                let abs = self.top_abs(t) + row;
                let alt = t.on_alternate();
                self.sel = t
                    .grid_mut()
                    .selection_begin(kind, abs, col, side)
                    .map(|s| (alt, s));
                self.dirty = true;
            }
            ViewCmd::Extend { row, col, side } => {
                let abs = self.top_abs(t) + row;
                if let Some((alt, mut s)) = self.sel.take() {
                    if alt == t.on_alternate() {
                        t.grid_mut().selection_extend(&mut s, abs, col, side);
                    }
                    self.sel = Some((alt, s));
                }
            }
            ViewCmd::Finish { copy } => {
                if let Some(target) = copy
                    && let Some(text) = self.text(t)
                {
                    return Some(Copied { target, text });
                }
            }
            ViewCmd::Clear => self.drop_selection(t),
            ViewCmd::All => {
                self.drop_selection(t);
                let g = t.grid();
                let last_abs = (g.history_rows() + g.rows()).saturating_sub(1);
                let last_col = g.cols().saturating_sub(1);
                let alt = t.on_alternate();
                if let Some(mut s) = t
                    .grid_mut()
                    .selection_begin(SelKind::Simple, 0, 0, Side::Left)
                {
                    t.grid_mut()
                        .selection_extend(&mut s, last_abs, last_col, Side::Right);
                    self.sel = Some((alt, s));
                }
            }
            ViewCmd::Scroll(by) => {
                let rows = t.rows().max(1);
                let hist = t.grid().history_rows();
                let cur = i64::try_from(self.offset).unwrap_or(i64::MAX);
                let want = match by {
                    ScrollBy::Lines(n) => cur + i64::from(n),
                    ScrollBy::Pages(n) => cur + i64::from(n) * rows as i64,
                    ScrollBy::HalfPages(n) => cur + i64::from(n) * (rows / 2).max(1) as i64,
                    ScrollBy::Top => hist as i64,
                    ScrollBy::Bottom => 0,
                };
                let new = usize::try_from(want.clamp(0, hist as i64)).unwrap_or(0);
                if new != self.offset {
                    self.offset = new;
                    self.anchor = None;
                    self.dirty = true;
                }
            }
            ViewCmd::Copy { target, clear } => {
                let text = self.text(t);
                if clear && text.is_some() {
                    self.drop_selection(t);
                }
                return text.map(|text| Copied { target, text });
            }
            ViewCmd::Settings(s) => {
                self.wc = WordChars::new(&s.word_extra);
                self.opts = ExtractOpts {
                    trim_trailing: s.trim_trailing,
                };
            }
            ViewCmd::Preedit(p) => {
                self.preedit = p.map(|mut p| {
                    if p.text.chars().count() > MAX_PREEDIT {
                        p.text = p.text.chars().take(MAX_PREEDIT).collect();
                    }
                    p
                });
                self.dirty = true;
            }
        }
        None
    }

    /// Bring the view in line with the term for one frame. `live` is the snapshot
    /// `snapshot_dirty` just updated; `output` says that call reported any change.
    pub(crate) fn sync(&mut self, t: &Term, live: &FrameSnapshot, output: bool) -> Source {
        let alt = t.on_alternate();
        if alt != self.last_alt {
            self.last_alt = alt;
            self.offset = 0;
            self.anchor = None;
            self.dirty = true;
        }
        let hist = t.grid().history_rows();
        if self.offset > 0
            && let Some(a) = self.anchor
        {
            // Keep the same text on screen: follow the anchor row through history growth and
            // head-page eviction. An evicted anchor leaves the view at the oldest retained row.
            let moved = match t.grid().pin_abs(&a) {
                Some((abs, _)) => hist.saturating_sub(abs),
                None => hist,
            };
            if moved != self.offset {
                self.offset = moved;
                self.dirty = true;
            }
        }
        self.offset = self.offset.min(hist);
        self.anchor = if self.offset > 0 {
            t.grid().pin_at(hist - self.offset, 0)
        } else {
            None
        };

        std::mem::swap(&mut self.highlights, &mut self.prev_highlights);
        self.highlights.clear();
        let top = hist - self.offset;
        let rows = t.rows();
        if let Some((salt, s)) = &self.sel
            && *salt == alt
            && let Some(r) = t.grid().selection_range(s, &self.wc)
        {
            let last = r.end.0.min(top + rows.saturating_sub(1));
            for abs in r.start.0.max(top)..=last {
                let (start, end) = if r.block {
                    (r.start.1, r.end.1 + 1)
                } else {
                    let a = if abs == r.start.0 { r.start.1 } else { 0 };
                    let b = if abs == r.end.0 {
                        r.end.1 + 1
                    } else {
                        t.cols()
                    };
                    (a, b)
                };
                if start < end {
                    self.highlights.push(Highlight {
                        row: abs - top,
                        start,
                        end,
                        bg: Rgb(0, 0, 0),
                        fg: None,
                    });
                }
            }
        }

        let composed = self.offset > 0 || self.preedit.is_some();
        if !composed {
            let was = std::mem::replace(&mut self.was_composed, false);
            if was {
                // Back to the live rows: the scene was built from the composed snapshot.
                return Source::LiveFull;
            }
            return Source::Live;
        }
        let rebuilt = self.dirty || output;
        if rebuilt {
            self.dirty = false;
            self.build(t, live);
        }
        self.was_composed = true;
        Source::Composed { full: rebuilt }
    }

    fn build(&mut self, t: &Term, live: &FrameSnapshot) {
        let rows = t.rows();
        if self.offset == 0 {
            self.comp.rows.clone_from(&live.rows);
        } else {
            let top = t.grid().history_rows() - self.offset;
            self.comp.rows.resize_with(rows, SnapRow::default);
            for y in 0..rows {
                if !t.grid().copy_abs_row(top + y, &mut self.comp.rows[y]) {
                    self.comp.rows[y].cells.clear();
                }
            }
        }
        self.comp.changed.clear();
        self.comp.changed.extend(0..self.comp.rows.len() as u32);
        if self.offset == 0
            && let Some(p) = &self.preedit
        {
            let cur = t.cursor();
            if let Some(row) = self.comp.rows.get_mut(cur.y) {
                patch_preedit(row, cur.x, p, &self.widths);
            }
        }
    }
}

/// Draw `p` into `row` from column `col`: underlined, the active segment inverse.
fn patch_preedit(row: &mut SnapRow, col: usize, p: &Preedit, w: &Widths) {
    let cols = row.cells.len();
    let mut x = col.min(cols);
    let mut byte = 0usize;
    for c in p.text.chars() {
        let here = byte;
        byte += c.len_utf8();
        let width = usize::from(w.scalar_width(c));
        if width == 0 {
            continue;
        }
        if x + width > cols {
            break;
        }
        let active =
            p.begin >= 0 && p.end > p.begin && here >= p.begin as usize && here < p.end as usize;
        let style = Style {
            underline: Underline::Single,
            attrs: Attrs(if active { Attrs::INVERSE } else { 0 }),
            ..Style::DEFAULT
        };
        let cell = SnapCell {
            cp: c as u32,
            cluster_start: 0,
            cluster_len: 0,
            wide: if width == 2 { Wide::Wide } else { Wide::Narrow },
            style,
            link: false,
            protected: false,
            semantic: row.cells[x].semantic,
        };
        row.cells[x] = cell;
        if width == 2 {
            row.cells[x + 1] = SnapCell {
                cp: 0,
                wide: Wide::SpacerTail,
                ..cell
            };
        }
        x += width;
    }
}

/// The visible rows as text, one line per row, trailing blanks trimmed (the test dump).
pub(crate) fn rows_text(rows: &[SnapRow]) -> String {
    let mut out = String::new();
    for r in rows {
        let line: String = r
            .cells
            .iter()
            .filter(|c| c.wide != Wide::SpacerTail)
            .map(|c| {
                if c.cp == 0 {
                    ' '
                } else {
                    char::from_u32(c.cp).unwrap_or(' ')
                }
            })
            .collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Fill the colours of the highlights from the palette (the view does not know them).
pub(crate) fn paint(h: &[Highlight], bg: Rgb, fg: Option<Rgb>, out: &mut Vec<Highlight>) {
    out.clear();
    out.extend(h.iter().map(|x| Highlight { bg, fg, ..*x }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_term::TermConfig;
    use std::time::Instant;

    fn term(rows: usize, cols: usize, input: &str) -> Term {
        let mut t = Term::new(TermConfig {
            rows,
            cols,
            ..TermConfig::default()
        });
        t.feed(input.as_bytes(), Instant::now());
        t
    }

    fn live(t: &mut Term) -> FrameSnapshot {
        let mut s = FrameSnapshot::new();
        t.grid_mut().snapshot_dirty(&mut s);
        s
    }

    fn settings(v: &mut View, t: &mut Term) {
        v.apply(
            t,
            ViewCmd::Settings(ViewSettings {
                word_extra: "-./_".into(),
                trim_trailing: true,
            }),
        );
    }

    fn copy(v: &mut View, t: &mut Term) -> Option<String> {
        v.apply(
            t,
            ViewCmd::Copy {
                target: CopyTarget::Clipboard,
                clear: false,
            },
        )
        .map(|c| c.text)
    }

    #[test]
    fn drag_selects_cells_and_copies() {
        let mut t = term(4, 20, "hello world\r\nsecond");
        let mut v = View::new();
        settings(&mut v, &mut t);
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Simple,
                row: 0,
                col: 0,
                side: Side::Left,
            },
        );
        v.apply(
            &mut t,
            ViewCmd::Extend {
                row: 0,
                col: 4,
                side: Side::Right,
            },
        );
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("hello"));
        let l = live(&mut t);
        v.sync(&t, &l, false);
        assert_eq!(v.highlights().len(), 1);
        assert_eq!((v.highlights()[0].start, v.highlights()[0].end), (0, 5));
    }

    #[test]
    fn click_without_drag_selects_nothing() {
        let mut t = term(3, 10, "abc");
        let mut v = View::new();
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Simple,
                row: 0,
                col: 1,
                side: Side::Left,
            },
        );
        assert_eq!(copy(&mut v, &mut t), None);
    }

    #[test]
    fn word_and_line_and_block() {
        let mut t = term(4, 20, "foo bar-baz qux\r\nline two");
        let mut v = View::new();
        settings(&mut v, &mut t);
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Word,
                row: 0,
                col: 5,
                side: Side::Left,
            },
        );
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("bar-baz"));
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Line,
                row: 1,
                col: 2,
                side: Side::Left,
            },
        );
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("line two"));
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Block,
                row: 0,
                col: 0,
                side: Side::Left,
            },
        );
        v.apply(
            &mut t,
            ViewCmd::Extend {
                row: 1,
                col: 2,
                side: Side::Right,
            },
        );
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("foo\nlin"));
    }

    #[test]
    fn finish_copies_only_when_asked_and_clear_on_copy_drops() {
        let mut t = term(3, 10, "abcdef");
        let mut v = View::new();
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Simple,
                row: 0,
                col: 0,
                side: Side::Left,
            },
        );
        v.apply(
            &mut t,
            ViewCmd::Extend {
                row: 0,
                col: 2,
                side: Side::Right,
            },
        );
        assert!(v.apply(&mut t, ViewCmd::Finish { copy: None }).is_none());
        let c = v
            .apply(
                &mut t,
                ViewCmd::Finish {
                    copy: Some(CopyTarget::Primary),
                },
            )
            .unwrap();
        assert_eq!((c.target, c.text.as_str()), (CopyTarget::Primary, "abc"));
        let c = v
            .apply(
                &mut t,
                ViewCmd::Copy {
                    target: CopyTarget::Clipboard,
                    clear: true,
                },
            )
            .unwrap();
        assert_eq!(c.text, "abc");
        assert!(!v.has_selection());
    }

    #[test]
    fn select_all_covers_history_and_screen() {
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("l{i}\r\n"));
        }
        let mut t = term(3, 8, &input);
        let mut v = View::new();
        settings(&mut v, &mut t);
        v.apply(&mut t, ViewCmd::All);
        let text = copy(&mut v, &mut t).unwrap();
        assert!(text.starts_with("l0\nl1\n") && text.contains("l9"));
    }

    fn numbered(n: usize) -> String {
        (0..n).map(|i| format!("row{i}\r\n")).collect()
    }

    #[test]
    fn scroll_clamps_and_shows_older_rows() {
        let mut t = term(4, 10, &numbered(30));
        let mut v = View::new();
        let l = live(&mut t);
        assert_eq!(v.sync(&t, &l, false), Source::Live);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Lines(5)));
        assert_eq!(v.offset(), 5);
        let src = v.sync(&t, &l, false);
        assert_eq!(src, Source::Composed { full: true });
        let shown = rows_text(&v.composed().rows);
        let live_text = rows_text(&l.rows);
        assert_ne!(shown, live_text);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Top));
        assert_eq!(v.offset(), t.grid().history_rows());
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Lines(1000)));
        assert_eq!(v.offset(), t.grid().history_rows());
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Pages(-1)));
        assert_eq!(v.offset(), t.grid().history_rows() - 4);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::HalfPages(-1)));
        assert_eq!(v.offset(), t.grid().history_rows() - 6);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Bottom));
        assert_eq!(v.offset(), 0);
        // Leaving the scrolled view redraws everything from the live rows once.
        assert_eq!(v.sync(&t, &l, false), Source::LiveFull);
        assert_eq!(v.sync(&t, &l, false), Source::Live);
    }

    #[test]
    fn new_output_keeps_the_scrolled_text_in_place() {
        let mut t = term(4, 10, &numbered(30));
        let mut v = View::new();
        let l = live(&mut t);
        v.sync(&t, &l, false);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Lines(3)));
        v.sync(&t, &l, false);
        let before = rows_text(&v.composed().rows);
        t.feed(numbered(5).as_bytes(), Instant::now());
        let l = live(&mut t);
        v.sync(&t, &l, true);
        assert_eq!(rows_text(&v.composed().rows), before);
        assert_eq!(v.offset(), 3 + 5);
    }

    #[test]
    fn capped_scrollback_keeps_the_scrolled_text_in_place() {
        let mut t = Term::new(TermConfig {
            rows: 4,
            cols: 10,
            scrollback: apoth_grid::Limits {
                bytes: 64 << 20,
                lines: Some(100),
            },
            ..TermConfig::default()
        });
        let mut n = 0usize;
        fn line(t: &mut Term, n: &mut usize) {
            t.feed(format!("row{n}\r\n").as_bytes(), Instant::now());
            *n += 1;
        }
        while t.grid().stats().evicted_pages == 0 {
            line(&mut t, &mut n);
            assert!(n < 100_000, "scrollback never evicted");
        }
        let mut v = View::new();
        let l = live(&mut t);
        v.sync(&t, &l, false);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Lines(3)));
        v.sync(&t, &l, false);
        let before = rows_text(&v.composed().rows);
        let mut evicted = t.grid().stats().evicted_pages;
        let mut followed = 0;
        for _ in 0..60_000 {
            line(&mut t, &mut n);
            let l = live(&mut t);
            v.sync(&t, &l, true);
            // The anchor row is only a few rows old: the text must not move, whatever the
            // history count did.
            assert_eq!(rows_text(&v.composed().rows), before);
            let e = t.grid().stats().evicted_pages;
            if e != evicted {
                followed += 1;
                evicted = e;
            }
            if followed >= 2 {
                break;
            }
        }
        assert!(
            followed > 0,
            "an eviction must happen while the view is frozen"
        );
    }

    #[test]
    fn selection_survives_scrolling_and_highlights_follow() {
        let mut t = term(4, 10, &numbered(30));
        let mut v = View::new();
        settings(&mut v, &mut t);
        v.apply(
            &mut t,
            ViewCmd::Begin {
                kind: SelKind::Line,
                row: 2,
                col: 0,
                side: Side::Left,
            },
        );
        let l = live(&mut t);
        v.sync(&t, &l, false);
        let rows: Vec<usize> = v.highlights().iter().map(|h| h.row).collect();
        assert_eq!(rows, vec![2]);
        v.apply(&mut t, ViewCmd::Scroll(ScrollBy::Lines(1)));
        v.sync(&t, &l, false);
        let rows: Vec<usize> = v.highlights().iter().map(|h| h.row).collect();
        assert_eq!(rows, vec![3]);
        assert_eq!(v.changed_rows(), vec![2, 3]);
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("row29"));
    }

    #[test]
    fn preedit_is_drawn_at_the_cursor_and_never_in_the_grid() {
        let mut t = term(3, 12, "ab");
        let mut v = View::new();
        let l = live(&mut t);
        v.apply(
            &mut t,
            ViewCmd::Preedit(Some(Preedit {
                text: "xyz".into(),
                begin: 1,
                end: 2,
            })),
        );
        let src = v.sync(&t, &l, false);
        assert_eq!(src, Source::Composed { full: true });
        let row = &v.composed().rows[0];
        assert_eq!(row.cells[2].cp, 'x' as u32);
        assert_eq!(row.cells[2].style.underline, Underline::Single);
        assert!(!row.cells[2].style.attrs.contains(Attrs::INVERSE));
        assert!(row.cells[3].style.attrs.contains(Attrs::INVERSE));
        assert_eq!(row.cells[4].cp, 'z' as u32);
        // The live snapshot and the grid never saw it.
        assert_eq!(l.rows[0].cells[2].cp, 0);
        assert_eq!(t.row_text(0).trim_end(), "ab");
        v.apply(&mut t, ViewCmd::Preedit(None));
        assert_eq!(v.sync(&t, &l, false), Source::LiveFull);
        assert_eq!(v.sync(&t, &l, false), Source::Live);
    }

    #[test]
    fn wide_preedit_takes_two_cells_and_stops_at_the_edge() {
        let mut t = term(2, 5, "abc");
        let mut v = View::new();
        let l = live(&mut t);
        v.apply(
            &mut t,
            ViewCmd::Preedit(Some(Preedit {
                text: "\u{4e2d}\u{6587}".into(),
                begin: -1,
                end: -1,
            })),
        );
        v.sync(&t, &l, false);
        let row = &v.composed().rows[0];
        assert_eq!(row.cells[3].wide, Wide::Wide);
        assert_eq!(row.cells[4].wide, Wide::SpacerTail);
        // The second character does not fit and is cut, not wrapped.
        assert_eq!(rows_text(&v.composed().rows[..1]).trim_end(), "abc\u{4e2d}");
    }

    #[test]
    fn rows_text_trims_and_skips_spacers() {
        let mut t = term(2, 6, "hi");
        let l = live(&mut t);
        assert_eq!(rows_text(&l.rows), "hi\n\n");
    }

    #[test]
    fn paint_copies_spans_with_colours() {
        let h = [Highlight {
            row: 1,
            start: 2,
            end: 4,
            bg: Rgb(0, 0, 0),
            fg: None,
        }];
        let mut out = Vec::new();
        paint(&h, Rgb(1, 2, 3), Some(Rgb(9, 9, 9)), &mut out);
        assert_eq!(out[0].bg, Rgb(1, 2, 3));
        assert_eq!(out[0].fg, Some(Rgb(9, 9, 9)));
        assert_eq!((out[0].row, out[0].start, out[0].end), (1, 2, 4));
    }
}
