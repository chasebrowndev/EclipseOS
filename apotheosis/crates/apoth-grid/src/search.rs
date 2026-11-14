// SPDX-License-Identifier: AGPL-3.0-only
//! Search primitives (T-02 §3 logical-line iterator, SCROLLBACKSEARCH-09; APO T-14 §6).
//!
//! The grid supplies the logical line (soft-wrapped rows joined, wide spacers skipped,
//! trailing blanks trimmed) as text with a byte-offset to cell map, and a directional
//! search over lines that returns matches as pin pairs. The matcher is a closure: literal
//! search is built in; a regex engine (T-14, regex-automata, app side) plugs into the same
//! closure, so the grid takes no regex dependency. Lines cross page seams transparently.
//! A logical line longer than [`MAX_LINE_BYTES`] is cut there (budget, T-18 §5).
//! One scratch value is reused across lines, so a warm search allocates nothing per line.

use crate::cell::{Tag, Wide};
use crate::page::{MAX_CLUSTER_CPS, row_flags as flags};
use crate::pin::Pin;
use crate::screen::Screen;

/// Longest logical line text handed to a matcher.
pub const MAX_LINE_BYTES: usize = 1 << 20;

/// Search direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Toward newer rows.
    Forward,
    /// Toward older rows.
    Backward,
}

#[derive(Clone, Copy, Debug)]
struct Ent {
    off: u32,
    abs: u32,
    col: u16,
    cells: u8,
}

/// Reusable buffers for one search.
#[derive(Debug, Default)]
pub struct LineScratch {
    /// The current logical line's text.
    pub text: String,
    map: Vec<Ent>,
    matches: Vec<(usize, usize)>,
}

impl LineScratch {
    /// The cell holding byte `byte` of [`LineScratch::text`]: `(abs row, column, cells)`
    /// where `cells` is 2 for a wide character. `None` past the end.
    #[must_use]
    pub fn locate(&self, byte: usize) -> Option<(usize, usize, usize)> {
        if self.map.is_empty() || byte >= self.text.len() {
            return None;
        }
        let i = self
            .map
            .partition_point(|e| e.off as usize <= byte)
            .saturating_sub(1);
        let e = self.map[i];
        Some((e.abs as usize, usize::from(e.col), usize::from(e.cells)))
    }
}

/// Where a logical line starts and where the next one begins (absolute rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineSpan {
    /// First row.
    pub start: usize,
    /// First row of the next line (equals the row count at the end).
    pub next: usize,
}

