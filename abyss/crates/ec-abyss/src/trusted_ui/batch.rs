// SPDX-License-Identifier: AGPL-3.0-only
//! The batch prompt (COMP-10 §3.7) for `preflight` (COMP-08 §4.2). **TCB.**
//!
//! A batch prompt that says "47 items" without saying which 47 is a category
//! grant wearing a count. So this one enumerates: every `(handle, node)` the
//! token would cover is a fact on the prompt, [`PAGE`] to a page, and "More
//! targets" pages through the rest in place. Approving grants exactly the
//! enumerated set; the agent's `summary` is in the untrusted block.
//!
//! One batch per agent waits at a time; a second is `rate_limited`. Batches
//! show one at a time, like every prompt, and Deny is the safe answer.

use std::collections::VecDeque;

use super::modal::{Button, Modal, Role};
use super::Choice;
use crate::state::AbyssState;

/// Batch prompt tokens: bit 61 and bit 58.
const BASE: u64 = (1 << 61) | (1 << 58);

/// Targets shown per page. Under §3.7's cap of 50 visible, and what fits.
pub const PAGE: usize = 20;

#[derive(Debug, Clone)]
struct Ask {
    agent: u64,
    req_id: u32,
    taxonomy: String,
    /// Each target with the line that describes it, computed when asked.
    targets: Vec<((u64, u64), String)>,
    summary: String,
}

#[derive(Debug, Default)]
pub struct Batches {
    waiting: VecDeque<Ask>,
    showing: Option<(u64, Ask, usize)>,
    next: u64,
}

const HEADING: &str = "An agent is asking to act on several things at once";
const BODY: &str =
    "Approving lets the agent do this to exactly the targets listed, each once, without asking again.";
const WELL: &str = "Agent's summary (untrusted):";

/// Queue a preflight. False when this agent already has one waiting.
pub fn ask(
    state: &mut AbyssState,
    agent: u64,
    req_id: u32,
    taxonomy: &str,
    targets: Vec<(u64, u64)>,
    summary: &str,
) -> bool {
    let b = &state.trusted_ui.batches;
    if b.waiting.iter().any(|a| a.agent == agent)
        || b.showing.as_ref().is_some_and(|(_, a, _)| a.agent == agent)
    {
        return false;
    }
    let described = targets
        .into_iter()
        .map(|t| {
            let line = match state.ipc.window_for(t.0) {
                Some(w) => crate::policy::scene::with_facts(state, &w, |f| {
                    format!("#{} {} - {}  node {}", t.0, f.app_id, f.title, t.1)
                }),
                None => format!("#{} (gone)  node {}", t.0, t.1),
            };
            (t, line)
        })
        .collect();
    state.trusted_ui.batches.waiting.push_back(Ask {
        agent,
        req_id,
        taxonomy: taxonomy.to_owned(),
        targets: described,
        summary: summary.to_owned(),
    });
    schedule(state);
    true
}

