// SPDX-License-Identifier: AGPL-3.0-only
//! The agent consent prompt (COMP-10 §3.2) and the parked requests behind it
//! (COMP-11 §4). **TCB.**
//!
//! A request the enforcement check answers `Prompt` is [`park`]ed here,
//! keyed by `(agent, req_id)`, and its protocol reply is withheld. Parked
//! requests are bounded at [`MAX_PER_AGENT`] per agent: past that a new one
//! is refused at once (`rate_limited`) rather than queueing a prompt storm.
//! They are shown one at a time, oldest first, whenever no other prompt
//! holds the seat.
//!
//! Each answer becomes a [`Resolved`] entry in the queue's outbox, which the
//! agent protocol layer drains. A [`Verdict::Proceed`] is permission, not a
//! snapshot of the world (COMP-11 §4 step 5): before executing, the caller
//! must re-run the enforcement order from capability through the class
//! re-check (COMP-08 §10, steps 4 to 8b) and refuse if anything moved.
//!
//! The prompt itself follows §3.2:
//!
//! - Facts the compositor holds (principal, action, app, window, task
//!   statement, category) are labelled single lines. The agent's note is
//!   the only agent-written text and goes in the untrusted block.
//! - Irreversible categories get the accent edge, the word "irreversible",
//!   and the taxonomy's canned reversal wording. Never free text.
//! - An untrusted source in the provenance chain always adds the warning
//!   line and the head source. The caller cannot leave it out: it is drawn
//!   from [`Ask::untrusted_source`] being set.
//! - Both "Allow for this task" and "Allow unattended 1h" show the exact
//!   scope they would grant, before the human answers.
//! - Default focus is Deny, Escape is Deny, Enter never allows, and the
//!   timeout is `prompt_timeout`, a denial.
//!
//! Not yet: nothing parks a request. Agent seats (M13) bring the first
//! actions that can return `Prompt`, and with them the caller of [`park`]
//! and the drain of [`Queue::take_resolved`]. Minting the grant for a task
//! or unattended answer is `policyd`'s; the outbox carries the scope to ask
//! for.

use std::collections::{BTreeMap, VecDeque};

use super::{
    modal::{Button, Modal, Role},
    Choice,
};
use crate::state::AbyssState;

/// COMP-11 §4: parked prompt-class requests per agent.
pub const MAX_PER_AGENT: usize = 8;

/// Tokens from here up to `phrase::TOKEN` are consent prompts. Below
/// `erase::TOKEN_BASE`, so the pointer may answer them.
const TOKEN_BASE: u64 = 1 << 62;

/// The S-06 §2 `reversal` property, as the canned sentence the prompt shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reversal {
    /// "This cannot be undone from here."
    None,
    /// "This goes to the trash."
    Trash,
    /// "Undoing this needs the other party."
    OtherParty,
}

impl Reversal {
    pub fn wording(self) -> &'static str {
        match self {
            Reversal::None => "Irreversible. This cannot be undone from here.",
            Reversal::Trash => "Irreversible. This goes to the trash.",
            Reversal::OtherParty => "Irreversible. Undoing this needs the other party.",
        }
    }
}

/// What the prompt shows about one parked request. Every field but `note`
/// is a fact the compositor holds; `note` is the agent's own words.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub principal: String,
    /// The concrete action, composed by the compositor from the request
    /// (`Click "Send"`).
    pub action: String,
    pub app: String,
    pub window: String,
    /// The task statement, fixed at task creation (A-04 §11).
    pub task: String,
    /// The matched irreversible taxonomy id and its reversal, if any.
    pub category: Option<(String, Reversal)>,
    /// The provenance chain's head source when its minimum trust is
    /// `untrusted`.
    pub untrusted_source: Option<String>,
    /// The exact scopes "for this task" and "unattended 1h" would grant.
    pub task_scope: String,
    pub unattended_scope: String,
    /// `agent_note`, untrusted.
    pub note: String,
}

/// The human's answer (COMP-11 §4 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    AllowForTask,
    AllowUnattended,
    Deny,
    DenyAndPause,
}

