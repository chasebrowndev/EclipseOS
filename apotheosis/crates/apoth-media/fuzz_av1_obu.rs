// SPDX-License-Identifier: AGPL-3.0-only
//! `av1_obu` fuzz target body (T-16 §6). The body is [`apoth_media::fuzz::fuzz_av1_obu`], so the
//! workspace tests exercise the same code over the seed corpus (`apoth-media/corpus/av1_obu/`).
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/av1_obu.rs` (copy it verbatim), in the separate
//! non-shipping cargo-fuzz workspace of ADR 0110. CI smoke: `cargo fuzz run av1_obu --
//! -max_total_time=60`. This file is not part of the apoth-media crate and is not compiled
//! by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_media::fuzz::fuzz_av1_obu(data);
});
