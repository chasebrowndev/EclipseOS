// SPDX-License-Identifier: AGPL-3.0-only
//! `term_feed` fuzz target (T-25 §4, T-03 §5): arbitrary bytes into `Term::feed` never
//! panic, memory stays bounded, and the result does not depend on how the bytes are
//! chunked. Body: [`apoth_term::fuzz::fuzz_term_feed`]. Seeds: `corpus/term_feed/`,
//! replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_term_feed(data);
});
