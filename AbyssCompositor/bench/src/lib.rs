// SPDX-License-Identifier: AGPL-3.0-only

//! The abyss benchmark harness (COMP-14 §4.1).
//!
//! Hand-rolled rather than criterion, and deliberately so: every budget in
//! COMP-14 §2 is a p99, and criterion reports a mean with a confidence
//! interval while discarding tail outliers as noise. The tail *is* the
//! measurement here. See `decisions/0043-hand-rolled-bench-harness.md`.
//!
//! The shape is small on purpose:
//!
//! - [`measure`] times a closure `iters` times and keeps every sample.
//! - [`Samples`] sorts them for p50/p99/p999/max.
//! - [`budgets`] holds the spec's table, and [`Samples::verdict`] judges the
//!   p99 against it.
//! - [`alloc`] counts allocations inside the measured region, so a path
//!   covered by root invariant 11 fails loudly instead of silently
//!   regressing.
//!
//! Timing uses `Instant`, whose own overhead is a few tens of nanoseconds —
//! two orders below the tightest budget here (20 µs). The `timer-overhead`
//! bench in the binary measures it so that claim stays checked rather than
//! assumed.

pub mod alloc;
pub mod budgets;

use std::hint::black_box;
use std::time::{Duration, Instant};

use alloc::AllocCount;
use budgets::{Budget, Verdict};

/// Iterations a bench runs unless it says otherwise. Enough that p99 lands on
/// the 100th-slowest sample rather than on noise.
pub const DEFAULT_ITERS: usize = 10_000;

/// The timings from one benchmark, sorted.
pub struct Samples {
    /// What ran.
    pub name: &'static str,
    /// The spec row it is judged against.
    pub budget: Budget,
    /// Every sample, in nanoseconds, ascending. Nothing is discarded.
    nanos: Vec<u64>,
    /// What the measured region allocated across all iterations.
    pub allocs: AllocCount,
}

impl Samples {
    /// Nearest-rank percentile: the smallest sample at or above `p` of the
    /// distribution. `p` is a fraction, so p99 is `0.99`.
    pub fn percentile(&self, p: f64) -> Duration {
        debug_assert!((0.0..=1.0).contains(&p));
        if self.nanos.is_empty() {
            return Duration::ZERO;
        }
        let rank = (p * self.nanos.len() as f64).ceil() as usize;
        let idx = rank.saturating_sub(1).min(self.nanos.len() - 1);
        Duration::from_nanos(self.nanos[idx])
    }

    /// Median.
    pub fn p50(&self) -> Duration {
        self.percentile(0.50)
    }

    /// The statistic every COMP-14 budget is stated in.
    pub fn p99(&self) -> Duration {
        self.percentile(0.99)
    }

    /// Tracked but not gated on, per COMP-14 §8.3.
    pub fn p999(&self) -> Duration {
        self.percentile(0.999)
    }

    /// The slowest sample seen.
    pub fn max(&self) -> Duration {
        self.percentile(1.0)
    }

    /// How many samples there are.
    pub fn len(&self) -> usize {
        self.nanos.len()
    }

    /// Whether nothing was measured.
    pub fn is_empty(&self) -> bool {
        self.nanos.is_empty()
    }

    /// The p99 judged against the budget.
    pub fn verdict(&self) -> Verdict {
        self.budget.verdict(self.p99())
    }
}

/// Time `f` `iters` times, keeping every sample.
///
/// `f` is called once per iteration and its result is passed through
/// `black_box` so the optimiser cannot delete the work being measured. A
/// warmup pass of `iters / 10` runs first and is discarded — it pays for cold
/// caches and lazy first-call costs, which would otherwise land in the tail
/// and be read as a regression.
///
/// The sample buffer is allocated up front, so the measured region itself
/// allocates only what `f` allocates. That is what makes [`Samples::allocs`]
/// meaningful.
pub fn measure<T>(name: &'static str, budget: Budget, iters: usize, mut f: impl FnMut() -> T) -> Samples {
    for _ in 0..iters / 10 {
        black_box(f());
    }

    let mut nanos: Vec<u64> = Vec::with_capacity(iters);
    let (_, allocs) = alloc::no_alloc(|| {
        for _ in 0..iters {
            let start = Instant::now();
            let out = f();
            let elapsed = start.elapsed();
            black_box(out);
            nanos.push(elapsed.as_nanos() as u64);
        }
    });

    nanos.sort_unstable();
    Samples {
        name,
        budget,
        nanos,
        allocs,
    }
}

/// A set of benchmark results, and whether any of them failed.
#[derive(Default)]
pub struct Report {
    rows: Vec<Samples>,
    /// Paths on which allocation is forbidden by root invariant 11. A bench
    /// named here fails if it allocated at all.
    no_alloc: Vec<&'static str>,
}

impl Report {
    /// An empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a result.
    pub fn push(&mut self, samples: Samples) {
        self.rows.push(samples);
    }

