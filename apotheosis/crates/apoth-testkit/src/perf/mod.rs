// SPDX-License-Identifier: AGPL-3.0-only
//! Performance harness (T-23 §1, §2, §6): deterministic corpora, headless benches, the
//! budgets table, regression tolerances and the key-to-commit latency hook.
//!
//! * [`corpus`]: seeded generators; corpora are identified by FNV-1a hash, never stored.
//! * [`bench`]: throughput, reflow, snapshot and memory measurements over `apoth-term`.
//! * [`budgets`]: the T-23 §2 table as data, and the `APOTH_PERF=1` gate.
//! * [`peers`]: `alacritty_terminal` and `vte` on the same corpus, for the relative targets.
//! * [`latency`]: the interface the binary calls to record key-to-commit latency.
//!
//! Nothing here logs or stores pane content; results are sizes, counts and durations.

pub mod bench;
pub mod budgets;
pub mod corpus;
pub mod latency;
pub mod peers;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

/// Tolerance class of a metric (T-23 §1): a result worse than the previous accepted
/// baseline by more than the class tolerance is a regression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Headless throughput median: 5 %.
    HeadlessThroughput,
    /// End-to-end median: 10 %.
    E2eMedian,
    /// End-to-end p99 latency: 15 %.
    E2eP99,
    /// Counts (frames, wakeups, dropped frames): exact.
    Count,
}

impl Kind {
    /// Allowed relative worsening.
    #[must_use]
    pub fn tolerance(self) -> f64 {
        match self {
            Kind::HeadlessThroughput => 0.05,
            Kind::E2eMedian => 0.10,
            Kind::E2eP99 => 0.15,
            Kind::Count => 0.0,
        }
    }
}

/// One measured value.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    /// Budget id (`THR-ASCII`, ...) or a free `id/variant` name for informative lines.
    pub id: String,
    /// Value in `unit`.
    pub value: f64,
    /// `MiB/s`, `ms`, `us`, `B`, ...
    pub unit: &'static str,
    /// Tolerance class.
    pub kind: Kind,
    /// True when a larger value is better (throughput), false for time and memory.
    pub higher_is_better: bool,
}

/// Compare `current` against `baseline`; returns the ids that regressed beyond tolerance.
/// Metrics absent from the baseline are new and never regress.
#[must_use]
pub fn regressions(baseline: &[Metric], current: &[Metric]) -> Vec<String> {
    let mut out = Vec::new();
    for c in current {
        let Some(b) = baseline.iter().find(|b| b.id == c.id) else {
            continue;
        };
        let tol = c.kind.tolerance();
        let worse = if c.higher_is_better {
            c.value < b.value * (1.0 - tol)
        } else {
            c.value > b.value * (1.0 + tol)
        };
        if worse {
            out.push(c.id.clone());
        }
    }
    out
}

/// Parse the output of [`to_json`] back into metrics (id, value, unit); `kind` and direction
/// come from the budget table when the id has a row, else throughput-like defaults by unit.
#[must_use]
pub fn from_json(text: &str) -> Vec<Metric> {
    fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let rest = &line[line.find(key)? + key.len()..];
        let rest = rest.trim_start_matches([':', ' ']);
        if let Some(q) = rest.strip_prefix('"') {
            q.split('"').next()
        } else {
            rest.split([',', '}']).next().map(str::trim)
        }
    }
    let mut out = Vec::new();
    for line in text.lines().filter(|l| l.contains("\"id\"")) {
        let (Some(id), Some(v), Some(u)) = (
            field(line, "\"id\""),
            field(line, "\"value\""),
            field(line, "\"unit\""),
        ) else {
            continue;
        };
        let (Ok(value), Some(unit)) = (
            v.parse::<f64>(),
            ["MiB/s", "ms", "us", "B", "KiB", "x"]
                .into_iter()
                .find(|k| *k == u),
        ) else {
            continue;
        };
        let higher_is_better = matches!(unit, "MiB/s" | "x");
        let kind = if unit == "B" || unit == "KiB" {
            Kind::Count
        } else {
            Kind::HeadlessThroughput
        };
        out.push(Metric {
            id: id.to_string(),
            value,
            unit,
            kind,
            higher_is_better,
        });
    }
    out
}

/// Warm up `warmup` runs (discarded), then return the median of `runs` timings.
pub fn median_of(warmup: usize, runs: usize, mut f: impl FnMut()) -> Duration {
    for _ in 0..warmup {
        f();
    }
    let mut v: Vec<Duration> = (0..runs.max(1))
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    v.sort_unstable();
    v[v.len() / 2]
}

/// Results as JSON (one metric per line; written to `apotheosis/bench/results/`).
#[must_use]
pub fn to_json(label: &str, scale: f64, metrics: &[Metric]) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{{\n  \"label\": \"{label}\",\n  \"scale\": {scale},\n  \"metrics\": ["
    );
    for (i, m) in metrics.iter().enumerate() {
        let comma = if i + 1 < metrics.len() { "," } else { "" };
        let _ = writeln!(
            s,
            "    {{\"id\": \"{}\", \"value\": {:.3}, \"unit\": \"{}\"}}{comma}",
            m.id, m.value, m.unit
        );
    }
    s.push_str("  ]\n}\n");
    s
}