/// The buttons, in drawn order. Deny is the one `Safe` button.
const BUTTONS: [(Answer, Button); 5] = [
    (
        Answer::AllowOnce,
        Button {
            label: "Allow once",
            role: Role::Grant,
        },
    ),
    (
        Answer::AllowForTask,
        Button {
            label: "Allow for this task",
            role: Role::Grant,
        },
    ),
    (
        Answer::AllowUnattended,
        Button {
            label: "Allow unattended 1h",
            role: Role::Grant,
        },
    ),
    (
        Answer::Deny,
        Button {
            label: "Deny",
            role: Role::Safe,
        },
    ),
    (
        Answer::DenyAndPause,
        Button {
            label: "Deny & pause agent",
            role: Role::Other,
        },
    ),
];

pub fn answer_of(button: usize) -> Answer {
    // Out of range cannot come from a prompt built here; it denies anyway.
    BUTTONS.get(button).map_or(Answer::Deny, |(a, _)| *a)
}

const HEADING: &str = "An agent is asking to act";
const UNTRUSTED: &str = "Part of this action's input came from an untrusted source.";
const WELL: &str = "Agent's note (untrusted):";

/// Build the §3.2 prompt for `ask`.
pub fn modal(token: u64, ask: &Ask) -> Option<Modal> {
    let warning = match (&ask.untrusted_source, &ask.category) {
        (Some(_), _) => Some(UNTRUSTED),
        (None, Some((_, r))) => Some(r.wording()),
        (None, None) => None,
    };
    let body = match (&ask.untrusted_source, &ask.category) {
        // Both apply: the untrusted line is the warning, so the reversal
        // wording moves to the body rather than being dropped.
        (Some(_), Some((_, r))) => r.wording(),
        _ => "",
    };
    let buttons = BUTTONS.iter().map(|(_, b)| *b).collect();
    let m = Modal::new(token, HEADING, warning, body, WELL, &ask.note, buttons).ok()?;
    let category = ask
        .category
        .as_ref()
        .map(|(id, _)| format!("{id} (irreversible)"))
        .unwrap_or_else(|| "routine".into());
    let source = ask.untrusted_source.as_deref().unwrap_or("none");
    let m = m.with_facts(&[
        ("Agent", &ask.principal),
        ("Wants to", &ask.action),
        ("In", &ask.app),
        ("Window", &ask.window),
        ("Task", &ask.task),
        ("Category", &category),
        ("Untrusted input", source),
    ]);
    // The scopes are shown whole or not at all: a prompt that could not show
    // exactly what it grants is not built, and the request is denied.
    let m = m
        .with_whole_fact("For this task", &ask.task_scope, 3)
        .and_then(|m| m.with_whole_fact("Unattended 1h", &ask.unattended_scope, 3))
        .ok()?;
    Some(if ask.category.is_some() {
        m.irreversible()
    } else {
        m
    })
}

/// What happened to a parked request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Allowed. `mint` is the scope to ask `policyd` to grant, for "for this
    /// task" and "unattended". Re-validate before executing.
    Proceed { mint: Option<Mint> },
    /// `prompt_denied`.
    Denied,
    /// `prompt_denied`, and pause the agent's seats.
    DeniedAndPause,
    /// `prompt_timeout`, a denial.
    TimedOut,
    /// The agent went away before an answer.
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mint {
    pub scope: String,
    pub unattended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub agent: u64,
    pub req: u64,
    pub verdict: Verdict,
}

/// Why [`park`] did not park a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// `rate_limited`: [`MAX_PER_AGENT`] already parked for this agent.
    RateLimited,
    /// `(agent, req_id)` is already parked.
    Duplicate,
}

#[derive(Debug, Clone)]
struct Parked {
    agent: u64,
    req: u64,
    ask: Ask,
}

/// Parked requests, the one on screen, and answers not yet collected.
#[derive(Debug, Default)]
pub struct Queue {
    parked: BTreeMap<u64, Parked>,
    order: VecDeque<u64>,
    showing: Option<u64>,
    next: u64,
    resolved: VecDeque<Resolved>,
}

impl Queue {
    fn count(&self, agent: u64) -> usize {
        self.parked.values().filter(|p| p.agent == agent).count()
    }

    /// Park one request; returns its prompt token.
    pub fn park(&mut self, agent: u64, req: u64, ask: Ask) -> Result<u64, Refused> {
        if self.parked.values().any(|p| p.agent == agent && p.req == req) {
            return Err(Refused::Duplicate);
        }
        if self.count(agent) >= MAX_PER_AGENT {
            return Err(Refused::RateLimited);
        }
        self.next += 1;
        let token = TOKEN_BASE | self.next;
        self.parked.insert(token, Parked { agent, req, ask });
        self.order.push_back(token);
        Ok(token)
    }

