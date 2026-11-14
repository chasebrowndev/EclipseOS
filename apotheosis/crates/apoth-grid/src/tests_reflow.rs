// SPDX-License-Identifier: AGPL-3.0-only
//! M12 tests: reflow, selection, extraction and search against a naive model (a list of
//! logical lines). Fixed-seed xorshift, so failures reproduce from the seed alone.

#![allow(clippy::needless_range_loop)]

use super::*;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n.max(1)
    }
}

fn width_of(ch: char) -> u8 {
    if ch as u32 >= 0x3000 { 2 } else { 1 }
}

/// A tiny terminal: prints with autowrap, scrolls the full screen into history.
struct Mini {
    s: Screen,
    cur: (usize, usize),
}

impl Mini {
    fn new(rows: usize, cols: usize, cap: Option<u16>) -> Mini {
        Mini {
            s: Screen::with_page_cap(rows, cols, ScreenConfig::default(), cap),
            cur: (0, 0),
        }
    }

    fn linefeed(&mut self) {
        let (rows, cols) = (self.s.rows(), self.s.cols());
        if self.cur.0 + 1 == rows {
            self.s
                .scroll_up(Region::full(rows, cols), 1, Color::Default);
        } else {
            self.cur.0 += 1;
        }
        self.cur.1 = 0;
    }

    fn put(&mut self, ch: char) {
        let w = width_of(ch);
        let cols = self.s.cols();
        if self.cur.1 + usize::from(w) > cols {
            if w == 2 && self.cur.1 + 1 == cols {
                self.s.set_spacer_head(self.cur.0);
            }
            self.s.set_row_flag(self.cur.0, flags::WRAPPED, true);
            self.linefeed();
            self.s.set_row_flag(self.cur.0, flags::WRAP_CONT, true);
        }
        let g = [Glyph {
            cp: ch as u32,
            width: w,
        }];
        let n = self
            .s
            .print_run(self.cur.0, self.cur.1, &g, &CellAttrs::default());
        assert_eq!(n, 1);
        self.cur.1 += usize::from(w);
    }

    fn text(&mut self, t: &str) {
        for ch in t.chars() {
            self.put(ch);
        }
    }

    fn line(&mut self, t: &str) {
        self.text(t);
        self.linefeed();
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        let r = self
            .s
            .resize_reflow(rows, cols, self.cur, ReflowOpts::default());
        self.cur = r.cursor;
        self.s.check_invariants().unwrap();
        wide_ok(&self.s);
    }
}

fn all_text(s: &Screen) -> String {
    let total = s.history_rows() + s.rows();
    let r = SelRange {
        start: (0, 0),
        end: (total - 1, s.cols() - 1),
        block: false,
    };
    let mut out = String::new();
    assert!(s.extract(&r, &ExtractOpts::default(), &mut out));
    out.trim_end_matches('\n').to_string()
}

fn wide_ok(s: &Screen) {
    let total = s.history_rows() + s.rows();
    let cols = s.cols();
    let mut row = SnapRow::default();
    for a in 0..total {
        assert!(s.copy_abs_row(a, &mut row));
        for x in 0..cols {
            let w = row.cells[x].wide;
            match w {
                Wide::Wide => {
                    assert!(x + 1 < cols, "wide at last column");
                    assert_eq!(row.cells[x + 1].wide, Wide::SpacerTail);
                }
                Wide::SpacerTail => {
                    assert!(x > 0 && row.cells[x - 1].wide == Wide::Wide, "orphan tail");
                }
                Wide::SpacerHead => {
                    assert_eq!(x, cols - 1);
                    assert!(row.wrapped, "spacer head on unwrapped row");
                }
                Wide::Narrow => {}
            }
        }
    }
}

fn random_line(r: &mut Rng) -> String {
    let n = r.below(40);
    let mut s = String::new();
    for i in 0..n {
        let k = r.below(10);
        if k == 0 && i > 0 && i + 1 < n {
            s.push(' ');
        } else if k == 1 {
            s.push(char::from_u32(0x3042 + r.below(20) as u32).unwrap());
        } else {
            s.push((b'a' + r.below(26) as u8) as char);
        }
    }
    s.trim_end().to_string()
}

