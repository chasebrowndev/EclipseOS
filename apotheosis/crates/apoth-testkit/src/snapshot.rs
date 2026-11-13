// SPDX-License-Identifier: AGPL-3.0-only
//! What the harness observes of a terminal, and readable diffs of it.
//!
//! The snapshot is plain data (strings and integers) so it is independent of the
//! `apoth-term` / `apoth-grid` types; [`crate::adapter`] is the only converter.

use std::fmt::Write as _;

/// Width class of a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WideKind {
    /// One column.
    Narrow,
    /// Left half of a wide glyph.
    Wide,
    /// Right half (spacer tail).
    Tail,
    /// Leading spacer left when a wide glyph wrapped (spacer head).
    Head,
}

impl WideKind {
    /// Fixture name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            WideKind::Narrow => "narrow",
            WideKind::Wide => "wide",
            WideKind::Tail => "tail",
            WideKind::Head => "head",
        }
    }
}

/// One cell, styles resolved to fixture strings.
///
/// Colours: `default`, `p<N>` (palette), `#rrggbb`. Underline: `none single double curly
/// dotted dashed`. Attributes: sorted, comma separated subset of `bold faint italic blink
/// inverse invisible strike overline`, or empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellInfo {
    /// Text of the cell (a cluster may be several scalars); empty for an empty cell.
    pub ch: String,
    /// Width class.
    pub wide: WideKind,
    /// Foreground.
    pub fg: String,
    /// Background.
    pub bg: String,
    /// Underline colour.
    pub ulc: String,
    /// Underline style.
    pub ul: String,
    /// Attributes.
    pub attrs: String,
    /// Hyperlink URI, if the cell carries one.
    pub link: Option<String>,
}

impl CellInfo {
    /// An empty default cell.
    #[must_use]
    pub fn blank() -> CellInfo {
        CellInfo {
            ch: String::new(),
            wide: WideKind::Narrow,
            fg: "default".into(),
            bg: "default".into(),
            ulc: "default".into(),
            ul: "none".into(),
            attrs: String::new(),
            link: None,
        }
    }
}

/// Cursor state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorInfo {
    /// 0-based row of the active area.
    pub row: usize,
    /// 0-based column.
    pub col: usize,
    /// DECTCEM.
    pub visible: bool,
    /// DECSCUSR Ps (0..=6; 0 means the configured default).
    pub shape: u8,
}

/// The observable state after a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Rows of the active area.
    pub rows: usize,
    /// Columns.
    pub cols: usize,
    /// Cells, `rows` x `cols`.
    pub cells: Vec<Vec<CellInfo>>,
    /// Cursor.
    pub cursor: CursorInfo,
    /// Primary-screen history, oldest first, text only (right-trimmed).
    pub scrollback: Vec<String>,
    /// Alternate screen active.
    pub alt: bool,
    /// Synchronized-output presentation hold active.
    pub hold: bool,
}

impl Snapshot {
    /// Text of one row: clusters in order, empty cells as spaces, spacer cells skipped,
    /// trailing spaces trimmed.
    #[must_use]
    pub fn row_text(&self, y: usize) -> String {
        row_text(&self.cells[y])
    }

    /// All rows as text.
    #[must_use]
    pub fn grid_text(&self) -> Vec<String> {
        (0..self.cells.len()).map(|y| self.row_text(y)).collect()
    }
}

/// Text of a row of cells (see [`Snapshot::row_text`]).
#[must_use]
pub fn row_text(cells: &[CellInfo]) -> String {
    let mut s = String::new();
    for c in cells {
        match c.wide {
            WideKind::Tail | WideKind::Head => {}
            _ => {
                if c.ch.is_empty() {
                    s.push(' ');
                } else {
                    s.push_str(&c.ch);
                }
            }
        }
    }
    s.truncate(s.trim_end_matches(' ').len());
    s
}

