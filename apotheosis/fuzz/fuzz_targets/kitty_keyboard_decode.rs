// SPDX-License-Identifier: AGPL-3.0-only
//! `kitty_keyboard_decode` fuzz target (T-18 §11, T-25 §4): arbitrary kitty keyboard
//! protocol requests (push, pop, set, query) never panic, the query reply is always `CSI ?
//! <flags> u`, and the flag stacks stay bounded. Body:
//! [`apoth_term::fuzz::fuzz_kitty_keyboard`]. Seeds: `corpus/kitty_keyboard_decode/`,
//! replayed on stable by `apoth-testkit/tests/fuzz_corpus.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    apoth_term::fuzz::fuzz_kitty_keyboard(data);
});