fn build(seed: u64, cap: Option<u16>) -> (Mini, String) {
    let mut r = Rng::new(seed);
    let mut m = Mini::new(6, 20 + r.below(20), cap);
    let mut model = Vec::new();
    for _ in 0..(10 + r.below(40)) {
        let l = random_line(&mut r);
        m.line(&l);
        model.push(l);
    }
    (m, model.join("\n"))
}

#[test]
fn reflow_roundtrip_is_lossless() {
    for seed in 1..60u64 {
        for cap in [None, Some(7u16)] {
            let (mut m, model) = build(seed, cap);
            let want = model.trim_end_matches('\n').to_string();
            assert_eq!(all_text(&m.s), want, "seed {seed} initial");
            let mut r = Rng::new(seed ^ 0xABCD);
            let orig_cols = m.s.cols();
            for _ in 0..6 {
                let c = 2 + r.below(60);
                let rows = 3 + r.below(10);
                m.resize(rows, c);
                assert_eq!(all_text(&m.s), want, "seed {seed} cols {c}");
            }
            m.resize(6, orig_cols);
            assert_eq!(all_text(&m.s), want, "seed {seed} back");
        }
    }
}

#[test]
fn rejoin_and_rewrap_rows() {
    let mut m = Mini::new(4, 10, Some(8));
    m.line("0123456789abcdef");
    m.resize(4, 20);
    // The logical line is one row again.
    let mut row = SnapRow::default();
    let total = m.s.history_rows() + m.s.rows();
    let mut found = false;
    for a in 0..total {
        m.s.copy_abs_row(a, &mut row);
        if row.cells[0].cp == u32::from('0') {
            assert_eq!(row.cells[15].cp, u32::from('f'));
            assert!(!row.wrapped);
            found = true;
        }
    }
    assert!(found);
    m.resize(4, 5);
    m.s.copy_abs_row(m.s.history_rows(), &mut row);
    assert_eq!(all_text(&m.s), "0123456789abcdef");
}

#[test]
fn pins_follow_content() {
    for seed in 1..40u64 {
        let mut m = Mini::new(5, 17, Some(6));
        let mut cps = Vec::new();
        // Unique code points so a pin identifies its cell.
        let mut next = 0x4E00u32;
        for _ in 0..6 {
            let mut l = String::new();
            for _ in 0..30 {
                let ch = char::from_u32(next).unwrap();
                next += 1;
                cps.push(ch);
                l.push(ch);
            }
            m.line(&l);
        }
        let mut r = Rng::new(seed);
        let mut pins = Vec::new();
        for _ in 0..8 {
            let i = r.below(cps.len());
            // Find the cell holding cps[i] right now.
            let total = m.s.history_rows() + m.s.rows();
            let mut row = SnapRow::default();
            'o: for a in 0..total {
                m.s.copy_abs_row(a, &mut row);
                for x in 0..m.s.cols() {
                    if row.cells[x].cp == cps[i] as u32 {
                        let id = m.s.track_abs(a, x, PinKind::Bookmark).unwrap();
                        pins.push((id, cps[i]));
                        break 'o;
                    }
                }
            }
        }
        for _ in 0..8 {
            m.resize(3 + r.below(8), 3 + r.below(50));
            let mut row = SnapRow::default();
            for &(id, ch) in &pins {
                let (a, x) = m.s.pin_pos(id).expect("pin dangles");
                m.s.copy_abs_row(a, &mut row);
                assert_eq!(row.cells[x].cp, ch as u32, "pin moved off its cell");
            }
        }
    }
}

#[test]
fn cursor_follows_text() {
    let mut m = Mini::new(5, 12, Some(8));
    m.line("first");
    m.text("hello world, this is long");
    for c in [7, 30, 12, 4, 26] {
        m.resize(5, c);
        let (y, x) = m.cur;
        // The cell before the cursor holds the last typed char.
        let (py, px) = if x > 0 { (y, x - 1) } else { (y - 1, c - 1) };
        let mut row = SnapRow::default();
        m.s.copy_row(py, &mut row);
        let mut cp = row.cells[px].cp;
        if row.cells[px].wide == Wide::SpacerHead {
            cp = 0;
        }
        assert!(
            cp == u32::from('g') || x == 0 || cp == 0,
            "cursor at {y},{x} cols {c} cp {cp}"
        );
    }
}

