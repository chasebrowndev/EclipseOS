// SPDX-License-Identifier: AGPL-3.0-only
//! Copy mode motions (T-14 §8, COPYMODE-01..12): pure functions `(grid, position, motion)
//! → position` over absolute rows (0 = oldest retained row). The key tables that call them
//! are ordinary KDL key tables (`binds::COPY_MODE_VI`, `binds::COPY_MODE_EMACS`).
//!
//! Pane text is user content: nothing here logs it.

use apoth_action::enums::Motion;
use apoth_grid::{Screen, SnapRow, Wide, flags};

/// Where the viewport is, for screen-relative motions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ctx {
    /// Absolute row at the top of the viewport.
    pub(crate) top: usize,
    pub(crate) rows: usize,
}

/// Read access to rows by absolute index (the grid, or a test double).
pub(crate) trait Rows {
    fn total(&self) -> usize;
    fn cols(&self) -> usize;
    /// Characters of row `abs`, one per cell (a wide character's tail is `\0`, blanks are
    /// spaces), and whether the row soft-wraps into the next.
    fn row(&self, abs: usize, out: &mut Vec<char>) -> bool;
    /// A prompt starts on row `abs`.
    fn prompt(&self, abs: usize) -> bool;
}

impl Rows for Screen {
    fn total(&self) -> usize {
        self.history_rows() + self.rows()
    }
    fn cols(&self) -> usize {
        Screen::cols(self)
    }
    fn row(&self, abs: usize, out: &mut Vec<char>) -> bool {
        out.clear();
        let mut r = SnapRow::default();
        if !self.copy_abs_row(abs, &mut r) {
            out.resize(Screen::cols(self), ' ');
            return false;
        }
        out.extend(r.cells.iter().map(|c| match c.wide {
            Wide::SpacerTail => '\0',
            _ if c.cp == 0 => ' ',
            _ => char::from_u32(c.cp).unwrap_or(' '),
        }));
        out.resize(Screen::cols(self), ' ');
        r.wrapped
    }
    fn prompt(&self, abs: usize) -> bool {
        self.row_flags_abs(abs)
            .is_some_and(|f| f & flags::PROMPT_START != 0)
    }
}

/// Character class for word motions: 0 blank, 1 word, 2 punctuation (`big`: 1 for all
/// non-blank).
fn class(c: char, big: bool) -> u8 {
    if c == ' ' || c == '\0' || c == '\t' {
        0
    } else if big || c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// A cursor walking cells across rows: soft wraps join rows, hard ends count as a blank
/// (so a word never spans a hard line end).
struct Walk<'a, R: Rows> {
    g: &'a R,
    row: Vec<char>,
    wrapped: bool,
    abs: usize,
    col: usize,
}

impl<'a, R: Rows> Walk<'a, R> {
    fn new(g: &'a R, (abs, col): (usize, usize)) -> Self {
        let mut row = Vec::new();
        let wrapped = g.row(abs, &mut row);
        Walk {
            g,
            row,
            wrapped,
            abs,
            col: col.min(g.cols().saturating_sub(1)),
        }
    }

    fn load(&mut self, abs: usize) {
        self.abs = abs;
        self.wrapped = self.g.row(abs, &mut self.row);
    }

    fn ch(&self) -> char {
        self.row.get(self.col).copied().unwrap_or(' ')
    }

    /// Step forward one cell; `hard` is set when the step crossed a hard line end.
    fn next(&mut self) -> Option<bool> {
        if self.col + 1 < self.row.len() {
            self.col += 1;
            return Some(false);
        }
        if self.abs + 1 >= self.g.total() {
            return None;
        }
        let hard = !self.wrapped;
        self.load(self.abs + 1);
        self.col = 0;
        Some(hard)
    }

    fn prev(&mut self) -> Option<bool> {
        if self.col > 0 {
            self.col -= 1;
            return Some(false);
        }
        if self.abs == 0 {
            return None;
        }
        self.load(self.abs - 1);
        self.col = self.row.len().saturating_sub(1);
        Some(!self.wrapped)
    }

    fn pos(&self) -> (usize, usize) {
        (self.abs, self.col)
    }

