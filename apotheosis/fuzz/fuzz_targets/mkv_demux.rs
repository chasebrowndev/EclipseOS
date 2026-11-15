// SPDX-License-Identifier: AGPL-3.0-only
//! `mkv_demux` fuzz target (T-16 §6). Body: [`apoth_media::fuzz::fuzz_mkv_demux`]. Seeds:
//! `corpus/mkv_demux/`, replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_media::fuzz::fuzz_mkv_demux(data);
});
