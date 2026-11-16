// SPDX-License-Identifier: AGPL-3.0-only
//! `media_apc` fuzz target body (T-16 §2, §6): arbitrary media placement APCs, chunked arbitrarily, answered by a fake player; requests name only announced placements, the per-pane cap holds, and denied graphics have no effect. The body is
//! [`apoth_term::fuzz::fuzz_media_apc`], so the workspace tests exercise the same code over the seed
//! corpus.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/media_apc.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-term`; exempt
//! from the language rule). The seed corpus is `apoth-term/corpus/media_apc/`; copy or symlink it
//! to `apotheosis/fuzz/corpus/media_apc/`. CI smoke: `cargo fuzz run media_apc --
//! -max_total_time=60`. This file is not part of the apoth-term crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_media_apc(data);
});
