// SPDX-License-Identifier: AGPL-3.0-only
//! `config_parse` fuzz target body (T-18 §11, T-25 §4): arbitrary text as the user config file loads without panicking, and loading is deterministic. The body is
//! [`apoth_config::fuzz::fuzz_config`], so the workspace tests exercise the same code over the seed
//! corpus.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/config_parse.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-config`; exempt
//! from the language rule). The seed corpus is `apoth-config/corpus/config_parse/`; copy or symlink it
//! to `apotheosis/fuzz/corpus/config_parse/`. CI smoke: `cargo fuzz run config_parse --
//! -max_total_time=60`. This file is not part of the apoth-config crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_config::fuzz::fuzz_config(data);
});
