// SPDX-License-Identifier: AGPL-3.0-only
//! Crate-level tests: random operation sequences against a naive model, pin stability,
//! scroll invariants, scrollback limits, interning refcounts and consumer independence.
//! Randomness is a fixed-seed xorshift so failures reproduce from the seed alone.

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

fn style(i: usize) -> Style {
    match i % 6 {
        0 => Style::DEFAULT,
        1 => Style {
            attrs: Attrs(Attrs::BOLD),
            ..Style::DEFAULT
        },
        2 => Style {
            fg: Color::Palette(2),
            ..Style::DEFAULT
        },
        3 => Style {
            fg: Color::Rgb(1, 2, 3),
            bg: Color::Palette(4),
            ..Style::DEFAULT
        },
        4 => Style {
            bg: Color::Palette(5),
            ..Style::DEFAULT
        },
        _ => Style {
            bg: Color::Rgb(7, 8, 9),
            underline: Underline::Double,
            ..Style::DEFAULT
        },
    }
}

fn bg(i: usize) -> Color {
    [Color::Default, Color::Palette(3), Color::Rgb(9, 9, 9)][i % 3]
}

const LINKS: [(&str, &str); 3] = [
    ("", "https://a.example/"),
    ("x", "https://b.example/"),
    ("y", "https://c.example/"),
];

type Norm = (u32, Vec<u32>, Style, bool, Wide);

fn norm_row(r: &SnapRow) -> Vec<Norm> {
    r.cells
        .iter()
        .map(|c| (c.cp, r.cluster(c).to_vec(), c.style, c.link, c.wide))
        .collect()
}

fn grid_rows(s: &Screen) -> Vec<Vec<Norm>> {
    let mut r = SnapRow::default();
    (0..s.rows())
        .map(|y| {
            s.copy_row(y, &mut r);
            norm_row(&r)
        })
        .collect()
}

// ---- naive model ---------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
struct M {
    text: Vec<u32>,
    style: Style,
    link: bool,
}

fn blank(bg: Color) -> M {
    M {
        text: vec![],
        style: Style {
            bg,
            ..Style::DEFAULT
        },
        link: false,
    }
}

struct Model {
    rows: usize,
    cols: usize,
    history: bool,
    grid: Vec<Vec<M>>,
    hist: Vec<Vec<M>>,
}

impl Model {
    fn new(rows: usize, cols: usize, history: bool) -> Model {
        Model {
            rows,
            cols,
            history,
            grid: vec![vec![blank(Color::Default); cols]; rows],
            hist: vec![],
        }
    }

    fn print(&mut self, y: usize, x: usize, cps: &[u32], st: Style, link: bool) {
        for (i, &cp) in cps.iter().enumerate() {
            if x + i < self.cols {
                self.grid[y][x + i] = M {
                    text: vec![cp],
                    style: st,
                    link,
                };
            }
        }
    }

    fn erase(&mut self, y: usize, x0: usize, x1: usize, bg: Color) {
        for x in x0..x1.min(self.cols) {
            self.grid[y][x] = blank(bg);
        }
    }

    fn insert(&mut self, y: usize, x: usize, n: usize, right: usize, bg: Color) {
        let n = n.min(right + 1 - x);
        for i in (x..right + 1 - n).rev() {
            self.grid[y][i + n] = self.grid[y][i].clone();
        }
        for i in x..x + n {
            self.grid[y][i] = blank(bg);
        }
    }

    fn delete(&mut self, y: usize, x: usize, n: usize, right: usize, bg: Color) {
        let n = n.min(right + 1 - x);
        for i in x..right + 1 - n {
            self.grid[y][i] = self.grid[y][i + n].clone();
        }
        for i in right + 1 - n..=right {
            self.grid[y][i] = blank(bg);
        }
    }

    fn scroll(&mut self, r: Region, n: usize, up: bool, allow_history: bool, bg: Color) {
        let h = r.bottom - r.top + 1;
        let n = n.min(h);
        let full_w = r.left == 0 && r.right == self.cols - 1;
        if up && allow_history && self.history && full_w && r.top == 0 && r.bottom == self.rows - 1
        {
            for _ in 0..n {
                let top = self.grid.remove(0);
                self.hist.push(top);
                self.grid.push(vec![blank(bg); self.cols]);
            }
            return;
        }
        if up {
            for y in r.top..r.bottom + 1 - n {
                for x in r.left..=r.right {
                    self.grid[y][x] = self.grid[y + n][x].clone();
                }
            }
            for y in r.bottom + 1 - n..=r.bottom {
                for x in r.left..=r.right {
                    self.grid[y][x] = blank(bg);
                }
            }
        } else {
            for y in (r.top + n..=r.bottom).rev() {
                for x in r.left..=r.right {
                    self.grid[y][x] = self.grid[y - n][x].clone();
                }
            }
            for y in r.top..r.top + n {
                for x in r.left..=r.right {
                    self.grid[y][x] = blank(bg);
                }
            }
        }
    }
}

fn row_matches(r: &SnapRow, m: &[M]) -> Result<(), String> {
    for (x, (c, e)) in r.cells.iter().zip(m).enumerate() {
        let content = match e.text.len() {
            0 => c.cp == 0 && c.cluster_len == 0,
            1 => c.cp == e.text[0] && c.cluster_len == 0,
            _ => r.cluster(c) == &e.text[..],
        };
        if !(content && c.style == e.style && c.link == e.link && c.wide == Wide::Narrow) {
            return Err(format!(
                "col {x}: grid {c:?} / {:?} vs model {e:?}",
                r.cluster(c)
            ));
        }
    }
    if r.cells.len() != m.len() {
        return Err("row width".into());
    }
    Ok(())
}

fn check_model(s: &Screen, m: &Model) -> Result<(), String> {
    s.check_invariants()?;
    let mut r = SnapRow::default();
    for y in 0..m.rows {
        s.copy_row(y, &mut r);
        row_matches(&r, &m.grid[y]).map_err(|e| format!("row {y}: {e}"))?;
    }
    let hr = s.history_rows();
    let k = hr.min(m.hist.len());
    for i in 0..k {
        if !s.copy_abs_row(hr - 1 - i, &mut r) {
            return Err("history row missing".into());
        }
        row_matches(&r, &m.hist[m.hist.len() - 1 - i]).map_err(|e| format!("hist -{i}: {e}"))?;
    }
    Ok(())
}

fn rand_region(rng: &mut Rng, rows: usize, cols: usize) -> Region {
    let top = rng.below(rows);
    let bottom = top + rng.below(rows - top);
    let (left, right) = if rng.below(3) == 0 {
        let l = rng.below(cols);
        (l, l + rng.below(cols - l))
    } else {
        (0, cols - 1)
    };
    Region {
        top,
        bottom,
        left,
        right,
    }
}