#[test]
fn wide_cells_never_split() {
    let mut m = Mini::new(4, 9, Some(5));
    m.line("あいうえおかきくけこさしすせそ");
    m.line("a あ b い");
    for c in [9, 4, 3, 2, 5, 7, 8, 20, 1] {
        m.resize(4, c);
        if c >= 2 {
            let t = all_text(&m.s);
            assert!(t.starts_with("あいうえお"), "cols {c}: {t}");
        }
    }
}

#[test]
fn alt_screen_never_reflows() {
    let cfg = ScreenConfig {
        history: false,
        ..ScreenConfig::default()
    };
    let mut s = Screen::new(3, 10, cfg);
    let g: Vec<Glyph> = "abcdefghij"
        .chars()
        .map(|c| Glyph {
            cp: c as u32,
            width: 1,
        })
        .collect();
    s.print_run(0, 0, &g, &CellAttrs::default());
    s.set_row_flag(0, flags::WRAPPED, true);
    let r = s.resize_reflow(3, 5, (0, 0), ReflowOpts::default());
    assert!(!r.reflowed);
    assert_eq!(s.history_rows(), 0);
    assert_eq!(all_text(&s), "abcde");
}

#[test]
fn reflow_disabled_truncates() {
    let mut m = Mini::new(4, 10, Some(8));
    m.line("0123456789abcdef");
    let opts = ReflowOpts {
        reflow: false,
        protect_from: None,
    };
    let r = m.s.resize_reflow(4, 4, m.cur, opts);
    assert!(!r.reflowed);
    m.s.check_invariants().unwrap();
    assert!(all_text(&m.s).starts_with("0123"));
    assert!(!all_text(&m.s).contains('9') || all_text(&m.s).contains("4567"));
}

#[test]
fn protected_prompt_rows_are_not_rewrapped() {
    let mut m = Mini::new(5, 20, Some(8));
    m.line("output line one two three");
    m.text("$ prompt text here ok");
    let prompt_y = m.cur.0 - 1;
    let pin = m.s.track_active(prompt_y, 0, PinKind::Mark).unwrap();
    let opts = ReflowOpts {
        reflow: true,
        protect_from: Some(pin),
    };
    let r = m.s.resize_reflow(5, 10, m.cur, opts);
    assert!(r.reflowed);
    m.s.check_invariants().unwrap();
    let (a, _) = m.s.pin_pos(pin).unwrap();
    let total = m.s.history_rows() + m.s.rows();
    // The output line wrapped into 3 rows; the protected rows stayed 1:1 (truncated).
    let mut row = SnapRow::default();
    m.s.copy_abs_row(a, &mut row);
    assert_eq!(row.cells[0].cp, u32::from('$'));
    assert!(!row.wrapped || a + 1 < total);
    let mut t = String::new();
    let rr = SelRange {
        start: (a, 0),
        end: (a, 9),
        block: false,
    };
    m.s.extract(&rr, &ExtractOpts::default(), &mut t);
    assert_eq!(t, "$ prompt t");
}

#[test]
fn reflow_stamps_fresh_sequence_numbers() {
    let mut m = Mini::new(4, 10, Some(8));
    m.line("0123456789abcdef");
    let mut row = SnapRow::default();
    m.s.copy_row(0, &mut row);
    let before = row.seq;
    m.resize(4, 7);
    let mut seen = SeenSet::new(4);
    let mut snap = RowSnapshot::default();
    let _ = m.s.snapshot_since(&mut snap, &mut seen);
    for y in 0..4 {
        m.s.copy_row(y, &mut row);
        assert!(row.seq > before);
    }
    let mut fs = FrameSnapshot::new();
    assert_eq!(m.s.snapshot_dirty(&mut fs), Damage::Full);
}

