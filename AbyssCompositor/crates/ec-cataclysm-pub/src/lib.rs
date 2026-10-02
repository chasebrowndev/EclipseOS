// SPDX-License-Identifier: Apache-2.0
//! `ec-cataclysm-pub` — the semantic publisher for the `cataclysm` terminal
//! (ADR 0034, P-04 §2, §3, §6.1, §7; COMP-16 milestone 21).
//!
//! Pure library: no Wayland, no I/O, no clock, no threads, no logging. The
//! emulator (the foot fork, over the C ABI in [`ffi`]) or `abyss` (natively)
//! feeds it grid state and a monotonic time; it answers with the
//! `eclipse_semantic_v1` requests to send. Nothing protocol-shaped is
//! decided by the caller.
//!
//! Grid text is never logged or retained beyond the screen and the last
//! publish. While the pty has `ECHO` off, nothing but the raise leaves.

pub mod ffi;
mod publisher;
pub mod semantic;

pub use publisher::{
    Batch, Config, Error, Op, Publisher, Stats, BATCH_STRUCTURAL, BATCH_URGENT, EXT_VALUE_MAX, GROUP_BASE,
    MAX_BLOCKS, MAX_COLS, MAX_LINE_BYTES, MAX_ROWS, ROOT_ID, SLOT_BASE,
};