/// One random operation applied to the screen and, when present, to the model.
fn step(rng: &mut Rng, s: &mut Screen, mut m: Option<&mut Model>) {
    let rows = s.rows();
    let cols = s.cols();
    match rng.below(12) {
        0..=3 => {
            let (y, x) = (rng.below(rows), rng.below(cols));
            let len = 1 + rng.below(12);
            let cps: Vec<u32> = (0..len).map(|_| 65 + rng.below(26) as u32).collect();
            let glyphs: Vec<Glyph> = cps.iter().map(|&cp| Glyph { cp, width: 1 }).collect();
            let st = style(rng.below(6));
            let lk = rng.below(4);
            let attrs = CellAttrs {
                style: st,
                link: (lk > 0).then(|| LINKS[lk - 1]),
                ..CellAttrs::default()
            };
            s.print_run(y, x, &glyphs, &attrs);
            if let Some(m) = m.as_deref_mut() {
                m.print(y, x, &cps, st, lk > 0);
            }
        }
        4 => {
            let (y, x) = (rng.below(rows), rng.below(cols));
            let n = 2 + rng.below(5);
            let mut cps = vec![65 + rng.below(26) as u32];
            cps.extend((1..n).map(|i| 0x300 + i as u32));
            let st = style(rng.below(6));
            let attrs = CellAttrs {
                style: st,
                ..CellAttrs::default()
            };
            assert!(s.print_cluster(y, x, &cps, 1, &attrs));
            if let Some(m) = m.as_deref_mut() {
                m.grid[y][x] = M {
                    text: cps,
                    style: st,
                    link: false,
                };
            }
        }
        5 => {
            let y = rng.below(rows);
            let x0 = rng.below(cols);
            let x1 = x0 + 1 + rng.below(cols);
            let b = bg(rng.below(3));
            s.erase_cells(y, x0, x1, b, false);
            if let Some(m) = m.as_deref_mut() {
                m.erase(y, x0, x1, b);
            }
        }
        6 => {
            let y0 = rng.below(rows);
            let y1 = (y0 + rng.below(3)).min(rows - 1);
            let b = bg(rng.below(3));
            s.erase_rows(y0, y1, b, false);
            if let Some(m) = m.as_deref_mut() {
                for y in y0..=y1 {
                    m.erase(y, 0, cols, b);
                }
            }
        }
        7 => {
            let (y, x) = (rng.below(rows), rng.below(cols));
            let right = x + rng.below(cols - x);
            let (n, b) = (1 + rng.below(5), bg(rng.below(3)));
            s.insert_chars(y, x, n, right, b);
            if let Some(m) = m.as_deref_mut() {
                m.insert(y, x, n, right, b);
            }
        }
        8 => {
            let (y, x) = (rng.below(rows), rng.below(cols));
            let right = x + rng.below(cols - x);
            let (n, b) = (1 + rng.below(5), bg(rng.below(3)));
            s.delete_chars(y, x, n, right, b);
            if let Some(m) = m.as_deref_mut() {
                m.delete(y, x, n, right, b);
            }
        }
        9 => {
            let r = rand_region(rng, rows, cols);
            let (n, b, up) = (1 + rng.below(4), bg(rng.below(3)), rng.below(2) == 0);
            if up {
                s.scroll_up(r, n, b);
            } else {
                s.scroll_down(r, n, b);
            }
            if let Some(m) = m {
                m.scroll(r, n, up, true, b);
            }
        }
        10 => {
            let r = Region::full(rows, cols);
            let (n, b) = (1 + rng.below(3), bg(rng.below(3)));
            s.scroll_up(r, n, b);
            if let Some(m) = m {
                m.scroll(r, n, true, true, b);
            }
        }
        _ => {
            let r = rand_region(rng, rows, cols);
            let (n, b, up) = (1 + rng.below(3), bg(rng.below(3)), rng.below(2) == 0);
            if up {
                s.delete_lines(r, n, b);
            } else {
                s.insert_lines(r, n, b);
            }
            if let Some(m) = m {
                m.scroll(r, n, up, false, b);
            }
        }
    }
}

fn small(history: bool, lines: Option<usize>) -> Screen {
    // Tiny pages so the active area always spans page seams.
    let cfg = ScreenConfig {
        history,
        limits: Limits {
            bytes: usize::MAX,
            lines,
        },
    };
    Screen::with_page_cap(8, 16, cfg, Some(5))
}

#[test]
fn random_ops_match_naive_model_with_history() {
    for seed in 0..24 {
        let mut rng = Rng::new(seed);
        let mut s = small(true, Some(40));
        let mut m = Model::new(8, 16, true);
        for i in 0..500 {
            step(&mut rng, &mut s, Some(&mut m));
            check_model(&s, &m).unwrap_or_else(|e| panic!("seed {seed} op {i}: {e}"));
            assert!(s.history_rows() <= 40, "lines limit exceeded");
        }
    }
}

#[test]
fn random_ops_match_naive_model_alt_screen() {
    for seed in 100..116 {
        let mut rng = Rng::new(seed);
        let mut s = small(false, None);
        let mut m = Model::new(8, 16, false);
        for i in 0..500 {
            step(&mut rng, &mut s, Some(&mut m));
            check_model(&s, &m).unwrap_or_else(|e| panic!("seed {seed} op {i}: {e}"));
            assert_eq!(s.history_rows(), 0);
        }
    }
}

#[test]
fn default_pages_match_model_too() {
    // Standard 512 KiB pages: no seam in the active area.
    let mut rng = Rng::new(777);
    let mut s = Screen::new(8, 16, ScreenConfig::default());
    let mut m = Model::new(8, 16, true);
    for i in 0..400 {
        step(&mut rng, &mut s, Some(&mut m));
        check_model(&s, &m).unwrap_or_else(|e| panic!("op {i}: {e}"));
    }
}

// ---- pins --------------------------------------------------------------------------------

#[test]
fn pins_follow_content_and_never_dangle() {
    for (seed, history) in [(1u64, true), (2, false), (3, true), (4, false)] {
        let mut rng = Rng::new(seed);
        let (rows, cols) = (8usize, 10usize);
        let cfg = ScreenConfig {
            history,
            limits: Limits {
                bytes: usize::MAX,
                lines: Some(30),
            },
        };
        let mut s = Screen::with_page_cap(rows, cols, cfg, Some(3));
        let mut next_id = rows as u64;
        let mut ids: Vec<u64> = (0..rows as u64).collect();
        let mut hist_ids: Vec<u64> = vec![];
        let mut pins: Vec<(PinId, u64)> = vec![];
        for step_no in 0..400 {
            if step_no % 7 == 0 && pins.len() < 40 {
                let y = rng.below(rows);
                let id = s.track_active(y, rng.below(cols), PinKind::Mark).unwrap();
                pins.push((id, ids[y]));
            }
            let top = rng.below(rows);
            let bottom = top + rng.below(rows - top);
            let r = Region {
                top,
                bottom,
                left: 0,
                right: cols - 1,
            };
            let n = (1 + rng.below(3)).min(bottom - top + 1);
            let up = rng.below(3) != 0;
            if up {
                s.scroll_up(r, n, Color::Default);
                if history && top == 0 && bottom == rows - 1 {
                    hist_ids.extend(ids.drain(0..n));
                    for _ in 0..n {
                        ids.push(next_id);
                        next_id += 1;
                    }
                } else {
                    for y in top..bottom + 1 - n {
                        ids[y] = ids[y + n];
                    }
                    for y in bottom + 1 - n..=bottom {
                        ids[y] = next_id;
                        next_id += 1;
                    }
                }
            } else {
                s.scroll_down(r, n, Color::Default);
                for y in (top + n..=bottom).rev() {
                    ids[y] = ids[y - n];
                }
                for y in top..top + n {
                    ids[y] = next_id;
                    next_id += 1;
                }
            }
            s.check_invariants()
                .unwrap_or_else(|e| panic!("seed {seed} step {step_no}: {e}"));
            for &(pid, rid) in &pins {
                let in_active = ids.iter().position(|&i| i == rid);
                match s.pin_state(pid).expect("tracked") {
                    PinState::Live(_) => {
                        if let Some(y) = in_active {
                            assert_eq!(
                                s.pin_active_row(pid),
                                Some(y),
                                "seed {seed} step {step_no}"
                            );
                        } else {
                            assert!(hist_ids.contains(&rid), "live pin on a discarded row");
                            assert_eq!(s.pin_active_row(pid), None);
                            assert!(s.pin_pos(pid).is_some());
                        }
                    }
                    PinState::Garbage => {
                        assert!(in_active.is_none(), "active row's pin went garbage")
                    }
                }
            }
        }
        let mut g = Vec::new();
        s.take_garbage(&mut g);
        for (pid, _) in g {
            assert_eq!(s.pin_state(pid), Some(PinState::Garbage));
        }
    }
}

