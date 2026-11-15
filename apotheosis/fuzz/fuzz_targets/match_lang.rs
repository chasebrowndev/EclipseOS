// SPDX-License-Identifier: AGPL-3.0-only
//! `match_lang` fuzz target (T-18 §11, T-25 §4): arbitrary text through the
//! match-expression validator never panics and returns within its budget. Body:
//! [`apoth_config::fuzz::fuzz_match`]. Seeds: `corpus/match_lang/`, replayed on stable by
//! `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_config::fuzz::fuzz_match(data);
});