    /// Class here, treating a cell right after a hard break as following a blank.
    fn cls(&self, big: bool) -> u8 {
        class(self.ch(), big)
    }
}

fn word_forward<R: Rows>(g: &R, p: (usize, usize), big: bool) -> (usize, usize) {
    let mut w = Walk::new(g, p);
    let start = w.cls(big);
    // Leave the current word.
    if start != 0 {
        loop {
            match w.next() {
                None => return w.pos(),
                Some(hard) => {
                    if hard {
                        break;
                    }
                    if w.cls(big) != start {
                        break;
                    }
                }
            }
        }
    }
    // Skip blanks to the next word start.
    while w.cls(big) == 0 {
        if w.next().is_none() {
            return w.pos();
        }
    }
    w.pos()
}

fn word_back<R: Rows>(g: &R, p: (usize, usize), big: bool) -> (usize, usize) {
    let mut w = Walk::new(g, p);
    // Step off the current cell, then skip blanks backwards.
    if w.prev().is_none() {
        return w.pos();
    }
    while w.cls(big) == 0 {
        if w.prev().is_none() {
            return w.pos();
        }
    }
    let c = w.cls(big);
    loop {
        let here = w.pos();
        match w.prev() {
            None => return w.pos(),
            Some(hard) => {
                if hard || w.cls(big) != c {
                    return here;
                }
            }
        }
    }
}

fn word_end<R: Rows>(g: &R, p: (usize, usize), big: bool) -> (usize, usize) {
    let mut w = Walk::new(g, p);
    if w.next().is_none() {
        return w.pos();
    }
    while w.cls(big) == 0 {
        if w.next().is_none() {
            return w.pos();
        }
    }
    let c = w.cls(big);
    loop {
        let here = w.pos();
        match w.next() {
            None => return w.pos(),
            Some(hard) => {
                if hard || w.cls(big) != c {
                    return here;
                }
            }
        }
    }
}

fn last_non_blank(row: &[char]) -> usize {
    row.iter()
        .rposition(|&c| c != ' ' && c != '\0')
        .unwrap_or(0)
}

fn first_non_blank(row: &[char]) -> usize {
    row.iter().position(|&c| c != ' ' && c != '\0').unwrap_or(0)
}

fn blank_row<R: Rows>(g: &R, abs: usize, buf: &mut Vec<char>) -> bool {
    g.row(abs, buf);
    buf.iter().all(|&c| c == ' ' || c == '\0')
}

/// Apply `m` once. Search motions are the caller's (they need the search run).
pub(crate) fn step<R: Rows>(g: &R, p: (usize, usize), m: Motion, ctx: Ctx) -> (usize, usize) {
    let total = g.total();
    if total == 0 {
        return (0, 0);
    }
    let last = total - 1;
    let cols = g.cols().max(1);
    let (abs, col) = (p.0.min(last), p.1.min(cols - 1));
    let mut buf = Vec::new();
    let page = ctx.rows.max(1);
    match m {
        Motion::Left => (abs, col.saturating_sub(1)),
        Motion::Right => (abs, (col + 1).min(cols - 1)),
        Motion::Up => (abs.saturating_sub(1), col),
        Motion::Down => ((abs + 1).min(last), col),
        Motion::WordForward => word_forward(g, (abs, col), false),
        Motion::BigWordForward => word_forward(g, (abs, col), true),
        Motion::WordBack => word_back(g, (abs, col), false),
        Motion::BigWordBack => word_back(g, (abs, col), true),
        Motion::WordEnd => word_end(g, (abs, col), false),
        Motion::BigWordEnd => word_end(g, (abs, col), true),
        Motion::LineStart => (abs, 0),
        Motion::LineEnd => {
            g.row(abs, &mut buf);
            (abs, last_non_blank(&buf))
        }
        Motion::FirstNonBlank => {
            g.row(abs, &mut buf);
            (abs, first_non_blank(&buf))
        }
        Motion::Top => (0, 0),
        Motion::Bottom => {
            g.row(last, &mut buf);
            (last, first_non_blank(&buf))
        }
        Motion::ScreenTop => (ctx.top.min(last), col),
        Motion::ScreenMiddle => ((ctx.top + page.saturating_sub(1) / 2).min(last), col),
        Motion::ScreenBottom => ((ctx.top + page - 1).min(last), col),
        Motion::PageUp => (abs.saturating_sub(page), col),
        Motion::PageDown => ((abs + page).min(last), col),
        Motion::HalfPageUp => (abs.saturating_sub((page / 2).max(1)), col),
        Motion::HalfPageDown => ((abs + (page / 2).max(1)).min(last), col),
        Motion::ParagraphBack => {
            let mut r = abs;
            // Leave blank rows, then find the next blank row above.
            while r > 0 && blank_row(g, r, &mut buf) {
                r -= 1;
            }
            while r > 0 && !blank_row(g, r, &mut buf) {
                r -= 1;
            }
            (r, 0)
        }
        Motion::ParagraphForward => {
            let mut r = abs;
            while r < last && blank_row(g, r, &mut buf) {
                r += 1;
            }
            while r < last && !blank_row(g, r, &mut buf) {
                r += 1;
            }
            (r, 0)
        }
        Motion::PromptPrev => (0..abs)
            .rev()
            .find(|&r| g.prompt(r))
            .map_or((abs, col), |r| (r, 0)),
        Motion::PromptNext => (abs + 1..total)
            .find(|&r| g.prompt(r))
            .map_or((abs, col), |r| (r, 0)),
        Motion::SearchNext | Motion::SearchPrev => (abs, col),
    }
}

