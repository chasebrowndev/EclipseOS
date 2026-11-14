// SPDX-License-Identifier: AGPL-3.0-only
//! The T-23 §2 budgets table as data: every row of the spec table, one `Budget` per limit
//! (a row with two limits, or a relative target, has `/p50`, `/REL`, ... sub-ids). Absolute values are the *provisional* column; M17
//! replaces them with measured peer baselines once the owner signs off (T-23 §7).
//!
//! Headless budgets are asserted by [`check`], which does nothing unless `APOTH_PERF=1`
//! (shared CI runners are too noisy to gate on absolute numbers; T-23 §6 pins a runner
//! class for that). End-to-end rows are listed for completeness with `headless: false`.

use super::Metric;

/// Direction of a bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    /// Measured value must be at least the limit.
    AtLeast,
    /// Measured value must be at most the limit.
    AtMost,
}

/// One budget row.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// T-23 id.
    pub id: &'static str,
    /// Bound direction.
    pub bound: Bound,
    /// Provisional absolute limit; NaN for a row that has none (relative only, recorded at
    /// M17 and compared by `regressions`).
    pub limit: f64,
    /// Unit of `limit`; must equal the metric's unit.
    pub unit: &'static str,
    /// Measured by the headless harness (else e2e, listed only).
    pub headless: bool,
    /// Milestone that gates it.
    pub gate: &'static str,
}

/// The table. Ids not produced by [`super::bench::run_all`] are e2e rows.
pub const BUDGETS: &[Budget] = &[
    Budget {
        id: "THR-ASCII",
        bound: Bound::AtLeast,
        limit: 400.0,
        unit: "MiB/s",
        headless: true,
        gate: "M9,M13",
    },
    Budget {
        id: "THR-DENSE",
        bound: Bound::AtLeast,
        limit: 100.0,
        unit: "MiB/s",
        headless: true,
        gate: "M13",
    },
    Budget {
        id: "THR-CSI",
        bound: Bound::AtLeast,
        limit: 120.0,
        unit: "MiB/s",
        headless: true,
        gate: "M13",
    },
    Budget {
        id: "RESIZE",
        bound: Bound::AtMost,
        limit: 50.0,
        unit: "ms",
        headless: true,
        gate: "M12",
    },
    // MEM-LINE hot <= 700 B (cold <= 200 B needs the compressor, M12; not measurable yet).
    Budget {
        id: "MEM-LINE",
        bound: Bound::AtMost,
        limit: 700.0,
        unit: "B",
        headless: true,
        gate: "M12",
    },
    // T-23 §3 rule 2: full-screen snapshot at 300x80 under the lock, <= 100 us.
    Budget {
        id: "SNAPSHOT-300x80",
        bound: Bound::AtMost,
        limit: 100.0,
        unit: "us",
        headless: true,
        gate: "M11",
    },
    Budget {
        id: "LAT-KEY",
        bound: Bound::AtMost,
        limit: 0.5,
        unit: "ms",
        headless: false,
        gate: "M11",
    },
    Budget {
        id: "LAT-ECHO",
        bound: Bound::AtMost,
        limit: 8.0,
        unit: "ms",
        headless: false,
        gate: "M17,M31",
    },
    Budget {
        id: "START-COLD",
        bound: Bound::AtMost,
        limit: 120.0,
        unit: "ms",
        headless: false,
        gate: "M17,M31",
    },
    Budget {
        id: "START-WARM",
        bound: Bound::AtMost,
        limit: 25.0,
        unit: "ms",
        headless: false,
        gate: "M17,M31",
    },
    Budget {
        id: "MEM-BASE",
        bound: Bound::AtMost,
        limit: 70.0 * 1024.0 * 1024.0,
        unit: "B",
        headless: false,
        gate: "M17,M31",
    },
    Budget {
        id: "MEM-PANE",
        bound: Bound::AtMost,
        limit: 3.0 * 1024.0 * 1024.0,
        unit: "B",
        headless: false,
        gate: "M22",
    },
    // Relative: >= 1.0x alacritty_terminal's Term on the same corpus (vte is informative).
    Budget {
        id: "THR-ASCII/REL",
        bound: Bound::AtLeast,
        limit: 1.0,
        unit: "x",
        headless: true,
        gate: "M9,M13",
    },
    Budget {
        id: "THR-DENSE/REL",
        bound: Bound::AtLeast,
        limit: 1.0,
        unit: "x",
        headless: true,
        gate: "M13",
    },
    Budget {
        id: "THR-CSI/REL",
        bound: Bound::AtLeast,
        limit: 1.0,
        unit: "x",
        headless: true,
        gate: "M13",
    },
    // p50 <= 4 ms; LAT-ECHO itself is the p99 <= 8 ms row.
    Budget {
        id: "LAT-ECHO/p50",
        bound: Bound::AtMost,
        limit: 4.0,
        unit: "ms",
        headless: false,
        gate: "M17,M31",
    },
    // p50 <= one refresh interval + 4 ms: the value is the excess over one refresh interval.
    Budget {
        id: "LAT-PHOTON",
        bound: Bound::AtMost,
        limit: 4.0,
        unit: "ms",
        headless: false,
        gate: "M31",
    },
    // vtebench: no benchmark slower than 1.15x best peer (geometric mean <= 1.0x is a second row).
    Budget {
        id: "THR-E2E/REL",
        bound: Bound::AtMost,
        limit: 1.15,
        unit: "x",
        headless: false,
        gate: "M17,M31",
    },
    Budget {
        id: "THR-E2E/GEO",
        bound: Bound::AtMost,
        limit: 1.0,
        unit: "x",
        headless: false,
        gate: "M17,M31",
    },
    // <= 1.1x foot's wall time for 1 M lines through a 100 k history.
    Budget {
        id: "THR-SCROLL/REL",
        bound: Bound::AtMost,
        limit: 1.1,
        unit: "x",
        headless: false,
        gate: "M17",
    },
    // CPU time over 60 s with a static screen (0 frames, 0 wakeups).
    Budget {
        id: "IDLE",
        bound: Bound::AtMost,
        limit: 10.0,
        unit: "ms",
        headless: false,
        gate: "M11,M31",
    },
    // As IDLE with a11y active and no AT client.
    Budget {
        id: "IDLE-A11Y",
        bound: Bound::AtMost,
        limit: 10.0,
        unit: "ms",
        headless: false,
        gate: "M11",
    },
    // Of one core, blinking cursor only (2 frames/s).
    Budget {
        id: "IDLE-BLINK",
        bound: Bound::AtMost,
        limit: 0.2,
        unit: "%",
        headless: false,
        gate: "M17",
    },
    // <= foot + 20 % CPU (and GPU where the driver reports it).
    Budget {
        id: "PWR-SCROLL/REL",
        bound: Bound::AtMost,
        limit: 1.2,
        unit: "x",
        headless: false,
        gate: "M31",
    },
    // Compressed cold pages; needs the M12 compressor, so no metric is produced yet.
    Budget {
        id: "MEM-LINE/cold",
        bound: Bound::AtMost,
        limit: 200.0,
        unit: "B",
        headless: true,
        gate: "M12",
    },
    // Regex over 1 M lines: first screenful.
    Budget {
        id: "SEARCH/first",
        bound: Bound::AtMost,
        limit: 50.0,
        unit: "ms",
        headless: true,
        gate: "M23",
    },
    // Full pass; the UI never blocks.
    Budget {
        id: "SEARCH/full",
        bound: Bound::AtMost,
        limit: 1500.0,
        unit: "ms",
        headless: true,
        gate: "M23",
    },
    // 4K PNG via kitty direct transmission to first display (<= kitty).
    Budget {
        id: "IMG-LOAD",
        bound: Bound::AtMost,
        limit: 150.0,
        unit: "ms",
        headless: false,
        gate: "M25",
    },
    // mpv --vo=kitty 1080p30 over shm: dropped frames.
    Budget {
        id: "VID-MPV",
        bound: Bound::AtMost,
        limit: 1.0,
        unit: "%",
        headless: false,
        gate: "M32",
    },
    // apoth play: dropped frames.
    Budget {
        id: "VID-NATIVE",
        bound: Bound::AtMost,
        limit: 1.0,
        unit: "%",
        headless: false,
        gate: "M34,M36",
    },
    // A/V skew.
    Budget {
        id: "VID-NATIVE/skew",
        bound: Bound::AtMost,
        limit: 40.0,
        unit: "ms",
        headless: false,
        gate: "M34,M36",
    },
    // Added frame cost of the glass interface at 4K with the tab strip visible.
    Budget {
        id: "GLASS/GPU",
        bound: Bound::AtMost,
        limit: 0.3,
        unit: "ms",
        headless: false,
        gate: "M14,M19",
    },
    Budget {
        id: "GLASS/CPU",
        bound: Bound::AtMost,
        limit: 0.2,
        unit: "ms",
        headless: false,
        gate: "M14,M19",
    },
];