#[test]
fn pin_on_evicted_row_becomes_garbage_once() {
    let cfg = ScreenConfig {
        history: true,
        limits: Limits {
            bytes: usize::MAX,
            lines: Some(8),
        },
    };
    let mut s = Screen::with_page_cap(3, 6, cfg, Some(4));
    let a = CellAttrs::default();
    s.print_run(
        0,
        0,
        &[Glyph {
            cp: 'A' as u32,
            width: 1,
        }],
        &a,
    );
    let old = s.track_active(0, 0, PinKind::Bookmark).unwrap();
    for _ in 0..40 {
        s.scroll_up(Region::full(3, 6), 1, Color::Default);
    }
    assert_eq!(s.pin_state(old), Some(PinState::Garbage));
    assert_eq!(s.pin_pos(old), None);
    let mut g = Vec::new();
    s.take_garbage(&mut g);
    assert_eq!(g, vec![(old, PinKind::Bookmark)]);
    g.clear();
    s.take_garbage(&mut g);
    assert!(g.is_empty());
    // A pin on a recent row survives further scrolling inside the limit.
    let cur = s.track_active(2, 1, PinKind::Cursor).unwrap();
    s.scroll_up(Region::full(3, 6), 1, Color::Default);
    assert_eq!(s.pin_active_row(cur), Some(1));
    assert!(s.history_rows() <= 8);
    s.check_invariants().unwrap();
}

#[test]
fn pin_survives_history_growth_and_reports_abs_position() {
    let mut s = Screen::new(4, 8, ScreenConfig::default());
    let a = CellAttrs::default();
    s.print_run(
        0,
        0,
        &[Glyph {
            cp: 'Q' as u32,
            width: 1,
        }],
        &a,
    );
    let p = s.track_active(0, 3, PinKind::Mark).unwrap();
    assert_eq!(s.pin_active_row(p), Some(0));
    for _ in 0..10 {
        s.scroll_up(Region::full(4, 8), 1, Color::Default);
    }
    assert_eq!(s.pin_pos(p), Some((0, 3)));
    assert_eq!(s.pin_active_row(p), None);
    let mut r = SnapRow::default();
    assert!(s.copy_abs_row(0, &mut r));
    assert_eq!(r.cells[0].cp, 'Q' as u32);
    assert!(s.set_pin_col(p, 5));
    assert_eq!(s.pin_pos(p), Some((0, 5)));
    assert!(s.untrack_pin(p));
    assert_eq!(s.pin_state(p), None);
}

// ---- scroll invariants, limits, pool ---------------------------------------------------

#[test]
fn full_scroll_copies_no_cells_and_keeps_order() {
    let mut s = Screen::new(5, 10, ScreenConfig::default());
    let a = CellAttrs::default();
    for round in 0..30u32 {
        for y in 0..5 {
            s.print_run(
                y,
                0,
                &[Glyph {
                    cp: 1000 + round * 5 + y as u32,
                    width: 1,
                }],
                &a,
            );
        }
        s.scroll_up(Region::full(5, 10), 5, Color::Default);
    }
    assert_eq!(s.history_rows(), 150 + 5 - 5);
    let mut r = SnapRow::default();
    for abs in 0..150 {
        assert!(s.copy_abs_row(abs, &mut r));
        assert_eq!(r.cells[0].cp, 1000 + abs as u32);
    }
    s.check_invariants().unwrap();
}

#[test]
fn eviction_honours_lines_and_reuses_pages() {
    let cfg = ScreenConfig {
        history: true,
        limits: Limits {
            bytes: usize::MAX,
            lines: Some(25),
        },
    };
    let mut s = Screen::with_page_cap(5, 10, cfg, Some(10));
    for _ in 0..300 {
        s.scroll_up(Region::full(5, 10), 1, Color::Default);
        assert!(s.history_rows() <= 25);
    }
    assert!(
        s.history_rows() >= 15,
        "evicts whole pages, never more than one beyond the limit"
    );
    let misses = s.stats().pool_misses;
    for _ in 0..500 {
        s.scroll_up(Region::full(5, 10), 1, Color::Default);
    }
    assert_eq!(
        s.stats().pool_misses,
        misses,
        "steady state allocates no pages"
    );
    assert!(s.stats().pool_hits > 0 && s.stats().evicted_pages > 0);
    assert!(s.pool_free() <= 2);
    s.check_invariants().unwrap();
}

#[test]
fn eviction_honours_bytes_and_runtime_change() {
    let page = 10 * (10 * 8 + 24);
    let cfg = ScreenConfig {
        history: true,
        limits: Limits {
            bytes: 3 * page + 1,
            lines: None,
        },
    };
    let mut s = Screen::with_page_cap(5, 10, cfg, Some(10));
    for _ in 0..200 {
        s.scroll_up(Region::full(5, 10), 1, Color::Default);
        assert!(s.page_count() <= 3);
        assert!(s.bytes_in_use() <= 3 * page + 1);
    }
    let a = CellAttrs::default();
    for y in 0..5 {
        s.print_run(
            y,
            0,
            &[Glyph {
                cp: 'a' as u32 + y as u32,
                width: 1,
            }],
            &a,
        );
    }
    let before = grid_rows(&s);
    s.set_limits(Limits {
        bytes: 1,
        lines: Some(0),
    });
    assert_eq!(grid_rows(&s), before, "the active area is never evicted");
    assert!(
        s.page_count() <= 2,
        "pages overlapping the active area are kept"
    );
    assert!(s.history_rows() < 10 + 5);
    s.check_invariants().unwrap();
    // Raising the limit again lets history grow back.
    s.set_limits(Limits {
        bytes: 10 * page,
        lines: None,
    });
    for _ in 0..100 {
        s.scroll_up(Region::full(5, 10), 1, Color::Default);
    }
    assert!(s.page_count() > 3);
}

