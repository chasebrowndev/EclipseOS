// SPDX-License-Identifier: AGPL-3.0-only
//! `media_apc` fuzz target (T-16 §2, §6): arbitrary media placement APCs, chunked
//! arbitrarily, answered by a fake player; requests name only announced placements, the
//! per-pane cap holds, and denied graphics have no effect. Body:
//! [`apoth_term::fuzz::fuzz_media_apc`]. Seeds: `corpus/media_apc/`, replayed on stable by
//! `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_media_apc(data);
});
