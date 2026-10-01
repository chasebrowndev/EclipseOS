<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# `bench/` — the abyss benchmark harness

Governing spec: **COMP-14** (VOL1 §2 budgets, §3 forbidden patterns, §4.1
benchmarks, §5 CI enforcement). Not TCB, but it is what gates TCB code: the
milestone 16 exit criterion is `check()` at **≤50 µs p99, measured here**.

```
cargo run --release -p abyss-bench
```

Exits non-zero if any bench is over its budget or allocates on a path where
root invariant 11 forbids it. That is the whole CI contract — COMP-14 §5 needs
no wrapper around it.

Always `--release`. A debug run measures the debug build, and the numbers are
not comparable to anything.

## Shape

| File | What |
|---|---|
| `src/lib.rs` | `measure()` — times a closure, keeps every sample, sorts. `Samples` — percentiles. `Report` — the table and the exit verdict. |
| `src/budgets.rs` | COMP-14 §2.1 and §2.1b as data. One place to change when the spec does. |
| `src/alloc.rs` | The counting allocator COMP-14 §3 asks for. `Report::forbid_alloc(name)` makes a bench fail on its first allocation. |
| `src/main.rs` | The runner. Installs the allocator, registers the benches, returns the exit code. |

## Why not criterion

COMP-14 §4.1 used to say "Criterion microbenchmarks". Criterion reports a mean
with a confidence interval and treats the slow tail as noise to be classified
out; every budget in COMP-14 §2 is a p99. See
`decisions/0043-hand-rolled-bench-harness.md` — the spec line was amended in
the same change.

## What runs today

Only `timer-overhead`, the harness measuring its own floor. The five subjects
§4.1 names do not exist yet: `check()` and scope match arrive with milestone
16, audit encode with 12, tree serialize with 22, damage merge with the render
work. Each lands with its milestone, as one `measure()` call plus a budget.

`timer-overhead` is not a placeholder. The tightest budget in the table is
20 µs (§2.1b, the irreversible matcher); if `Instant`'s round trip were
anywhere near that, every number this crate prints would be measuring itself.
It currently runs ~18 ns at p99, three orders below.

## Still owed

COMP-14 §5 wants per-commit baselines and a build failure at >10% regression
against the rolling median of the last 10 green builds. Not built — there is
nothing to baseline until real subjects land. Tracked in ADR 0043.
