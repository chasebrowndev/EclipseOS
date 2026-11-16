// SPDX-License-Identifier: AGPL-3.0-only
//! Perf harness tests: tiny inputs run in normal CI; absolute budgets only with APOTH_PERF=1.

use apoth_testkit::perf::budgets::{self, BUDGETS};
use apoth_testkit::perf::corpus::{self, Corpus};
use apoth_testkit::perf::latency::LatencyProbe;
use apoth_testkit::perf::{Kind, Metric, bench, regressions, to_json};

fn metric(id: &str, value: f64, unit: &'static str, kind: Kind, hib: bool) -> Metric {
    Metric {
        id: id.into(),
        value,
        unit,
        kind,
        higher_is_better: hib,
    }
}

#[test]
fn corpora_are_deterministic_and_distinct() {
    let mut hashes = Vec::new();
    for c in Corpus::ALL {
        let a = corpus::generate(c, 8192);
        assert!(a.len() >= 8192, "{}", c.name());
        assert_eq!(a, corpus::generate(c, 8192), "{}", c.name());
        assert_eq!(Corpus::from_name(c.name()), Some(c));
        hashes.push(corpus::hash(&a));
    }
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), Corpus::ALL.len());
}

#[test]
fn lines_have_exact_width() {
    let l = corpus::lines(5, 78);
    assert_eq!(
        l.split(|&b| b == b'\n').filter(|x| !x.is_empty()).count(),
        5
    );
    assert!(
        l.split(|&b| b == b'\n')
            .filter(|x| !x.is_empty())
            .all(|x| x.len() == 79)
    );
}

#[test]
fn harness_runs_on_tiny_inputs() {
    let m = bench::run_all(0.002, 1);
    for id in [
        "THR-ASCII",
        "THR-DENSE",
        "THR-CSI",
        "RESIZE",
        "SNAPSHOT-300x80",
        "MEM-LINE",
        "MEM-100K-ROWS",
    ] {
        let x = m
            .iter()
            .find(|x| x.id == id)
            .unwrap_or_else(|| panic!("missing {id}"));
        assert!(x.value.is_finite() && x.value > 0.0, "{id} = {}", x.value);
    }
    assert!(to_json("t", 0.002, &m).contains("\"THR-ASCII\""));
}

#[test]
fn budget_assertions_gated_by_env() {
    // Skipped unless APOTH_PERF=1 (stable CI); then full-size, budget-checked.
    if !budgets::enforced() {
        return;
    }
    budgets::check(&bench::run_all(1.0, 10));
}

#[test]
fn budget_table_is_sane() {
    let mut ids: Vec<_> = BUDGETS.iter().map(|b| b.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), BUDGETS.len());
    for want in [
        "THR-ASCII",
        "THR-DENSE",
        "THR-CSI",
        "RESIZE",
        "MEM-LINE",
        "LAT-KEY",
        "START-COLD",
        "MEM-BASE",
    ] {
        assert!(ids.contains(&want), "{want}");
    }
}

#[test]
fn violations_detected_both_directions_and_units() {
    let ok = [
        metric("THR-ASCII", 500.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("RESIZE", 10.0, "ms", Kind::HeadlessThroughput, false),
    ];
    assert!(budgets::violations(&ok).is_empty());
    let bad = [
        metric("THR-ASCII", 10.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("RESIZE", 90.0, "ms", Kind::HeadlessThroughput, false),
        metric("MEM-LINE", 1.0, "KiB", Kind::Count, false),
    ];
    assert_eq!(budgets::violations(&bad).len(), 3);
}

#[test]
fn regression_tolerances() {
    let base = [
        metric("a", 100.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("b", 10.0, "ms", Kind::E2eP99, false),
        metric("c", 5.0, "n", Kind::Count, false),
    ];
    let cur = [
        metric("a", 96.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("b", 11.4, "ms", Kind::E2eP99, false),
        metric("c", 5.0, "n", Kind::Count, false),
        metric("new", 1.0, "n", Kind::Count, false),
    ];
    assert!(regressions(&base, &cur).is_empty());
    let worse = [
        metric("a", 94.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("b", 11.6, "ms", Kind::E2eP99, false),
        metric("c", 6.0, "n", Kind::Count, false),
    ];
    assert_eq!(regressions(&base, &worse), ["a", "b", "c"]);
}

#[test]
fn latency_probe_percentiles_and_metrics() {
    let mut p = LatencyProbe::new();
    for i in 0..100u64 {
        let k = p.key(1_000_000 * i);
        p.pty_written(k, 1_000_000 * i + (i + 1) * 1000);
        p.pty_written(k, 1_000_000 * i + 999_999_999); // ignored: first write wins
        p.presented(k, 1_000_000 * i + 3_000_000);
    }
    let s = p.key_summary().unwrap();
    assert_eq!((s.n, s.p50_ns, s.p99_ns), (100, 50_000, 99_000));
    let m = p.metrics();
    assert_eq!(m.len(), 3);
    assert!(budgets::violations(&m).is_empty()); // 0.099 ms, 3 ms p99, 3 ms p50
    assert!(LatencyProbe::new().metrics().is_empty());
}

#[test]
fn latency_probe_ring_overwrites_old_keys() {
    let mut p = LatencyProbe::new();
    let first = p.key(0);
    for i in 0..apoth_testkit::perf::latency::CAPACITY as u64 {
        p.key(i);
    }
    p.pty_written(first, 5); // slot reused: ignored, no panic
    assert!(p.key_summary().is_none());
}

#[test]
fn json_round_trips_and_baseline_regressions_are_found() {
    let base = [
        metric("THR-ASCII", 500.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("RESIZE", 10.0, "ms", Kind::HeadlessThroughput, false),
    ];
    let back = apoth_testkit::perf::from_json(&to_json("t", 1.0, &base));
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].id, "THR-ASCII");
    assert!((back[1].value - 10.0).abs() < 1e-9 && !back[1].higher_is_better);
    assert!(regressions(&back, &base).is_empty());
    let worse = [
        metric("THR-ASCII", 400.0, "MiB/s", Kind::HeadlessThroughput, true),
        metric("RESIZE", 10.2, "ms", Kind::HeadlessThroughput, false),
    ];
    assert_eq!(regressions(&back, &worse), ["THR-ASCII"]);
}

#[test]
fn relative_rows_exist_and_are_reported() {
    let m = bench::run_all(0.002, 1);
    for id in [
        "THR-ASCII/REL",
        "THR-DENSE/REL",
        "THR-CSI/REL",
        "THR-ASCII/alacritty",
        "THR-ASCII/vte",
    ] {
        assert!(m.iter().any(|x| x.id == id && x.value > 0.0), "{id}");
    }
    // The table covers every row of T-23 §2.
    for want in [
        "GLASS/GPU",
        "GLASS/CPU",
        "IDLE",
        "IDLE-BLINK",
        "IDLE-A11Y",
        "THR-E2E/REL",
        "THR-SCROLL/REL",
        "PWR-SCROLL/REL",
        "LAT-PHOTON",
        "IMG-LOAD",
        "VID-MPV",
        "VID-NATIVE",
        "SEARCH/first",
        "MEM-LINE/cold",
        "LAT-ECHO/p50",
    ] {
        assert!(BUDGETS.iter().any(|b| b.id == want), "{want}");
    }
}
