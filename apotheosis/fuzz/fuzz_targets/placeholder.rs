// SPDX-License-Identifier: AGPL-3.0-only
//! `placeholder` fuzz target (T-15 §8): Unicode placeholder cell scripts over two virtual
//! placements; quads stay inside their images. Body:
//! [`apoth_term::fuzz::fuzz_placeholder`]. Seeds: `corpus/placeholder/`, replayed on stable
//! by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_placeholder(data);
});