fn fold_char(c: char) -> char {
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// Fold text the way [`Screen::logical_line`] does with `fold` set.
#[must_use]
pub fn fold_text(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

impl Screen {
    /// Build the logical line containing row `abs` into `scratch`. With `fold`, characters
    /// are lower-cased one-to-one so offsets stay consistent with [`fold_text`].
    pub fn logical_line(
        &self,
        abs: usize,
        fold: bool,
        scratch: &mut LineScratch,
    ) -> Option<LineSpan> {
        let total = self.history_rows() + self.rows();
        if abs >= total {
            return None;
        }
        let mut start = abs;
        while start > 0 {
            match self.row_ref(start - 1) {
                Some((p, pos)) if p.row_flags(pos) & flags::WRAPPED != 0 => start -= 1,
                _ => break,
            }
        }
        scratch.text.clear();
        scratch.map.clear();
        let cols = self.cols();
        let mut r = start;
        loop {
            let (p, pos) = self.row_ref(r)?;
            for x in 0..cols {
                let c = p.cell(pos, x);
                if matches!(c.wide(), Wide::SpacerTail | Wide::SpacerHead) {
                    continue;
                }
                if scratch.text.len() >= MAX_LINE_BYTES {
                    break;
                }
                scratch.map.push(Ent {
                    off: scratch.text.len() as u32,
                    abs: r as u32,
                    col: x as u16,
                    cells: if c.wide() == Wide::Wide { 2 } else { 1 },
                });
                match c.tag() {
                    Tag::Scalar => {
                        let ch = c.scalar_char().unwrap_or(' ');
                        scratch.text.push(if fold { fold_char(ch) } else { ch });
                    }
                    Tag::Cluster => {
                        let mut buf = ['\0'; MAX_CLUSTER_CPS];
                        let n = p.cluster_read(c.cluster_index(), &mut buf);
                        for &ch in &buf[..n] {
                            scratch.text.push(if fold { fold_char(ch) } else { ch });
                        }
                    }
                    _ => scratch.text.push(' '),
                }
            }
            let wrapped = p.row_flags(pos) & flags::WRAPPED != 0;
            r += 1;
            if !wrapped || r >= total {
                break;
            }
        }
        while scratch.text.ends_with(' ') {
            scratch.text.pop();
            scratch.map.pop();
        }
        Some(LineSpan { start, next: r })
    }

    fn ent_index(scratch: &LineScratch, byte: usize) -> usize {
        scratch
            .map
            .partition_point(|e| e.off as usize <= byte)
            .saturating_sub(1)
    }

    /// Pins of the byte range `s..e` of the scratch line (end pin on the last cell).
    fn match_pins(&self, scratch: &LineScratch, s: usize, e: usize) -> Option<(Pin, Pin)> {
        if scratch.map.is_empty() || s >= e {
            return None;
        }
        let a = scratch.map[Self::ent_index(scratch, s)];
        let b = scratch.map[Self::ent_index(scratch, e - 1)];
        let end_col = (usize::from(b.col) + usize::from(b.cells) - 1).min(self.cols() - 1);
        Some((
            self.pin_at(a.abs as usize, usize::from(a.col))?,
            self.pin_at(b.abs as usize, end_col)?,
        ))
    }

    /// Search for matches line by line. `matcher` appends every match range (byte offsets,
    /// ascending) of the text it is given to the vector. Forward finds the first match
    /// starting after `from` (or the first overall); backward the last match starting
    /// before it (or the last overall). The match is `(first cell, last cell)` pins.
    /// No wrap-around: the caller restarts from the other end.
    pub fn search(
        &self,
        dir: Dir,
        from: Option<Pin>,
        fold: bool,
        scratch: &mut LineScratch,
        mut matcher: impl FnMut(&str, &mut Vec<(usize, usize)>),
    ) -> Option<(Pin, Pin)> {
        let total = self.history_rows() + self.rows();
        let from_cell = match from {
            Some(p) => Some(self.pin_abs(&p)?),
            None => None,
        };
        let mut row = match (dir, from_cell) {
            (_, Some((a, _))) => a,
            (Dir::Forward, None) => 0,
            (Dir::Backward, None) => total - 1,
        };
        loop {
            let span = self.logical_line(row, fold, scratch)?;
            let mut m = std::mem::take(&mut scratch.matches);
            m.clear();
            matcher(&scratch.text, &mut m);
            let mut found = None;
            let cell_of = |s: &LineScratch, b: usize| {
                let e = s.map[Self::ent_index(s, b)];
                (e.abs as usize, usize::from(e.col))
            };
            if !scratch.map.is_empty() {
                let ok = |s: &LineScratch, a: usize, b: usize| {
                    a < b
                        && b <= s.text.len()
                        && match (dir, from_cell) {
                            (_, None) => true,
                            (Dir::Forward, Some(f)) => cell_of(s, a) > f,
                            (Dir::Backward, Some(f)) => cell_of(s, a) < f,
                        }
                };
                match dir {
                    Dir::Forward => {
                        found = m.iter().copied().find(|&(a, b)| ok(scratch, a, b));
                    }
                    Dir::Backward => {
                        found = m.iter().rev().copied().find(|&(a, b)| ok(scratch, a, b));
                    }
                }
            }
            scratch.matches = m;
            if let Some((a, b)) = found {
                return self.match_pins(scratch, a, b);
            }
            match dir {
                Dir::Forward => {
                    if span.next >= total {
                        return None;
                    }
                    row = span.next;
                }
                Dir::Backward => {
                    if span.start == 0 {
                        return None;
                    }
                    row = span.start - 1;
                }
            }
        }
    }

    /// Literal search (see [`Screen::search`]). With `case_insensitive` both sides are
    /// folded; the empty needle matches nothing.
    pub fn find_literal(
        &self,
        needle: &str,
        dir: Dir,
        from: Option<Pin>,
        case_insensitive: bool,
        scratch: &mut LineScratch,
    ) -> Option<(Pin, Pin)> {
        if needle.is_empty() {
            return None;
        }
        let folded;
        let n = if case_insensitive {
            folded = fold_text(needle);
            folded.as_str()
        } else {
            needle
        };
        self.search(dir, from, case_insensitive, scratch, |text, out| {
            let mut i = 0;
            while let Some(k) = text[i..].find(n) {
                out.push((i + k, i + k + n.len()));
                i += k + text[i + k..].chars().next().map_or(1, char::len_utf8);
            }
        })
    }
}
