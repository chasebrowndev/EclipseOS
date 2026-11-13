// SPDX-License-Identifier: AGPL-3.0-only
//! Selection geometry and text extraction (T-02 §3, NAVIGATION-12, SCROLLBACKSEARCH-21).
//!
//! A [`Selection`] is a pair of tracked pins with a cell side each, so it survives scroll
//! and reflow. Word, line, block and semantic-zone kinds are resolved against the grid
//! when asked ([`Screen::selection_range`]), never stored as row numbers. The gesture state
//! machine, autoscroll, copy-on-select and smart-selection rules belong to the app (T-05).
//!
//! Spec gaps decided here (reported): the word-chars default is alphanumerics plus `_`;
//! `trim-trailing` defaults to true; a semantic zone is the maximal run of cells with the
//! anchor's semantic bits and block id.

use crate::cell::{Semantic, Tag, Wide};
use crate::page::{MAX_CLUSTER_CPS, Page, row_flags as flags};
use crate::pin::{PinId, PinKind};
use crate::screen::Screen;

/// Most cells one word/zone expansion may walk.
const MAX_WALK: usize = 1 << 22;

/// Selection geometry kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelKind {
    /// Cell to cell, reading order.
    Simple,
    /// Whole words at both ends.
    Word,
    /// Whole logical lines (soft-wrapped rows count as one line).
    Line,
    /// Rectangle.
    Block,
    /// The shell-integration zone (prompt, input, output of one block).
    Semantic,
}

/// Which half of a cell a pin sits on (selects whether the cell is in or out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Left half.
    Left,
    /// Right half.
    Right,
}

/// A selection: two tracked pins, in any order.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    /// Geometry.
    pub kind: SelKind,
    /// Anchor pin.
    pub start: PinId,
    /// Moving pin.
    pub end: PinId,
    /// Side of the anchor.
    pub start_side: Side,
    /// Side of the moving pin.
    pub end_side: Side,
}

/// Word-boundary character classes (`selection.word-chars`).
#[derive(Clone, Debug, Default)]
pub struct WordChars {
    extra: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Other,
}

impl WordChars {
    /// Alphanumerics, `_` and every character of `extra` are word characters.
    #[must_use]
    pub fn new(extra: &str) -> WordChars {
        WordChars {
            extra: extra.to_owned(),
        }
    }

    fn class(&self, c: Option<char>) -> Class {
        match c {
            None | Some(' ') => Class::Space,
            Some(c) if c.is_alphanumeric() || c == '_' || self.extra.contains(c) => Class::Word,
            Some(_) => Class::Other,
        }
    }
}

/// Resolved selection: inclusive cell coordinates, `(abs row, col)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelRange {
    /// First cell.
    pub start: (usize, usize),
    /// Last cell (inclusive).
    pub end: (usize, usize),
    /// Rectangle: columns `start.1..=end.1` on every row (`start.1 <= end.1`).
    pub block: bool,
}

impl SelRange {
    /// Whether `(abs, col)` is inside the range.
    #[must_use]
    pub fn contains(&self, abs: usize, col: usize) -> bool {
        if abs < self.start.0 || abs > self.end.0 {
            return false;
        }
        if self.block {
            return col >= self.start.1 && col <= self.end.1;
        }
        (abs, col) >= self.start && (abs, col) <= self.end
    }
}

/// Extraction options.
#[derive(Clone, Copy, Debug)]
pub struct ExtractOpts {
    /// Drop blanks at the end of each (unwrapped) row (`selection.trim-trailing`).
    pub trim_trailing: bool,
}

impl Default for ExtractOpts {
    fn default() -> Self {
        ExtractOpts {
            trim_trailing: true,
        }
    }
}

fn first_char(page: &Page, pos: usize, col: usize) -> Option<char> {
    let c = page.cell(pos, col);
    match c.tag() {
        Tag::Scalar => c.scalar_char(),
        Tag::Cluster => {
            let mut buf = ['\0'; MAX_CLUSTER_CPS];
            let n = page.cluster_read(c.cluster_index(), &mut buf);
            (n > 0).then(|| buf[0])
        }
        _ => None,
    }
}

impl Screen {
    pub(crate) fn row_ref(&self, abs: usize) -> Option<(&Page, usize)> {
        let (pi, pos) = self.loc_abs(abs)?;
        Some((&self.pages[pi], pos))
    }

    fn total_abs_rows(&self) -> usize {
        self.history_rows() + self.rows()
    }

    /// Cell before `(abs, col)` across soft wraps.
    fn prev_cell(&self, abs: usize, col: usize, wrap_only: bool) -> Option<(usize, usize)> {
        if col > 0 {
            return Some((abs, col - 1));
        }
        let a = abs.checked_sub(1)?;
        let (p, pos) = self.row_ref(a)?;
        if wrap_only && p.row_flags(pos) & flags::WRAPPED == 0 {
            return None;
        }
        Some((a, self.cols() - 1))
    }