/// True when `APOTH_PERF=1`: absolute budgets are enforced.
#[must_use]
pub fn enforced() -> bool {
    std::env::var("APOTH_PERF").is_ok_and(|v| v == "1")
}

/// Budget violations in `metrics` (empty = all met). Metrics without a budget row, and
/// budget rows without a metric, are ignored; a unit mismatch is itself a violation.
#[must_use]
pub fn violations(metrics: &[Metric]) -> Vec<String> {
    let mut v = Vec::new();
    for m in metrics {
        let Some(b) = BUDGETS.iter().find(|b| b.id == m.id) else {
            continue;
        };
        if b.limit.is_nan() {
            continue;
        }
        if b.unit != m.unit {
            v.push(format!(
                "{}: unit {} != budget unit {}",
                m.id, m.unit, b.unit
            ));
            continue;
        }
        let ok = match b.bound {
            Bound::AtLeast => m.value >= b.limit,
            Bound::AtMost => m.value <= b.limit,
        };
        if !ok {
            let op = if b.bound == Bound::AtLeast {
                ">="
            } else {
                "<="
            };
            v.push(format!(
                "{}: {:.2} {} violates {op} {} (gate {})",
                m.id, m.value, m.unit, b.limit, b.gate
            ));
        }
    }
    v
}

/// Enforce the budgets: no-op unless `APOTH_PERF=1`, else panics listing violations.
pub fn check(metrics: &[Metric]) {
    if !enforced() {
        return;
    }
    let v = violations(metrics);
    assert!(v.is_empty(), "perf budgets violated:\n{}", v.join("\n"));
}