    /// The oldest parked request not yet shown, marked as showing.
    fn next_to_show(&mut self) -> Option<(u64, &Ask)> {
        if self.showing.is_some() {
            return None;
        }
        let token = *self.order.front()?;
        self.showing = Some(token);
        self.parked.get(&token).map(|p| (token, &p.ask))
    }

    /// Turn the answer to prompt `token` into a verdict.
    fn answer(&mut self, token: u64, answer: Answer, timed_out: bool) -> Option<Resolved> {
        let p = self.parked.remove(&token)?;
        self.order.retain(|&t| t != token);
        if self.showing == Some(token) {
            self.showing = None;
        }
        let verdict = match (timed_out, answer) {
            (true, _) => Verdict::TimedOut,
            (false, Answer::AllowOnce) => Verdict::Proceed { mint: None },
            (false, Answer::AllowForTask) => Verdict::Proceed {
                mint: Some(Mint {
                    scope: p.ask.task_scope.clone(),
                    unattended: false,
                }),
            },
            (false, Answer::AllowUnattended) => Verdict::Proceed {
                mint: Some(Mint {
                    scope: p.ask.unattended_scope.clone(),
                    unattended: true,
                }),
            },
            (false, Answer::Deny) => Verdict::Denied,
            (false, Answer::DenyAndPause) => Verdict::DeniedAndPause,
        };
        let r = Resolved {
            agent: p.agent,
            req: p.req,
            verdict,
        };
        self.resolved.push_back(r.clone());
        Some(r)
    }

    /// Withdraw everything `agent` has parked (it disconnected or was
    /// terminated). Returns the token on screen if it was one of them.
    pub fn withdraw(&mut self, agent: u64) -> Option<u64> {
        let gone: Vec<u64> = self
            .parked
            .iter()
            .filter(|(_, p)| p.agent == agent)
            .map(|(t, _)| *t)
            .collect();
        let mut on_screen = None;
        for t in gone {
            if let Some(p) = self.parked.remove(&t) {
                self.resolved.push_back(Resolved {
                    agent: p.agent,
                    req: p.req,
                    verdict: Verdict::Withdrawn,
                });
            }
            self.order.retain(|&o| o != t);
            if self.showing == Some(t) {
                self.showing = None;
                on_screen = Some(t);
            }
        }
        on_screen
    }

    /// Answers since the last call, oldest first.
    pub fn take_resolved(&mut self) -> Vec<Resolved> {
        self.resolved.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.parked.is_empty()
    }

    /// Parked requests, the one on screen included.
    pub fn len(&self) -> usize {
        self.parked.len()
    }
}

/// Park a request and show it if nothing else holds the seat.
pub fn park(state: &mut AbyssState, agent: u64, req: u64, ask: Ask) -> Result<u64, Refused> {
    let token = state.trusted_ui.consent.park(agent, req, ask)?;
    schedule(state);
    Ok(token)
}

/// Withdraw an agent's parked requests, taking its prompt down if it is up.
pub fn withdraw(state: &mut AbyssState, agent: u64) {
    if let Some(token) = state.trusted_ui.consent.withdraw(agent) {
        super::cancel(state, token);
    }
    super::batch::withdraw(state, agent);
    crate::policy::enforce::drain(state);
    schedule(state);
}

