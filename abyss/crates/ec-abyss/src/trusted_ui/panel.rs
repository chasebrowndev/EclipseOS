// SPDX-License-Identifier: AGPL-3.0-only
//! The emergency panel (COMP-10 §3.3). **TCB.**
//!
//! The audit tail `policyd` answers for the panel arrives here.

use ec_policy_eval::link::TailRecord;

use crate::state::AbyssState;

/// `policyd`'s answer to an `audit_tail` request.
pub fn audit_records(_state: &mut AbyssState, req: u64, records: Vec<TailRecord>) {
    tracing::debug!(req, n = records.len(), "audit tail for no open panel; dropped");
}
