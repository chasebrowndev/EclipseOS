# 0111 — Apotheosis writes its own parser and grid
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
Rust offers `vte` (parser), `alacritty_terminal` (parser + grid) and `termwiz`. The research matrix asks for things none of them provide together: streaming APC/DCS sinks with per-protocol byte budgets, a bulk printable fast path, a paged grid with relocatable pages and tracked pins, 8-byte cells with interned styles, images anchored to rows, compressed cold history, and deterministic replay.

## Options
1. Depend on `alacritty_terminal`. Proven; its grid, cell and scrollback model would cap memory, reflow and graphics design, and upstream does not take such changes.
2. Depend on `vte` for parsing only. Its handling of APC and long strings buffers whole sequences, which T-18 forbids.
3. Own `apoth-vt` and `apoth-grid`, with `vte` and `alacritty_terminal` as dev-dependency differential oracles.

## Decision
Option 3 (T-01, T-02, T-25 §2). Third-party emulators never ship in a binary; they detect accidents in tests and are never the specification.

## Consequences
We own conformance: the golden-fixture corpus (3 000 at M31), fuzz targets and the oracle exclusion list are part of the product. Throughput budgets (T-23) are ours to meet.

## Revisit when
The fixture corpus cannot be kept green at reasonable cost, or an upstream crate grows the same design.
