# 0043 — The benchmark harness is hand-rolled, not criterion
Status: accepted
Date: 2026-09-17
Deciders: chase (owner), Claude (advisory)

## Context
COMP-14 §4.1 says "Criterion microbenchmarks: `check()`, damage merge, tree
serialize, scope match, audit encode." Milestone 16's gate is `check()` at
**≤50 µs p99 measured by the 9f harness**, and every other row of COMP-14 §2.1
and §2.1b is stated the same way — a percentile, usually p99, occasionally
p99.9 (§8.3 proposes gating on p99 and tracking p99.9).

Criterion's model is the opposite one. It reports a mean with a bootstrapped
confidence interval, and its outlier classifier exists specifically to identify
and set aside the slow tail as measurement noise. That is the right instrument
for "did this function get faster", and the wrong one for "does this path ever
exceed 50 µs". Getting a p99 out of it means keeping raw samples it is built to
summarise away.

The second force is invariant 11. COMP-14 §3 requires allocation counts on the
forbidden paths to be asserted by "a counting allocator in benches". A counting
allocator is process-wide; the measured region has to be the only thing
running, and the harness itself must not allocate inside it. Criterion
allocates during measurement, so the counter would be measuring criterion.

Third, smaller: `bench/` sits against TCB code — `check()` is the headline
subject — and criterion pulls a dependency tree behind it. It clears
`deny.toml` (checked: licenses, bans and sources all pass), so this is not a
supply-chain veto, just a thumb on the scale.

## Options
1. **Criterion, as §4.1 literally says.** Pro: standard, familiar output,
   regression tracking built in. Con: reports the one statistic the spec never
   asks for; p99 requires fighting the tool; the counting-allocator requirement
   of §3 can't be satisfied inside its measurement loop.
2. **Hand-rolled sampler.** Pro: keeps every sample, so percentiles are exact
   and p99.9 is free; no allocation inside the measured region, so §3's
   counting allocator works; zero new dependencies on a TCB-adjacent path.
   Con: we own the statistics, the output format and the baseline comparison
   that criterion would have given us; roughly 200 lines to maintain.
3. **Both** — criterion for the mean, hand-rolled for the tail. Con: two
   harnesses measuring the same functions, two sets of numbers to reconcile,
   and CI gating on one of them. Rejected on cost.

## Decision
Hand-rolled. `bench/` times with `std::time::Instant`, retains every sample,
sorts, and reports p50/p99/p99.9/max against the COMP-14 budget table encoded
as data in `bench/src/budgets.rs`. A counting allocator (`bench/src/alloc.rs`)
is installed in the runner binary so a bench declared `forbid_alloc` fails if
the measured region allocated at all. The runner exits non-zero on any budget
failure, so COMP-14 §5's CI gate is a single command.

COMP-14 §4.1 is amended in the same change to say "microbenchmarks" rather than
"Criterion microbenchmarks", with a pointer here. The spec named an
implementation where it meant a requirement; the requirement — those five
subjects, measured — is unchanged.

## Consequences
- Milestone 16 can gate on `≤50 µs p99` directly, which was the point.
- Invariant 11 becomes enforceable rather than reviewed: a `forbid_alloc` bench
  fails on the first allocation.
- We owe the baseline machinery COMP-14 §5 wants — per-commit baselines and a
  >10% regression failure against the rolling median of the last 10 green
  builds. Not in this change; it needs a baseline store and CI wiring, and
  there is nothing to baseline until real subjects land.
- The five subjects in §4.1 arrive with the milestones that create them:
  `check()` and scope match with 16, audit encode with 12, tree serialize with
  22, damage merge with the render work. Today `bench/` runs only its own timer
  floor.
- Numbers are only comparable when produced the same way — `--release`, same
  machine. That is already true of the GPU benches (§5 pins them to the
  reference machine); it now applies to the microbenchmarks too.

## Revisit when
Either the baseline/regression machinery in COMP-14 §5 turns out to be more
work to hand-roll than criterion's is to bend toward percentiles, or criterion
gains first-class percentile reporting over retained samples.
