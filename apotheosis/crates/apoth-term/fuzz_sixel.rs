// SPDX-License-Identifier: AGPL-3.0-only
//! `sixel` fuzz target body (T-15 §8): arbitrary sixel data under every mode combination, split at arbitrary points; caps hold and images stay cell-row slices. The body is
//! [`apoth_term::fuzz::fuzz_sixel`], so the workspace tests exercise the same code over the seed
//! corpus.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/sixel.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-term`; exempt
//! from the language rule). The seed corpus is `apoth-term/corpus/sixel/`; copy or symlink it
//! to `apotheosis/fuzz/corpus/sixel/`. CI smoke: `cargo fuzz run sixel --
//! -max_total_time=60`. This file is not part of the apoth-term crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_sixel(data);
});