#[test]
fn styles_and_links_survive_reflow() {
    let mut s = Screen::with_page_cap(3, 8, ScreenConfig::default(), Some(6));
    let st = Style {
        fg: Color::Rgb(1, 2, 3),
        ..Style::DEFAULT
    };
    let attrs = CellAttrs {
        style: st,
        link: Some(("id", "https://x.example/")),
        ..CellAttrs::default()
    };
    let g: Vec<Glyph> = "abcdefgh"
        .chars()
        .map(|c| Glyph {
            cp: c as u32,
            width: 1,
        })
        .collect();
    s.print_run(0, 0, &g, &attrs);
    s.set_row_flag(0, flags::WRAPPED, true);
    s.set_row_block(0, 7);
    s.print_run(1, 0, &g[..2], &attrs);
    s.set_row_flag(1, flags::WRAP_CONT, true);
    let r = s.resize_reflow(3, 20, (1, 2), ReflowOpts::default());
    s.check_invariants().unwrap();
    assert_eq!(r.cursor.1, 10);
    let y = r.cursor.0;
    let mut row = SnapRow::default();
    s.copy_row(y, &mut row);
    assert_eq!(row.cells[9].cp, u32::from('b'));
    assert_eq!(row.cells[9].style, st);
    assert!(row.cells[9].link);
    assert_eq!(row.block, 7);
    assert_eq!(s.link_at(y, 9).unwrap().1, "https://x.example/");
}

// ---- selection ----------------------------------------------------------------------

fn find(s: &Screen, ch: char) -> (usize, usize) {
    let total = s.history_rows() + s.rows();
    let mut row = SnapRow::default();
    for a in 0..total {
        s.copy_abs_row(a, &mut row);
        for x in 0..s.cols() {
            if row.cells[x].cp == ch as u32 {
                return (a, x);
            }
        }
    }
    panic!("char not found");
}

#[test]
fn selection_text_survives_reflow() {
    for seed in 1..40u64 {
        let mut m = Mini::new(6, 23, Some(6));
        let mut next = 0x4E00u32;
        let mut chars = Vec::new();
        for l in 0..5 {
            let mut t = String::new();
            for i in 0..(10 + 7 * l) {
                if i % 6 == 5 {
                    t.push(' ');
                } else {
                    let ch = char::from_u32(next).unwrap();
                    next += 1;
                    chars.push(ch);
                    t.push(ch);
                }
            }
            m.line(t.trim_end());
        }
        let mut r = Rng::new(seed);
        let (i, j) = (r.below(chars.len()), r.below(chars.len()));
        let (pa, pb) = (find(&m.s, chars[i]), find(&m.s, chars[j]));
        let kinds = [SelKind::Simple, SelKind::Word, SelKind::Line];
        let kind = kinds[r.below(3)];
        let sides = [Side::Left, Side::Right];
        let mut sel =
            m.s.selection_begin(kind, pa.0, pa.1, sides[r.below(2)])
                .unwrap();
        m.s.selection_extend(&mut sel, pb.0, pb.1, sides[r.below(2)]);
        let wc = WordChars::default();
        let mut before = String::new();
        let had =
            m.s.selection_text(&sel, &wc, &ExtractOpts::default(), &mut before);
        for _ in 0..5 {
            m.resize(4 + r.below(6), 4 + r.below(50));
            let mut after = String::new();
            let has =
                m.s.selection_text(&sel, &wc, &ExtractOpts::default(), &mut after);
            assert_eq!(had, has, "seed {seed} kind {kind:?}");
            if had {
                assert_eq!(before, after, "seed {seed} kind {kind:?}");
            }
        }
    }
}

#[test]
fn word_line_block_selection() {
    let mut m = Mini::new(5, 10, Some(8));
    m.line("foo bar_baz");
    m.line("x.y");
    let wc = WordChars::default();
    let o = ExtractOpts::default();
    // word: "bar_baz" spans the wrap; clicking inside selects all of it
    let (a, x) = find(&m.s, 'z');
    let sel =
        m.s.selection_begin(SelKind::Word, a, x, Side::Left)
            .unwrap();
    let mut t = String::new();
    m.s.selection_text(&sel, &wc, &o, &mut t);
    assert_eq!(t, "bar_baz");
    m.s.selection_clear(sel);
    // line: both rows of the wrapped line, no newline between them
    let (a, x) = find(&m.s, 'f');
    let sel =
        m.s.selection_begin(SelKind::Line, a, x, Side::Left)
            .unwrap();
    t.clear();
    m.s.selection_text(&sel, &wc, &o, &mut t);
    assert_eq!(t, "foo bar_baz");
    m.s.selection_clear(sel);
    // custom word chars: '.' joins "x.y"
    let (a, x) = find(&m.s, 'x');
    let sel =
        m.s.selection_begin(SelKind::Word, a, x, Side::Left)
            .unwrap();
    t.clear();
    m.s.selection_text(&sel, &wc, &o, &mut t);
    assert_eq!(t, "x");
    t.clear();
    m.s.selection_text(&sel, &WordChars::new("."), &o, &mut t);
    assert_eq!(t, "x.y");
    m.s.selection_clear(sel);
    // block: columns 0..=1 of the two rows after the wrap
    let (a, _) = find(&m.s, 'x');
    let mut sel =
        m.s.selection_begin(SelKind::Block, a - 1, 0, Side::Left)
            .unwrap();
    m.s.selection_extend(&mut sel, a, 1, Side::Right);
    t.clear();
    m.s.selection_text(&sel, &wc, &o, &mut t);
    assert_eq!(t, "z\nx.");
}

