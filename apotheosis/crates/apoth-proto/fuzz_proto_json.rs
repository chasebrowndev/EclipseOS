// SPDX-License-Identifier: Apache-2.0
//! `proto_json` fuzz target body (T-12 §6, T-18 §11, T-25 §4): arbitrary bytes through the
//! control socket's line decoder, request decoder and server-message decoder never panic,
//! buffers stay under the line limit, and a decoded request re-encodes to itself. The body
//! is [`apoth_proto::fuzz_parse`], so the workspace tests exercise the same code.
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/proto_json.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-proto`; exempt
//! from the language rule). The seed corpus is `apoth-proto/corpus/proto_json/`; copy or symlink it to
//! `apotheosis/fuzz/corpus/proto_json/`. CI smoke: `cargo fuzz run proto_json --
//! -max_total_time=60`. This file is not part of the apoth-proto crate (it is not referenced
//! from `src/` or `tests/`) and is not compiled by the workspace gate.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_proto::fuzz_parse(data);
});
