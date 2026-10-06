// SPDX-License-Identifier: AGPL-3.0-only
//! The enforcement order for acting agent requests (COMP-08 §10). TCB.
//!
//! The answers `policyd` sends to a deferral or a mint come back here. No
//! acting request exists before agent seats (M13), so nothing is waiting on
//! either yet: an answer for a request that is not pending is dropped, which
//! is the fail-closed reading of an answer nobody asked for.

use ec_policy_eval::check::DeferAnswer;

use crate::state::AbyssState;

/// `policyd` answered deferral `req`.
pub fn defer_answer(_state: &mut AbyssState, req: u64, answer: DeferAnswer) {
    tracing::debug!(req, ?answer, "defer answer for no pending request; dropped");
}

/// `policyd` minted (or refused to mint) a grant for prompt answer `req`.
pub fn minted(_state: &mut AbyssState, req: u64, grant: Option<Vec<u8>>) {
    tracing::debug!(
        req,
        minted = grant.is_some(),
        "mint answer for no pending request; dropped"
    );
}
