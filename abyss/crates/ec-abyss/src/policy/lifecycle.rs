// SPDX-License-Identifier: AGPL-3.0-only
//! Agent lifecycle as abyss enforces it (COMP-10 §3.3, COMP-04 §6). TCB.
//!
//! What this module decides is whether an agent may act. Revocation is
//! `policyd`'s word: a `revoked` message drops every grant the principal
//! holds here, at once, and remembers the task so the same grants cannot be
//! re-admitted this session.

use ec_policy_eval::Ulid;

use crate::state::AbyssState;

/// Tasks whose grants `policyd` revoked this session. Admission refuses
/// any grant on one of them.
#[derive(Debug, Default)]
pub struct Revoked {
    tasks: Vec<Ulid>,
}

impl Revoked {
    pub fn covers(&self, task: Ulid) -> bool {
        self.tasks.iter().any(|t| t.0 == task.0)
    }
}

/// `policyd` revoked every grant `principal` holds.
pub fn revoked(state: &mut AbyssState, principal: &str) {
    let tasks = state.agents.revoke_principal(principal);
    for t in tasks {
        if !state.revoked.covers(t) {
            state.revoked.tasks.push(t);
        }
    }
    tracing::info!(principal, "grants revoked by policyd");
}
