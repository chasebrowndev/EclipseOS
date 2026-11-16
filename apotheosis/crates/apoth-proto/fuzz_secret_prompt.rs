// SPDX-License-Identifier: Apache-2.0
//! `secret_prompt` fuzz target body (T-22 §3, T-18 §11): arbitrary bytes through the
//! `ec-secret-prompt` Terminal-target context and reply decoders never panic, and a decoded
//! context re-encodes within its budgets. The body is
//! [`apoth_proto::secret_prompt::fuzz_secret_prompt`], so the workspace tests run it too.
//!
//! WIRING (orchestrator): copy verbatim to `apotheosis/fuzz/fuzz_targets/secret_prompt.rs`
//! in the ADR 0110 fuzz workspace. Not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_proto::secret_prompt::fuzz_secret_prompt(data);
});
