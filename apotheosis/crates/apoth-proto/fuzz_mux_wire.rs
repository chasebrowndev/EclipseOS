// SPDX-License-Identifier: Apache-2.0
//! `mux_wire` fuzz target body (T-21 §3, T-18 §11): arbitrary bytes through the mux attach
//! protocol's record decoder and both message decoders never panic, the buffer stays
//! bounded, and a decoded message re-encodes to itself. The body is
//! [`apoth_proto::mux::fuzz_mux`], so the workspace tests exercise the same code.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): copy this file
//! verbatim to `apotheosis/fuzz/fuzz_targets/mux_wire.rs` in the ADR 0110 fuzz workspace.
//! Seeds: `apoth-proto/corpus/mux_wire/`. CI smoke: `cargo fuzz run mux_wire --
//! -max_total_time=60`. Not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_proto::mux::fuzz_mux(data);
});
