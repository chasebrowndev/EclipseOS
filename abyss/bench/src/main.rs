// SPDX-License-Identifier: AGPL-3.0-only

//! The benchmark runner (COMP-14 §4.1, §5).
//!
//! Exits non-zero if any bench fails its budget or allocates on a path where
//! root invariant 11 forbids it, so CI can gate on it directly.
//!
//! COMP-14 §4.1 names five microbenchmarks: `check()`, damage merge, tree
//! serialize, scope match, audit encode. None of their subjects exist yet —
//! `policy/`, `audit/` and the semantic tree are Phase 2 milestones 16, 12 and
//! 22. Each one lands with the milestone that creates it; this crate is the
//! harness they plug into, and milestone 16 is the first customer (its gate is
//! `check()` at ≤50 µs p99, measured here).
//!
//! What runs today is the harness measuring itself, which is not filler: the
//! tightest budget in the table is 20 µs, and a timer whose own overhead were
//! anywhere near that would quietly invalidate every number this crate ever
//! prints.

use std::process::ExitCode;
use std::time::Duration;

use abyss_bench::alloc::Counting;
use abyss_bench::budgets::Budget;
use abyss_bench::{measure, Report, DEFAULT_ITERS};

/// Installed process-wide so `Report::forbid_alloc` has something to count.
/// Only this binary installs it; the library's own tests run under the default
/// allocator and assert shape, not counts.
#[global_allocator]
static ALLOC: Counting = Counting;

/// `Instant::now()` twice, round trip. Not a spec row — a floor on what this
/// harness can resolve. 1 µs is 1/20th of the tightest budget in COMP-14
/// §2.1b; above that, the harness is measuring itself more than its subject.
const TIMER_OVERHEAD: Budget = Budget {
    cite: "harness floor",
    what: "Instant::now() round trip",
    target: Duration::from_micros(1),
    hard_fail: None,
};

fn main() -> ExitCode {
    let mut report = Report::new();

    // The empty closure still pays for one `Instant::now()` pair per
    // iteration, which is exactly the quantity being bounded.
    report.push(measure("timer-overhead", TIMER_OVERHEAD, DEFAULT_ITERS, || {}));
    report.forbid_alloc("timer-overhead");

    if report.finish() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