/// Show the oldest parked request, if no prompt is up.
pub fn schedule(state: &mut AbyssState) {
    if state.trusted_ui.is_open() {
        return;
    }
    let Some((token, m)) = state
        .trusted_ui
        .consent
        .next_to_show()
        .map(|(t, ask)| (t, modal(t, ask)))
    else {
        return;
    };
    let opened = m.is_some_and(|m| super::open(state, m));
    if !opened {
        // A prompt that cannot be drawn cannot be answered: deny it now
        // rather than leave the agent waiting on nothing.
        tracing::error!(token, "consent prompt could not be opened; denied");
        state.trusted_ui.consent.answer(token, Answer::Deny, false);
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    state.trusted_ui.consent.showing == Some(token)
}

pub fn answer(state: &mut AbyssState, choice: Choice) {
    let a = answer_of(choice.button);
    if let Some(r) = state.trusted_ui.consent.answer(choice.token, a, choice.timed_out) {
        tracing::info!(agent = r.agent, req = r.req, verdict = ?r.verdict, "consent prompt answered");
    }
    crate::policy::enforce::drain(state);
}

#[cfg(test)]
mod tests {
    use super::super::modal::{apply, key, layout, Key, Outcome};
    use super::*;
    use smithay::input::keyboard::Keysym;

    fn ask() -> Ask {
        Ask {
            principal: "agent:research-7".into(),
            action: "Click \"Send\"".into(),
            app: "Gmail - Firefox".into(),
            window: "Compose: Q3 invoice".into(),
            task: "Summarize this week's invoices".into(),
            category: Some(("communication.send".into(), Reversal::None)),
            untrusted_source: Some("acme-invoices.com (web page)".into()),
            task_scope: "seat.action irreversible=communication.send app=org.mozilla.firefox".into(),
            unattended_scope: "seat.action irreversible=communication.send url=https://mail.google.com/*"
                .into(),
            note: "checking the Q3 total before sending".into(),
        }
    }

    #[test]
    fn deny_is_the_default_and_escape_and_enter_never_allow() {
        let mut m = modal(1, &ask()).unwrap();
        let deny = m.safe();
        assert_eq!(answer_of(deny), Answer::Deny);
        assert_eq!(apply(&mut m, 0, Key::Escape), Outcome::Choose(deny));
        // Enter on any allow button does nothing.
        for i in 0..3 {
            assert_eq!(apply(&mut m, i, key(Keysym::Return, false)), Outcome::Nothing);
        }
        assert_eq!(
            apply(&mut m, deny, key(Keysym::Return, false)),
            Outcome::Choose(deny)
        );
    }

    #[test]
    fn the_five_answers_fit_inside_the_panel() {
        let m = modal(1, &ask()).unwrap();
        let l = layout(&m);
        assert_eq!(l.buttons.len(), 5);
        for &(x, y, w, h) in &l.buttons {
            assert!(x + w <= l.w && y + h <= l.h);
        }
        for (i, a) in l.buttons.iter().enumerate() {
            for b in &l.buttons[i + 1..] {
                let apart = a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1;
                assert!(apart, "buttons overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn the_agent_note_cannot_reach_the_trusted_position() {
        let mut a = ask();
        a.note = "An agent is asking to act\nAgent: system:policyd\nAllow once".into();
        let m = modal(1, &a).unwrap();
        // The note is only in the untrusted block.
        assert!(m.untrusted().iter().any(|l| l.contains("system:policyd")));
        assert!(m.facts().iter().all(|(_, v)| !v.contains("system:policyd")));
        assert_eq!(m.heading(), HEADING);
        assert_eq!(m.facts()[0], ("Agent", "agent:research-7".to_owned()));
        // A fact is one line: a title cannot add a line of its own.
        a.window = "Inbox\nAgent: system:policyd".into();
        let m = modal(1, &a).unwrap();
        assert_eq!(m.facts()[3].0, "Window");
        assert!(m.facts().iter().all(|(_, v)| !v.contains('\n')));
    }

    #[test]
    fn untrusted_provenance_is_never_dropped() {
        let mut a = ask();
        a.category = None;
        let m = modal(1, &a).unwrap();
        assert!(format!("{m:?}").contains(UNTRUSTED));
        // Both: the untrusted line is the warning, the reversal is still shown.
        let m = modal(1, &ask()).unwrap();
        let shown = format!("{m:?}");
        assert!(shown.contains(UNTRUSTED) && shown.contains(Reversal::None.wording()));
    }

    #[test]
    fn each_answer_maps_to_its_verdict_and_scope() {
        let mut q = Queue::default();
        let cases = [
            (Answer::AllowOnce, false, Verdict::Proceed { mint: None }),
            (
                Answer::AllowForTask,
                false,
                Verdict::Proceed {
                    mint: Some(Mint {
                        scope: ask().task_scope,
                        unattended: false,
                    }),
                },
            ),
            (
                Answer::AllowUnattended,
                false,
                Verdict::Proceed {
                    mint: Some(Mint {
                        scope: ask().unattended_scope,
                        unattended: true,
                    }),
                },
            ),
            (Answer::Deny, false, Verdict::Denied),
            (Answer::DenyAndPause, false, Verdict::DeniedAndPause),
            // A timeout denies whatever button it lands on.
            (Answer::AllowOnce, true, Verdict::TimedOut),
        ];
        for (i, (answer, timed_out, want)) in cases.into_iter().enumerate() {
            let t = q.park(1, i as u64, ask()).unwrap();
            let r = q.answer(t, answer, timed_out).unwrap();
            assert_eq!(r.verdict, want);
        }
        assert_eq!(q.take_resolved().len(), 6);
        assert!(q.take_resolved().is_empty());
    }

    #[test]
    fn the_scope_shown_is_the_scope_minted() {
        let mut q = Queue::default();
        let a = ask();
        let m = modal(1, &a).unwrap();
        let shown = format!("{m:?}");
        let t = q.park(1, 1, a.clone()).unwrap();
        let Verdict::Proceed { mint: Some(mint) } = q.answer(t, Answer::AllowForTask, false).unwrap().verdict
        else {
            panic!("not a mint");
        };
        let on_screen: String = m
            .facts()
            .iter()
            .skip_while(|(l, _)| *l != "For this task")
            .take_while(|(l, _)| *l == "For this task" || l.is_empty())
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            on_screen, mint.scope,
            "the minted scope is the scope on screen, whole"
        );
        let _ = shown;
    }

    #[test]
    fn a_scope_too_long_to_show_whole_is_never_prompted() {
        let mut a = ask();
        a.task_scope = "x".repeat(400);
        assert!(modal(1, &a).is_none());
    }

    #[test]
    fn parking_is_bounded_per_agent_and_refuses_duplicates() {
        let mut q = Queue::default();
        for req in 0..MAX_PER_AGENT as u64 {
            q.park(1, req, ask()).unwrap();
        }
        assert_eq!(q.park(1, 99, ask()), Err(Refused::RateLimited));
        // Another agent has its own budget.
        assert!(q.park(2, 0, ask()).is_ok());
        assert_eq!(q.park(2, 0, ask()), Err(Refused::Duplicate));
    }

    #[test]
    fn shown_one_at_a_time_oldest_first_and_withdrawn_with_the_agent() {
        let mut q = Queue::default();
        let a = q.park(1, 10, ask()).unwrap();
        let b = q.park(2, 20, ask()).unwrap();
        assert_eq!(q.next_to_show().map(|(t, _)| t), Some(a));
        assert_eq!(q.next_to_show(), None, "one at a time");
        assert_eq!(q.withdraw(1), Some(a));
        assert_eq!(q.next_to_show().map(|(t, _)| t), Some(b));
        let r = q.take_resolved();
        assert_eq!(
            r,
            vec![Resolved {
                agent: 1,
                req: 10,
                verdict: Verdict::Withdrawn
            }]
        );
    }

    #[test]
    fn a_routine_request_has_no_accent_and_says_so() {
        let mut a = ask();
        a.category = None;
        a.untrusted_source = None;
        let m = modal(1, &a).unwrap();
        let shown = format!("{m:?}");
        assert!(shown.contains("accent: false") && shown.contains("routine"));
        let m = modal(1, &ask()).unwrap();
        assert!(format!("{m:?}").contains("accent: true"));
    }

    #[test]
    fn a_parked_request_is_prompted_on_the_seat_and_escape_denies_it() {
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        let token = park(s, 7, 42, ask()).unwrap();
        assert_eq!(s.trusted_ui.token(), Some(token), "shown at once");
        assert!(super::super::holds_seat(s));
        // A second request waits behind the first.
        let second = park(s, 7, 43, ask()).unwrap();
        assert_eq!(s.trusted_ui.token(), Some(token));
        // Escape counts even before the prompt is armed. The answer is
        // carried through at once (`policy::enforce::drain` takes the outbox),
        // so what shows is that the next request comes up.
        super::super::key(s, Keysym::Escape);
        assert!(
            s.trusted_ui.consent.take_resolved().is_empty(),
            "drained on answer"
        );
        assert_eq!(s.trusted_ui.token(), Some(second), "the next one comes up");
        withdraw(s, 7);
        assert!(!s.trusted_ui.is_open(), "withdrawn with its agent");
        assert!(s.trusted_ui.consent.is_empty());
    }
}