#[test]
fn alt_screen_has_no_history_and_scrolls_in_place() {
    let mut s = Screen::new(
        4,
        6,
        ScreenConfig {
            history: false,
            ..ScreenConfig::default()
        },
    );
    let a = CellAttrs::default();
    for y in 0..4 {
        s.print_run(
            y,
            0,
            &[Glyph {
                cp: 'a' as u32 + y as u32,
                width: 1,
            }],
            &a,
        );
    }
    s.scroll_up(Region::full(4, 6), 1, Color::Default);
    assert_eq!(s.history_rows(), 0);
    assert_eq!(s.page_count(), 1);
    assert_eq!(s.cell(0, 0).payload(), 'b' as u32);
    assert_eq!(s.cell(2, 0).payload(), 'd' as u32);
    assert_eq!(s.cell(3, 0), Cell::EMPTY);
}

// ---- wide characters, clusters, links, flags -----------------------------------------

fn wg(cp: u32) -> Glyph {
    Glyph { cp, width: 2 }
}

#[test]
fn wide_print_overflow_and_seam_repair_keep_wrap_flag() {
    let mut s = Screen::new(3, 6, ScreenConfig::default());
    let a = CellAttrs::default();
    assert_eq!(
        s.print_run(0, 0, &[wg(0x4E2D), wg(0x6587), wg(0x5B57)], &a),
        3
    );
    assert_eq!(s.cell(0, 0).wide(), Wide::Wide);
    assert_eq!(s.cell(0, 1).wide(), Wide::SpacerTail);
    assert_eq!(
        s.print_run(1, 5, &[wg(1)], &a),
        0,
        "does not fit: caller wraps"
    );
    s.set_row_flag(0, flags::WRAPPED, true);
    s.print_run(0, 1, &[Glyph { cp: 65, width: 1 }], &a);
    assert_eq!(s.cell(0, 0), Cell::EMPTY);
    assert_eq!(s.cell(0, 1).payload(), 65);
    s.check_invariants().unwrap();
    s.print_run(0, 2, &[Glyph { cp: 66, width: 1 }], &a);
    assert_eq!(s.cell(0, 3), Cell::EMPTY);
    s.check_invariants().unwrap();
    assert_ne!(s.row_flags(0) & flags::WRAPPED, 0);
    // Erase cutting a pair, then ICH pushing a pair off the margin.
    s.print_run(1, 4, &[wg(0x4E2D)], &a);
    s.erase_cells(1, 5, 6, Color::Default, false);
    assert_eq!(s.cell(1, 4), Cell::EMPTY);
    s.print_run(1, 4, &[wg(0x4E2D)], &a);
    s.insert_chars(1, 0, 1, 5, Color::Default);
    assert_eq!(s.cell(1, 5), Cell::EMPTY);
    s.check_invariants().unwrap();
}

#[test]
fn clusters_and_appended_marks() {
    let mut s = Screen::new(2, 10, ScreenConfig::default());
    let a = CellAttrs::default();
    let family = [0x1F468, 0x200D, 0x1F469, 0x200D, 0x1F467, 0x200D, 0x1F466];
    assert!(s.print_cluster(0, 0, &family, 2, &a));
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    assert_eq!(r.cells[0].wide, Wide::Wide);
    assert_eq!(r.cluster(&r.cells[0]), &family[..]);
    assert_eq!(r.cells[1].wide, Wide::SpacerTail);
    // Appending through the spacer resolves to the head.
    assert!(s.append_cp(0, 1, 0xFE0F));
    let mut t = Vec::new();
    s.cell_text(0, 0, &mut t);
    assert_eq!(t.len(), 8);
    // Scalar -> cluster, up to the 64 code point cap.
    s.print_run(
        1,
        0,
        &[Glyph {
            cp: 'e' as u32,
            width: 1,
        }],
        &a,
    );
    let mut ok = 0;
    for _ in 0..70 {
        if s.append_cp(1, 0, 0x301) {
            ok += 1;
        }
    }
    assert_eq!(ok, 63);
    s.cell_text(1, 0, &mut t);
    assert_eq!(t.len(), 64);
    assert!(!s.append_cp(1, 5, 0x301), "nothing to attach to");
    s.erase_rows(0, 1, Color::Default, false);
    s.check_invariants().unwrap();
    assert_eq!(s.row_flags(0) & flags::HAS_MASK, 0);
}

#[test]
fn links_styles_and_semantic_bits_resolve() {
    let mut s = Screen::new(2, 10, ScreenConfig::default());
    let st = style(3);
    let a = CellAttrs {
        style: st,
        link: Some(("id", "https://x.example/")),
        protected: true,
        semantic: Semantic::Prompt,
    };
    s.print_run(
        0,
        2,
        &[Glyph {
            cp: 'z' as u32,
            width: 1,
        }],
        &a,
    );
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    let c = r.cells[2];
    assert!(c.link && c.protected);
    assert_eq!(c.semantic, Semantic::Prompt);
    assert_eq!(c.style, st);
    // Selective erase spares protected cells; a plain erase does not.
    s.erase_cells(0, 0, 10, Color::Default, true);
    assert_eq!(s.cell(0, 2).payload(), 'z' as u32);
    s.erase_cells(0, 0, 10, Color::Default, false);
    assert_eq!(s.cell(0, 2), Cell::EMPTY);
    s.check_invariants().unwrap();
}

#[test]
fn bce_fills_palette_and_rgb_without_touching_the_style_table() {
    let mut s = Screen::new(1, 6, ScreenConfig::default());
    s.erase_cells(0, 0, 3, Color::Palette(9), false);
    s.erase_cells(0, 3, 6, Color::Rgb(1, 2, 3), false);
    assert_eq!(s.cell(0, 0).tag(), Tag::BgOnly);
    assert_eq!(
        s.cell(0, 0).style_id(),
        0,
        "palette bg touches no style table"
    );
    assert_eq!(s.cell(0, 4).tag(), Tag::BgOnly);
    assert_eq!(
        s.cell(0, 4).style_id(),
        0,
        "RGB bg is packed in the cell (T-02 §1.2), not interned"
    );
    assert_eq!(s.cell(0, 4).bg_color(), Color::Rgb(1, 2, 3));
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    assert_eq!(r.cells[1].style.bg, Color::Palette(9));
    assert_eq!(r.cells[5].style.bg, Color::Rgb(1, 2, 3));
    s.check_invariants().unwrap();
}

#[test]
fn row_flags_block_and_tabs_are_independent_of_cells() {
    let mut s = Screen::new(3, 5, ScreenConfig::default());
    s.set_row_flag(1, flags::PROMPT_START, true);
    s.set_row_block(1, 42);
    assert_ne!(s.row_flags(1) & flags::PROMPT_START, 0);
    assert_eq!(s.row_block(1), 42);
    s.set_row_flag(1, flags::DIRTY, true); // not an allowed flag: ignored
    s.scroll_up(Region::full(3, 5), 1, Color::Default);
    assert_eq!(s.row_block(0), 42, "block id moves with its row");
    assert_eq!(s.row_block(2), 0);
}

// ---- snapshots ---------------------------------------------------------------------------

