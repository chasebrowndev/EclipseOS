// SPDX-License-Identifier: AGPL-3.0-only
//! Differential run against `alacritty_terminal` (T-25 section 2). The oracle is a full
//! terminal grid: it is fed the same bytes and compared cell by cell (text, cursor) with ours
//! over the whole corpus. A disagreement must be listed in `oracle::EXCLUSIONS` with a T-03
//! row and a classification; otherwise this test fails and prints the entry to review.
//! Oracles are never the specification (T-25 section 2).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use apoth_testkit::adapter::TermDriver;
use apoth_testkit::conformance;
use apoth_testkit::oracle::{Oracle, OracleView, Verdict, compare, disagreement_report};
use apoth_testkit::{fixtures_dir, load_fixtures};

struct AlacrittyOracle;

impl Oracle for AlacrittyOracle {
    fn name(&self) -> &'static str {
        "alacritty_terminal"
    }

    fn run(&mut self, rows: usize, cols: usize, bytes: &[u8]) -> OracleView {
        let size = TermSize::new(cols, rows);
        let mut term = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        let grid = term.grid();
        let mut lines = Vec::with_capacity(rows);
        for r in 0..rows {
            let mut s = String::new();
            for c in 0..cols {
                let cell = &grid[Line(i32::try_from(r).unwrap_or(i32::MAX))][Column(c)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                // The oracle stores a tab it skipped over as a `\t` cell; it displays blank.
                s.push(if cell.c == '\t' { ' ' } else { cell.c });
                if let Some(z) = cell.zerowidth() {
                    s.extend(z.iter());
                }
            }
            lines.push(s.trim_end().to_string());
        }
        let p = grid.cursor.point;
        OracleView {
            lines,
            cursor: (usize::try_from(p.line.0).unwrap_or(0), p.column.0),
            unsupported: None,
        }
    }
}

#[test]
fn corpus_agrees_with_alacritty_terminal_or_is_excluded() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    let table = std::fs::read_to_string(fixtures_dir().join("conformance.kdl")).expect("table");
    let rows = conformance::parse(&table).expect("table parses");
    let (mut agreed, mut skipped, mut fails) = (0usize, 0usize, Vec::new());
    for fx in &fixtures {
        match compare::<TermDriver, _>(fx, &mut AlacrittyOracle).expect("well-formed") {
            Verdict::Agree => agreed += 1,
            Verdict::Skipped(_) => skipped += 1,
            Verdict::Disagree(d) => fails.push((fx.id.clone(), d)),
        }
    }
    assert!(
        agreed >= 700,
        "only {agreed} fixtures compared ({skipped} skipped)"
    );
    assert!(
        fails.is_empty(),
        "{} disagreements without an exclusion (classify each ours-right / theirs-right / \
         unspecified and cite its T-03 row in oracle::EXCLUSIONS):\n\n{}",
        fails.len(),
        disagreement_report("alacritty_terminal", &rows, &fails)
    );
}