fn modal(token: u64, a: &Ask, page: usize) -> Option<Modal> {
    let pages = a.targets.len().div_ceil(PAGE).max(1);
    let mut buttons = vec![Button {
        label: "Approve all listed",
        role: Role::Grant,
    }];
    if pages > 1 {
        buttons.push(Button {
            label: "More targets",
            role: Role::Other,
        });
    }
    buttons.push(Button {
        label: "Deny",
        role: Role::Safe,
    });
    let m = Modal::new(
        token,
        HEADING,
        Some("Every target below is covered. Irreversible actions may follow."),
        BODY,
        WELL,
        &a.summary,
        buttons,
    )
    .ok()?
    .irreversible();
    let principal = format!("agent #{}", a.agent);
    let count = format!("{} targets, page {} of {pages}", a.targets.len(), page + 1);
    let mut facts: Vec<(&'static str, String)> = vec![
        ("Agent", principal),
        ("Category", a.taxonomy.clone()),
        ("Covers", count),
    ];
    for (_, line) in a.targets.iter().skip(page * PAGE).take(PAGE) {
        facts.push(("Target", line.clone()));
    }
    let refs: Vec<(&'static str, &str)> = facts.iter().map(|(l, v)| (*l, v.as_str())).collect();
    Some(m.with_facts(&refs))
}

/// Show the next waiting batch, if no prompt is up.
pub fn schedule(state: &mut AbyssState) {
    if state.trusted_ui.is_open() || state.trusted_ui.batches.showing.is_some() {
        return;
    }
    let Some(a) = state.trusted_ui.batches.waiting.pop_front() else {
        return;
    };
    state.trusted_ui.batches.next += 1;
    let token = BASE | state.trusted_ui.batches.next;
    match modal(token, &a, 0) {
        Some(m) => {
            state.trusted_ui.batches.showing = Some((token, a, 0));
            if !super::open(state, m) {
                if let Some((_, a, _)) = state.trusted_ui.batches.showing.take() {
                    state.trusted_ui.batches.waiting.push_front(a);
                }
            }
        }
        None => crate::policy::batch::refused(state, a.agent, a.req_id, false),
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    state
        .trusted_ui
        .batches
        .showing
        .as_ref()
        .is_some_and(|(t, ..)| *t == token)
}

pub fn answer(state: &mut AbyssState, choice: Choice) {
    let Some((token, a, page)) = state.trusted_ui.batches.showing.take() else {
        return;
    };
    let pages = a.targets.len().div_ceil(PAGE).max(1);
    let label = modal(token, &a, page).and_then(|m| m.buttons().get(choice.button).map(|b| b.label));
    match label {
        Some("Approve all listed") if !choice.timed_out => {
            let targets = a.targets.into_iter().map(|(t, _)| t).collect();
            crate::policy::batch::approved(state, a.agent, a.req_id, targets);
        }
        Some("More targets") if !choice.timed_out => {
            let next = (page + 1) % pages;
            if let Some(m) = modal(token, &a, next) {
                state.trusted_ui.batches.showing = Some((token, a, next));
                if !super::open(state, m) {
                    // Could not reopen: deny rather than leave it hanging.
                    if let Some((_, a, _)) = state.trusted_ui.batches.showing.take() {
                        crate::policy::batch::refused(state, a.agent, a.req_id, false);
                    }
                }
                return;
            }
            crate::policy::batch::refused(state, a.agent, a.req_id, false);
        }
        _ => crate::policy::batch::refused(state, a.agent, a.req_id, choice.timed_out),
    }
}

/// Drop `agent`'s waiting or shown batch (it went away).
pub fn withdraw(state: &mut AbyssState, agent: u64) {
    state.trusted_ui.batches.waiting.retain(|a| a.agent != agent);
    if state
        .trusted_ui
        .batches
        .showing
        .as_ref()
        .is_some_and(|(_, a, _)| a.agent == agent)
    {
        if let Some((token, ..)) = state.trusted_ui.batches.showing.take() {
            super::cancel(state, token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn many(n: usize) -> Ask {
        Ask {
            agent: 1,
            req_id: 2,
            taxonomy: "communication.send".into(),
            targets: (0..n)
                .map(|i| ((i as u64 + 1, 0), format!("#{} mail", i + 1)))
                .collect(),
            summary: "send the 47 invoices".into(),
        }
    }

    #[test]
    fn every_target_is_on_some_page_and_none_twice() {
        let a = many(47);
        let pages = 47usize.div_ceil(PAGE);
        let mut seen = Vec::new();
        for p in 0..pages {
            let m = modal(1, &a, p).unwrap();
            for (l, v) in m.facts() {
                if *l == "Target" {
                    seen.push(v.clone());
                }
            }
        }
        let want: Vec<String> = a.targets.iter().map(|(_, l)| l.clone()).collect();
        assert_eq!(seen, want, "the enumerated set, in order, exactly once");
    }

    #[test]
    fn deny_is_safe_and_a_small_batch_has_no_paging() {
        let m = modal(1, &many(3), 0).unwrap();
        let labels: Vec<&str> = m.buttons().iter().map(|b| b.label).collect();
        assert_eq!(labels, ["Approve all listed", "Deny"]);
        assert_eq!(m.buttons()[m.safe()].label, "Deny");
        assert!(m.untrusted().iter().any(|l| l.contains("47 invoices")));
    }
}
