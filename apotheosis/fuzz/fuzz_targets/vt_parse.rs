// SPDX-License-Identifier: AGPL-3.0-only
//! `vt_parse` fuzz target (T-18 §11, T-25 §4): arbitrary bytes through the parser, whole
//! and chunked, give the same action stream; text runs carry no controls and strings stay
//! under their caps. Body: [`apoth_vt::fuzz_parse`]. Seeds: `corpus/vt_parse/`, replayed on
//! stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_vt::fuzz_parse(data);
});
