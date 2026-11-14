// SPDX-License-Identifier: AGPL-3.0-only
//! The throughput fast paths against their general paths: `print_ascii` and the narrow-run
//! store against `print_run`, in-place page growth against copy growth, and row-handle
//! rotation against the swap cascade (checked through the screen's invariants).

use super::*;
use crate::page::{Ceil, Full, Page, PageSpec};

struct Rng(u64);

impl Rng {
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

fn screen() -> Screen {
    Screen::new(
        6,
        20,
        ScreenConfig {
            history: true,
            limits: Limits {
                bytes: 1 << 24,
                lines: Some(100),
            },
        },
    )
}

fn dump(s: &Screen) -> String {
    let mut out = String::new();
    let mut row = SnapRow::default();
    for y in 0..s.rows() {
        s.copy_row(y, &mut row);
        out.push_str(&format!("{y} w={}:", row.wrapped));
        for c in &row.cells {
            out.push_str(&format!(
                "[{:x} {:?} {:?} {}]",
                c.cp, c.wide, c.style, c.link
            ));
        }
        out.push('\n');
    }
    out
}

fn style_of(i: usize) -> Style {
    Style {
        fg: Color::Rgb(i as u8, 1, 2),
        ..Style::DEFAULT
    }
}

#[test]
fn print_ascii_matches_print_run() {
    let mut rng = Rng(0xFEED_F00D_1234_5678);
    for _ in 0..300 {
        let (mut a, mut b) = (screen(), screen());
        for _ in 0..60 {
            let (y, x) = (rng.below(6), rng.below(20));
            let st = if rng.below(3) == 0 {
                Style::DEFAULT
            } else {
                style_of(rng.below(5))
            };
            let link = (rng.below(5) == 0).then_some(("id", "https://example.org"));
            let attrs = CellAttrs {
                style: st,
                link,
                protected: rng.below(7) == 0,
                semantic: Semantic::Output,
            };
            let n = 1 + rng.below(25);
            match rng.below(4) {
                0 => {
                    // Wide and clustered content that later writes must repair.
                    let run = [
                        Glyph {
                            cp: 0x4e16,
                            width: 2,
                        },
                        Glyph { cp: 0x41, width: 1 },
                        Glyph {
                            cp: 0x754c,
                            width: 2,
                        },
                    ];
                    a.print_run(y, x, &run, &attrs);
                    b.print_run(y, x, &run, &attrs);
                }
                1 => {
                    a.erase_cells(y, x, 20, Color::Palette(2), false);
                    b.erase_cells(y, x, 20, Color::Palette(2), false);
                }
                _ => {
                    let text: Vec<u8> = (0..n).map(|k| 0x20 + ((k * 7 + y) % 95) as u8).collect();
                    let run: Vec<Glyph> = text
                        .iter()
                        .map(|&c| Glyph {
                            cp: u32::from(c),
                            width: 1,
                        })
                        .collect();
                    let ra = a.print_ascii(y, x, &text, &attrs);
                    let rb = b.print_run(y, x, &run, &attrs);
                    assert_eq!(ra, rb);
                }
            }
        }
        assert_eq!(dump(&a), dump(&b));
        a.check_invariants().unwrap();
        b.check_invariants().unwrap();
    }
}

#[test]
fn in_place_growth_equals_copy_growth() {
    let spec = PageSpec::with_rows(12, 8);
    for which in [
        Full::Styles,
        Full::Chunks,
        Full::Links,
        Full::LinkMap,
        Full::Strings,
    ] {
        let mut p = Page::new(spec, 7);
        for _ in 0..4 {
            p.push_row();
        }
        let sid = p.style_intern(&style_of(3), 0).unwrap();
        let sid2 = p.style_intern(&style_of(4), 0).unwrap();
        let l = p.link_intern("a", "https://example.org/x", 0).unwrap();
        p.link_reserve(2).unwrap();
        p.style_acquire(sid, 1);
        p.link_acquire(l, 1);
        p.put(0, 1, Cell::scalar('x', sid).with_link(true), Some(l));
        p.style_acquire(sid2, 1);
        p.put(1, 2, Cell::scalar('y', sid2), None);
        let head = p.cluster_alloc(&['e', '\u{301}']).unwrap();
        p.put(2, 3, Cell::cluster(head, 0), None);
        let copied = p.grown_with(which, &Ceil::MAX).unwrap();
        let mut inplace = Page::from_bytes(p.as_bytes().to_vec()).unwrap();
        assert!(inplace.grow_in_place(which, &Ceil::MAX));
        assert_eq!(copied.as_bytes(), inplace.as_bytes(), "{which:?}");
        inplace.audit().unwrap();
        // The page still works afterwards.
        let sid3 = inplace.style_intern(&style_of(9), 1).unwrap();
        assert_eq!(inplace.style_refs(sid3), 1);
        let none = Ceil {
            styles: 1,
            chunks: 1,
            links: 1,
            lmap: 1,
            strs: 1,
        };
        assert!(!inplace.grow_in_place(which, &none));
    }
}

#[test]
fn recycled_pages_open_clean_rows() {
    let mut s = screen();
    let attrs = CellAttrs {
        style: style_of(1),
        ..CellAttrs::default()
    };
    // Enough output to evict pages and reuse recycled buffers, with every cell written.
    let line: Vec<u8> = (0..20).map(|k| b'a' + k).collect();
    for round in 0..20_000 {
        for y in 0..6 {
            s.print_ascii(y, 0, &line, &attrs);
        }
        s.scroll_up(Region::full(6, 20), 1, Color::Default);
        if round % 4000 == 0 {
            s.check_invariants().unwrap();
        }
    }
    assert!(s.stats().pool_hits > 0, "{:?}", s.stats());
    // A freshly opened bottom row is blank even though its buffer was used before.
    let mut row = SnapRow::default();
    s.copy_row(5, &mut row);
    assert!(row.cells.iter().all(|c| c.cp == 0));
}
