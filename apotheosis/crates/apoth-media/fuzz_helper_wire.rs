// SPDX-License-Identifier: AGPL-3.0-only
//! `helper_wire` fuzz target body (T-16 §6). The body is [`apoth_media::fuzz::fuzz_helper_wire`], so the
//! workspace tests exercise the same code over the seed corpus (`apoth-media/corpus/helper_wire/`).
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/helper_wire.rs` (copy it verbatim), in the separate
//! non-shipping cargo-fuzz workspace of ADR 0110. CI smoke: `cargo fuzz run helper_wire --
//! -max_total_time=60`. This file is not part of the apoth-media crate and is not compiled
//! by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_media::fuzz::fuzz_helper_wire(data);
});