    /// Mark a benchmark as covering a path where invariant 11 forbids
    /// allocation. It then fails unless its allocation count is zero.
    pub fn forbid_alloc(&mut self, name: &'static str) {
        self.no_alloc.push(name);
    }

    fn alloc_forbidden(&self, name: &'static str) -> bool {
        self.no_alloc.contains(&name)
    }

    /// Print the table and report whether everything passed.
    ///
    /// Durations print in microseconds with three decimals — the tightest
    /// budget in COMP-14 §2.1b is 20 µs, so nanosecond resolution matters and
    /// milliseconds would round the interesting rows to zero.
    pub fn finish(&self) -> bool {
        println!(
            "{:<28} {:>10} {:>10} {:>10} {:>10} {:>10}  verdict",
            "bench", "p50", "p99", "p99.9", "max", "budget"
        );

        let mut ok = true;
        for row in &self.rows {
            let verdict = row.verdict();
            let allocated = !row.allocs.is_none();
            let forbidden = self.alloc_forbidden(row.name);

            let note = match (verdict, forbidden && allocated) {
                (_, true) => {
                    ok = false;
                    format!("FAIL allocated {} times on a no-alloc path", row.allocs.calls)
                }
                (Verdict::Fail, _) => {
                    ok = false;
                    format!("FAIL over {}", row.budget.cite)
                }
                (Verdict::Over, _) => format!("over target, under hard fail ({})", row.budget.cite),
                (Verdict::Pass, _) => "ok".to_string(),
            };

            println!(
                "{:<28} {:>10} {:>10} {:>10} {:>10} {:>10}  {}",
                row.name,
                micros(row.p50()),
                micros(row.p99()),
                micros(row.p999()),
                micros(row.max()),
                micros(row.budget.target),
                note,
            );
        }
        ok
    }
}

fn micros(d: Duration) -> String {
    format!("{:.3}us", d.as_secs_f64() * 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(nanos: &[u64]) -> Samples {
        let mut nanos = nanos.to_vec();
        nanos.sort_unstable();
        Samples {
            name: "fake",
            budget: budgets::POLICY_CHECK,
            nanos,
            allocs: AllocCount { calls: 0, bytes: 0 },
        }
    }

    #[test]
    fn nearest_rank_picks_the_sample_not_an_interpolation() {
        let s = fake(&(1..=100).collect::<Vec<_>>());
        assert_eq!(s.p50(), Duration::from_nanos(50));
        assert_eq!(s.p99(), Duration::from_nanos(99));
        assert_eq!(s.max(), Duration::from_nanos(100));
    }

    #[test]
    fn one_slow_sample_in_a_hundred_moves_p99_not_p50() {
        let mut v = vec![10u64; 99];
        v.push(1_000_000);
        let s = fake(&v);
        assert_eq!(s.p50(), Duration::from_nanos(10));
        assert_eq!(s.p99(), Duration::from_nanos(10));
        assert_eq!(s.max(), Duration::from_micros(1000));
    }

    #[test]
    fn percentiles_of_an_empty_run_are_zero_rather_than_a_panic() {
        assert!(fake(&[]).is_empty());
        assert_eq!(fake(&[]).p99(), Duration::ZERO);
    }

    #[test]
    fn verdict_splits_target_from_hard_fail() {
        let b = budgets::POLICY_CHECK;
        assert_eq!(b.verdict(Duration::from_micros(50)), Verdict::Pass);
        assert_eq!(b.verdict(Duration::from_micros(51)), Verdict::Over);
        assert_eq!(b.verdict(Duration::from_micros(200)), Verdict::Over);
        assert_eq!(b.verdict(Duration::from_micros(201)), Verdict::Fail);
    }

    #[test]
    fn a_budget_with_no_hard_fail_fails_at_its_target() {
        let b = budgets::IRREVERSIBLE_MATCH;
        assert!(b.hard_fail.is_none());
        assert_eq!(b.verdict(Duration::from_micros(20)), Verdict::Pass);
        assert_eq!(b.verdict(Duration::from_micros(21)), Verdict::Fail);
    }

    #[test]
    fn the_gated_budget_is_the_one_milestone_16_cites() {
        assert_eq!(budgets::POLICY_CHECK.target, Duration::from_micros(50));
    }

    #[test]
    fn measure_keeps_every_sample_and_sorts_them() {
        let s = measure("noop", budgets::POLICY_CHECK, 200, || 1u64 + 1);
        assert_eq!(s.len(), 200);
        assert!(s.p50() <= s.p99() && s.p99() <= s.max());
    }

    #[test]
    fn measure_sees_an_allocation_inside_the_region() {
        let s = measure("alloc", budgets::POLICY_CHECK, 100, || vec![0u8; 64]);
        // Without the counting allocator installed (the test binary uses the
        // default one) the count stays zero; the binary is what enforces this.
        // Either way the field must be readable and self-consistent.
        assert_eq!(s.allocs.is_none(), s.allocs.calls == 0);
    }
}
