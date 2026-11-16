// SPDX-License-Identifier: AGPL-3.0-only
//! `term_feed` fuzz target body (T-25 section 4, T-03 section 6): arbitrary bytes into
//! `apoth_term::Term::feed` never panic, memory stays bounded, and the result does not depend
//! on how the bytes are chunked (T-03: feed is deterministic).
//!
//! WIRING (orchestrator; `apotheosis/fuzz/` does not exist in this worktree): this file is the
//! body of `apotheosis/fuzz/fuzz_targets/term_feed.rs` (copy it verbatim). `apotheosis/fuzz/`
//! is the separate non-shipping cargo-fuzz workspace of ADR 0110 (libfuzzer-sys, its own
//! `rust-toolchain.toml` pinning a dated nightly, path dependency on `apoth-term`; exempt
//! from the language rule). The seed corpus is `apoth-testkit/fuzz_seeds/term_feed/`; copy or
//! symlink it to `apotheosis/fuzz/corpus/term_feed/`. CI smoke: `cargo fuzz run term_feed
//! -- -max_total_time=60`. This file is not part of the apoth-testkit crate (it is not
//! referenced from `src/` or `tests/`) and is not compiled by the workspace gate.
//!
//! Input layout: byte 0 = rows (1..=40), byte 1 = cols (1..=120), byte 2 = chunk size
//! (0 = random-ish split from the data), the rest is the stream.
#![no_main]

use std::time::{Duration, Instant};

use apoth_grid::Limits;
use apoth_term::{Term, TermConfig};
use libfuzzer_sys::fuzz_target;

/// Scrollback lines kept in the fuzz configuration; history above this is a bug.
const HISTORY_LINES: usize = 64;
/// Per-feed bound on queued replies. The reply queue itself is capped (T-03 section 5); this
/// is a loose ceiling that catches unbounded growth.
const REPLY_CEILING: usize = 1 << 20;

fn config(rows: usize, cols: usize) -> TermConfig {
    TermConfig {
        rows,
        cols,
        scrollback: Limits {
            bytes: 1 << 20,
            lines: Some(HISTORY_LINES),
        },
        ..TermConfig::default()
    }
}

fuzz_target!(|data: &[u8]| {
    let [r, c, k, stream @ ..] = data else {
        return;
    };
    let rows = usize::from(*r % 40) + 1;
    let cols = usize::from(*c % 120) + 1;
    let epoch = Instant::now();
    let now = epoch + Duration::from_millis(1);

    let mut whole = Term::new(config(rows, cols));
    let _ = whole.feed(stream, now);
    let whole_replies = whole.replies().take_all();
    assert!(whole_replies.len() <= REPLY_CEILING, "unbounded reply queue");
    assert!(
        whole.primary().history_rows() <= HISTORY_LINES,
        "history over its limit"
    );

    // Same bytes in another chunking: same replies and same history length.
    let mut split = Term::new(config(rows, cols));
    let mut rest = stream;
    let mut n = usize::from(*k);
    while !rest.is_empty() {
        let take = if *k == 0 {
            // Data-derived split: the next byte picks 1..=16.
            n = usize::from(rest[0] % 16) + 1;
            n
        } else {
            n.max(1)
        };
        let (head, tail) = rest.split_at(take.min(rest.len()));
        let _ = split.feed(head, now);
        rest = tail;
    }
    let split_replies = split.replies().take_all();
    assert_eq!(whole_replies, split_replies, "chunking changed the replies");
    assert_eq!(
        whole.primary().history_rows(),
        split.primary().history_rows(),
        "chunking changed the history"
    );
});
