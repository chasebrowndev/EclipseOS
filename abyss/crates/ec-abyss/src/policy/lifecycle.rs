// SPDX-License-Identifier: AGPL-3.0-only
//! Agent lifecycle as abyss enforces it (COMP-10 §3.3, COMP-04 §6). TCB.
//!
//! What this module decides is whether an agent may act:
//!
//! - **Pause** is abyss's own: a paused agent's every request answers
//!   `paused` and changes nothing. Per agent, or every agent at once (the
//!   override chord, COMP-04 §6). Resume is explicit and comes only from the
//!   human seat: the emergency panel or the human socket, never an agent.
//! - **Revoke** asks `policyd` to cancel the principal's task, and drops its
//!   grants here at once rather than waiting for the answer.
//! - **Terminate** revokes, tells `policyd`, withdraws the agent's parked
//!   prompts and ends its client.
//! - A `revoked` message from `policyd` drops every grant the principal
//!   holds here and remembers the task, so the same grants cannot be
//!   re-admitted this session.

use ec_policy_eval::Ulid;

use crate::state::AbyssState;

/// Which agents the human has paused.
#[derive(Debug, Default)]
pub struct Paused {
    ids: Vec<u64>,
    /// Every agent, including ones admitted later (COMP-04 §6).
    all: bool,
}

pub fn is_paused(state: &AbyssState, id: u64) -> bool {
    state.paused.all || state.paused.ids.contains(&id)
}

pub fn all_paused(state: &AbyssState) -> bool {
    state.paused.all
}

fn note(state: &mut AbyssState, id: u64, event: &str) {
    if let Some(p) = state.agents.principal_of(id) {
        crate::audit::agent(state, crate::audit::lifecycle(&p, event));
    }
}

pub fn pause(state: &mut AbyssState, id: u64) {
    if !state.paused.ids.contains(&id) {
        state.paused.ids.push(id);
        note(state, id, "pause");
        tracing::warn!(agent = id, "agent paused");
    }
}

/// Resume one agent. Callers are human-seat paths only.
pub fn resume(state: &mut AbyssState, id: u64) {
    let before = state.paused.ids.len();
    state.paused.ids.retain(|i| *i != id);
    if state.paused.ids.len() != before {
        note(state, id, "resume");
        tracing::info!(agent = id, "agent resumed");
    }
}

/// Pause every agent, present and future (COMP-04 §6).
pub fn pause_all(state: &mut AbyssState) {
    if !state.paused.all {
        state.paused.all = true;
        for (id, _) in state.agents.list() {
            note(state, id, "pause");
        }
        tracing::warn!("every agent paused");
    }
}

/// Lift the fleet-wide pause. Agents paused one by one stay paused.
pub fn resume_all(state: &mut AbyssState) {
    if state.paused.all {
        state.paused.all = false;
        for (id, _) in state.agents.list() {
            if !state.paused.ids.contains(&id) {
                note(state, id, "resume");
            }
        }
        tracing::info!("fleet pause lifted");
    }
}

/// Drop `id`'s grants here and ask `policyd` to cancel its task.
pub fn revoke(state: &mut AbyssState, id: u64) {
    let Some(principal) = state.agents.principal_of(id) else {
        return;
    };
    state.agents.revoke_id(id);
    crate::policy::link::send(state, &ec_policy_eval::link::ToPolicyd::Revoke { principal });
    tracing::warn!(agent = id, "agent grants revoked by the human");
}

/// End agent `id`: its grants, its parked prompts and its client.
pub fn terminate(state: &mut AbyssState, id: u64) {
    let Some(principal) = state.agents.principal_of(id) else {
        return;
    };
    state.agents.revoke_id(id);
    crate::trusted_ui::consent::withdraw(state, id);
    note(state, id, "stop");
    crate::policy::link::send(
        state,
        &ec_policy_eval::link::ToPolicyd::Terminate {
            principal: principal.clone(),
        },
    );
    state.agents.disconnect(id);
    forget(state, id);
    tracing::warn!(agent = id, principal, "agent terminated by the human");
}

/// Terminate every agent (COMP-04 §6, the secondary chord).
pub fn terminate_all(state: &mut AbyssState) {
    pause_all(state);
    for (id, _) in state.agents.list() {
        terminate(state, id);
    }
}

/// An agent object is gone; drop what was kept about it.
pub fn forget(state: &mut AbyssState, id: u64) {
    state.paused.ids.retain(|i| *i != id);
}

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
