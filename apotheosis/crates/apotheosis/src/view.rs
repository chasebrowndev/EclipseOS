// SPDX-License-Identifier: AGPL-3.0-only
//! The render thread's view state (T-02 §5-§6, T-05 §5): which rows of the scrollback are
//! shown, the selection with its highlight spans, and the IME preedit drawn at the cursor.
//! Main never takes the term lock, so every selection and scroll command crosses to the render
//! thread as a [`ViewCmd`] and is applied here under the lock the frame already holds.
//!
//! It also owns navigation (T-14 §5-§8): the scrollback search run, copy mode's cursor, hint
//! labels and the hovered hyperlink. Results main must act on leave through [`View::take_out`].
//!
//! Selection text, preedit text, search queries, hint and link text are user content:
//! nothing here logs them.

use std::time::Duration;

use apoth_action::enums::Motion;
use apoth_grid::{
    Attrs, ExtractOpts, FrameSnapshot, LineScratch, Pin, SelKind, Selection, Side, SnapCell,
    SnapRow, Style, Underline, Wide, WordChars, flags,
};
use apoth_render::colour::Rgb;
use apoth_render::scene::Highlight;
use apoth_term::Term;
use apoth_unicode::Widths;

use crate::copymode::{self, Ctx};
use crate::hints::{self, Kind as HintKind, Matchers};
use crate::search::{Query, Run, Status};

/// Longest preedit kept, in characters (a longer string is cut: an IME never needs more).
const MAX_PREEDIT: usize = 512;

/// Search time per frame: the rest of history is scanned on later frames.
const SEARCH_SLICE: Duration = Duration::from_millis(2);

/// Highlight kinds, carried in the red channel of `Highlight::bg` until [`paint`] colours
/// them (the view does not know the palette).
pub(crate) mod hl {
    pub(crate) const SELECTION: u8 = 0;
    pub(crate) const MATCH: u8 = 1;
    pub(crate) const CURRENT: u8 = 2;
    pub(crate) const CURSOR: u8 = 3;
    pub(crate) const HINT: u8 = 4;
    /// The label drawn over the start of a hint match.
    pub(crate) const LABEL: u8 = 5;
}

/// Colours per highlight kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HlColours {
    pub(crate) sel_bg: Rgb,
    pub(crate) sel_fg: Option<Rgb>,
    pub(crate) match_bg: Rgb,
    pub(crate) match_fg: Option<Rgb>,
    pub(crate) current_bg: Rgb,
    pub(crate) current_fg: Option<Rgb>,
    pub(crate) cursor_bg: Rgb,
    pub(crate) cursor_fg: Option<Rgb>,
    pub(crate) hint_bg: Rgb,
    pub(crate) hint_fg: Option<Rgb>,
    pub(crate) label_bg: Rgb,
    pub(crate) label_fg: Option<Rgb>,
}

/// Navigation results for main (sent as `MainMsg::Nav`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavMsg {
    /// Search progress for the bar.
    Search(Status),
    /// A hint label was typed: run the matcher's action on `text`; `menu` when the label was
    /// typed in upper case (the Open menu instead of the default action, T-14 §11).
    Hint {
        kind: HintKind,
        text: String,
        menu: bool,
    },
    /// Hint mode ended (picked, cancelled, nothing to label, or the screen changed).
    HintsEnded,
    /// The hovered hyperlink's URI (the tooltip), or none.
    Link(Option<String>),
    /// The link under an open-modifier click: main checks and opens it.
    Open { uri: String, visible: String },
    /// Copy mode ended without main asking (the screen switched).
    CopyModeEnded,
    /// The answer to `TakeTarget`: the recognised text (`visible` = the link's visible text),
    /// or `None` when there is nothing there.
    OpenTarget {
        intent: crate::open::Intent,
        found: Option<(crate::open::RawKind, String, String)>,
        /// Asked at a cell (a right-click), not for the selection.
        at_cell: bool,
    },
}

struct HintItem {
    start: (usize, usize),
    end: (usize, usize),
    kind: HintKind,
    text: String,
    label: String,
}

struct HintState {
    items: Vec<HintItem>,
    typed: String,
    /// A label character was typed in upper case.
    menu: bool,
}

struct CopyCur {
    pin: Pin,
    /// Where an open visual selection started.
    anchor: Option<Pin>,
}

/// Push highlight spans of kind `kind` for cells `start..=end` (absolute rows) clipped to
/// the viewport `top..top + rows`.
fn span(
    out: &mut Vec<Highlight>,
    kind: u8,
    start: (usize, usize),
    end: (usize, usize),
    top: usize,
    rows: usize,
    cols: usize,
) {
    if rows == 0 || end.0 < top {
        return;
    }
    let last = end.0.min(top + rows - 1);
    for abs in start.0.max(top)..=last {
        let a = if abs == start.0 { start.1 } else { 0 };
        let b = if abs == end.0 {
            (end.1 + 1).min(cols)
        } else {
            cols
        };
        if a < b {
            out.push(Highlight {
                row: abs - top,
                start: a,
                end: b,
                bg: Rgb(kind, 0, 0),
                fg: None,
            });
        }
    }
}

