// SPDX-License-Identifier: AGPL-3.0-only
//! `osc_all` fuzz target (T-18 §11, T-25 §4): every OSC number with a hostile payload,
//! allowed and denied: denied means no observable effect and no reply, and every reply
//! matches a closed grammar. Body: [`apoth_term::fuzz::fuzz_osc`]. Seeds:
//! `corpus/osc_all/`, replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_osc(data);
});
