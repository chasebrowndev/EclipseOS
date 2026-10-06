// SPDX-License-Identifier: AGPL-3.0-only
//! Batch approval: `preflight` and batch tokens (COMP-08 §4.2, S-06 §6).
//! TCB.
//!
//! An agent about to take the same irreversible action on many targets asks
//! once, listing every `(handle, node)`. The human sees every target on the
//! batch prompt (COMP-10 §3.7) and, on approval, the agent gets a token
//! bound to exactly that set:
//!
//! - **Bound to the agent and its task.** Another agent's token, or one from
//!   a task that has ended, covers nothing.
//! - **Single use per target.** A target is consumed when an act on it runs
//!   under the token.
//! - **Only in place of a prompt.** At step 7 a `prompt` outcome on a covered
//!   target runs without asking again. A `deny` is still a deny, and a target
//!   outside the set is `batch_exhausted`.
//! - Nodes are 0 until the semantic tree exists (COMP-09): an act names no
//!   node, so only `(handle, 0)` targets can be consumed. Listing a nonzero
//!   node covers nothing, which fails closed.

use ec_protocols::agent::server::eclipse_agent_v1::Status;

use crate::policy::enforce::Submitted;
use crate::state::AbyssState;

/// Targets one preflight may list.
pub const MAX_TARGETS: usize = 1000;

#[derive(Debug, Clone)]
struct Token {
    token: u64,
    agent: u64,
    task: Option<ec_policy_eval::Ulid>,
    targets: Vec<(u64, u64)>,
}

#[derive(Debug, Default)]
pub struct Batches {
    tokens: Vec<Token>,
    next: u64,
}

/// `preflight(req_id, taxonomy_id, handles, nodes, summary)`.
pub fn preflight(
    state: &mut AbyssState,
    agent: u64,
    req_id: u32,
    taxonomy: &str,
    targets: &[(u64, u64)],
    summary: &str,
) -> Submitted {
    if taxonomy.is_empty() || taxonomy.len() > 64 || targets.is_empty() || targets.len() > MAX_TARGETS {
        return Submitted::Answered(Status::InvalidArgument, String::new());
    }
    if state.policy_key.is_none() || crate::policy::lifecycle::is_paused(state, agent) {
        return Submitted::Answered(Status::Paused, String::new());
    }
    // Every target must be one the agent can see: a batch cannot be used to
    // learn that a hidden window exists, or to approve acting on one.
    let Some(mut a) = state.agents.take_agent(agent) else {
        return Submitted::Answered(Status::NoCapability, "scene.list".into());
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut seen: Vec<(u64, u64)> = Vec::with_capacity(targets.len());
    let mut ok = true;
    if let Some(views) = a.view_at(now) {
        for t in targets {
            if seen.contains(t) {
                continue;
            }
            if crate::policy::scene::resolve(state, views.list, t.0).is_none() {
                ok = false;
                break;
            }
            seen.push(*t);
        }
    } else {
        ok = false;
    }
    state.agents.put_agent(agent, a);
    if !ok {
        return Submitted::Answered(Status::OutOfScope, "scene.list".into());
    }
    if crate::trusted_ui::batch::ask(state, agent, req_id, taxonomy, seen, summary) {
        Submitted::Pending
    } else {
        Submitted::Answered(Status::RateLimited, String::new())
    }
}

/// The human approved `targets` for `agent`'s request `req_id`: mint the
/// token and tell the agent.
pub fn approved(state: &mut AbyssState, agent: u64, req_id: u32, targets: Vec<(u64, u64)>) {
    state.batches.next += 1;
    // Unguessable is not what makes it safe: a token is bound to its agent,
    // so another agent presenting it gets nothing.
    let token = (state.batches.next << 16) | (agent & 0xffff);
    let covered = targets.len() as u32;
    let task = state.agents.task_of(agent);
    state.batches.tokens.push(Token {
        token,
        agent,
        task,
        targets,
    });
    crate::protocols::agent::seat::send_batch_token(state, agent, req_id, token, covered, 0);
    crate::protocols::agent::seat::reply(state, agent, req_id, Status::Ok, "");
}

/// The human refused (or the prompt timed out).
pub fn refused(state: &mut AbyssState, agent: u64, req_id: u32, timed_out: bool) {
    let s = if timed_out {
        Status::PromptTimeout
    } else {
        Status::PromptDenied
    };
    crate::protocols::agent::seat::reply(state, agent, req_id, s, "");
}

/// Step 7, for a `prompt` outcome on a request carrying `token`: consume
/// `(handle, 0)` from it, or say why not.
pub fn consume(state: &mut AbyssState, agent: u64, token: u64, handle: u64) -> Result<(), Status> {
    let task = state.agents.task_of(agent);
    let Some(t) = state
        .batches
        .tokens
        .iter_mut()
        .find(|t| t.token == token && t.agent == agent)
    else {
        return Err(Status::BatchExhausted);
    };
    if t.task.map(|x| x.0) != task.map(|x| x.0) {
        return Err(Status::TaskClosed);
    }
    let Some(i) = t.targets.iter().position(|x| *x == (handle, 0)) else {
        return Err(Status::BatchExhausted);
    };
    t.targets.remove(i);
    state.batches.tokens.retain(|t| !t.targets.is_empty());
    Ok(())
}

/// The agent is gone: its tokens go with it.
pub fn forget(state: &mut AbyssState, agent: u64) {
    state.batches.tokens.retain(|t| t.agent != agent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_covers_its_targets_once_for_its_agent_only() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        approved(s, 3, 1, vec![(10, 0), (11, 0), (12, 5)]);
        let token = s.batches.tokens[0].token;
        assert_eq!(
            consume(s, 4, token, 10),
            Err(Status::BatchExhausted),
            "another agent's token"
        );
        assert_eq!(consume(s, 3, token, 10), Ok(()));
        assert_eq!(
            consume(s, 3, token, 10),
            Err(Status::BatchExhausted),
            "single use"
        );
        assert_eq!(
            consume(s, 3, token, 99),
            Err(Status::BatchExhausted),
            "outside the set"
        );
        assert_eq!(
            consume(s, 3, token, 12),
            Err(Status::BatchExhausted),
            "a node target needs a node"
        );
        assert_eq!(consume(s, 3, token, 11), Ok(()));
        forget(s, 3);
        assert!(s.batches.tokens.is_empty());
    }
}