    fn next_cell(&self, abs: usize, col: usize, wrap_only: bool) -> Option<(usize, usize)> {
        if col + 1 < self.cols() {
            return Some((abs, col + 1));
        }
        if wrap_only {
            let (p, pos) = self.row_ref(abs)?;
            if p.row_flags(pos) & flags::WRAPPED == 0 {
                return None;
            }
        }
        (abs + 1 < self.total_abs_rows()).then_some((abs + 1, 0))
    }

    /// Snap a column onto the head of a wide character.
    fn head_col(&self, abs: usize, col: usize) -> usize {
        match self.row_ref(abs) {
            Some((p, pos)) if col > 0 && p.cell(pos, col).wide() == Wide::SpacerTail => col - 1,
            _ => col,
        }
    }

    fn tail_col(&self, abs: usize, col: usize) -> usize {
        match self.row_ref(abs) {
            Some((p, pos)) if col + 1 < self.cols() && p.cell(pos, col).wide() == Wide::Wide => {
                col + 1
            }
            _ => col,
        }
    }

    fn is_spacer_head(&self, at: (usize, usize)) -> bool {
        self.row_ref(at.0)
            .is_some_and(|(p, pos)| p.cell(pos, at.1).wide() == Wide::SpacerHead)
    }

    fn class_at(&self, wc: &WordChars, abs: usize, col: usize) -> Class {
        let col = self.head_col(abs, col);
        match self.row_ref(abs) {
            Some((p, pos)) => wc.class(first_char(p, pos, col)),
            None => Class::Space,
        }
    }

    fn word_bounds(&self, wc: &WordChars, at: (usize, usize)) -> ((usize, usize), (usize, usize)) {
        let k = self.class_at(wc, at.0, at.1);
        let (mut s, mut e) = (at, at);
        let mut n = 0;
        let mut cur = s;
        while let Some(p) = self.prev_cell(cur.0, cur.1, true) {
            n += 1;
            cur = p;
            if self.is_spacer_head(p) {
                if n > MAX_WALK {
                    break;
                }
                continue;
            }
            if n > MAX_WALK || self.class_at(wc, p.0, p.1) != k {
                break;
            }
            s = p;
        }
        let mut cur = e;
        while let Some(p) = self.next_cell(cur.0, cur.1, true) {
            n += 1;
            cur = p;
            if self.is_spacer_head(p) {
                if n > MAX_WALK {
                    break;
                }
                continue;
            }
            if n > MAX_WALK || self.class_at(wc, p.0, p.1) != k {
                break;
            }
            e = p;
        }
        (
            (s.0, self.head_col(s.0, s.1)),
            (e.0, self.tail_col(e.0, e.1)),
        )
    }

    fn line_rows(&self, abs: usize) -> (usize, usize) {
        let mut s = abs;
        while s > 0 {
            match self.row_ref(s - 1) {
                Some((p, pos)) if p.row_flags(pos) & flags::WRAPPED != 0 => s -= 1,
                _ => break,
            }
        }
        let mut e = abs;
        while e + 1 < self.total_abs_rows() {
            match self.row_ref(e) {
                Some((p, pos)) if p.row_flags(pos) & flags::WRAPPED != 0 => e += 1,
                _ => break,
            }
        }
        (s, e)
    }

    fn zone_key(&self, abs: usize, col: usize) -> Option<(Semantic, u32)> {
        let (p, pos) = self.row_ref(abs)?;
        Some((p.cell(pos, col).semantic(), p.row_block(pos)))
    }

    fn zone_bounds(&self, at: (usize, usize)) -> ((usize, usize), (usize, usize)) {
        let Some(k) = self.zone_key(at.0, at.1) else {
            return (at, at);
        };
        let (mut s, mut e) = (at, at);
        let mut n = 0;
        while let Some(p) = self.prev_cell(s.0, s.1, false) {
            n += 1;
            if n > MAX_WALK || self.zone_key(p.0, p.1) != Some(k) {
                break;
            }
            s = p;
        }
        while let Some(p) = self.next_cell(e.0, e.1, false) {
            n += 1;
            if n > MAX_WALK || self.zone_key(p.0, p.1) != Some(k) {
                break;
            }
            e = p;
        }
        (s, e)
    }

    // ---- selection lifecycle ------------------------------------------------------

    /// Begin a selection anchored at `(abs, col)`; both pins start there.
    pub fn selection_begin(
        &mut self,
        kind: SelKind,
        abs: usize,
        col: usize,
        side: Side,
    ) -> Option<Selection> {
        let start = self.track_abs(abs, col, PinKind::SelectionStart)?;
        let Some(end) = self.track_abs(abs, col, PinKind::SelectionEnd) else {
            self.untrack_pin(start);
            return None;
        };
        Some(Selection {
            kind,
            start,
            end,
            start_side: side,
            end_side: side,
        })
    }

    /// Move the free end to `(abs, col)`.
    pub fn selection_extend(&mut self, sel: &mut Selection, abs: usize, col: usize, side: Side) {
        if let Some(n) = self.track_abs(abs, col, PinKind::SelectionEnd) {
            self.untrack_pin(sel.end);
            sel.end = n;
            sel.end_side = side;
        }
    }

