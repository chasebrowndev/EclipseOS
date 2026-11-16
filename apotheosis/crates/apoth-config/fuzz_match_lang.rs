// SPDX-License-Identifier: AGPL-3.0-only
//! `match_lang` fuzz target body (T-18 §11, T-25 §4): arbitrary text through the match-expression validator never panics and returns within its budget. The body is
//! [`apoth_config::fuzz::fuzz_match`], so the workspace tests exercise the same code over the seed
//! corpus.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/match_lang.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-config`; exempt
//! from the language rule). The seed corpus is `apoth-config/corpus/match_lang/`; copy or symlink it
//! to `apotheosis/fuzz/corpus/match_lang/`. CI smoke: `cargo fuzz run match_lang --
//! -max_total_time=60`. This file is not part of the apoth-config crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_config::fuzz::fuzz_match(data);
});
