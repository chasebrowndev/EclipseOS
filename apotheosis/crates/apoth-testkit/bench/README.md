# apoth-testkit bench

- `apoth-bench run [--scale F] [--runs N]` prints headless results as JSON (T-23 §6);
  `APOTH_PERF=1` also enforces the absolute budgets in `src/perf/budgets.rs` (exit 1).
- `apoth-bench hash <corpus> <bytes>` pins a corpus by FNV-1a hash (T-23 §1).
- `record-peers.sh <outdir>` records peer wall-clock baselines as JSON data (reference
  machine, serial, AC power, performance governor). It runs peer CLIs only.
- Key-to-commit latency: `src/perf/latency.rs` documents the probe the binary calls.
- `cargo test` exercises all of it at tiny scale; the absolute assertion test
  (`budget_assertions_gated_by_env`) runs only with `APOTH_PERF=1`.
