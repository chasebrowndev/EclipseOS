// SPDX-License-Identifier: AGPL-3.0-only
//! The text model of one visible row (T-20 §3): extracted text in Unicode scalars, one
//! grapheme per cell (a cell's cluster is its grapheme), no padding blanks, and a position
//! table mapping graphemes to cells.

use apoth_grid::{SnapRow, Wide, WordChars, WordClass};

/// UTF-8 bytes kept per grapheme (AccessKit's `character_lengths` is a byte per character,
/// counted in UTF-8 code units).
const MAX_GRAPHEME_BYTES: usize = 255;

/// One grapheme of a row: its UTF-8 length in [`RowText::text`] and the cells it covers.
/// Offsets reported to assistive technology are scalar indices; AccessKit derives them from
/// these lengths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grapheme {
    /// UTF-8 bytes in the row text.
    pub bytes: u8,
    /// First cell.
    pub col: u16,
    /// Cells covered (2 for a wide character).
    pub width: u8,
}

/// The text of a visible row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowText {
    /// Extracted text, trailing blanks removed.
    pub text: String,
    /// One entry per grapheme, in order.
    pub graphemes: Vec<Grapheme>,
    /// The row soft-wraps into the next (joined without a line break).
    pub wrapped: bool,
}

impl RowText {
    /// An empty row.
    #[must_use]
    pub fn new(wrapped: bool) -> RowText {
        RowText {
            text: String::new(),
            graphemes: Vec::new(),
            wrapped,
        }
    }

    /// Append the grapheme `g` covering `width` cells at `col`. Graphemes are expected in
    /// column order; an empty `g` is ignored.
    pub fn push(&mut self, col: u16, g: &str, width: u8) {
        let mut n = 0usize;
        for c in g.chars() {
            if n + c.len_utf8() > MAX_GRAPHEME_BYTES {
                break;
            }
            self.text.push(c);
            n += c.len_utf8();
        }
        if n > 0 {
            self.graphemes.push(Grapheme {
                bytes: n as u8,
                col,
                width: width.max(1),
            });
        }
    }

    /// A row of single-width scalars from `s`, one per cell (tests and simple callers).
    #[must_use]
    pub fn plain(s: &str, wrapped: bool) -> RowText {
        let mut r = RowText::new(wrapped);
        let mut buf = [0u8; 4];
        for (i, c) in s.chars().enumerate() {
            r.push(i as u16, c.encode_utf8(&mut buf), 1);
        }
        r.trim();
        r
    }

    /// Text of a copied grid row. Blank cells inside the row are spaces; trailing blanks
    /// are dropped; spacer cells belong to the wide character before them.
    #[must_use]
    pub fn from_snap(row: &SnapRow) -> RowText {
        let mut r = RowText::new(row.wrapped);
        let mut buf = String::new();
        for (x, c) in row.cells.iter().enumerate() {
            let col = x as u16;
            match c.wide {
                Wide::SpacerTail => continue,
                Wide::SpacerHead => {
                    r.push(col, " ", 1);
                    continue;
                }
                _ => {}
            }
            let width = if c.wide == Wide::Wide { 2 } else { 1 };
            buf.clear();
            if c.cluster_len > 0 {
                for &cp in row.cluster(c) {
                    buf.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                }
            } else if c.cp == 0 {
                buf.push(' ');
            } else {
                buf.push(char::from_u32(c.cp).unwrap_or('\u{fffd}'));
            }
            r.push(col, &buf, width);
        }
        r.trim();
        r
    }

    /// Drop trailing blank graphemes (no padding blanks, T-20 §3).
    pub fn trim(&mut self) {
        while let Some(g) = self.graphemes.last() {
            let start = self.text.len() - usize::from(g.bytes);
            if self.text[start..].chars().all(|c| c == ' ') {
                self.text.truncate(start);
                self.graphemes.pop();
            } else {
                break;
            }
        }
    }