#[test]
fn damage_classification() {
    let mut s = Screen::new(5, 10, ScreenConfig::default());
    let mut f = FrameSnapshot::new();
    let a = CellAttrs::default();
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Full);
    assert_eq!(s.snapshot_dirty(&mut f), Damage::None);
    s.mark_cursor_dirty();
    assert_eq!(s.snapshot_dirty(&mut f), Damage::CursorOnly);
    s.print_run(2, 0, &[Glyph { cp: 65, width: 1 }], &a);
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Rows);
    assert_eq!(f.changed, vec![2]);
    // Full-screen scroll accumulates into one delta.
    s.scroll_up(Region::full(5, 10), 1, Color::Default);
    s.scroll_up(Region::full(5, 10), 1, Color::Default);
    assert_eq!(
        s.snapshot_dirty(&mut f),
        Damage::Scroll {
            top: 0,
            bottom: 4,
            left: 0,
            right: 9,
            delta: 2
        }
    );
    assert_eq!(f.changed, vec![3, 4]);
    // Up then down by the same amount nets to no scroll; only rows are dirty.
    s.scroll_up(Region::full(5, 10), 1, Color::Default);
    s.scroll_down(Region::full(5, 10), 1, Color::Default);
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Rows);
    // Different region, delta >= height, palette change: Full.
    s.scroll_up(
        Region {
            top: 1,
            bottom: 3,
            left: 0,
            right: 9,
        },
        1,
        Color::Default,
    );
    s.scroll_up(Region::full(5, 10), 1, Color::Default);
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Full);
    s.scroll_up(Region::full(5, 10), 5, Color::Default);
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Full);
    s.mark_full();
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Full);
    // A new buffer (pane moved to another window) starts Full.
    let mut g = FrameSnapshot::new();
    assert_eq!(s.snapshot_dirty(&mut g), Damage::Full);
}

#[test]
fn snapshot_since_is_independent_of_dirty() {
    let mut s = Screen::new(4, 8, ScreenConfig::default());
    let a = CellAttrs::default();
    let (mut f, mut r1, mut r2) = (
        FrameSnapshot::new(),
        RowSnapshot::default(),
        RowSnapshot::default(),
    );
    let (mut s1, mut s2) = (SeenSet::new(4), SeenSet::new(4));
    assert_eq!(s.snapshot_since(&mut r1, &mut s1), Changes::Full);
    assert_eq!(s.snapshot_since(&mut r1, &mut s1), Changes::None);
    s.print_run(1, 0, &[Glyph { cp: 66, width: 1 }], &a);
    // The renderer consuming dirty does not hide the change from the other consumers.
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Full);
    assert_eq!(s.snapshot_since(&mut r1, &mut s1), Changes::Rows);
    assert_eq!(r1.changed, vec![1]);
    assert_eq!(s.snapshot_since(&mut r2, &mut s2), Changes::Full);
    // And consuming `since` does not clear dirty.
    s.print_run(2, 0, &[Glyph { cp: 67, width: 1 }], &a);
    assert_eq!(s.snapshot_since(&mut r1, &mut s1), Changes::Rows);
    assert_eq!(s.snapshot_dirty(&mut f), Damage::Rows);
    assert_eq!(f.changed, vec![2]);
    // A scroll shifts positions, so position-keyed consumers re-copy the region.
    s.scroll_up(Region::full(4, 8), 1, Color::Default);
    assert_eq!(s.snapshot_since(&mut r1, &mut s1), Changes::Rows);
    assert_eq!(r1.changed.len(), 4);
    // Reset makes the next call a full copy.
    s1.reset();
    assert_eq!(s.snapshot_since(&mut r1, &mut s1), Changes::Full);
}

fn replay(prev: &mut Vec<Vec<Norm>>, d: Damage, f: &FrameSnapshot) {
    match d {
        Damage::Full => *prev = f.rows.iter().map(norm_row).collect(),
        Damage::Rows => {
            for &y in &f.changed {
                prev[y as usize] = norm_row(&f.rows[y as usize]);
            }
        }
        Damage::Scroll {
            top, bottom, delta, ..
        } => {
            let region = &mut prev[top..=bottom];
            if delta > 0 {
                region.rotate_left(delta as usize);
            } else {
                region.rotate_right((-delta) as usize);
            }
            for &y in &f.changed {
                prev[y as usize] = norm_row(&f.rows[y as usize]);
            }
        }
        Damage::None | Damage::CursorOnly => {}
    }
}

#[test]
fn consumers_are_independent_and_scroll_damage_replays_to_full() {
    for seed in 0..16 {
        let mut rng = Rng::new(900 + seed);
        let mut s = small(true, Some(40));
        let mut f = FrameSnapshot::new();
        let mut prev: Vec<Vec<Norm>> = vec![];
        let mut since: Vec<(RowSnapshot, SeenSet)> = (0..2)
            .map(|_| (RowSnapshot::default(), SeenSet::new(8)))
            .collect();
        for i in 0..400 {
            for _ in 0..1 + rng.below(3) {
                step(&mut rng, &mut s, None);
            }
            let truth = grid_rows(&s);
            if rng.below(2) == 0 {
                let d = s.snapshot_dirty(&mut f);
                let view: Vec<Vec<Norm>> = f.rows.iter().map(norm_row).collect();
                assert_eq!(view, truth, "seed {seed} step {i}: renderer view");
                replay(&mut prev, d, &f);
                assert_eq!(prev, truth, "seed {seed} step {i}: replay of {d:?}");
            }
            for (snap, seen) in &mut since {
                if rng.below(2) == 0 {
                    s.snapshot_since(snap, seen);
                    let view: Vec<Vec<Norm>> = snap.rows.iter().map(norm_row).collect();
                    assert_eq!(view, truth, "seed {seed} step {i}: since view");
                }
            }
        }
    }
}

#[test]
fn resolved_snapshot_rows_do_not_reallocate() {
    let mut s = Screen::new(4, 8, ScreenConfig::default());
    let mut f = FrameSnapshot::new();
    s.snapshot_dirty(&mut f);
    let ptrs: Vec<*const SnapCell> = f.rows.iter().map(|r| r.cells.as_ptr()).collect();
    let a = CellAttrs {
        style: style(2),
        ..CellAttrs::default()
    };
    s.print_run(1, 0, &[Glyph { cp: 65, width: 1 }], &a);
    s.snapshot_dirty(&mut f);
    assert_eq!(f.rows[1].cells.as_ptr(), ptrs[1]);
}

// ---- page tables never lose content: grow, then split, then (only then) degrade ---------

use crate::page::{Ceil, PageSpec};

fn tiny_spec(cols: usize, rows_cap: usize) -> PageSpec {
    let mut s = PageSpec::with_rows(cols, rows_cap);
    s.style_cap = 4;
    s.chunk_cap = 2;
    s.link_cap = 1;
    s.lmap_cap = 2;
    s.str_cap = 16;
    s
}

fn pal(n: usize) -> Style {
    Style {
        fg: Color::Palette(n as u8),
        ..Style::DEFAULT
    }
}

fn glyph(c: char) -> [Glyph; 1] {
    [Glyph {
        cp: c as u32,
        width: 1,
    }]
}

