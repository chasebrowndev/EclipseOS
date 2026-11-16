// SPDX-License-Identifier: AGPL-3.0-only
//! `kitty_apc` fuzz target (T-15 §8, T-18 §11): arbitrary kitty graphics APCs, chunked
//! arbitrarily; the store stays within quota and caps, every reply is in the closed
//! grammar, and denied graphics have no effect. Body: [`apoth_term::fuzz::fuzz_kitty_apc`].
//! Seeds: `corpus/kitty_apc/`, replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_kitty_apc(data);
});
