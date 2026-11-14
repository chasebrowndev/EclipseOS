// SPDX-License-Identifier: AGPL-3.0-only
//! `pane.text` extraction (T-12 §2): rows of the screen, the scrollback or both, trailing
//! blanks dropped, wrapped rows joined on request, cut to a byte budget keeping the newest
//! text (the reply must fit one 1 MiB line, T-18 §5). Runs on a worker under the pane's
//! taker lock; nothing here is logged.

use apoth_a11y::RowText;
use apoth_grid::SnapRow;
use apoth_grid::screen::Screen;
use apoth_proto::params::Extent;

/// Text bytes a reply may carry (the line limit less room for the envelope and escaping).
pub(crate) const TEXT_BUDGET: usize = 256 * 1024;

/// The text of `extent` (not `Selection`, which the caller answers) and whether it was cut.
#[must_use]
pub(crate) fn pane_text(g: &Screen, extent: Extent, wrap: bool, budget: usize) -> (String, bool) {
    let hist = g.history_rows();
    let (from, to) = match extent {
        Extent::Screen | Extent::Selection => (hist, hist + g.rows()),
        Extent::Scrollback => (0, hist),
        Extent::All => (0, hist + g.rows()),
    };
    let mut out = String::new();
    let mut row = SnapRow::default();
    for abs in from..to {
        if !g.copy_abs_row(abs, &mut row) {
            break;
        }
        let t = RowText::from_snap(&row);
        out.push_str(&t.text);
        if !(wrap && t.wrapped) {
            out.push('\n');
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if out == "\n" {
        out.clear();
    }
    if out.len() <= budget {
        return (out, false);
    }
    // Keep the newest text, starting at a line (or at least a character) boundary.
    let mut cut = out.len() - budget;
    while !out.is_char_boundary(cut) {
        cut += 1;
    }
    if let Some(nl) = out[cut..].find('\n')
        && nl + 1 < out.len() - cut
    {
        cut += nl + 1;
    }
    (out[cut..].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_term::{Term, TermConfig};
    use std::time::Instant;

    fn term(bytes: &[u8]) -> Term {
        let mut t = Term::new(TermConfig {
            rows: 3,
            cols: 10,
            ..TermConfig::default()
        });
        t.feed(bytes, Instant::now());
        t
    }

    #[test]
    fn screen_scrollback_and_wrap() {
        let t = term(b"one\r\ntwo\r\nthree\r\nfour 567890abc");
        let g = t.grid();
        let (s, cut) = pane_text(g, Extent::Screen, false, TEXT_BUDGET);
        assert_eq!(s, "three\nfour 56789\n0abc\n");
        assert!(!cut);
        let (s, _) = pane_text(g, Extent::Screen, true, TEXT_BUDGET);
        assert_eq!(s, "three\nfour 567890abc\n");
        let (s, _) = pane_text(g, Extent::Scrollback, false, TEXT_BUDGET);
        assert_eq!(s, "one\ntwo\n");
        let (s, _) = pane_text(g, Extent::All, true, TEXT_BUDGET);
        assert_eq!(s, "one\ntwo\nthree\nfour 567890abc\n");
        let (s, _) = pane_text(term(b"").grid(), Extent::Screen, false, TEXT_BUDGET);
        assert_eq!(s, "");
    }

    #[test]
    fn budget_keeps_the_newest_lines() {
        let t = term(b"aaaa\r\nbbbb\r\ncccc");
        let (s, cut) = pane_text(t.grid(), Extent::Screen, false, 8);
        assert!(cut);
        assert_eq!(s, "cccc\n");
    }
}
