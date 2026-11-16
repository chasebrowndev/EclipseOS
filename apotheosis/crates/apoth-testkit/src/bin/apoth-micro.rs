// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth-micro [scale]`: splits the headless throughput cost by layer (parser only, term
//! with and without scrollback) per corpus, plus grid-call and per-sequence costs. Dev tool;
//! prints MiB/s or ns, median of a few runs. `apoth-micro spin <corpus> <secs>` feeds one
//! corpus in a loop so a sampling debugger can be pointed at it (no `perf` on the dev host).

use std::io::Write;
use std::time::Instant;

use apoth_grid::Limits;
use apoth_term::{Term, TermConfig};
use apoth_testkit::perf::corpus::{self, Corpus};
use apoth_vt::{Parser, Sink, bench::CountSink};

const MIB: f64 = 1024.0 * 1024.0;

macro_rules! say {
    ($($a:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($a)*);
    }};
}

type Keep = fn(&[u8]) -> bool;

fn med(runs: usize, mut f: impl FnMut()) -> f64 {
    let mut v: Vec<f64> = (0..runs)
        .map(|_| {
            let s = Instant::now();
            f();
            s.elapsed().as_secs_f64()
        })
        .collect();
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn term(hist: usize) -> Term {
    Term::new(TermConfig {
        rows: 40,
        cols: 120,
        scrollback: Limits {
            bytes: 1 << 30,
            lines: Some(hist),
        },
        ..TermConfig::default()
    })
}

fn run_vt<S: Sink>(data: &[u8], s: &mut S) {
    let mut p = Parser::new();
    for c in data.chunks(64 * 1024) {
        p.advance(c, s);
    }
}

fn grid_parts() {
    use apoth_grid::{CellAttrs, Color, Glyph, Region, Screen, ScreenConfig};
    let cfg = ScreenConfig {
        history: true,
        limits: Limits {
            bytes: 1 << 30,
            lines: Some(10_000),
        },
    };
    let attrs = CellAttrs::default();
    let n = 1_000_000usize;
    for len in [1u32, 5, 20, 80] {
        let run: Vec<Glyph> = (0..len)
            .map(|i| Glyph {
                cp: 0x20 + i,
                width: 1,
            })
            .collect();
        let mut g = Screen::new(40, 120, cfg);
        let t = med(3, || {
            for i in 0..n {
                g.print_run(i % 40, 0, &run, &attrs);
            }
        });
        say!("grid print_run({len}): {:.0} ns/call", t * 1e9 / n as f64);
    }
    let mut g = Screen::new(40, 120, cfg);
    let t = med(3, || {
        for i in 0..n {
            g.erase_cells(i % 40, 10, 120, Color::Default, false);
        }
    });
    say!("grid erase_cells: {:.0} ns/call", t * 1e9 / n as f64);
    let t = med(3, || {
        for i in 0..n {
            g.set_row_flag(i % 40, 1, false);
        }
    });
    say!("grid set_row_flag: {:.0} ns/call", t * 1e9 / n as f64);
    let mut g = Screen::new(40, 120, cfg);
    let r = Region::full(40, 120);
    let t = med(3, || {
        for _ in 0..n {
            g.scroll_up(r, 1, Color::Default);
        }
    });
    say!("grid scroll_up: {:.0} ns/call", t * 1e9 / n as f64);
}

fn synth() {
    let now = Instant::now();
    let cases: [(&str, Vec<u8>); 10] = [
        ("cup", b"\x1b[12;34H".repeat(1 << 21)),
        ("el", b"\x1b[0K".repeat(1 << 21)),
        ("sgr-rgb", b"\x1b[38;2;12;200;40m".repeat(1 << 20)),
        ("sgr-colon", b"\x1b[48:2::12:200:40m".repeat(1 << 20)),
        ("sgr-1;31", b"\x1b[1;31m".repeat(1 << 21)),
        ("sgr0", b"\x1b[0m".repeat(1 << 21)),
        ("lf", b"\n".repeat(1 << 22)),
        (
            "el0-bg",
            [&b"\x1b[48;2;9;9;9m"[..], &b"\x1b[0K".repeat(1 << 21)].concat(),
        ),
        (
            "el2-bg",
            [&b"\x1b[48;2;9;9;9m"[..], &b"\x1b[2K".repeat(1 << 21)].concat(),
        ),
        (
            "el1-bg",
            [
                &b"\x1b[48;2;9;9;9m\x1b[1;100H"[..],
                &b"\x1b[1K".repeat(1 << 21),
            ]
            .concat(),
        ),
    ];
    for (name, d) in &cases {
        let mb = d.len() as f64 / MIB;
        let t = med(3, || {
            let mut t = term(10_000);
            for ch in d.chunks(64 * 1024) {
                let _ = t.feed(ch, now);
            }
        });
        let v = med(3, || run_vt(d, &mut CountSink::default()));
        say!("{name:10} term {:6.0} MiB/s  vt {:6.0}", mb / t, mb / v);
    }
}

fn drop_kinds() {
    let now = Instant::now();
    let data = corpus::generate(Corpus::CsiRedraw, 16 << 20);
    let seqs: Vec<&[u8]> = data
        .split_inclusive(|&b| matches!(b, b'H' | b'K' | b'm'))
        .collect();
    let kinds: [(&str, Keep); 5] = [
        ("all", |_| true),
        ("no-H", |s| !s.ends_with(b"H")),
        ("no-K", |s| !s.ends_with(b"K")),
        ("no-38", |s| !s.starts_with(b"\x1b[38")),
        ("no-48", |s| !s.starts_with(b"\x1b[48")),
    ];
    for (name, keep) in kinds {
        let d: Vec<u8> = seqs
            .iter()
            .filter(|s| keep(s))
            .flat_map(|s| s.iter().copied())
            .collect();
        let mb = d.len() as f64 / MIB;
        let secs = med(3, || {
            let mut t = term(10_000);
            for ch in d.chunks(64 * 1024) {
                let _ = t.feed(ch, now);
            }
        });
        say!(
            "csi {name:6} {:6.0} MiB/s ({} seq)",
            mb / secs,
            seqs.iter().filter(|s| keep(s)).count()
        );
    }
}

fn spin(name: &str, secs: u64) {
    let now = Instant::now();
    let c = Corpus::from_name(name).unwrap_or(Corpus::CatLarge);
    let data = corpus::generate(c, 16 << 20);
    let end = Instant::now() + std::time::Duration::from_secs(secs);
    while Instant::now() < end {
        let mut t = term(10_000);
        for ch in data.chunks(64 * 1024) {
            let _ = t.feed(ch, now);
        }
    }
}

fn stats() {
    let now = Instant::now();
    for c in [Corpus::Dense, Corpus::CatLarge] {
        let data = corpus::generate(c, 16 << 20);
        let mut t = term(10_000);
        for ch in data.chunks(64 * 1024) {
            let _ = t.feed(ch, now);
        }
        let g = t.primary();
        say!(
            "{} stats {:?} pages {}",
            c.name(),
            g.stats(),
            g.page_count()
        );
    }
}

fn chars() {
    let now = Instant::now();
    let line = |c: &str, n: usize| format!("{}\r\n", c.repeat(n)).into_bytes();
    for (name, l) in [
        ("a", line("a", 100)),
        ("e-acute", line("\u{e9}", 100)),
        ("cjk", line("\u{4e16}", 50)),
    ] {
        let d: Vec<u8> = l.repeat(40_000);
        let chars = 100.0 * 40_000.0 * if name == "cjk" { 0.5 } else { 1.0 };
        let secs = med(3, || {
            let mut t = term(10_000);
            for ch in d.chunks(64 * 1024) {
                let _ = t.feed(ch, now);
            }
        });
        say!("char {name:8} {:5.1} ns/char", secs * 1e9 / chars);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.get(1).map(String::as_str) == Some("spin") {
        spin(&a[2], a[3].parse().unwrap_or(10));
        return;
    }
    chars();
    stats();
    drop_kinds();
    synth();
    grid_parts();
    let scale: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.25);
    let bytes = (64.0 * MIB * scale) as usize;
    let now = Instant::now();
    for c in Corpus::ALL {
        let data = corpus::generate(c, bytes);
        let mb = data.len() as f64 / MIB;
        let vt = med(5, || run_vt(&data, &mut CountSink::default()));
        let t0 = med(5, || {
            let mut t = term(0);
            for ch in data.chunks(64 * 1024) {
                let _ = t.feed(ch, now);
                let _ = t.replies().take_all();
                let _ = t.events();
            }
        });
        let t1 = med(5, || {
            let mut t = term(10_000);
            for ch in data.chunks(64 * 1024) {
                let _ = t.feed(ch, now);
                let _ = t.replies().take_all();
                let _ = t.events();
            }
        });
        say!(
            "{:12} vt {:7.0}  term(hist0) {:6.0}  term(hist10k) {:6.0} MiB/s",
            c.name(),
            mb / vt,
            mb / t0,
            mb / t1
        );
    }
}