    /// Number of graphemes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.graphemes.len()
    }

    /// No text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.graphemes.is_empty()
    }

    /// The grapheme at or after cell `col` (the caret's character index); `len()` past
    /// the text.
    #[must_use]
    pub fn index_of_col(&self, col: usize) -> usize {
        self.graphemes
            .iter()
            .position(|g| usize::from(g.col) + usize::from(g.width) > col)
            .unwrap_or(self.graphemes.len())
    }

    /// The first scalar of each grapheme, in order.
    pub fn first_chars(&self) -> impl Iterator<Item = char> + '_ {
        let mut at = 0usize;
        self.graphemes.iter().map(move |g| {
            let first = self.text[at..].chars().next().unwrap_or(' ');
            at += usize::from(g.bytes);
            first
        })
    }

    /// Word classes per grapheme under the selection rules (T-02), so screen-reader words
    /// and double-click words agree (T-20 §3).
    #[must_use]
    pub fn classes(&self, wc: &WordChars) -> Vec<WordClass> {
        self.first_chars().map(|c| wc.class(Some(c))).collect()
    }
}

/// Word starts (grapheme indices) of a row given the class of what precedes it: a word is
/// a run of one class; whitespace belongs to the word before it, and a line's leading
/// whitespace is a word of its own.
#[must_use]
pub fn word_starts(classes: &[WordClass], before: Option<WordClass>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut prev = before;
    for (i, &c) in classes.iter().enumerate() {
        let start = match prev {
            None => true,
            Some(p) => c != WordClass::Space && c != p,
        };
        if start {
            out.push(i);
        }
        prev = Some(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_grid::{Semantic, SnapCell, Style};

    fn cell(cp: char, wide: Wide) -> SnapCell {
        SnapCell {
            cp: cp as u32,
            cluster_start: 0,
            cluster_len: 0,
            wide,
            style: Style::default(),
            link: false,
            protected: false,
            semantic: Semantic::default(),
        }
    }

    #[test]
    fn plain_rows_trim_trailing_blanks() {
        let r = RowText::plain("ab c   ", false);
        assert_eq!(r.text, "ab c");
        assert_eq!(r.len(), 4);
        assert_eq!(r.graphemes[3].col, 3);
        assert!(RowText::plain("    ", false).is_empty());
    }

    #[test]
    fn wide_and_combining_cells_map_to_one_grapheme_each() {
        let mut row = SnapRow::default();
        // 'a', wide '界' + spacer, cluster "e\u{301}", blank, 'z', blanks.
        row.cells.push(cell('a', Wide::Narrow));
        row.cells.push(cell('界', Wide::Wide));
        row.cells.push(cell('\0', Wide::SpacerTail));
        let mut c = cell('e', Wide::Narrow);
        c.cluster_start = 0;
        c.cluster_len = 2;
        row.clusters = vec!['e' as u32, 0x301];
        row.cells.push(c);
        row.cells.push(cell('\0', Wide::Narrow));
        row.cells.push(cell('z', Wide::Narrow));
        row.cells.push(cell('\0', Wide::Narrow));
        let r = RowText::from_snap(&row);
        assert_eq!(r.text, "a界e\u{301} z");
        let lens: Vec<u8> = r.graphemes.iter().map(|g| g.bytes).collect();
        assert_eq!(lens, [1, 3, 3, 1, 1]);
        let cols: Vec<u16> = r.graphemes.iter().map(|g| g.col).collect();
        assert_eq!(cols, [0, 1, 3, 4, 5]);
        assert_eq!(r.graphemes[1].width, 2);
        // Cells to graphemes: the wide character's second cell is still grapheme 1.
        assert_eq!(r.index_of_col(2), 1);
        assert_eq!(r.index_of_col(3), 2);
        assert_eq!(r.index_of_col(40), 5);
    }

    #[test]
    fn words_follow_the_selection_classes() {
        let wc = WordChars::new("-");
        let r = RowText::plain("  ls -la foo.txt", false);
        let starts = word_starts(&r.classes(&wc), None);
        // Leading blanks, "ls ", "-la ", "foo", ".", "txt".
        assert_eq!(starts, [0, 2, 5, 9, 12, 13]);
        // A wrapped continuation inside a word starts no word.
        let cont = RowText::plain("bar baz", false);
        let s = word_starts(&cont.classes(&wc), Some(WordClass::Word));
        assert_eq!(s, [4]);
    }
}
