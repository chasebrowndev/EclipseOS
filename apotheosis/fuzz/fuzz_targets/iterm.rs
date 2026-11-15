// SPDX-License-Identifier: AGPL-3.0-only
//! `iterm` fuzz target (T-15 §8): OSC 1337 File= with arbitrary file bytes; every container
//! decoder behind it. Body: [`apoth_term::fuzz::fuzz_iterm`]. Seeds: `corpus/iterm/`,
//! replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_iterm(data);
});
