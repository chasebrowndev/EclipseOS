// SPDX-License-Identifier: AGPL-3.0-only
//! Differential run against `vte` (T-25 section 2). `vte` is only a parser, so this adapter
//! carries a minimal screen (ASCII print, BS HT LF CR, CUU/CUD/CUF/CUB/CUP/CHA/VPA, ED, EL)
//! and flags anything else as unsupported; unsupported streams are skipped, everything the
//! adapter does model must agree with ours or be listed in `EXCLUSIONS`.

use apoth_testkit::adapter::TermDriver;
use apoth_testkit::conformance;
use apoth_testkit::oracle::{Oracle, OracleView, Verdict, compare, disagreement_report};
use apoth_testkit::{fixtures_dir, load_fixtures};
use vte::{Params, Parser, Perform};

struct Screen {
    rows: usize,
    cols: usize,
    cells: Vec<Vec<char>>,
    r: usize,
    c: usize,
    wrap: bool,
    bad: Option<String>,
}

impl Screen {
    fn lf(&mut self) {
        self.wrap = false;
        if self.r + 1 == self.rows {
            self.cells.remove(0);
            self.cells.push(vec![' '; self.cols]);
        } else {
            self.r += 1;
        }
    }
    fn unsupported(&mut self, what: String) {
        self.bad.get_or_insert(what);
    }
}

fn p(params: &Params, i: usize, default: usize) -> usize {
    match params.iter().nth(i).and_then(|s| s.first().copied()) {
        Some(0) | None => default,
        Some(v) => usize::from(v),
    }
}

impl Perform for Screen {
    fn print(&mut self, ch: char) {
        if ch == '\u{7f}' {
            return; // DEL is ignored (ours-right: the first run printed it)
        }
        if !ch.is_ascii() {
            self.unsupported("non-ASCII text".into());
            return;
        }
        if self.wrap {
            self.c = 0;
            self.lf();
            self.wrap = false;
        }
        self.cells[self.r][self.c] = ch;
        if self.c + 1 == self.cols {
            self.wrap = true;
        } else {
            self.c += 1;
        }
    }
    fn execute(&mut self, b: u8) {
        match b {
            0x08 => {
                self.wrap = false;
                self.c = self.c.saturating_sub(1);
            }
            0x09 => {
                self.wrap = false;
                self.c = ((self.c / 8 + 1) * 8).min(self.cols - 1);
            }
            0x0a..=0x0c => self.lf(),
            0x0d => {
                self.wrap = false;
                self.c = 0;
            }
            0x07 | 0x00 => {}
            _ => self.unsupported(format!("C0 {b:#04x}")),
        }
    }
    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], ignore: bool, action: char) {
        if ignore || !inter.is_empty() || params.iter().any(|s| s.len() > 1) {
            self.unsupported(format!("CSI {action} with intermediates/subparams"));
            return;
        }
        let n = p(params, 0, 1);
        self.wrap = false;
        match action {
            'A' => self.r = self.r.saturating_sub(n),
            'B' => self.r = (self.r + n).min(self.rows - 1),
            'C' => self.c = (self.c + n).min(self.cols - 1),
            'D' => self.c = self.c.saturating_sub(n),
            'G' => self.c = (n - 1).min(self.cols - 1),
            'd' => self.r = (n - 1).min(self.rows - 1),
            'H' | 'f' => {
                self.r = (p(params, 0, 1) - 1).min(self.rows - 1);
                self.c = (p(params, 1, 1) - 1).min(self.cols - 1);
            }
            'J' | 'K' => {
                let mode = params.iter().next().map_or(0, |s| s[0]);
                let (r, c) = (self.r, self.c);
                let (rows, cols) = (self.rows, self.cols);
                let blank = |s: &mut Self, r: usize, a: usize, b: usize| {
                    for x in a..b {
                        s.cells[r][x] = ' ';
                    }
                };
                match (action, mode) {
                    ('K', 0) => blank(self, r, c, cols),
                    ('K', 1) => blank(self, r, 0, c + 1),
                    ('K', 2) => blank(self, r, 0, cols),
                    ('J', 0) => {
                        blank(self, r, c, cols);
                        (r + 1..rows).for_each(|y| blank(self, y, 0, cols));
                    }
                    ('J', 1) => {
                        blank(self, r, 0, c + 1);
                        (0..r).for_each(|y| blank(self, y, 0, cols));
                    }
                    ('J', 2) => (0..rows).for_each(|y| blank(self, y, 0, cols)),
                    _ => self.unsupported(format!("CSI {mode} {action}")),
                }
            }
            'm' => {}
            other => self.unsupported(format!("CSI {other}")),
        }
    }
    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, b: u8) {
        self.unsupported(format!("ESC {inter:?} {b:#04x}"));
    }
    fn osc_dispatch(&mut self, _p: &[&[u8]], _bell: bool) {
        self.unsupported("OSC".into());
    }
    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {
        self.unsupported("DCS".into());
    }
}

struct VteOracle;

impl Oracle for VteOracle {
    fn name(&self) -> &'static str {
        "vte"
    }
    fn run(&mut self, rows: usize, cols: usize, bytes: &[u8]) -> OracleView {
        let mut s = Screen {
            rows,
            cols,
            cells: vec![vec![' '; cols]; rows],
            r: 0,
            c: 0,
            wrap: false,
            bad: None,
        };
        Parser::new().advance(&mut s, bytes);
        OracleView {
            lines: s
                .cells
                .iter()
                .map(|r| r.iter().collect::<String>().trim_end().to_string())
                .collect(),
            cursor: (s.r, s.c),
            unsupported: s.bad,
        }
    }
}

#[test]
fn corpus_agrees_with_vte_where_vte_models_it() {
    let fixtures = load_fixtures(&fixtures_dir()).expect("fixtures load");
    let table = std::fs::read_to_string(fixtures_dir().join("conformance.kdl")).expect("table");
    let rows = conformance::parse(&table).expect("table parses");
    let (mut compared, mut skipped, mut fails) = (0usize, 0usize, Vec::new());
    for fx in &fixtures {
        match compare::<TermDriver, _>(fx, &mut VteOracle).expect("well-formed") {
            Verdict::Agree => compared += 1,
            Verdict::Skipped(_) => skipped += 1,
            Verdict::Disagree(d) => fails.push((fx.id.clone(), d)),
        }
    }
    assert!(
        compared >= 150,
        "only {compared} fixtures compared ({skipped} skipped)"
    );
    assert!(
        fails.is_empty(),
        "{} disagreements without an exclusion:\n{}",
        fails.len(),
        disagreement_report("vte", &rows, &fails)
    );
}
