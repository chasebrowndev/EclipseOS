// SPDX-License-Identifier: Apache-2.0
//! `askpass` fuzz target body (T-22 §3, T-18 §11): arbitrary bytes through the armed askpass
//! request decoder never panic, and a decoded request re-encodes to itself. The body is
//! [`apoth_proto::askpass::fuzz_askpass`], so the workspace tests exercise the same code.
//!
//! WIRING (orchestrator): copy verbatim to `apotheosis/fuzz/fuzz_targets/askpass.rs` in the
//! ADR 0110 fuzz workspace. Not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_proto::askpass::fuzz_askpass(data);
});