/// The viewport's hint matches with labels (T-14 §7): built-in matchers per logical line,
/// OSC 8 links per row (a link wins over a pattern match it overlaps), the newest `max`
/// kept, shortest labels nearest the bottom.
fn find_hints(
    t: &Term,
    top: usize,
    rows: usize,
    kinds: &[HintKind],
    max: usize,
    alphabet: &str,
) -> Vec<HintItem> {
    let g = t.grid();
    let total = g.history_rows() + g.rows();
    let end = (top + rows).min(total);
    let mut items: Vec<HintItem> = Vec::new();
    // OSC 8 links first.
    if kinds.is_empty() || kinds.contains(&HintKind::Link) {
        let mut row = SnapRow::default();
        for abs in top..end {
            if !g.copy_abs_row(abs, &mut row) {
                continue;
            }
            let mut x = 0;
            while x < row.cells.len() {
                if !row.cells[x].link {
                    x += 1;
                    continue;
                }
                let Some((_, uri)) = g.link_at_abs(abs, x) else {
                    x += 1;
                    continue;
                };
                let start = x;
                while x + 1 < row.cells.len()
                    && row.cells[x + 1].link
                    && g.link_at_abs(abs, x + 1).is_some_and(|(_, u)| u == uri)
                {
                    x += 1;
                }
                items.push(HintItem {
                    start: (abs, start),
                    end: (abs, x),
                    kind: HintKind::Link,
                    text: uri,
                    label: String::new(),
                });
                x += 1;
            }
        }
    }
    let pattern_kinds: Vec<HintKind> = kinds
        .iter()
        .copied()
        .filter(|k| *k != HintKind::Link)
        .collect();
    if kinds.is_empty() || !pattern_kinds.is_empty() {
        let mut m = Matchers::new(&pattern_kinds);
        let mut scratch = LineScratch::default();
        let links = items.len();
        let mut abs = top;
        while abs < end {
            let Some(sp) = g.logical_line(abs, false, &mut scratch) else {
                break;
            };
            for f in m.find(&scratch.text) {
                let (Some(a), Some(b)) = (scratch.locate(f.start), scratch.locate(f.end - 1))
                else {
                    continue;
                };
                let (s, e) = ((a.0, a.1), (b.0, b.1 + b.2 - 1));
                if s.0 < top || s.0 >= end {
                    continue;
                }
                let overlaps = items[..links].iter().any(|l| !(e < l.start || l.end < s));
                if !overlaps {
                    items.push(HintItem {
                        start: s,
                        end: e,
                        kind: f.kind,
                        text: scratch.text[f.start..f.end].to_string(),
                        label: String::new(),
                    });
                }
            }
            abs = sp.next.max(abs + 1);
        }
    }
    items.sort_by_key(|i| i.start);
    if items.len() > max {
        items.drain(..items.len() - max);
    }
    let labels = hints::labels(alphabet, items.len());
    for (it, l) in items.iter_mut().rev().zip(labels) {
        it.label = l;
    }
    items
}

/// Text of the link run around `(abs, col)` on its row (the visible text of an OSC 8 link).
fn link_text(t: &Term, abs: usize, col: usize, uri: &str) -> String {
    let g = t.grid();
    let mut row = SnapRow::default();
    if !g.copy_abs_row(abs, &mut row) {
        return String::new();
    }
    let same = |x: usize| g.link_at_abs(abs, x).is_some_and(|(_, u)| u == uri);
    let mut a = col;
    while a > 0 && same(a - 1) {
        a -= 1;
    }
    let mut b = col;
    while b + 1 < row.cells.len() && same(b + 1) {
        b += 1;
    }
    row.cells[a..=b.min(row.cells.len().saturating_sub(1))]
        .iter()
        .filter(|c| c.wide != Wide::SpacerTail)
        .map(|c| char::from_u32(c.cp).filter(|_| c.cp != 0).unwrap_or(' '))
        .collect()
}

