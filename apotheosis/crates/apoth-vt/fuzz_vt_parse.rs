// SPDX-License-Identifier: Apache-2.0
//! `vt_parse` fuzz target body (T-18 §11, T-25 §4): arbitrary bytes through the parser, whole and chunked, give the same action stream; text runs carry no controls and strings stay under their caps. The body is
//! [`apoth_vt::fuzz_parse`], so the workspace tests exercise the same code over the seed
//! corpus.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/vt_parse.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-vt`; exempt
//! from the language rule). The seed corpus is `apoth-vt/corpus/vt_parse/`; copy or symlink it
//! to `apotheosis/fuzz/corpus/vt_parse/`. CI smoke: `cargo fuzz run vt_parse --
//! -max_total_time=60`. This file is not part of the apoth-vt crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_vt::fuzz_parse(data);
});