fn tiny_screen(rows: usize, cols: usize, ceil: Ceil) -> Screen {
    let cfg = ScreenConfig {
        history: false,
        limits: Limits::default(),
    };
    Screen::with_spec(rows, cols, cfg, tiny_spec(cols, rows), ceil)
}

#[test]
fn style_table_overflow_grows_the_page() {
    let mut s = tiny_screen(2, 60, Ceil::MAX);
    for x in 0..50 {
        let a = CellAttrs {
            style: pal(x + 1),
            ..CellAttrs::default()
        };
        assert_eq!(s.print_run(0, x, &glyph('a'), &a), 1);
        s.check_invariants().unwrap();
    }
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    for x in 0..50 {
        assert_eq!(r.cells[x].style, pal(x + 1), "col {x}");
    }
    let st = s.stats();
    assert!(st.page_growths > 0);
    assert_eq!((st.degraded, st.page_splits), (0, 0));
}

#[test]
fn cluster_arena_overflow_grows_the_page() {
    let mut s = tiny_screen(2, 40, Ceil::MAX);
    let a = CellAttrs::default();
    for x in 0..30 {
        assert!(s.print_cluster(
            0,
            x,
            &['e' as u32, 0x301, 0x302, 0x303, 0x304, 0x305],
            1,
            &a
        ));
    }
    // Appending to an existing cluster past its last chunk also needs room.
    for _ in 0..70 {
        s.append_cp(0, 0, 0x306);
    }
    let mut t = Vec::new();
    s.cell_text(0, 29, &mut t);
    assert_eq!(t.len(), 6);
    s.cell_text(0, 0, &mut t);
    assert_eq!(t.len(), 64);
    s.check_invariants().unwrap();
    let st = s.stats();
    assert!(st.page_growths > 0);
    assert_eq!(st.degraded, 0);
}

#[test]
fn link_tables_overflow_grow_the_page() {
    let mut s = tiny_screen(2, 80, Ceil::MAX);
    // Link items and the string arena: many distinct, long URIs.
    for x in 0..20 {
        let uri = format!("https://example.org/{}/{x}", "p".repeat(300));
        let a = CellAttrs {
            link: Some(("", &uri)),
            ..CellAttrs::default()
        };
        s.print_run(0, x, &glyph('l'), &a);
    }
    // Link map entries: one link over a long run.
    let run: Vec<Glyph> = (0..40)
        .map(|i| Glyph {
            cp: 'a' as u32 + (i % 26),
            width: 1,
        })
        .collect();
    let a = CellAttrs {
        link: Some(("k", "https://one.example/")),
        ..CellAttrs::default()
    };
    assert_eq!(s.print_run(1, 0, &run, &a), 40);
    s.check_invariants().unwrap();
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    assert!(r.cells[..20].iter().all(|c| c.link));
    s.copy_row(1, &mut r);
    assert!(r.cells[..40].iter().all(|c| c.link));
    assert_eq!(s.stats().degraded, 0);
    assert!(s.stats().page_growths >= 3);
    // Erasing releases the entries again.
    s.erase_rows(0, 1, Color::Default, false);
    s.check_invariants().unwrap();
}

#[test]
fn table_at_its_ceiling_splits_the_page_and_loses_nothing() {
    let ceil = Ceil {
        styles: 20,
        chunks: 6,
        links: 14,
        lmap: 30,
        strs: 600,
    };
    let (rows, cols) = (6, 12);
    let mut s = tiny_screen(rows, cols, ceil);
    let pin = s.track_active(4, 3, PinKind::Mark).unwrap();
    // 72 distinct styles over 6 rows: no single page may hold them all.
    for y in 0..rows {
        for x in 0..cols {
            let uri = format!("https://e.example/{y}/{x}");
            let a = CellAttrs {
                style: pal(y * cols + x + 1),
                link: Some(("", &uri)),
                ..CellAttrs::default()
            };
            s.print_run(y, x, &glyph('a'), &a);
        }
        s.print_cluster(
            y,
            0,
            &['e' as u32, 0x301, 0x302],
            1,
            &CellAttrs {
                style: pal(200 + y),
                ..CellAttrs::default()
            },
        );
        s.check_invariants().unwrap();
    }
    let st = s.stats();
    assert!(st.page_splits > 0, "{st:?}");
    assert_eq!(st.degraded, 0, "{st:?}");
    assert!(s.page_count() > 1);
    let mut r = SnapRow::default();
    for y in 0..rows {
        s.copy_row(y, &mut r);
        for x in 0..cols {
            let c = &r.cells[x];
            if x == 0 {
                assert_eq!(c.style, pal(200 + y));
                assert_eq!(r.cluster(c), &[0x65, 0x301, 0x302]);
            } else {
                assert_eq!(c.style, pal(y * cols + x + 1), "({y},{x})");
                assert_eq!(c.cp, 'a' as u32);
            }
            if x != 0 {
                assert!(c.link);
            }
        }
    }
    assert_eq!(
        s.pin_active_row(pin),
        Some(4),
        "pins follow rows through a split"
    );
}

#[test]
fn row_that_exceeds_every_ceiling_degrades_and_counts() {
    let ceil = Ceil {
        styles: 4,
        ..Ceil::MAX
    };
    let mut s = tiny_screen(2, 12, ceil);
    for x in 0..12 {
        let a = CellAttrs {
            style: pal(x + 1),
            ..CellAttrs::default()
        };
        s.print_run(0, x, &glyph('z'), &a);
    }
    s.check_invariants().unwrap();
    assert!(s.stats().degraded > 0);
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    assert!(
        r.cells.iter().all(|c| c.cp == 'z' as u32),
        "text is never lost"
    );
}

#[test]
fn scrolls_across_a_full_page_grow_the_destination() {
    // Region scrolls copy across a page seam; the destination must grow, not degrade.
    let cfg = ScreenConfig {
        history: true,
        limits: Limits::default(),
    };
    let mut spec = tiny_spec(10, 3);
    spec.rows_cap = 3;
    let mut s = Screen::with_spec(6, 10, cfg, spec, Ceil::MAX);
    for y in 0..6 {
        for x in 0..10 {
            let a = CellAttrs {
                style: pal(y * 10 + x + 1),
                ..CellAttrs::default()
            };
            s.print_run(y, x, &glyph('q'), &a);
        }
    }
    let reg = Region {
        top: 0,
        bottom: 5,
        left: 2,
        right: 7,
    };
    s.scroll_up(reg, 2, Color::Default);
    s.check_invariants().unwrap();
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    assert_eq!(r.cells[3].style, pal(2 * 10 + 3 + 1));
    assert_eq!(r.cells[0].style, pal(1));
    assert_eq!(s.stats().degraded, 0);
}

#[test]
fn hostile_dimensions_are_clamped() {
    for history in [true, false] {
        let cfg = ScreenConfig {
            history,
            limits: Limits::default(),
        };
        let s = Screen::new(65_535, 65_535, cfg);
        assert!(
            s.rows() * s.cols() <= MAX_CELLS,
            "{}x{}",
            s.rows(),
            s.cols()
        );
        assert!(s.rows() >= 1 && s.cols() == 65_535);
        let s = Screen::new(usize::MAX, usize::MAX, cfg);
        assert!(s.rows() * s.cols() <= MAX_CELLS);
        let s = Screen::new(0, 0, cfg);
        assert_eq!((s.rows(), s.cols()), (1, 1));
    }
}