/// Overwrite cell `x` with `ch` in `style`, keeping wide-character pairs consistent.
fn put_cell(row: &mut SnapRow, x: usize, ch: char, style: Style) {
    let n = row.cells.len();
    if x >= n {
        return;
    }
    let blank = |c: &SnapCell| SnapCell {
        cp: u32::from(' '),
        wide: Wide::Narrow,
        ..*c
    };
    if row.cells[x].wide == Wide::SpacerTail && x > 0 {
        row.cells[x - 1] = blank(&row.cells[x - 1]);
    }
    if row.cells[x].wide == Wide::Wide && x + 1 < n {
        row.cells[x + 1] = blank(&row.cells[x + 1]);
    }
    row.cells[x] = SnapCell {
        cp: ch as u32,
        cluster_start: 0,
        cluster_len: 0,
        wide: Wide::Narrow,
        style,
        link: false,
        protected: false,
        semantic: row.cells[x].semantic,
    };
}

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
    /// Start (or replace) the search; `None` closes it.
    Search(Option<Query>),
    /// Move to the next match toward older (or newer) rows.
    SearchStep {
        older: bool,
    },
    CopyEnter,
    CopyExit,
    CopyMove {
        motion: Motion,
        count: u32,
    },
    CopyJump {
        ch: char,
        backward: bool,
        till: bool,
    },
    /// Start or end a visual selection from the copy-mode cursor.
    CopySelect(SelKind),
    /// Label the viewport's matches (`kinds` empty = every built-in matcher).
    Hints {
        kinds: Vec<HintKind>,
        alphabet: String,
        max: usize,
    },
    /// A key typed in hint mode (`\u{8}` = backspace).
    HintKey(char),
    HintsCancel,
    /// The pointer is over viewport cell `(row, col)`, or left the grid.
    Hover(Option<(usize, usize)>),
    /// Open-modifier click on viewport cell `(row, col)`.
    Activate {
        row: usize,
        col: usize,
    },
    /// Scroll to the previous (older) or next prompt.
    BlockJump {
        older: bool,
    },
    /// Find what an `open.*` action or a right-click opens (T-14 §11): at viewport cell
    /// `(row, col)` the OSC 8 link, URL or path covering it; with no cell the selection.
    TakeTarget {
        at: Option<(usize, usize)>,
        intent: crate::open::Intent,
    },
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
    search: Option<Run>,
    /// Jump to the newest match once one is found.
    search_jump: bool,
    last_status: Option<Status>,
    copy: Option<CopyCur>,
    hints: Option<HintState>,
    /// The hovered link: viewport position and URI.
    hover: Option<String>,
    out: Vec<NavMsg>,
    busy: bool,
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
            search: None,
            search_jump: false,
            last_status: None,
            copy: None,
            hints: None,
            hover: None,
            out: Vec::new(),
            busy: false,
        }
    }

    /// Navigation results since the last call.
    pub(crate) fn take_out(&mut self) -> Vec<NavMsg> {
        std::mem::take(&mut self.out)
    }

    /// Work remains (the search is still scanning history): draw another frame.
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }

    /// Copy mode is on.
    #[cfg(test)]
    pub(crate) fn in_copy_mode(&self) -> bool {
        self.copy.is_some()
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
            ViewCmd::Search(q) => {
                self.search = q.map(|q| Run::new(q, t.on_alternate()));
                self.search_jump = self.search.is_some();
                self.last_status = None;
                self.busy = self.search.is_some();
                self.dirty = true;
            }
            ViewCmd::SearchStep { older } => self.search_step(t, older),
            ViewCmd::CopyEnter => {
                if self.copy.is_none() {
                    let hist = t.grid().history_rows();
                    let top = hist - self.offset.min(hist);
                    let cur = t.cursor();
                    let p = if self.offset == 0 {
                        (top + cur.y, cur.x)
                    } else {
                        (top + t.rows().saturating_sub(1), 0)
                    };
                    if let Some(pin) = t.grid().pin_at(p.0, p.1.min(t.cols().saturating_sub(1))) {
                        self.copy = Some(CopyCur { pin, anchor: None });
                        self.dirty = true;
                    }
                }
            }
            ViewCmd::CopyExit => {
                if let Some(c) = self.copy.take() {
                    if c.anchor.is_some() {
                        self.drop_selection(t);
                    }
                    self.dirty = true;
                }
            }
            ViewCmd::CopyMove { motion, count } => {
                if let Some(p) = self.copy_pos(t) {
                    let n = match motion {
                        // The search runs newest-first, so "next" continues toward older rows.
                        Motion::SearchNext | Motion::SearchPrev => {
                            let older = motion == Motion::SearchNext;
                            let mut q = p;
                            if let Some(r) = &self.search {
                                for _ in 0..count.clamp(1, 1000) {
                                    match r.peek(older, q) {
                                        Some(n) => q = n,
                                        None => break,
                                    }
                                }
                            }
                            q
                        }
                        _ => {
                            let ctx = Ctx {
                                top: self.top_abs(t),
                                rows: t.rows(),
                            };
                            copymode::repeat(t.grid(), p, motion, count, ctx)
                        }
                    };
                    self.set_copy_pos(t, n);
                }
            }
            ViewCmd::CopyJump { ch, backward, till } => {
                if let Some(p) = self.copy_pos(t)
                    && let Some(n) = copymode::jump(t.grid(), p, ch, backward, till)
                {
                    self.set_copy_pos(t, n);
                }
            }
            ViewCmd::CopySelect(kind) => {
                if let Some(p) = self.copy_pos(t) {
                    let open = self.copy.as_ref().is_some_and(|c| c.anchor.is_some());
                    self.drop_selection(t);
                    let anchor = if open {
                        None
                    } else {
                        let alt = t.on_alternate();
                        self.sel = t
                            .grid_mut()
                            .selection_begin(kind, p.0, p.1, Side::Left)
                            .map(|s| (alt, s));
                        t.grid().pin_at(p.0, p.1)
                    };
                    if let Some(c) = self.copy.as_mut() {
                        c.anchor = anchor;
                    }
                    self.dirty = true;
                }
            }
            ViewCmd::Hints {
                kinds,
                alphabet,
                max,
            } => {
                let items = find_hints(t, self.top_abs(t), t.rows(), &kinds, max.max(1), &alphabet);
                if items.is_empty() {
                    self.hints = None;
                    self.out.push(NavMsg::HintsEnded);
                } else {
                    self.hints = Some(HintState {
                        items,
                        typed: String::new(),
                        menu: false,
                    });
                }
                self.dirty = true;
            }
            ViewCmd::HintKey(c) => self.hint_key(c),
            ViewCmd::HintsCancel => {
                if self.hints.take().is_some() {
                    self.out.push(NavMsg::HintsEnded);
                    self.dirty = true;
                }
            }
            ViewCmd::Hover(pos) => {
                let top = self.top_abs(t);
                let uri = pos
                    .and_then(|(r, c)| t.grid().link_at_abs(top + r, c))
                    .map(|(_, u)| u);
                if uri != self.hover {
                    self.hover.clone_from(&uri);
                    self.out.push(NavMsg::Link(uri));
                    self.dirty = true;
                }
            }
            ViewCmd::Activate { row, col } => {
                let abs = self.top_abs(t) + row;
                if let Some((_, uri)) = t.grid().link_at_abs(abs, col) {
                    let visible = link_text(t, abs, col, &uri);
                    self.out.push(NavMsg::Open { uri, visible });
                }
            }
            ViewCmd::TakeTarget { at, intent } => {
                use crate::open::RawKind;
                let found = match at {
                    None => self.text(t).map(|s| {
                        let s = s.trim().to_string();
                        (RawKind::Unknown, s.clone(), s)
                    }),
                    Some((row, col)) => {
                        let top = self.top_abs(t);
                        let abs = top + row;
                        if let Some((_, uri)) = t.grid().link_at_abs(abs, col) {
                            let visible = link_text(t, abs, col, &uri);
                            Some((RawKind::Link, uri, visible))
                        } else {
                            let kinds = [HintKind::Url, HintKind::Path];
                            find_hints(
                                t,
                                top,
                                t.rows(),
                                &kinds,
                                usize::MAX,
                                "abcdefghijklmnopqrstuvwxyz",
                            )
                            .into_iter()
                            .find(|i| i.start <= (abs, col) && (abs, col) <= i.end)
                            .map(|i| {
                                let k = if i.kind == HintKind::Url {
                                    RawKind::Url
                                } else {
                                    RawKind::Path
                                };
                                (k, i.text.clone(), i.text)
                            })
                        }
                    }
                };
                let found = found.filter(|f| !f.1.is_empty());
                self.out.push(NavMsg::OpenTarget {
                    intent,
                    found,
                    at_cell: at.is_some(),
                });
            }
            ViewCmd::BlockJump { older } => {
                let g = t.grid();
                let hist = g.history_rows();
                let total = hist + t.rows();
                let top = self.top_abs(t);
                let prompt = |r: usize| {
                    g.row_flags_abs(r)
                        .is_some_and(|f| f & flags::PROMPT_START != 0)
                };
                let target = if older {
                    (0..top).rev().find(|&r| prompt(r))
                } else {
                    (top + 1..total).find(|&r| prompt(r))
                };
                if let Some(r) = target {
                    let new = hist.saturating_sub(r.min(hist));
                    if new != self.offset {
                        self.offset = new;
                        self.anchor = None;
                        self.dirty = true;
                    }
                }
            }
        }
        None
    }

    /// Scroll so absolute row `abs` is visible; `center` puts it mid-view when it moves.
    fn show_row(&mut self, t: &Term, abs: usize, center: bool) {
        let hist = t.grid().history_rows();
        let rows = t.rows().max(1);
        let top = hist.saturating_sub(self.offset);
        if abs >= top && abs < top + rows {
            return;
        }
        let new_top = if center {
            abs.saturating_sub(rows / 2)
        } else if abs < top {
            abs
        } else {
            abs + 1 - rows
        };
        let new = hist.saturating_sub(new_top.min(hist));
        if new != self.offset {
            self.offset = new;
            self.anchor = None;
            self.dirty = true;
        }
    }

    fn copy_pos(&self, t: &Term) -> Option<(usize, usize)> {
        let c = self.copy.as_ref()?;
        Some(t.grid().pin_abs(&c.pin).unwrap_or((0, 0)))
    }

    fn set_copy_pos(&mut self, t: &mut Term, p: (usize, usize)) {
        let p = (p.0, p.1.min(t.cols().saturating_sub(1)));
        let Some(pin) = t.grid().pin_at(p.0, p.1) else {
            return;
        };
        let Some(c) = self.copy.as_mut() else { return };
        c.pin = pin;
        let anchor = c.anchor.and_then(|a| t.grid().pin_abs(&a));
        if let Some(a) = anchor
            && let Some((alt, mut s)) = self.sel.take()
        {
            if alt == t.on_alternate() {
                let side = if p < a { Side::Left } else { Side::Right };
                t.grid_mut().selection_extend(&mut s, p.0, p.1, side);
            }
            self.sel = Some((alt, s));
        }
        self.show_row(t, p.0, false);
        self.dirty = true;
    }

    fn search_step(&mut self, t: &mut Term, older: bool) {
        let total = t.grid().history_rows() + t.rows();
        let from = self
            .copy_pos(t)
            .unwrap_or(if older { (total, 0) } else { (0, 0) });
        let Some(r) = self.search.as_mut() else {
            return;
        };
        if r.step(older, from).is_none() {
            return;
        }
        let cur = r.current();
        let st = r.status();
        self.last_status = Some(st.clone());
        self.out.push(NavMsg::Search(st));
        if let Some((s, _)) = cur {
            if self.copy.is_some() {
                self.set_copy_pos(t, s);
            } else {
                self.show_row(t, s.0, true);
            }
        }
        self.dirty = true;
    }

    fn hint_key(&mut self, c: char) {
        let Some(h) = self.hints.as_mut() else {
            return;
        };
        if c == '\u{8}' {
            h.typed.pop();
        } else if c.is_uppercase() && !h.items.iter().any(|i| i.label.contains(c)) {
            // Shift on a lower-case alphabet: the same label, asking for the menu.
            h.menu = true;
            h.typed.extend(c.to_lowercase());
        } else {
            h.typed.push(c);
        }
        let labels: Vec<String> = h.items.iter().map(|i| i.label.clone()).collect();
        match hints::pick(&labels, &h.typed) {
            hints::Pick::Hit(i) => {
                let it = &h.items[i];
                self.out.push(NavMsg::Hint {
                    kind: it.kind,
                    text: it.text.clone(),
                    menu: h.menu,
                });
                self.hints = None;
                self.out.push(NavMsg::HintsEnded);
            }
            hints::Pick::Prefix => {}
            hints::Pick::None => {
                self.hints = None;
                self.out.push(NavMsg::HintsEnded);
            }
        }
        self.dirty = true;
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
            if let Some(r) = self.search.as_mut() {
                *r = Run::new(r.query.clone(), alt);
            }
            if self.hints.take().is_some() {
                self.out.push(NavMsg::HintsEnded);
            }
            if self.copy.take().is_some() {
                self.out.push(NavMsg::CopyModeEnded);
            }
            if self.hover.take().is_some() {
                self.out.push(NavMsg::Link(None));
            }
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
        self.drive_search(t, output);
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
                        bg: Rgb(hl::SELECTION, 0, 0),
                        fg: None,
                    });
                }
            }
        }
        let cols = t.cols();
        if let Some(r) = &self.search {
            for (a, b, cur) in r.visible(top, rows) {
                let kind = if cur { hl::CURRENT } else { hl::MATCH };
                span(&mut self.highlights, kind, a, b, top, rows, cols);
            }
        }
        if let Some(h) = &self.hints {
            let typed = h.typed.chars().count();
            for it in h.items.iter().filter(|i| i.label.starts_with(&h.typed)) {
                span(
                    &mut self.highlights,
                    hl::HINT,
                    it.start,
                    it.end,
                    top,
                    rows,
                    cols,
                );
            }
            // The labels over the matches' first cells, after (so over) the match spans.
            for it in h.items.iter().filter(|i| i.label.starts_with(&h.typed)) {
                let n = it.label.chars().count().saturating_sub(typed);
                if n > 0 {
                    let from = it.start;
                    span(
                        &mut self.highlights,
                        hl::LABEL,
                        from,
                        (from.0, from.1 + n - 1),
                        top,
                        rows,
                        cols,
                    );
                }
            }
        }
        if let Some(p) = self.copy_pos(t) {
            span(&mut self.highlights, hl::CURSOR, p, p, top, rows, cols);
        }

        let composed = self.offset > 0
            || self.preedit.is_some()
            || self.hints.is_some()
            || self.hover.is_some();
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

    /// Advance the search one slice and follow its first match.
    fn drive_search(&mut self, t: &Term, output: bool) {
        self.busy = false;
        let alt = t.on_alternate();
        let Some(r) = self.search.as_mut() else {
            return;
        };
        if r.alt() != alt {
            *r = Run::new(r.query.clone(), alt);
        }
        if !r.refresh(t.grid(), output, SEARCH_SLICE) {
            // The run's reference row left the scrollback: start over.
            *r = Run::new(r.query.clone(), alt);
            r.refresh(t.grid(), true, SEARCH_SLICE);
        }
        self.busy = !r.done();
        let mut jump = None;
        if self.search_jump && (r.status().count > 0 || r.done()) {
            self.search_jump = false;
            let total = t.grid().history_rows() + t.rows();
            if r.step(true, (total, 0)).is_some() {
                jump = r.current().map(|(s, _)| s.0);
            }
        }
        let st = r.status();
        if self.last_status.as_ref() != Some(&st) {
            self.last_status = Some(st.clone());
            self.out.push(NavMsg::Search(st));
        }
        if let Some(abs) = jump {
            self.show_row(t, abs, true);
        }
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
        let top = t.grid().history_rows() - self.offset;
        if let Some(uri) = &self.hover {
            for (y, row) in self.comp.rows.iter_mut().enumerate() {
                for x in 0..row.cells.len() {
                    if row.cells[x].link
                        && t.grid()
                            .link_at_abs(top + y, x)
                            .is_some_and(|(_, u)| &u == uri)
                    {
                        row.cells[x].style.underline = Underline::Single;
                    }
                }
            }
        }
        if let Some(h) = &self.hints {
            // The label's colours come from its highlight (`hl::LABEL`), not an inversion.
            let style = Style {
                attrs: Attrs(Attrs::BOLD),
                ..Style::DEFAULT
            };
            for it in h.items.iter().filter(|i| i.label.starts_with(&h.typed)) {
                if it.start.0 < top || it.start.0 >= top + rows {
                    continue;
                }
                let row = &mut self.comp.rows[it.start.0 - top];
                for (i, ch) in it.label.chars().skip(h.typed.chars().count()).enumerate() {
                    put_cell(row, it.start.1 + i, ch, style);
                }
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

/// Fill the colours of the highlights from the palette by kind (the view does not know them).
pub(crate) fn paint(h: &[Highlight], c: &HlColours, out: &mut Vec<Highlight>) {
    out.clear();
    out.extend(h.iter().map(|x| {
        let (bg, fg) = match x.bg.0 {
            hl::MATCH => (c.match_bg, c.match_fg),
            hl::CURRENT => (c.current_bg, c.current_fg),
            hl::CURSOR => (c.cursor_bg, c.cursor_fg),
            hl::HINT => (c.hint_bg, c.hint_fg),
            hl::LABEL => (c.label_bg, c.label_fg),
            _ => (c.sel_bg, c.sel_fg),
        };
        Highlight { bg, fg, ..*x }
    }));
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

    fn kinds(v: &View, kind: u8) -> Vec<(usize, usize, usize)> {
        v.highlights()
            .iter()
            .filter(|h| h.bg.0 == kind)
            .map(|h| (h.row, h.start, h.end))
            .collect()
    }

    #[test]
    fn search_finds_jumps_and_reports_progress() {
        let mut text = String::new();
        for i in 0..60 {
            text.push_str(&format!("line {i}\r\n"));
        }
        text.push_str("needle here\r\n");
        for i in 0..60 {
            text.push_str(&format!("after {i}\r\n"));
        }
        let mut t = term(5, 20, &text);
        let mut v = View::new();
        v.apply(
            &mut t,
            ViewCmd::Search(Some(Query {
                text: "needle".into(),
                ..Query::default()
            })),
        );
        let mut done = false;
        for _ in 0..1000 {
            let l = live(&mut t);
            v.sync(&t, &l, false);
            if !v.busy() {
                done = true;
                break;
            }
        }
        assert!(done, "the search finishes in slices");
        let out = v.take_out();
        let last = out.iter().rev().find_map(|m| match m {
            NavMsg::Search(s) => Some(s.clone()),
            _ => None,
        });
        let st = last.expect("a status reached main");
        assert_eq!((st.count, st.current, st.done), (1, Some(1), true));
        assert!(v.offset() > 0, "the view scrolled back to the match");
        let l = live(&mut t);
        v.sync(&t, &l, false);
        let cur = kinds(&v, hl::CURRENT);
        assert_eq!(cur.len(), 1);
        assert_eq!((cur[0].1, cur[0].2), (0, 6));
        assert_eq!(
            rows_text(&v.composed().rows[cur[0].0..=cur[0].0]).trim_end(),
            "needle here"
        );
        v.apply(&mut t, ViewCmd::Search(None));
        let l = live(&mut t);
        v.sync(&t, &l, false);
        assert!(kinds(&v, hl::CURRENT).is_empty());
    }

    #[test]
    fn copy_mode_moves_selects_and_yanks() {
        let mut t = term(4, 20, "alpha beta\r\ngamma delta\r\n$ ");
        let mut v = View::new();
        settings(&mut v, &mut t);
        v.apply(&mut t, ViewCmd::CopyEnter);
        assert!(v.in_copy_mode());
        let l = live(&mut t);
        v.sync(&t, &l, false);
        assert_eq!(
            kinds(&v, hl::CURSOR),
            vec![(2, 2, 3)],
            "starts at the cursor"
        );
        let mv = |v: &mut View, t: &mut Term, motion, count| {
            v.apply(t, ViewCmd::CopyMove { motion, count });
        };
        mv(&mut v, &mut t, Motion::Up, 2);
        mv(&mut v, &mut t, Motion::LineStart, 1);
        v.apply(&mut t, ViewCmd::CopySelect(SelKind::Simple));
        mv(&mut v, &mut t, Motion::WordEnd, 1);
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("alpha"));
        mv(&mut v, &mut t, Motion::Down, 1);
        assert_eq!(copy(&mut v, &mut t).as_deref(), Some("alpha beta\ngamma"));
        v.apply(
            &mut t,
            ViewCmd::CopyJump {
                ch: 'l',
                backward: false,
                till: false,
            },
        );
        assert_eq!(
            copy(&mut v, &mut t).as_deref(),
            Some("alpha beta\ngamma del")
        );
        v.apply(&mut t, ViewCmd::CopyExit);
        assert!(!v.in_copy_mode());
        assert!(!v.has_selection(), "leaving copy mode drops its selection");
    }

    #[test]
    fn hints_label_matches_and_pick_reports_the_text() {
        let mut t = term(3, 40, "see https://x.org/a, commit 1a2b3c4d\r\n");
        let mut v = View::new();
        v.apply(
            &mut t,
            ViewCmd::Hints {
                kinds: Vec::new(),
                alphabet: "ab".into(),
                max: 10,
            },
        );
        let l = live(&mut t);
        v.sync(&t, &l, false);
        let rows = rows_text(&v.composed().rows);
        let first = rows.lines().next().unwrap().to_string();
        // Two matches, labels `a` and `b`; the newest (rightmost) gets the first label.
        assert!(
            first.starts_with("see bttps://x.org/a, commit a"),
            "{first}"
        );
        assert_eq!(kinds(&v, hl::HINT).len(), 2);
        v.apply(&mut t, ViewCmd::HintKey('b'));
        let out = v.take_out();
        assert!(out.contains(&NavMsg::Hint {
            kind: HintKind::Url,
            text: "https://x.org/a".into(),
            menu: false,
        }));
        assert!(out.contains(&NavMsg::HintsEnded));
        // A key that matches no label ends hint mode.
        v.apply(
            &mut t,
            ViewCmd::Hints {
                kinds: vec![HintKind::Hash],
                alphabet: "ab".into(),
                max: 10,
            },
        );
        v.apply(&mut t, ViewCmd::HintKey('z'));
        assert_eq!(v.take_out(), vec![NavMsg::HintsEnded]);
    }

    #[test]
    fn osc8_hover_underlines_and_activate_reports_uri_and_text() {
        let mut t = term(
            3,
            30,
            "go \x1b]8;;https://e.example/\x1b\\here\x1b]8;;\x1b\\ now",
        );
        let mut v = View::new();
        v.apply(&mut t, ViewCmd::Hover(Some((0, 4))));
        assert_eq!(
            v.take_out(),
            vec![NavMsg::Link(Some("https://e.example/".into()))]
        );
        let l = live(&mut t);
        assert!(matches!(v.sync(&t, &l, false), Source::Composed { .. }));
        let row = &v.composed().rows[0];
        let under: Vec<usize> = (0..row.cells.len())
            .filter(|&x| row.cells[x].style.underline == Underline::Single)
            .collect();
        assert_eq!(under, vec![3, 4, 5, 6]);
        v.apply(&mut t, ViewCmd::Activate { row: 0, col: 6 });
        assert_eq!(
            v.take_out(),
            vec![NavMsg::Open {
                uri: "https://e.example/".into(),
                visible: "here".into()
            }]
        );
        v.apply(&mut t, ViewCmd::Hover(Some((0, 0))));
        assert_eq!(v.take_out(), vec![NavMsg::Link(None)]);
        v.apply(&mut t, ViewCmd::Activate { row: 0, col: 0 });
        assert!(v.take_out().is_empty(), "no link, nothing to open");
    }

    #[test]
    fn upper_case_hint_label_asks_for_the_menu() {
        let mut t = term(3, 40, "see https://x.org/a, commit 1a2b3c4d\r\n");
        let mut v = View::new();
        let hints = ViewCmd::Hints {
            kinds: Vec::new(),
            alphabet: "ab".into(),
            max: 10,
        };
        v.apply(&mut t, hints);
        v.apply(&mut t, ViewCmd::HintKey('B'));
        assert!(v.take_out().contains(&NavMsg::Hint {
            kind: HintKind::Url,
            text: "https://x.org/a".into(),
            menu: true,
        }));
        // An alphabet with upper-case letters keeps them as labels.
        v.apply(
            &mut t,
            ViewCmd::Hints {
                kinds: Vec::new(),
                alphabet: "AB".into(),
                max: 10,
            },
        );
        v.apply(&mut t, ViewCmd::HintKey('B'));
        assert!(
            v.take_out()
                .iter()
                .any(|m| matches!(m, NavMsg::Hint { menu: false, .. }))
        );
    }

    #[test]
    fn take_target_finds_link_url_path_or_selection() {
        use crate::open::{Intent, RawKind};
        let mut t = term(
            3,
            60,
            "\x1b]8;;https://e.example/\x1b\\here\x1b]8;;\x1b\\ https://x.org /tmp/a.png zz",
        );
        let mut v = View::new();
        let at = |v: &mut View, t: &mut Term, col| {
            v.apply(
                t,
                ViewCmd::TakeTarget {
                    at: Some((0, col)),
                    intent: Intent::Menu,
                },
            );
            match v.take_out().pop() {
                Some(NavMsg::OpenTarget { found, at_cell, .. }) => {
                    assert!(at_cell);
                    found
                }
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(
            at(&mut v, &mut t, 1),
            Some((RawKind::Link, "https://e.example/".into(), "here".into()))
        );
        assert_eq!(
            at(&mut v, &mut t, 8).map(|f| (f.0, f.1)),
            Some((RawKind::Url, "https://x.org".into()))
        );
        assert_eq!(
            at(&mut v, &mut t, 25).map(|f| (f.0, f.1)),
            Some((RawKind::Path, "/tmp/a.png".into()))
        );
        assert_eq!(at(&mut v, &mut t, 31), None);
        // No cell: the selection, or nothing.
        v.apply(
            &mut t,
            ViewCmd::TakeTarget {
                at: None,
                intent: Intent::Default,
            },
        );
        assert_eq!(
            v.take_out(),
            vec![NavMsg::OpenTarget {
                intent: Intent::Default,
                found: None,
                at_cell: false
            }]
        );
    }

    #[test]
    fn block_jump_scrolls_to_prompts() {
        let mut text = String::new();
        for i in 0..3 {
            text.push_str(&format!("\x1b]133;A\x07$ cmd{i}\r\n"));
            for j in 0..10 {
                text.push_str(&format!("out {i}.{j}\r\n"));
            }
        }
        let mut t = term(4, 20, &text);
        let mut v = View::new();
        v.apply(&mut t, ViewCmd::BlockJump { older: true });
        let l = live(&mut t);
        v.sync(&t, &l, false);
        assert_eq!(rows_text(&v.composed().rows).lines().next(), Some("$ cmd2"));
        v.apply(&mut t, ViewCmd::BlockJump { older: true });
        let l = live(&mut t);
        v.sync(&t, &l, false);
        assert_eq!(rows_text(&v.composed().rows).lines().next(), Some("$ cmd1"));
        v.apply(&mut t, ViewCmd::BlockJump { older: false });
        let l = live(&mut t);
        v.sync(&t, &l, false);
        assert_eq!(rows_text(&v.composed().rows).lines().next(), Some("$ cmd2"));
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
        let c = HlColours {
            sel_bg: Rgb(1, 2, 3),
            sel_fg: Some(Rgb(9, 9, 9)),
            match_bg: Rgb(4, 4, 4),
            match_fg: Some(Rgb(8, 8, 8)),
            current_bg: Rgb(5, 5, 5),
            current_fg: None,
            cursor_bg: Rgb(6, 6, 6),
            cursor_fg: None,
            hint_bg: Rgb(7, 7, 7),
            hint_fg: None,
            label_bg: Rgb(10, 10, 10),
            label_fg: Some(Rgb(11, 11, 11)),
        };
        paint(&h, &c, &mut out);
        assert_eq!(out[0].bg, Rgb(1, 2, 3));
        assert_eq!(out[0].fg, Some(Rgb(9, 9, 9)));
        assert_eq!((out[0].row, out[0].start, out[0].end), (1, 2, 4));
        let m = [Highlight {
            bg: Rgb(hl::CURRENT, 0, 0),
            ..h[0]
        }];
        paint(&m, &c, &mut out);
        assert_eq!(out[0].bg, Rgb(5, 5, 5));
        for (kind, bg, fg) in [
            (hl::MATCH, Rgb(4, 4, 4), Some(Rgb(8, 8, 8))),
            (hl::HINT, Rgb(7, 7, 7), None),
            (hl::LABEL, Rgb(10, 10, 10), Some(Rgb(11, 11, 11))),
        ] {
            let k = [Highlight {
                bg: Rgb(kind, 0, 0),
                ..h[0]
            }];
            paint(&k, &c, &mut out);
            assert_eq!((out[0].bg, out[0].fg), (bg, fg), "kind {kind}");
        }
    }
}