#[test]
fn simple_selection_sides_and_trim() {
    let mut m = Mini::new(3, 12, Some(8));
    m.line("ab  ");
    m.line("cd");
    let wc = WordChars::default();
    let (a, _) = find(&m.s, 'a');
    let mut sel =
        m.s.selection_begin(SelKind::Simple, a, 0, Side::Left)
            .unwrap();
    m.s.selection_extend(&mut sel, a + 1, 1, Side::Right);
    let mut t = String::new();
    m.s.selection_text(&sel, &wc, &ExtractOpts::default(), &mut t);
    assert_eq!(t, "ab\ncd");
    t.clear();
    m.s.selection_text(
        &sel,
        &wc,
        &ExtractOpts {
            trim_trailing: false,
        },
        &mut t,
    );
    assert_eq!(t, "ab          \ncd");
    // Right side of the anchor and Left side of the end exclude both end cells.
    let mut sel =
        m.s.selection_begin(SelKind::Simple, a, 0, Side::Right)
            .unwrap();
    m.s.selection_extend(&mut sel, a + 1, 1, Side::Left);
    t.clear();
    m.s.selection_text(&sel, &wc, &ExtractOpts::default(), &mut t);
    assert_eq!(t, "b\nc");
    // A click (same cell, both Left) selects nothing.
    let sel =
        m.s.selection_begin(SelKind::Simple, a, 0, Side::Left)
            .unwrap();
    assert!(m.s.selection_range(&sel, &wc).is_none());
}

#[test]
fn semantic_zone_selection() {
    let mut s = Screen::with_page_cap(4, 10, ScreenConfig::default(), Some(8));
    let prompt = CellAttrs {
        semantic: Semantic::Prompt,
        ..CellAttrs::default()
    };
    let g = |t: &str| -> Vec<Glyph> {
        t.chars()
            .map(|c| Glyph {
                cp: c as u32,
                width: 1,
            })
            .collect()
    };
    s.print_run(0, 0, &g("$ "), &prompt);
    s.print_run(
        0,
        2,
        &g("ls"),
        &CellAttrs {
            semantic: Semantic::Input,
            ..CellAttrs::default()
        },
    );
    s.print_run(1, 0, &g("out1"), &CellAttrs::default());
    let base = s.history_rows();
    let sel = s
        .selection_begin(SelKind::Semantic, base, 2, Side::Left)
        .unwrap();
    let mut t = String::new();
    s.selection_text(&sel, &WordChars::default(), &ExtractOpts::default(), &mut t);
    assert_eq!(t, "ls");
}

#[test]
fn selection_survives_scroll_and_reports_eviction() {
    let mut m = Mini::new(3, 8, Some(4));
    m.line("keep");
    let (a, _) = find(&m.s, 'k');
    let sel =
        m.s.selection_begin(SelKind::Simple, a, 0, Side::Left)
            .unwrap();
    for i in 0..5 {
        m.line(&format!("l{i}"));
    }
    assert!(m.s.pin_pos(sel.start).is_some());
    m.s.set_limits(Limits {
        bytes: 1,
        lines: Some(0),
    });
    for i in 0..30 {
        m.line(&format!("m{i}"));
    }
    let mut g = Vec::new();
    m.s.take_garbage(&mut g);
    assert!(g.iter().any(|(_, k)| *k == PinKind::SelectionStart));
    assert!(m.s.selection_range(&sel, &WordChars::default()).is_none());
}

// ---- search -------------------------------------------------------------------------

fn pin_cell(s: &Screen, p: &Pin) -> (usize, usize) {
    s.pin_abs(p).unwrap()
}

