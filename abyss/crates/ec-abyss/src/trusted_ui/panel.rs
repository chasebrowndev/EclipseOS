// SPDX-License-Identifier: AGPL-3.0-only
//! The emergency panel (COMP-10 §3.3). **TCB.**
//!
//! Opened by the override chord (COMP-04 §6), which pauses every agent
//! first: the panel is what the human sees after the brakes are already on.
//! Opening it pauses nothing by itself.
//!
//! It is built from the modal primitive, so it has everything a prompt has:
//! compositor-drawn, the personal secret at the top, the human seat only,
//! never in a capture, armed against keys already in flight. It is a set of
//! pages:
//!
//! - **Overview**: every agent (id, state, principal, task), and the fleet
//!   controls: Resume all, Terminate all, Agents, Change secret phrase,
//!   Close.
//! - **Agent**: one agent's principal, task, state, grants held, and its last
//!   20 audited actions, fetched from `policyd` when the page opens (abyss
//!   holds no audit store). Pause or Resume, Revoke grants, Terminate, Next
//!   agent, Back.
//! - **Confirm**: Revoke, Terminate and Terminate all ask first; the safe
//!   answer is Cancel.
//!
//! Every value on a page is a fact (compiled-in label, one sanitised line),
//! so nothing an agent named itself can reach a heading or a button.

use ec_policy_eval::link::{TailRecord, ToPolicyd};

use super::modal::{Button, Modal, Role};
use super::Choice;
use crate::policy::lifecycle;
use crate::state::AbyssState;

/// Panel tokens: bit 61 and bit 59, below `erase::TOKEN_BASE`, without the
/// consent bit (62) or the notice bit (60).
const BASE: u64 = (1 << 61) | (1 << 59);
const OVERVIEW: u64 = BASE | 1;
const AGENT: u64 = BASE | 2;
const CONFIRM: u64 = BASE | 3;

/// Actions shown per agent page.
const TAIL: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destructive {
    Revoke(u64),
    Terminate(u64),
    TerminateAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Page {
    #[default]
    Closed,
    Overview,
    Agent(u64),
    Confirm(Destructive),
}

#[derive(Debug, Default)]
pub struct Panel {
    page: Page,
    /// The audit tail for the agent page: (agent, request id, records).
    tail: Option<(u64, u64, Vec<TailRecord>)>,
    /// The `audit_tail` request in flight: (agent, request id).
    pending: Option<(u64, u64)>,
    /// The buttons of the page on screen, as drawn. An answer is read
    /// against these, never against a page rebuilt from state that may
    /// have moved since (a Pause the human saw must not be read as Resume).
    shown: Vec<&'static str>,
    next_req: u64,
}

const HEADING: &str = "Emergency panel";
const OVERVIEW_BODY: &str = "Agents are listed below. Resume is explicit: nothing restarts on its own.";
const AGENT_HEADING: &str = "Agent";
const CONFIRM_HEADING: &str = "Are you sure?";
const REVOKE_BODY: &str =
    "Revoke every grant this agent holds. Its task is cancelled; it can do nothing more.";
const TERMINATE_BODY: &str = "Terminate this agent: its grants are revoked and its connection is closed.";
const TERMINATE_ALL_BODY: &str =
    "Terminate every agent: all grants are revoked and every agent connection is closed.";

fn btn(label: &'static str, role: Role) -> Button {
    Button { label, role }
}

fn state_word(state: &AbyssState, id: u64) -> &'static str {
    if lifecycle::is_paused(state, id) {
        "paused"
    } else {
        "running"
    }
}

