// SPDX-License-Identifier: AGPL-3.0-only
//! `proto_json` fuzz target (T-12 §6, T-18 §11, T-25 §4): arbitrary bytes through the
//! control socket's line decoder, request decoder and server-message decoder never panic,
//! buffers stay under the line limit, and a decoded request re-encodes to itself. Body:
//! [`apoth_proto::fuzz_parse`]. Seeds: `corpus/proto_json/`, replayed on stable by
//! `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_proto::fuzz_parse(data);
});
