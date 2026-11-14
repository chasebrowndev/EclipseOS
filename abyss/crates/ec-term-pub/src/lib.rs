// SPDX-License-Identifier: Apache-2.0
//! `ec-term-pub` — the terminal grid publisher for `eclipse_semantic_v1`
//! (ADR 0114, T-17 §4; P-04 §2, §3, §6.1, §7).
//!
//! Pure library: no Wayland, no I/O, no clock, no threads, no logging. The
//! emulator feeds it grid state and a monotonic time; it answers with the
//! `eclipse_semantic_v1` requests to send. Nothing protocol-shaped is
//! decided by the caller beyond where the `terminal` node hangs.
//!
//! Grid text is never logged or retained beyond the screen and the last
//! publish. While raised (echo off), nothing but the raise leaves.
#![forbid(unsafe_code)]

mod ids;
mod publisher;
pub mod semantic;

pub use ids::{IdRange, NodeIds, PANE_IDS};
pub use publisher::{
    Batch, Config, Error, Op, Publisher, Stats, BATCH_STRUCTURAL, BATCH_URGENT, EXT_VALUE_MAX, MAX_BLOCKS,
    MAX_COLS, MAX_LINE_BYTES, MAX_ROWS,
};