// ---- resize (no reflow) ---------------------------------------------------------------

fn abs_text(s: &Screen, abs: usize) -> String {
    let mut r = SnapRow::default();
    assert!(s.copy_abs_row(abs, &mut r));
    r.cells
        .iter()
        .map(|c| char::from_u32(c.cp).filter(|&c| c != '\0').unwrap_or(' '))
        .collect()
}

/// Every retained row (history first), as text.
fn all_text(s: &Screen) -> Vec<String> {
    (0..s.history_rows() + s.rows())
        .map(|a| abs_text(s, a))
        .collect()
}

fn fill_rows(s: &mut Screen) {
    let a = CellAttrs::default();
    for y in 0..s.rows() {
        for x in 0..s.cols() {
            let ch = (b'a' + ((y * 7 + x) % 26) as u8) as u32;
            s.print_run(y, x, &[Glyph { cp: ch, width: 1 }], &a);
        }
    }
}

fn cfg(history: bool) -> ScreenConfig {
    ScreenConfig {
        history,
        limits: Limits::default(),
    }
}

#[test]
fn resize_grow_rows_appends_blank_rows_and_pulls_no_history() {
    let mut s = Screen::new(3, 4, cfg(true));
    fill_rows(&mut s);
    s.scroll_up(Region::full(3, 4), 1, Color::Default);
    let before = all_text(&s);
    assert_eq!(s.history_rows(), 1);
    let r = s.resize(5, 4);
    assert_eq!((r.rows, r.cols, r.cursor_row, r.pushed), (5, 4, 2, 0));
    assert_eq!(s.history_rows(), 1, "no history pull");
    let after = all_text(&s);
    assert_eq!(&after[..before.len()], &before[..]);
    assert_eq!(after[before.len()], "    ");
    assert_eq!(after.len(), 6);
    s.check_invariants().unwrap();
}

#[test]
fn resize_shrink_rows_pushes_top_rows_into_scrollback_on_primary() {
    let mut s = Screen::new(4, 4, cfg(true));
    fill_rows(&mut s);
    let before = all_text(&s);
    let r = s.resize(2, 4);
    assert_eq!((r.rows, r.cursor_row, r.pushed), (2, 1, 2));
    assert_eq!(s.history_rows(), 2);
    assert_eq!(
        all_text(&s),
        before,
        "nothing lost, top rows are history now"
    );
    s.check_invariants().unwrap();
}

#[test]
fn resize_shrink_rows_drops_rows_below_the_cursor_first() {
    let mut s = Screen::new(4, 4, cfg(true));
    fill_rows(&mut s);
    let before = all_text(&s);
    // Cursor on row 1: two rows below it go, nothing is pushed.
    let r = s.resize_keeping(2, 4, 1);
    assert_eq!((r.cursor_row, r.pushed), (1, 0));
    assert_eq!(s.history_rows(), 0);
    assert_eq!(all_text(&s), before[..2].to_vec());
    // Cursor on row 2 of 4, shrink to 2: one row dropped below, one pushed.
    let mut s = Screen::new(4, 4, cfg(true));
    fill_rows(&mut s);
    let r = s.resize_keeping(2, 4, 2);
    assert_eq!((r.cursor_row, r.pushed), (1, 1));
    assert_eq!(all_text(&s), before[..3].to_vec());
    assert_eq!(s.history_rows(), 1);
    s.check_invariants().unwrap();
}

#[test]
fn resize_alt_screen_drops_instead_of_scrollback() {
    let mut s = Screen::new(4, 4, cfg(false));
    fill_rows(&mut s);
    let before = all_text(&s);
    let r = s.resize(2, 4);
    assert_eq!((r.pushed, r.cursor_row), (0, 1));
    assert_eq!(s.history_rows(), 0, "alt has no history");
    assert_eq!(all_text(&s), before[2..].to_vec());
    let r = s.resize(5, 6);
    assert_eq!(r.rows, 5);
    assert_eq!(s.history_rows(), 0);
    assert_eq!(abs_text(&s, 0), format!("{:<6}", before[2]));
    s.check_invariants().unwrap();
}

#[test]
fn resize_truncates_and_pads_columns() {
    let mut s = Screen::new(3, 6, cfg(true));
    fill_rows(&mut s);
    let before = all_text(&s);
    s.set_row_flag(1, flags::WRAPPED, true);
    s.set_row_block(1, 7);
    s.resize(3, 3);
    let narrow = all_text(&s);
    for (n, b) in narrow.iter().zip(&before) {
        assert_eq!(n, &b[..3]);
    }
    assert_ne!(s.row_flags(1) & flags::WRAPPED, 0);
    assert_eq!(s.row_block(1), 7);
    s.resize(3, 8);
    for (n, b) in all_text(&s).iter().zip(&before) {
        assert_eq!(n, &format!("{:<8}", &b[..3]));
    }
    assert_eq!(s.row_block(1), 7);
    s.check_invariants().unwrap();
}

#[test]
fn resize_round_trip_when_growing_first() {
    let mut s = Screen::new(5, 7, cfg(true));
    fill_rows(&mut s);
    s.scroll_up(Region::full(5, 7), 2, Color::Default);
    let before = all_text(&s);
    s.resize(9, 12);
    s.resize(5, 7);
    // Growing appended blanks at the bottom; shrinking with the cursor on the bottom row
    // pushes them... the blank rows are now history, the original rows are intact.
    let after = all_text(&s);
    assert_eq!(&after[..before.len()], &before[..]);
    s.check_invariants().unwrap();
    // Cursor-aware shrink gives the exact original back.
    let mut s = Screen::new(5, 7, cfg(true));
    fill_rows(&mut s);
    let before = all_text(&s);
    s.resize(9, 12);
    s.resize_keeping(5, 7, 4);
    assert_eq!(all_text(&s), before);
}

#[test]
fn resize_wide_char_at_the_new_right_edge_is_blanked() {
    let a = CellAttrs::default();
    let mut s = Screen::new(2, 6, cfg(true));
    s.print_run(0, 3, &[wg(0x4E2D)], &a); // cols 3..=4
    s.print_run(1, 4, &[wg(0x4E2D)], &a); // cols 4..=5
    s.resize(2, 4);
    assert_eq!(s.cell(0, 3), Cell::EMPTY, "head cut off from its tail");
    s.check_invariants().unwrap();
    // A wide char that fits whole survives.
    let mut s = Screen::new(1, 6, cfg(true));
    s.print_run(0, 2, &[wg(0x4E2D)], &a);
    s.resize(1, 4);
    assert_eq!(s.cell(0, 2).wide(), Wide::Wide);
    assert_eq!(s.cell(0, 3).wide(), Wide::SpacerTail);
    s.resize(1, 3);
    assert_eq!(s.cell(0, 2), Cell::EMPTY);
    assert_eq!(s.cell(0, 1), Cell::EMPTY);
    s.check_invariants().unwrap();
}

