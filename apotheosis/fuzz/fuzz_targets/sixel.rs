// SPDX-License-Identifier: AGPL-3.0-only
//! `sixel` fuzz target (T-15 §8): arbitrary sixel data under every mode combination, split
//! at arbitrary points; caps hold and images stay cell-row slices. Body:
//! [`apoth_term::fuzz::fuzz_sixel`]. Seeds: `corpus/sixel/`, replayed on stable by
//! `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_sixel(data);
});
