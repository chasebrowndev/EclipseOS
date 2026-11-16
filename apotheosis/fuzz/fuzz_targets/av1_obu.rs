// SPDX-License-Identifier: AGPL-3.0-only
//! `av1_obu` fuzz target (T-16 §6). Body: [`apoth_media::fuzz::fuzz_av1_obu`]. Seeds:
//! `corpus/av1_obu/`, replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_media::fuzz::fuzz_av1_obu(data);
});