#[test]
fn resize_pins_survive_and_dropped_rows_become_garbage() {
    let mut s = Screen::new(4, 6, cfg(true));
    fill_rows(&mut s);
    s.scroll_up(Region::full(4, 6), 2, Color::Default);
    let texts = all_text(&s);
    let hist = s.history_rows();
    let a = s.track_abs(0, 5, PinKind::Mark).unwrap();
    let b = s.track_active(1, 2, PinKind::Cursor).unwrap();
    let c = s.track_active(3, 0, PinKind::SavedCursor).unwrap();
    // Shrink with the cursor on row 1: row 3 (pin c) is dropped.
    s.resize_keeping(2, 3, 1);
    assert_eq!(s.pin_pos(a), Some((0, 2)), "col clamped to the new width");
    assert_eq!(s.pin_pos(b), Some((hist + 1, 2)));
    assert_eq!(abs_text(&s, hist + 1), texts[hist + 1][..3]);
    assert_eq!(s.pin_state(c), Some(PinState::Garbage));
    let mut g = Vec::new();
    s.take_garbage(&mut g);
    assert_eq!(g, vec![(c, PinKind::SavedCursor)]);
    s.check_invariants().unwrap();
    // Row-only fast path keeps pins as well.
    s.resize(6, 3);
    assert_eq!(s.pin_pos(b), Some((hist + 1, 2)));
    s.check_invariants().unwrap();
}

#[test]
fn resize_alt_pin_on_discarded_top_row_becomes_garbage() {
    let mut s = Screen::new(4, 4, cfg(false));
    let top = s.track_active(0, 0, PinKind::Mark).unwrap();
    let keep = s.track_active(3, 1, PinKind::Mark).unwrap();
    s.resize(2, 4);
    assert_eq!(s.pin_state(top), Some(PinState::Garbage));
    assert_eq!(s.pin_pos(keep), Some((1, 1)));
}

#[test]
fn resize_marks_everything_dirty_and_clamps_to_max_cells() {
    let mut s = Screen::new(3, 4, cfg(true));
    let mut snap = FrameSnapshot::new();
    s.snapshot_dirty(&mut snap);
    assert!(matches!(s.snapshot_dirty(&mut snap), Damage::None));
    s.resize(3, 5);
    assert!(matches!(s.snapshot_dirty(&mut snap), Damage::Full));
    s.resize(4, 5);
    assert!(matches!(s.snapshot_dirty(&mut snap), Damage::Full));
    let r = s.resize(65_535, 65_535);
    assert!(r.rows * r.cols <= MAX_CELLS && s.rows() * s.cols() <= MAX_CELLS);
    assert_eq!(s.cols(), 65_535);
    let r = s.resize(0, 0);
    assert_eq!((r.rows, r.cols), (1, 1));
    s.check_invariants().unwrap();
}

#[test]
fn resize_keeps_styles_links_and_clusters() {
    let mut s = Screen::new(3, 6, cfg(true));
    let st = style(3);
    let attrs = CellAttrs {
        style: st,
        link: Some(("x", "https://b.example/")),
        ..CellAttrs::default()
    };
    s.print_cluster(0, 1, &[0x65, 0x301], 1, &attrs);
    s.print_run(1, 0, &[Glyph { cp: 66, width: 1 }], &attrs);
    s.resize(3, 4);
    s.resize(5, 9);
    let mut r = SnapRow::default();
    s.copy_row(0, &mut r);
    assert_eq!(r.cells[1].cp, 0x65);
    assert_eq!(r.cluster(&r.cells[1]), &[0x65, 0x301]);
    assert_eq!(r.cells[1].style, st);
    assert!(r.cells[1].link);
    s.check_invariants().unwrap();
}

#[test]
fn resize_random_sequences_match_a_naive_model() {
    // Model: all retained rows as Vec<Vec<char>> (history first), blanks are ' '.
    for seed in 1..=40u64 {
        let mut rng = Rng::new(seed);
        for history in [true, false] {
            let mut s = Screen::with_page_cap(4, 5, cfg(history), Some(3));
            let mut model: Vec<Vec<char>> = vec![vec![' '; s.cols()]; s.rows()];
            let mut cols = s.cols();
            let mut rows = s.rows();
            let mut pins: Vec<(PinId, usize, usize)> = Vec::new(); // id, abs row, col
            for _ in 0..60 {
                match rng.below(5) {
                    0 | 1 => {
                        let (y, x) = (rng.below(rows), rng.below(cols));
                        let ch = char::from(b'a' + rng.below(26) as u8);
                        s.print_run(
                            y,
                            x,
                            &[Glyph {
                                cp: ch as u32,
                                width: 1,
                            }],
                            &CellAttrs::default(),
                        );
                        let base = model.len() - rows;
                        model[base + y][x] = ch;
                    }
                    2 => {
                        if history {
                            let n = (1 + rng.below(2)).min(rows);
                            s.scroll_up(Region::full(rows, cols), n, Color::Default);
                            for _ in 0..n {
                                model.push(vec![' '; cols]);
                            }
                        }
                    }
                    3 => {
                        let (y, x) = (rng.below(rows), rng.below(cols));
                        let base = model.len() - rows;
                        if let Some(id) = s.track_active(y, x, PinKind::Mark) {
                            pins.push((id, base + y, x));
                        }
                    }
                    _ => {
                        let nr = 1 + rng.below(8);
                        let nc = 1 + rng.below(9);
                        let cy = rng.below(rows);
                        let r = s.resize_keeping(nr, nc, cy);
                        assert_eq!((r.rows, r.cols), (nr, nc));
                        let d = rows.saturating_sub(nr);
                        let trim = d.min(rows - 1 - cy);
                        let drop_top = d - trim;
                        let keep_start = if history {
                            0
                        } else {
                            model.len() - rows + drop_top
                        };
                        let keep_end = model.len() - trim;
                        let mut kept: Vec<Vec<char>> = model[keep_start..keep_end].to_vec();
                        for row in &mut kept {
                            row.resize(nc, ' ');
                        }
                        let append = nr.saturating_sub(rows);
                        for _ in 0..append {
                            kept.push(vec![' '; nc]);
                        }
                        pins.retain_mut(|(id, abs, col)| {
                            if *abs < keep_start || *abs >= keep_end {
                                assert_eq!(s.pin_state(*id), Some(PinState::Garbage));
                                false
                            } else {
                                *abs -= keep_start;
                                *col = (*col).min(nc - 1);
                                true
                            }
                        });
                        model = kept;
                        rows = nr;
                        cols = nc;
                        assert_eq!(r.cursor_row, cy - drop_top);
                    }
                }
                assert_eq!(s.rows(), rows);
                assert_eq!(
                    all_text(&s),
                    model
                        .iter()
                        .map(|r| r.iter().collect::<String>())
                        .collect::<Vec<_>>(),
                    "seed {seed} history {history}"
                );
                for (id, abs, col) in &pins {
                    assert_eq!(s.pin_pos(*id), Some((*abs, *col)), "seed {seed}");
                }
                s.check_invariants().unwrap();
            }
        }
    }
}