fn overview(state: &AbyssState) -> Option<Modal> {
    let agents = state.agents.list();
    let mut rows: Vec<(String, String)> = Vec::new();
    for (id, principal) in &agents {
        let task = state
            .agents
            .task_of(*id)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "(no task)".into());
        rows.push((
            format!("#{id} {}", state_word(state, *id)),
            format!("{principal}  task {task}"),
        ));
    }
    let fleet = if lifecycle::all_paused(state) {
        "all agents paused"
    } else {
        "not paused"
    };
    let none = agents.is_empty();
    let mut buttons = vec![
        btn("Resume all", Role::Grant),
        btn("Terminate all", Role::Other),
        btn("Agents", Role::Other),
        btn("Change secret phrase", Role::Other),
        btn("Close", Role::Safe),
    ];
    if none {
        // Nothing to act on: only the phrase and Close.
        buttons = vec![btn("Change secret phrase", Role::Other), btn("Close", Role::Safe)];
    }
    let well = if none {
        "No agent is connected.".to_owned()
    } else {
        format!("{} agent(s) connected.", agents.len())
    };
    let m = Modal::new(OVERVIEW, HEADING, None, OVERVIEW_BODY, "Status:", &well, buttons).ok()?;
    // Labels are compiled-in, so the per-agent label is a fixed word and
    // the variable part goes in the value.
    let mut facts: Vec<(&'static str, String)> = vec![("Fleet", fleet.into())];
    for (label, value) in rows {
        facts.push(("Agent", format!("{label}  {value}")));
    }
    let refs: Vec<(&'static str, &str)> = facts.iter().map(|(l, v)| (*l, v.as_str())).collect();
    Some(m.with_facts(&refs))
}

fn agent_page(state: &AbyssState, id: u64) -> Option<Modal> {
    let principal = state.agents.principal_of(id)?;
    let paused = lifecycle::is_paused(state, id);
    let buttons = vec![
        if paused {
            btn("Resume", Role::Grant)
        } else {
            btn("Pause", Role::Other)
        },
        btn("Revoke grants", Role::Other),
        btn("Terminate", Role::Other),
        btn("Next agent", Role::Other),
        btn("Back", Role::Safe),
    ];
    let m = Modal::new(
        AGENT,
        AGENT_HEADING,
        None,
        "",
        "Recent actions come from policyd's journal:",
        "Use ec-audit trace for the full chain.",
        buttons,
    )
    .ok()?;
    let task = state
        .agents
        .task_of(id)
        .map(|t| t.to_string())
        .unwrap_or_else(|| "(no task)".into());
    let grants = state.agents.grants_of(id);
    let mut facts: Vec<(&'static str, String)> = vec![
        ("Agent", format!("#{id} {principal}")),
        ("Task", task),
        ("State", state_word(state, id).into()),
    ];
    if grants.is_empty() {
        facts.push(("Grants", "none".into()));
    }
    for g in grants.iter().take(4) {
        facts.push(("Grant", g.clone()));
    }
    match &state.trusted_ui.panel.tail {
        Some((a, _, records)) if *a == id => {
            if records.is_empty() {
                facts.push(("Actions", "none recorded".into()));
            }
            for r in records.iter().take(TAIL as usize) {
                let req = r.req_id.map(|q| format!(" req {q}")).unwrap_or_default();
                facts.push((
                    "Action",
                    format!("{}{req} at {}", r.kind, r.at_ns / 1_000_000_000),
                ));
            }
        }
        _ => facts.push(("Actions", "asking policyd...".into())),
    }
    let refs: Vec<(&'static str, &str)> = facts.iter().map(|(l, v)| (*l, v.as_str())).collect();
    Some(m.with_facts(&refs))
}

fn confirm(state: &AbyssState, what: Destructive) -> Option<Modal> {
    let (body, label, who) = match what {
        Destructive::Revoke(id) => (REVOKE_BODY, "Revoke", state.agents.principal_of(id)),
        Destructive::Terminate(id) => (TERMINATE_BODY, "Terminate", state.agents.principal_of(id)),
        Destructive::TerminateAll => (TERMINATE_ALL_BODY, "Terminate all", Some("every agent".into())),
    };
    let m = Modal::new(
        CONFIRM,
        CONFIRM_HEADING,
        Some("This cannot be undone from here."),
        body,
        "Applies to:",
        who.as_deref().unwrap_or("(gone)"),
        vec![btn("Cancel", Role::Safe), btn(label, Role::Other)],
    )
    .ok()?;
    Some(m.irreversible())
}

fn build(state: &AbyssState, page: Page) -> Option<Modal> {
    match page {
        Page::Closed => None,
        Page::Overview => overview(state),
        Page::Agent(id) => agent_page(state, id),
        Page::Confirm(d) => confirm(state, d),
    }
}

fn show(state: &mut AbyssState, page: Page) {
    state.trusted_ui.panel.page = page;
    if let Page::Agent(id) = page {
        fetch_tail(state, id);
    }
    match build(state, page) {
        Some(m) => {
            state.trusted_ui.panel.shown = m.buttons().iter().map(|b| b.label).collect();
            if !super::open(state, m) {
                // Another prompt holds the seat; the panel comes back with
                // the next chord.
                state.trusted_ui.panel.page = Page::Closed;
            }
        }
        // The agent this page was about is gone: back to the overview.
        None if page != Page::Overview && page != Page::Closed => show(state, Page::Overview),
        None => state.trusted_ui.panel.page = Page::Closed,
    }
}

fn fetch_tail(state: &mut AbyssState, id: u64) {
    let Some(principal) = state.agents.principal_of(id) else {
        return;
    };
    state.trusted_ui.panel.next_req += 1;
    let req = state.trusted_ui.panel.next_req;
    // Until the answer comes the page says it is asking.
    state.trusted_ui.panel.tail = None;
    crate::policy::link::send(
        state,
        &ToPolicyd::AuditTail {
            req,
            principal,
            n: TAIL,
        },
    );
    state.trusted_ui.panel.pending = Some((id, req));
}

/// Open the panel on its overview. The override chord calls this after
/// pausing every agent.
pub fn open(state: &mut AbyssState) {
    if state.trusted_ui.panel.page != Page::Closed && state.trusted_ui.is_open() {
        return;
    }
    show(state, Page::Overview);
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    matches!(token, OVERVIEW | AGENT | CONFIRM) && state.trusted_ui.panel.page != Page::Closed
}

fn next_agent(state: &AbyssState, after: u64) -> Option<u64> {
    let ids: Vec<u64> = state.agents.list().into_iter().map(|(i, _)| i).collect();
    ids.iter()
        .copied()
        .find(|i| *i > after)
        .or_else(|| ids.first().copied())
}

pub fn answer(state: &mut AbyssState, choice: Choice) {
    let page = state.trusted_ui.panel.page;
    let label = button_label(state, page, choice.button);
    // A timeout picks the safe button: close or back, never an action.
    match (page, label) {
        (Page::Overview, Some("Resume all")) => {
            lifecycle::resume_all(state);
            show(state, Page::Overview);
        }
        (Page::Overview, Some("Terminate all")) => show(state, Page::Confirm(Destructive::TerminateAll)),
        (Page::Overview, Some("Agents")) => match next_agent(state, 0) {
            Some(id) => show(state, Page::Agent(id)),
            None => show(state, Page::Overview),
        },
        (Page::Overview, Some("Change secret phrase")) => {
            state.trusted_ui.panel.page = Page::Closed;
            super::phrase::prompt_change(state);
        }
        (Page::Agent(id), Some("Pause")) => {
            lifecycle::pause(state, id);
            show(state, Page::Agent(id));
        }
        (Page::Agent(id), Some("Resume")) => {
            lifecycle::resume(state, id);
            show(state, Page::Agent(id));
        }
        (Page::Agent(id), Some("Revoke grants")) => show(state, Page::Confirm(Destructive::Revoke(id))),
        (Page::Agent(id), Some("Terminate")) => show(state, Page::Confirm(Destructive::Terminate(id))),
        (Page::Agent(id), Some("Next agent")) => match next_agent(state, id) {
            Some(n) => show(state, Page::Agent(n)),
            None => show(state, Page::Overview),
        },
        (Page::Agent(_), _) => show(state, Page::Overview),
        (Page::Confirm(d), Some("Revoke" | "Terminate" | "Terminate all")) if choice.role == Role::Other => {
            match d {
                Destructive::Revoke(id) => {
                    lifecycle::revoke(state, id);
                    show(state, Page::Agent(id));
                }
                Destructive::Terminate(id) => {
                    lifecycle::terminate(state, id);
                    show(state, Page::Overview);
                }
                Destructive::TerminateAll => {
                    lifecycle::terminate_all(state);
                    show(state, Page::Overview);
                }
            }
        }
        (Page::Confirm(Destructive::Revoke(id) | Destructive::Terminate(id)), _) => {
            show(state, Page::Agent(id))
        }
        (Page::Confirm(Destructive::TerminateAll), _) => show(state, Page::Overview),
        // Close, or anything else: the panel goes away.
        _ => state.trusted_ui.panel.page = Page::Closed,
    }
}

fn button_label(state: &AbyssState, _page: Page, i: usize) -> Option<&'static str> {
    state.trusted_ui.panel.shown.get(i).copied()
}

/// `policyd`'s answer to an `audit_tail` request.
pub fn audit_records(state: &mut AbyssState, req: u64, records: Vec<TailRecord>) {
    let Some((id, want)) = state.trusted_ui.panel.pending else {
        return;
    };
    if want != req {
        return;
    }
    state.trusted_ui.panel.pending = None;
    state.trusted_ui.panel.tail = Some((id, req, records));
    if state.trusted_ui.panel.page == Page::Agent(id) {
        if let Some(m) = agent_page(state, id) {
            let labels: Vec<&'static str> = m.buttons().iter().map(|b| b.label).collect();
            if super::replace(state, m) {
                state.trusted_ui.panel.shown = labels;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::keyboard::Keysym;

    fn press(s: &mut AbyssState, sym: Keysym) {
        super::super::arm_now(s);
        super::super::key(s, sym);
    }

    #[test]
    fn the_panel_opens_on_the_overview_and_escape_closes_it() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        open(s);
        assert_eq!(s.trusted_ui.token(), Some(OVERVIEW));
        assert!(super::super::holds_seat(s));
        press(s, Keysym::Escape);
        assert!(!s.trusted_ui.is_open());
        assert_eq!(s.trusted_ui.panel.page, Page::Closed);
    }

    #[test]
    fn with_no_agents_only_the_phrase_and_close_are_offered() {
        let h = crate::shell::focus::state_tests::harness();
        let m = overview(&h.state).unwrap();
        let labels: Vec<&str> = m.buttons().iter().map(|b| b.label).collect();
        assert_eq!(labels, ["Change secret phrase", "Close"]);
    }

    #[test]
    fn a_timeout_or_escape_on_a_confirm_never_acts() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        s.trusted_ui.panel.page = Page::Confirm(Destructive::TerminateAll);
        let m = confirm(s, Destructive::TerminateAll).unwrap();
        s.trusted_ui.panel.shown = m.buttons().iter().map(|b| b.label).collect();
        assert_eq!(m.buttons()[m.safe()].label, "Cancel");
        // The safe button resolves back to the overview, terminating nothing.
        answer(
            s,
            Choice {
                token: CONFIRM,
                button: m.safe(),
                role: Role::Safe,
                typed: None,
                timed_out: true,
            },
        );
        assert_eq!(s.trusted_ui.panel.page, Page::Overview);
    }

    #[test]
    fn an_answer_is_read_against_the_buttons_that_were_drawn() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        s.trusted_ui.panel.page = Page::Agent(5);
        s.trusted_ui.panel.shown = vec!["Pause", "Revoke grants", "Terminate", "Next agent", "Back"];
        // Whatever the agent's state is now, button 0 is the Pause that was
        // on screen.
        assert_eq!(button_label(s, Page::Agent(5), 0), Some("Pause"));
        assert_eq!(button_label(s, Page::Agent(5), 9), None);
    }

    #[test]
    fn the_audit_tail_fills_the_agent_page_only_for_its_own_request() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        s.trusted_ui.panel.pending = Some((7, 3));
        audit_records(s, 2, vec![]);
        assert!(s.trusted_ui.panel.tail.is_none(), "a stale answer is dropped");
        audit_records(
            s,
            3,
            vec![TailRecord {
                kind: "request".into(),
                req_id: Some(9),
                at_ns: 5_000_000_000,
            }],
        );
        assert_eq!(s.trusted_ui.panel.tail.as_ref().map(|t| t.2.len()), Some(1));
    }
}
