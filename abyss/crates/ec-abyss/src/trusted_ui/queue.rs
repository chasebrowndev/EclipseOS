// SPDX-License-Identifier: AGPL-3.0-only
//! The pending-decision queue (COMP-10 §3.13, Appendix F-12). **TCB.**
//!
//! Every parked prompt, across all agents, oldest first. Opened by
//! `agent-attention` (`Super+Space`) or by the console's `show_decisions`
//! (A-08 §7), which may say *that* decisions wait and never what they are
//! (A-08 §4.1).
//!
//! v1 has no "decide later" button on a prompt, so the oldest parked prompt
//! is always the one on screen whenever the seat is free, and the queue is
//! that sequence: opening it brings the oldest prompt up with its safe
//! button (Deny) focused, which is what an answer from the queue is
//! (identical to answering the modal, F-12). A list view with per-entry
//! expansion needs deferral first; KNOWNBUGS QUEUE-01.

use crate::state::AbyssState;

/// Parked decisions: consent prompts and batch prompts waiting on the human.
pub fn pending(state: &AbyssState) -> u32 {
    let n = state.trusted_ui.consent.len() + state.trusted_ui.batches.len();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Bring the oldest decision forward, if the seat is free.
pub fn open(state: &mut AbyssState) {
    super::consent::schedule(state);
    super::batch::schedule(state);
}
