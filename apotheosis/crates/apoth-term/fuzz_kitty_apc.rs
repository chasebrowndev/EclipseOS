// SPDX-License-Identifier: AGPL-3.0-only
//! `kitty_apc` fuzz target body (T-15 §8, T-18 §11): arbitrary kitty graphics APCs, chunked arbitrarily; the store stays within quota and caps, every reply is in the closed grammar, and denied graphics have no effect. The body is
//! [`apoth_term::fuzz::fuzz_kitty_apc`], so the workspace tests exercise the same code over the seed
//! corpus.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/kitty_apc.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-term`; exempt
//! from the language rule). The seed corpus is `apoth-term/corpus/kitty_apc/`; copy or symlink it
//! to `apotheosis/fuzz/corpus/kitty_apc/`. CI smoke: `cargo fuzz run kitty_apc --
//! -max_total_time=60`. This file is not part of the apoth-term crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_kitty_apc(data);
});