/// Make control characters in text visible for diffs.
#[must_use]
pub fn show(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\u{1b}' => o.push_str("\\e"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                let _ = write!(o, "\\u{{{:x}}}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

/// A readable line-by-line diff of two row lists. `None` when equal.
///
/// Rows are numbered from 0; differing rows are marked `!` and both versions are shown,
/// delimited by `|` so trailing spaces are visible.
#[must_use]
pub fn diff_rows(label: &str, expected: &[String], actual: &[String]) -> Option<String> {
    if expected == actual {
        return None;
    }
    let n = expected.len().max(actual.len());
    let mut o = format!(
        "{label} differs (expected {} rows, actual {} rows)\n",
        expected.len(),
        actual.len()
    );
    for i in 0..n {
        let e = expected.get(i);
        let a = actual.get(i);
        let mark = if e == a { ' ' } else { '!' };
        let es = e.map_or_else(|| "<none>".to_string(), |s| format!("|{}|", show(s)));
        let a_s = a.map_or_else(|| "<none>".to_string(), |s| format!("|{}|", show(s)));
        if e == a {
            let _ = writeln!(o, "  {mark} {i:>3}  {es}");
        } else {
            let _ = writeln!(o, "  {mark} {i:>3}  expected {es}\n        actual   {a_s}");
        }
    }
    Some(o)
}

/// A readable diff of two byte strings (replies), shown escaped, with the first differing
/// offset. `None` when equal.
#[must_use]
pub fn diff_bytes(label: &str, expected: &[u8], actual: &[u8]) -> Option<String> {
    if expected == actual {
        return None;
    }
    let first = expected
        .iter()
        .zip(actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    Some(format!(
        "{label} differs at byte {first}\n  expected ({} bytes) {}\n  actual   ({} bytes) {}\n",
        expected.len(),
        crate::escapes::visible(expected),
        actual.len(),
        crate::escapes::visible(actual)
    ))
}

/// A readable diff of two string lists (events). `None` when equal.
#[must_use]
pub fn diff_list(label: &str, expected: &[String], actual: &[String]) -> Option<String> {
    if expected == actual {
        return None;
    }
    let mut o = format!("{label} differs\n");
    let n = expected.len().max(actual.len());
    for i in 0..n {
        let e = expected.get(i).map(|s| show(s));
        let a = actual.get(i).map(|s| show(s));
        if e == a {
            let _ = writeln!(o, "    {i:>3}  {}", e.unwrap_or_default());
        } else {
            let _ = writeln!(
                o,
                "  ! {i:>3}  expected {}\n        actual   {}",
                e.unwrap_or_else(|| "<none>".into()),
                a.unwrap_or_else(|| "<none>".into())
            );
        }
    }
    Some(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: &str, wide: WideKind) -> CellInfo {
        CellInfo {
            ch: ch.into(),
            wide,
            ..CellInfo::blank()
        }
    }

    #[test]
    fn row_text_skips_spacers_and_trims() {
        let row = vec![
            cell("a", WideKind::Narrow),
            cell("日", WideKind::Wide),
            cell("", WideKind::Tail),
            cell("", WideKind::Narrow),
            cell("", WideKind::Narrow),
        ];
        assert_eq!(row_text(&row), "a日");
        let row = vec![
            cell("a", WideKind::Narrow),
            cell("", WideKind::Narrow),
            cell("b", WideKind::Narrow),
        ];
        assert_eq!(row_text(&row), "a b");
    }

    #[test]
    fn row_diff_marks_only_differences() {
        let e = vec!["abc".to_string(), "x".to_string()];
        let a = vec!["abc".to_string(), "y".to_string(), "z".to_string()];
        let d = diff_rows("grid", &e, &a).unwrap();
        assert!(d.contains("expected 2 rows, actual 3 rows"));
        assert!(d.contains("!   1  expected |x|"));
        assert!(d.contains("actual   |y|"));
        assert!(d.contains("<none>"));
        assert!(!d.contains("!   0"));
        assert!(diff_rows("grid", &e, &e).is_none());
    }

    #[test]
    fn byte_diff_reports_offset_and_escapes() {
        let d = diff_bytes("replies", b"\x1b[?62c", b"\x1b[?63c").unwrap();
        assert!(d.contains("at byte 4"));
        assert!(d.contains("\\e[?62c"));
        assert!(diff_bytes("r", b"a", b"a").is_none());
    }

    #[test]
    fn show_escapes_controls() {
        assert_eq!(show("a\u{1b}\u{7}"), "a\\e\\u{7}");
    }
}
