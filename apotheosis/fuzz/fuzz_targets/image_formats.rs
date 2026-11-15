// SPDX-License-Identifier: AGPL-3.0-only
//! `image_formats` fuzz target (T-15 §8): the PNG/APNG, GIF, JPEG and WebP decoder wrappers
//! straight; caps hold. Body: [`apoth_term::fuzz::fuzz_image_formats`]. Seeds:
//! `corpus/image_formats/`, replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_image_formats(data);
});