#[test]
fn literal_search_across_wraps_and_seams() {
    let mut m = Mini::new(4, 10, Some(3));
    m.line("alpha beta gamma delta");
    m.line("beta again BETA");
    m.line("end");
    let mut sc = LineScratch::default();
    let mut found = Vec::new();
    let mut from = None;
    while let Some((a, b)) = m.s.find_literal("beta", Dir::Forward, from, false, &mut sc) {
        found.push((pin_cell(&m.s, &a), pin_cell(&m.s, &b)));
        from = Some(a);
    }
    assert_eq!(found.len(), 2);
    // First match "beta" at logical offset 6 wraps no rows at width 10: row 0 cols 6..=9.
    let (a0, a1) = found[0];
    assert_eq!(a0.1, 6);
    assert_eq!(a1.1, 9);
    // A match that wraps: "gamma delta" spans a row boundary.
    let (a, b) =
        m.s.find_literal("ma delt", Dir::Forward, None, false, &mut sc)
            .unwrap();
    assert!(pin_cell(&m.s, &b).0 > pin_cell(&m.s, &a).0);
    // Backward enumerates in reverse.
    let mut back = Vec::new();
    let mut from = None;
    while let Some((a, b)) = m.s.find_literal("beta", Dir::Backward, from, true, &mut sc) {
        back.push((pin_cell(&m.s, &a), pin_cell(&m.s, &b)));
        from = Some(a);
    }
    assert_eq!(back.len(), 3);
    assert_eq!(back[2].0, found[0].0);
    assert!(
        m.s.find_literal("zzz", Dir::Forward, None, false, &mut sc)
            .is_none()
    );
    assert!(
        m.s.find_literal("", Dir::Forward, None, false, &mut sc)
            .is_none()
    );
}

#[test]
fn search_matches_model_under_reflow() {
    for seed in 1..30u64 {
        let (mut m, model) = build(seed, Some(5));
        let mut r = Rng::new(seed);
        let want: usize = model
            .lines()
            .map(|l| {
                let mut n = 0;
                let mut i = 0;
                while let Some(k) = l[i..].find("ab") {
                    n += 1;
                    i += k + 1;
                }
                n
            })
            .sum();
        for _ in 0..4 {
            m.resize(4 + r.below(6), 6 + r.below(40));
            let mut sc = LineScratch::default();
            let mut n = 0;
            let mut from = None;
            while let Some((a, _)) = m.s.find_literal("ab", Dir::Forward, from, false, &mut sc) {
                n += 1;
                from = Some(a);
            }
            // A non-ASCII line never contains "ab" differently; counts must match.
            assert_eq!(n, want, "seed {seed}");
        }
    }
}

#[test]
fn custom_matcher_gets_whole_logical_lines() {
    let mut m = Mini::new(4, 6, Some(4));
    m.line("0123456789");
    let mut sc = LineScratch::default();
    let mut seen = String::new();
    let hit = m.s.search(Dir::Forward, None, false, &mut sc, |t, out| {
        seen = t.to_string();
        if let Some(i) = t.find("5678") {
            out.push((i, i + 4));
        }
    });
    assert!(hit.is_some());
    assert_eq!(seen, "0123456789");
}

/// T-23 RESIZE measurement for the eager reflow: 100 000 history lines, 80 -> 100 columns.
/// Reflow is eager (lazy history conversion is owed to a later milestone), so this prints the
/// real cost for the budget decision instead of asserting one. Run with
/// `cargo test -p apoth-grid --release resize_reflow_100k -- --ignored --nocapture`.
#[test]
#[ignore = "measurement; run on request"]
fn resize_reflow_100k_lines_measured() {
    let mut m = Mini::new(40, 80, None);
    m.s = Screen::with_page_cap(
        40,
        80,
        ScreenConfig {
            history: true,
            limits: Limits {
                bytes: 1 << 30,
                lines: Some(100_000),
            },
        },
        None,
    );
    for i in 0..100_000 {
        m.line(&format!(
            "line {i} the quick brown fox jumps over the lazy dog"
        ));
    }
    let t = std::time::Instant::now();
    m.resize(40, 100);
    eprintln!(
        "RESIZE eager reflow, {} history rows: {:?}",
        m.s.history_rows(),
        t.elapsed()
    );
}
