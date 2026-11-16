// SPDX-License-Identifier: AGPL-3.0-only
//! `helper_wire` fuzz target (T-16 §6). Body: [`apoth_media::fuzz::fuzz_helper_wire`].
//! Seeds: `corpus/helper_wire/`, replayed on stable by
//! `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_media::fuzz::fuzz_helper_wire(data);
});
