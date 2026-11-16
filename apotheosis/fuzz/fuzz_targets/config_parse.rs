// SPDX-License-Identifier: AGPL-3.0-only
//! `config_parse` fuzz target (T-18 §11, T-25 §4): arbitrary text as the user config file
//! loads without panicking, and loading is deterministic. Body:
//! [`apoth_config::fuzz::fuzz_config`]. Seeds: `corpus/config_parse/`, replayed on stable
//! by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_config::fuzz::fuzz_config(data);
});