    /// Drop the selection's pins.
    pub fn selection_clear(&mut self, sel: Selection) {
        self.untrack_pin(sel.start);
        self.untrack_pin(sel.end);
    }

    /// Resolve the selection to inclusive cells; `None` if a pin is garbage or the
    /// selection is empty.
    #[must_use]
    pub fn selection_range(&self, sel: &Selection, wc: &WordChars) -> Option<SelRange> {
        let a = self.pin_pos(sel.start)?;
        let b = self.pin_pos(sel.end)?;
        let (mut p, mut ps, mut q, mut qs) = (a, sel.start_side, b, sel.end_side);
        if p > q {
            std::mem::swap(&mut p, &mut q);
            std::mem::swap(&mut ps, &mut qs);
        }
        match sel.kind {
            SelKind::Simple => {
                let mut s = (p.0, self.head_col(p.0, p.1));
                let mut e = (q.0, self.tail_col(q.0, q.1));
                if ps == Side::Right {
                    s = self.next_cell(s.0, self.tail_col(s.0, s.1), false)?;
                }
                if qs == Side::Left {
                    e = self.prev_cell(e.0, self.head_col(e.0, e.1), false)?;
                }
                // A SpacerHead is not content: step over it so the selection is empty
                // exactly when it would be at any other wrap position.
                while self.is_spacer_head(s) {
                    s = self.next_cell(s.0, s.1, false)?;
                }
                while self.is_spacer_head(e) {
                    e = self.prev_cell(e.0, e.1, false)?;
                }
                (s <= e).then_some(SelRange {
                    start: s,
                    end: e,
                    block: false,
                })
            }
            SelKind::Word => Some(SelRange {
                start: self.word_bounds(wc, p).0,
                end: self.word_bounds(wc, q).1,
                block: false,
            }),
            SelKind::Line => Some(SelRange {
                start: (self.line_rows(p.0).0, 0),
                end: (self.line_rows(q.0).1, self.cols() - 1),
                block: false,
            }),
            SelKind::Block => {
                let (c0, c1) = (p.1.min(q.1), p.1.max(q.1));
                let c0 = self.head_col(q.0, self.head_col(p.0, c0));
                let c1 = self.tail_col(q.0, self.tail_col(p.0, c1));
                Some(SelRange {
                    start: (p.0, c0),
                    end: (q.0, c1),
                    block: true,
                })
            }
            SelKind::Semantic => Some(SelRange {
                start: self.zone_bounds(p).0,
                end: self.zone_bounds(q).1,
                block: false,
            }),
        }
    }

    /// Append the text of `r` to `out`: soft-wrapped rows are joined, rows are separated by
    /// `\n`, trailing blanks are trimmed per `opts`, clusters are written whole, wide
    /// spacers are skipped. Returns false if a row is gone.
    pub fn extract(&self, r: &SelRange, opts: &ExtractOpts, out: &mut String) -> bool {
        let cols = self.cols();
        let mut pending = 0usize;
        for abs in r.start.0..=r.end.0 {
            let Some((p, pos)) = self.row_ref(abs) else {
                return false;
            };
            let (c0, c1) = if r.block {
                (r.start.1, r.end.1)
            } else {
                (
                    if abs == r.start.0 { r.start.1 } else { 0 },
                    if abs == r.end.0 { r.end.1 } else { cols - 1 },
                )
            };
            for x in c0..=c1.min(cols - 1) {
                let c = p.cell(pos, x);
                match c.wide() {
                    Wide::SpacerTail | Wide::SpacerHead => continue,
                    _ => {}
                }
                let blank = match c.tag() {
                    Tag::Scalar => c.scalar_char() == Some(' '),
                    Tag::Cluster => false,
                    _ => true,
                };
                if blank {
                    pending += 1;
                    continue;
                }
                for _ in 0..pending {
                    out.push(' ');
                }
                pending = 0;
                if c.tag() == Tag::Cluster {
                    let mut buf = ['\0'; MAX_CLUSTER_CPS];
                    let n = p.cluster_read(c.cluster_index(), &mut buf);
                    out.extend(buf[..n].iter());
                } else if let Some(ch) = c.scalar_char() {
                    out.push(ch);
                }
            }
            let last = abs == r.end.0;
            let wrapped = !r.block && !last && p.row_flags(pos) & flags::WRAPPED != 0;
            if wrapped {
                continue;
            }
            if !opts.trim_trailing {
                for _ in 0..pending {
                    out.push(' ');
                }
            }
            pending = 0;
            if !last {
                out.push('\n');
            }
        }
        true
    }

    /// Text of a selection (`selection_range` then `extract`).
    pub fn selection_text(
        &self,
        sel: &Selection,
        wc: &WordChars,
        opts: &ExtractOpts,
        out: &mut String,
    ) -> bool {
        match self.selection_range(sel, wc) {
            Some(r) => self.extract(&r, opts, out),
            None => false,
        }
    }
}