/// `f`/`F`/`t`/`T` on the cursor's row; `None` when the character is not there.
pub(crate) fn jump<R: Rows>(
    g: &R,
    (abs, col): (usize, usize),
    ch: char,
    backward: bool,
    till: bool,
) -> Option<(usize, usize)> {
    let mut buf = Vec::new();
    g.row(abs, &mut buf);
    if backward {
        let i = (0..col).rev().find(|&i| buf[i] == ch)?;
        Some((abs, if till { (i + 1).min(col) } else { i }))
    } else {
        let i = (col + 1..buf.len()).find(|&i| buf[i] == ch)?;
        Some((
            abs,
            if till {
                i.saturating_sub(1).max(col)
            } else {
                i
            },
        ))
    }
}

/// Apply `m` `count` times.
pub(crate) fn repeat<R: Rows>(
    g: &R,
    mut p: (usize, usize),
    m: Motion,
    count: u32,
    ctx: Ctx,
) -> (usize, usize) {
    for _ in 0..count.clamp(1, 10_000) {
        let n = step(g, p, m, ctx);
        if n == p {
            break;
        }
        p = n;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows of text with explicit wrap flags and prompt rows.
    struct Fake {
        rows: Vec<(String, bool)>,
        prompts: Vec<usize>,
        cols: usize,
    }

    impl Rows for Fake {
        fn total(&self) -> usize {
            self.rows.len()
        }
        fn cols(&self) -> usize {
            self.cols
        }
        fn row(&self, abs: usize, out: &mut Vec<char>) -> bool {
            out.clear();
            out.extend(self.rows[abs].0.chars());
            out.resize(self.cols, ' ');
            self.rows[abs].1
        }
        fn prompt(&self, abs: usize) -> bool {
            self.prompts.contains(&abs)
        }
    }

    fn fake(lines: &[&str]) -> Fake {
        Fake {
            rows: lines
                .iter()
                .map(|l| match l.strip_suffix('\\') {
                    Some(s) => (s.to_string(), true),
                    None => (l.to_string(), false),
                })
                .collect(),
            prompts: Vec::new(),
            cols: 12,
        }
    }

    const CTX: Ctx = Ctx { top: 0, rows: 4 };

    /// The motion table: (rows, start, motion, expected).
    #[test]
    fn motion_table() {
        let g = fake(&["foo bar.baz", "  qux", "", "end-of itxyz\\", "wrapped tail"]);
        use Motion::*;
        type Row = ((usize, usize), Motion, (usize, usize));
        let table: &[Row] = &[
            ((0, 0), WordForward, (0, 4)),
            ((0, 4), WordForward, (0, 7)),
            ((0, 7), WordForward, (0, 8)),
            ((0, 8), WordForward, (1, 2)),
            ((0, 0), BigWordForward, (0, 4)),
            ((0, 4), BigWordForward, (1, 2)),
            ((1, 2), WordBack, (0, 8)),
            ((0, 9), WordBack, (0, 8)),
            ((0, 8), WordBack, (0, 7)),
            ((0, 8), BigWordBack, (0, 4)),
            ((0, 0), WordEnd, (0, 2)),
            ((0, 2), WordEnd, (0, 6)),
            ((0, 4), BigWordEnd, (0, 10)),
            ((0, 5), LineStart, (0, 0)),
            ((0, 0), LineEnd, (0, 10)),
            ((1, 0), FirstNonBlank, (1, 2)),
            ((1, 4), Up, (0, 4)),
            ((0, 4), Up, (0, 4)),
            ((4, 4), Down, (4, 4)),
            ((0, 11), Right, (0, 11)),
            ((0, 0), Left, (0, 0)),
            ((3, 0), Top, (0, 0)),
            ((0, 3), Bottom, (4, 0)),
            ((4, 0), ScreenTop, (0, 0)),
            ((0, 0), ScreenMiddle, (1, 0)),
            ((0, 0), ScreenBottom, (3, 0)),
            ((4, 1), PageUp, (0, 1)),
            ((0, 1), PageDown, (4, 1)),
            ((0, 1), HalfPageDown, (2, 1)),
            ((0, 0), ParagraphForward, (2, 0)),
            ((4, 0), ParagraphBack, (2, 0)),
            // A word crosses a soft wrap but not a hard line end.
            ((3, 7), WordEnd, (4, 6)),
            ((4, 0), WordBack, (3, 7)),
        ];
        for &(from, m, want) in table {
            assert_eq!(step(&g, from, m, CTX), want, "{m:?} from {from:?}");
        }
    }

    #[test]
    fn prompt_motions() {
        let mut g = fake(&["$ a", "out", "$ b", "out", "$ c"]);
        g.prompts = vec![0, 2, 4];
        assert_eq!(step(&g, (3, 1), Motion::PromptPrev, CTX), (2, 0));
        assert_eq!(step(&g, (2, 0), Motion::PromptPrev, CTX), (0, 0));
        assert_eq!(step(&g, (0, 0), Motion::PromptPrev, CTX), (0, 0));
        assert_eq!(step(&g, (1, 0), Motion::PromptNext, CTX), (2, 0));
        assert_eq!(repeat(&g, (0, 0), Motion::PromptNext, 5, CTX), (4, 0));
    }

    #[test]
    fn jumps() {
        let g = fake(&["a,b,c d"]);
        assert_eq!(jump(&g, (0, 0), ',', false, false), Some((0, 1)));
        assert_eq!(jump(&g, (0, 1), ',', false, false), Some((0, 3)));
        assert_eq!(jump(&g, (0, 0), ',', false, true), Some((0, 0)));
        assert_eq!(jump(&g, (0, 0), 'd', false, true), Some((0, 5)));
        assert_eq!(jump(&g, (0, 6), ',', true, false), Some((0, 3)));
        assert_eq!(jump(&g, (0, 6), ',', true, true), Some((0, 4)));
        assert_eq!(jump(&g, (0, 0), 'z', false, false), None);
    }

    #[test]
    fn real_grid_rows() {
        use apoth_term::{Term, TermConfig};
        let mut t = Term::new(TermConfig {
            rows: 3,
            cols: 10,
            ..TermConfig::default()
        });
        t.feed(
            b"\x1b]133;A\x07$ ls\r\nabcdefghijklm\r\n",
            std::time::Instant::now(),
        );
        let g = t.grid();
        let total = Rows::total(g);
        assert!(total >= 3);
        let ctx = Ctx { top: 0, rows: 3 };
        let start = (total - 1, 0);
        let p = step(g, start, Motion::PromptPrev, ctx);
        let mut buf = Vec::new();
        g.row(p.0, &mut buf);
        assert_eq!(buf.iter().take(4).collect::<String>(), "$ ls");
        // The wrapped word: its end is on the continuation row.
        let w = step(g, (p.0 + 1, 0), Motion::WordEnd, ctx);
        assert_eq!(w, (p.0 + 2, 2));
    }
}
