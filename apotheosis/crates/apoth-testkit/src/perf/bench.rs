// SPDX-License-Identifier: AGPL-3.0-only
//! Headless benches over `apoth-term` (T-23 §1 "headless" layer): no PTY, no renderer.

use std::time::Instant;

use apoth_grid::{Limits, SnapRow};
use apoth_term::{Term, TermConfig};

use super::corpus::{self, Corpus};
use super::{Kind, Metric, median_of, peers};

/// Chunk size fed per call: the io thread's lock-yield bound (T-23 §3 rule 7).
pub const CHUNK: usize = 64 * 1024;

fn new_term(rows: usize, cols: usize, history: usize) -> Term {
    Term::new(TermConfig {
        rows,
        cols,
        scrollback: Limits {
            bytes: 1 << 30,
            lines: Some(history),
        },
        ..TermConfig::default()
    })
}

/// Feed `data` in [`CHUNK`]s and drain replies/events as the io thread would.
pub fn feed_all(term: &mut Term, data: &[u8], now: Instant) {
    for chunk in data.chunks(CHUNK) {
        let _ = term.feed(chunk, now);
        let _ = term.replies().take_all();
        let _ = term.events();
    }
}

/// Throughput of one corpus in MiB/s: median of `runs` fresh-terminal feeds.
#[must_use]
pub fn throughput(data: &[u8], rows: usize, cols: usize, runs: usize) -> f64 {
    let now = Instant::now();
    let d = median_of(1, runs, || {
        let mut t = new_term(rows, cols, 10_000);
        feed_all(&mut t, data, now);
    });
    data.len() as f64 / d.as_secs_f64().max(1e-9) / (1024.0 * 1024.0)
}

/// Reflow cost in milliseconds: width change of a terminal holding `lines` history lines.
#[must_use]
pub fn reflow_ms(lines: usize, runs: usize) -> f64 {
    let data = corpus::lines(lines, 78);
    let now = Instant::now();
    let mut samples = Vec::new();
    for _ in 0..runs.max(1) {
        let mut t = new_term(40, 80, lines + 100);
        feed_all(&mut t, &data, now);
        let s = Instant::now();
        let _ = t.resize(40, 100);
        samples.push(s.elapsed().as_secs_f64() * 1e3);
    }
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

/// Full-screen snapshot cost in microseconds at `rows` x `cols` (`copy_row` of every row).
#[must_use]
pub fn snapshot_us(rows: usize, cols: usize, runs: usize) -> f64 {
    let mut t = new_term(rows, cols, 1000);
    feed_all(
        &mut t,
        &corpus::lines(rows * 2, cols.saturating_sub(2)),
        Instant::now(),
    );
    let mut scratch = SnapRow::default();
    let g = t.grid();
    let d = median_of(2, runs.max(5), || {
        for y in 0..rows {
            g.copy_row(y, &mut scratch);
        }
    });
    d.as_secs_f64() * 1e6
}

/// Logical bytes per 80-column plain-text line (page bytes divided by all lines held).
#[must_use]
pub fn bytes_per_line(lines: usize) -> f64 {
    let mut t = new_term(40, 80, lines + 100);
    feed_all(&mut t, &corpus::lines(lines, 78), Instant::now());
    let g = t.primary();
    g.bytes_in_use() as f64 / (g.history_rows() + g.rows()).max(1) as f64
}

/// Memory for `lines` of history in KiB per 100 k rows (the M17 "memory per 100k rows").
#[must_use]
pub fn kib_per_100k_rows(lines: usize) -> f64 {
    bytes_per_line(lines) * 100_000.0 / 1024.0
}

fn m(id: String, value: f64, unit: &'static str, kind: Kind, hib: bool) -> Metric {
    Metric {
        id,
        value,
        unit,
        kind,
        higher_is_better: hib,
    }
}

/// Run every headless bench at `scale` (1.0 = budget size: 64 MiB streams, 100 k lines).
/// Tiny scales exercise the same code in normal CI.
#[must_use]
pub fn run_all(scale: f64, runs: usize) -> Vec<Metric> {
    let bytes = ((64.0 * 1024.0 * 1024.0) * scale) as usize;
    let lines = (100_000.0 * scale) as usize;
    let mut out = Vec::new();
    for c in Corpus::ALL {
        let data = corpus::generate(c, bytes.max(1024));
        let id = match c {
            Corpus::CatLarge => "THR-ASCII".to_string(),
            Corpus::Dense => "THR-DENSE".to_string(),
            Corpus::CsiRedraw => "THR-CSI".to_string(),
            other => format!("THR/{}", other.name()),
        };
        let ours = throughput(&data, 40, 120, runs);
        out.push(m(id.clone(), ours, "MiB/s", Kind::HeadlessThroughput, true));
        // T-23 §2: THR-ASCII/DENSE/CSI are relative to `alacritty_terminal` on the same corpus;
        // `vte` is the informative parser-only line.
        if matches!(c, Corpus::CatLarge | Corpus::Dense | Corpus::CsiRedraw) {
            let peer = peers::alacritty_throughput(&data, 40, 120, runs);
            out.push(m(
                format!("{id}/alacritty"),
                peer,
                "MiB/s",
                Kind::HeadlessThroughput,
                true,
            ));
            out.push(m(
                format!("{id}/REL"),
                ours / peer.max(1e-9),
                "x",
                Kind::HeadlessThroughput,
                true,
            ));
            out.push(m(
                format!("{id}/vte"),
                peers::vte_throughput(&data, runs),
                "MiB/s",
                Kind::HeadlessThroughput,
                true,
            ));
        }
    }
    out.push(m(
        "RESIZE".into(),
        reflow_ms(lines.max(100), runs),
        "ms",
        Kind::HeadlessThroughput,
        false,
    ));
    out.push(m(
        "SNAPSHOT-300x80".into(),
        snapshot_us(80, 300, runs),
        "us",
        Kind::HeadlessThroughput,
        false,
    ));
    let per_line = bytes_per_line(lines.max(100));
    out.push(m("MEM-LINE".into(), per_line, "B", Kind::Count, false));
    out.push(m(
        "MEM-100K-ROWS".into(),
        per_line * 100_000.0 / 1024.0,
        "KiB",
        Kind::Count,
        false,
    ));
    out
}
