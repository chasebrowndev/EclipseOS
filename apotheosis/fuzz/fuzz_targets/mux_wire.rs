// SPDX-License-Identifier: AGPL-3.0-only
//! `mux_wire` fuzz target (T-21 §3, T-18 §11): arbitrary bytes through the mux attach
//! protocol's record decoder and both message decoders never panic, the buffer stays
//! bounded, and a decoded message re-encodes to itself. Body:
//! [`apoth_proto::mux::fuzz_mux`]. Seeds: `corpus/mux_wire/`, replayed on stable by
//! `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_proto::mux::fuzz_mux(data);
});
