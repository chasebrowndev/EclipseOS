// SPDX-License-Identifier: AGPL-3.0-only
//! The enforcement order for acting agent requests (COMP-08 §10). TCB.
//!
//! Every `eclipse_agent_seat_v1` request comes through [`submit`] as an
//! [`Act`]. This module decides; `protocols::agent::seat` only executes what
//! it is told to and carries the answer back. The order, and where each step
//! stands:
//!
//! 1. Validate arguments: `invalid_argument`.
//! 2. Dedupe: not yet (M14).
//! 3. Paused (policyd down, or paused by the human): `paused`.
//! 4. Capability and scope: the agent's view for the act's capability
//!    resolves the target (a handle, a point, or the seat's focus) or the
//!    request is `no_capability` / `out_of_scope`.
//! 5. Rate and quota: not yet (grant `rate` constraints are carried but not
//!    counted).
//! 6. Sensitivity: a `secret` window is never in any view, so it can never
//!    be a target.
//! 7. The table: `check()` with a [`RequestCtx`] built on the stack.
//!    `allow` runs; `deny` is `policy_denied`; `prompt` parks the request
//!    behind the consent prompt; `defer` asks `policyd`, bounded by
//!    [`DEFER_TIMEOUT`], and no answer is `deferred_timeout`.
//! 8. Generations: not yet (M14).
//!
//!    8b. Before a parked request runs, steps 3 to 6 run again: a human
//!    "allow" is permission, not a snapshot of the world (COMP-11 §4 step 5).
//! 9. Execute. Then `result`, and the audit records.
//!
//! Nothing is mutated before step 9. The audit `request` record goes into
//! the socket first, or the agent is answered `paused` (COMP-12 §1).

use std::time::Duration;

use ec_policy_eval::check::{self, DeferAnswer, GrantFact, NodeFacts, Outcome, ProvenanceFacts, RequestCtx};
use ec_policy_eval::link::ToPolicyd;
use ec_protocols::agent::server::eclipse_agent_v1::Status;
use smithay::desktop::Window;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::Point;

use crate::audit;
use crate::policy::scene;
use crate::state::AbyssState;
use crate::trusted_ui::consent;

/// COMP-11 §5, S-02 `defer_timeout` default.
pub const DEFER_TIMEOUT: Duration = Duration::from_millis(500);

/// The longest text one `text` request may carry.
pub const MAX_TEXT: usize = 4096;

/// One acting request, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Focus {
        handle: u64,
    },
    Key {
        keycode: u32,
        pressed: bool,
        mods: u32,
    },
    Keysym {
        keysym: u32,
        pressed: bool,
    },
    Text {
        handle: u64,
        text: String,
    },
    PointerAbs {
        x: i32,
        y: i32,
    },
    PointerRel {
        dx: f64,
        dy: f64,
    },
    Button {
        button: u32,
        pressed: bool,
    },
    Axis {
        axis: u32,
        value: f64,
        discrete: i32,
        source: u32,
    },
    TouchDown {
        id: i32,
        x: i32,
        y: i32,
    },
    TouchUp {
        id: i32,
    },
    TouchMotion {
        id: i32,
        x: i32,
        y: i32,
    },
    Click {
        handle: u64,
        x: i32,
        y: i32,
        button: u32,
    },
}

/// What an act is aimed at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Aim {
    Handle(u64),
    Point(i32, i32),
    /// The window the agent's seat has keyboard (or pointer) focus on.
    SeatFocus,
}

impl Act {
    /// The capability the act needs (S-01 §2).
    pub fn capability(&self) -> &'static str {
        match self {
            Act::Focus { .. } => "seat.focus",
            Act::Key { .. } | Act::Keysym { .. } => "seat.key",
            Act::Text { .. } => "seat.text",
            Act::PointerAbs { .. } | Act::PointerRel { .. } | Act::Button { .. } | Act::Axis { .. } => {
                "seat.pointer"
            }
            Act::TouchDown { .. } | Act::TouchUp { .. } | Act::TouchMotion { .. } => "seat.touch",
            Act::Click { .. } => "click",
        }
    }

    /// The request name, for the audit record.
    pub fn name(&self) -> &'static str {
        match self {
            Act::Focus { .. } => "focus",
            Act::Key { .. } => "key",
            Act::Keysym { .. } => "keysym",
            Act::Text { .. } => "text",
            Act::PointerAbs { .. } => "pointer_abs",
            Act::PointerRel { .. } => "pointer_rel",
            Act::Button { .. } => "button",
            Act::Axis { .. } => "axis",
            Act::TouchDown { .. } => "touch_down",
            Act::TouchUp { .. } => "touch_up",
            Act::TouchMotion { .. } => "touch_motion",
            Act::Click { .. } => "click",
        }
    }

    pub fn aim(&self) -> Aim {
        match *self {
            Act::Focus { handle } | Act::Text { handle, .. } => Aim::Handle(handle),
            Act::Click { handle: 0, x, y, .. } => Aim::Point(x, y),
            Act::Click { handle, .. } => Aim::Handle(handle),
            Act::PointerAbs { x, y } | Act::TouchDown { x, y, .. } | Act::TouchMotion { x, y, .. } => {
                Aim::Point(x, y)
            }
            _ => Aim::SeatFocus,
        }
    }

    /// Step 1. Text the compositor would never deliver is refused here.
    fn valid(&self) -> bool {
        match self {
            Act::Text { text, .. } => !text.is_empty() && text.len() <= MAX_TEXT,
            Act::Focus { handle } => *handle != 0,
            Act::PointerRel { dx, dy } => dx.is_finite() && dy.is_finite(),
            Act::Axis { value, .. } => value.is_finite(),
            _ => true,
        }
    }

    fn audit_args(&self) -> audit::Args<'_> {
        match *self {
            Act::Focus { handle } | Act::Text { handle, .. } => audit::Args::Handle(handle),
            Act::Click { handle: 0, x, y, .. } => audit::Args::Point(i64::from(x), i64::from(y)),
            Act::Click { handle, .. } => audit::Args::Handle(handle),
            Act::PointerAbs { x, y } | Act::TouchDown { x, y, .. } | Act::TouchMotion { x, y, .. } => {
                audit::Args::Point(i64::from(x), i64::from(y))
            }
            // Never keys or text by content (S-04 §2).
            _ => audit::Args::None,
        }
    }
}

/// One acting request.
#[derive(Debug, Clone)]
pub struct Request {
    pub agent: u64,
    pub req_id: u32,
    pub act: Act,
    /// `provenance_ids` was the empty array: an act with no asserted input
    /// (`provenance_absent`, COMP-08 §4.1). Not trusted for being empty.
    pub provenance_empty: bool,
    /// The batch token the act runs under (COMP-08 §4.2); 0 is none.
    pub batch_token: u64,
}

/// What [`submit`] did with a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Submitted {
    /// Answered now: the caller sends this `result`.
    Answered(Status, String),
    /// Waiting on the human or `policyd`; the answer comes through
    /// `seat::reply` later.
    Pending,
}

/// Requests waiting on a prompt or a deferral.
#[derive(Debug, Default)]
pub struct Pending {
    /// With the target's class when the prompt went up (S-05 §5 rule 3).
    prompts: Vec<(u64, Request, ec_policy_eval::Class)>,
    defers: Vec<(u64, Request, String)>,
}

fn key(agent: u64, req_id: u32) -> u64 {
    // Unique per (agent, req) for this session: agent ids are never reused.
    (agent << 32) | u64::from(req_id)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Steps 3 to 6: whether the agent may act on anything now, and on what.
fn resolve(state: &mut AbyssState, req: &Request) -> Result<Window, (Status, String)> {
    if state.policy_key.is_none() || crate::policy::lifecycle::is_paused(state, req.agent) {
        return Err((Status::Paused, String::new()));
    }
    let cap = req.act.capability();
    let Some(mut agent) = state.agents.take_agent(req.agent) else {
        return Err((Status::NoCapability, cap.to_owned()));
    };
    let target = match agent.act_view(now_ms(), cap) {
        None => Err((Status::NoCapability, cap.to_owned())),
        Some(view) => match req.act.aim() {
            Aim::Handle(h) => scene::resolve(state, view, h).ok_or((Status::OutOfScope, cap.to_owned())),
            Aim::Point(x, y) => scene::hit(state, view, Point::from((f64::from(x), f64::from(y))))
                .map(|(w, _)| w)
                .ok_or((Status::OutOfScope, cap.to_owned())),
            Aim::SeatFocus => match crate::protocols::agent::seat::focused_window(state, req.agent) {
                None => Err((Status::FocusLost, String::new())),
                Some(w) if scene::visible(state, view, &w) => Ok(w),
                Some(_) => Err((Status::OutOfScope, cap.to_owned())),
            },
        },
    };
    state.agents.put_agent(req.agent, agent);
    target
}

/// Step 7, with no allocation: the table's answer for `req` on `window`.
fn decide(state: &mut AbyssState, req: &Request, window: &Window) -> (Outcome, String) {
    let Some(agent) = state.agents.peek_agent(req.agent) else {
        return (Outcome::Deny, check::NO_MATCH.to_owned());
    };
    let Some(table) = state.policy_table.as_ref() else {
        return (Outcome::Deny, check::NO_MATCH.to_owned());
    };
    let mut facts = [GrantFact {
        capability: "",
        unattended: false,
    }; 32];
    let n = agent.grant_facts(&mut facts);
    let irreversible_capable = crate::shell::rules::irreversible_capable_of(window);
    let class = scene::class_of(state, window);
    let app_trust = crate::policy::classes::trust_of(state, window);
    let d = scene::with_facts(state, window, |w| {
        let ctx = RequestCtx {
            principal: agent.principal(),
            profile: "",
            grants: &facts[..n],
            capability: req.act.capability(),
            app_id: Some(w.app_id).filter(|a| !a.is_empty()),
            title: Some(w.title).filter(|t| !t.is_empty()),
            class,
            app_trust,
            app_irreversible_capable: irreversible_capable,
            node: None::<NodeFacts<'_>>,
            url: None,
            // The S-06 taxonomy matcher is M18: nothing matches yet.
            irreversible: None,
            provenance: ProvenanceFacts {
                empty: req.provenance_empty,
                trusts: [false; 4],
                sources: &[],
            },
        };
        check::check(table, &ctx)
    });
    (d.outcome, d.rule.to_owned())
}

/// Submit one acting request. Called by the seat protocol for every one.
pub fn submit(state: &mut AbyssState, req: Request) -> Submitted {
    if !req.act.valid() {
        return Submitted::Answered(Status::InvalidArgument, String::new());
    }
    let Some(principal) = state.agents.principal_of(req.agent) else {
        return Submitted::Answered(Status::NoCapability, req.act.capability().to_owned());
    };
    let task = state.agents.task_of(req.agent);
    let rec = audit::request(
        &principal,
        task,
        req.req_id,
        "eclipse_agent_seat_v1",
        req.act.name(),
        req.act.audit_args(),
    );
    if !audit::begin(state, rec) {
        return Submitted::Answered(Status::Paused, String::new());
    }
    let window = match resolve(state, &req) {
        Ok(w) => w,
        Err((s, d)) => return answered(state, &principal, &req, s, d),
    };
    let (outcome, rule) = decide(state, &req, &window);
    let phase = match outcome {
        Outcome::Allow => "allow",
        Outcome::Deny => "deny",
        Outcome::Prompt => "prompt",
        Outcome::Defer => "defer",
    };
    audit::agent(
        state,
        audit::decision(
            &principal,
            task,
            req.req_id,
            outcome == Outcome::Allow,
            &rule,
            phase,
            0,
        ),
    );
    match outcome {
        Outcome::Allow => execute(state, &principal, &req, &window),
        Outcome::Deny => answered(state, &principal, &req, Status::PolicyDenied, rule),
        // "prompt → batch token check first" (COMP-08 §10 step 7): a token
        // covering this target stands in for the prompt, and nothing else.
        Outcome::Prompt if req.batch_token != 0 => {
            let handle = state.ipc.existing_handle(&window).unwrap_or(0);
            match crate::policy::batch::consume(state, req.agent, req.batch_token, handle) {
                Ok(()) => execute(state, &principal, &req, &window),
                Err(s) => answered(state, &principal, &req, s, String::new()),
            }
        }
        Outcome::Prompt => park(state, &principal, req, &window, &rule),
        Outcome::Defer => defer(state, &principal, req, rule),
    }
}

fn answered(state: &mut AbyssState, principal: &str, req: &Request, s: Status, detail: String) -> Submitted {
    audit::agent(state, audit::result(principal, req.req_id, s as u32, &detail, 0));
    Submitted::Answered(s, detail)
}

/// Step 9.
fn execute(state: &mut AbyssState, principal: &str, req: &Request, window: &Window) -> Submitted {
    match crate::protocols::agent::seat::execute(state, req.agent, &req.act, window) {
        Ok(detail) => answered(state, principal, req, Status::Ok, detail),
        Err((s, d)) => answered(state, principal, req, s, d),
    }
}

/// Deliver a delayed answer and record it.
fn reply(state: &mut AbyssState, req: &Request, s: Status, detail: String) {
    if let Some(p) = state.agents.principal_of(req.agent) {
        audit::agent(state, audit::result(&p, req.req_id, s as u32, &detail, 0));
    }
    crate::protocols::agent::seat::reply(state, req.agent, req.req_id, s, &detail);
}

/// What the consent prompt shows for `req` on `window`.
fn ask(state: &AbyssState, principal: &str, req: &Request, window: &Window) -> consent::Ask {
    let (app, title) = scene::with_facts(state, window, |w| (w.app_id.to_owned(), w.title.to_owned()));
    let handle = state.ipc.existing_handle(window).unwrap_or(0);
    let cap = req.act.capability();
    let action = match &req.act {
        Act::Focus { .. } => "Focus this window".to_owned(),
        Act::Key { .. } | Act::Keysym { .. } => "Press a key".to_owned(),
        Act::Text { text, .. } => format!("Type {} characters", text.chars().count()),
        Act::Click { .. } => "Click".to_owned(),
        Act::Button { .. } => "Press a pointer button".to_owned(),
        Act::Axis { .. } => "Scroll".to_owned(),
        Act::PointerAbs { .. } | Act::PointerRel { .. } => "Move the pointer".to_owned(),
        Act::TouchDown { .. } | Act::TouchUp { .. } | Act::TouchMotion { .. } => "Touch".to_owned(),
    };
    consent::Ask {
        principal: principal.to_owned(),
        action,
        app: app.clone(),
        window: title,
        task: state
            .agents
            .task_of(req.agent)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "(no task)".into()),
        category: None,
        untrusted_source: None,
        // The narrowest scopes that cover this request (COMP-10 §3.2).
        task_scope: format!("{cap} handle:{handle}"),
        unattended_scope: format!("{cap} app_id:{}", if app.is_empty() { "-" } else { &app }),
        note: String::new(),
    }
}

fn park(state: &mut AbyssState, principal: &str, req: Request, window: &Window, rule: &str) -> Submitted {
    let a = ask(state, principal, &req, window);
    match consent::park(state, req.agent, u64::from(req.req_id), a) {
        Ok(_) => {
            tracing::info!(
                agent = req.agent,
                req = req.req_id,
                rule,
                "request parked behind a consent prompt"
            );
            let class = scene::class_of(state, window);
            state
                .enforce
                .prompts
                .push((key(req.agent, req.req_id), req, class));
            Submitted::Pending
        }
        Err(consent::Refused::RateLimited) => {
            answered(state, principal, &req, Status::RateLimited, String::new())
        }
        Err(consent::Refused::Duplicate) => {
            answered(state, principal, &req, Status::Duplicate, String::new())
        }
    }
}

fn defer(state: &mut AbyssState, principal: &str, req: Request, rule: String) -> Submitted {
    let k = key(req.agent, req.req_id);
    crate::policy::link::send(
        state,
        &ToPolicyd::Defer {
            req: k,
            principal: principal.to_owned(),
            capability: req.act.capability().to_owned(),
            rule: rule.clone(),
        },
    );
    state.enforce.defers.push((k, req, rule));
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(DEFER_TIMEOUT), move |_, _, state| {
            resolve_defer(state, k, None);
            TimeoutAction::Drop
        });
    if timer.is_err() {
        // No timer means no bounded wait: answer now, closed.
        resolve_defer(state, k, None);
    }
    Submitted::Pending
}

/// `policyd` answered deferral `k` (or the timeout did, with `None`).
fn resolve_defer(state: &mut AbyssState, k: u64, answer: Option<DeferAnswer>) {
    let Some(i) = state.enforce.defers.iter().position(|(x, ..)| *x == k) else {
        return;
    };
    let (_, req, rule) = state.enforce.defers.remove(i);
    let Some(principal) = state.agents.principal_of(req.agent) else {
        return;
    };
    // The world may have moved while policyd thought: steps 3 to 6 again.
    let window = match resolve(state, &req) {
        Ok(w) => w,
        Err((s, d)) => return reply(state, &req, s, d),
    };
    let outcome = match (answer, state.policy_table.as_ref()) {
        (None, _) | (_, None) => (Outcome::Deny, check::DEFERRED_TIMEOUT.to_owned()),
        (Some(a), Some(_)) => decide_after_defer(state, &req, &window, &rule, a),
    };
    match outcome {
        (Outcome::Allow, _) => {
            if let Submitted::Answered(s, d) = execute(state, &principal, &req, &window) {
                crate::protocols::agent::seat::reply(state, req.agent, req.req_id, s, &d);
            }
        }
        (Outcome::Prompt, r) => {
            if let Submitted::Answered(s, d) = park(state, &principal, req.clone(), &window, &r) {
                crate::protocols::agent::seat::reply(state, req.agent, req.req_id, s, &d);
            }
        }
        (_, r) if r == check::DEFERRED_TIMEOUT => reply(state, &req, Status::DeferredTimeout, String::new()),
        (_, r) => reply(state, &req, Status::PolicyDenied, r),
    }
}

fn decide_after_defer(
    state: &mut AbyssState,
    req: &Request,
    window: &Window,
    rule: &str,
    answer: DeferAnswer,
) -> (Outcome, String) {
    match answer {
        DeferAnswer::Deny => (Outcome::Deny, rule.to_owned()),
        DeferAnswer::Prompt => (Outcome::Prompt, rule.to_owned()),
        // The static allow phase only: never broader than the table.
        DeferAnswer::Fallthrough => match allow_phase(state, req, window) {
            Some(r) => (Outcome::Allow, r),
            None => (Outcome::Deny, check::NO_MATCH.to_owned()),
        },
    }
}

/// The allow phase alone, as `check::after_defer` defines fallthrough.
fn allow_phase(state: &mut AbyssState, req: &Request, window: &Window) -> Option<String> {
    let agent = state.agents.peek_agent(req.agent)?;
    let table = state.policy_table.as_ref()?;
    let mut facts = [GrantFact {
        capability: "",
        unattended: false,
    }; 32];
    let n = agent.grant_facts(&mut facts);
    let class = scene::class_of(state, window);
    let irreversible_capable = crate::shell::rules::irreversible_capable_of(window);
    let app_trust = crate::policy::classes::trust_of(state, window);
    scene::with_facts(state, window, |w| {
        let ctx = RequestCtx {
            principal: agent.principal(),
            profile: "",
            grants: &facts[..n],
            capability: req.act.capability(),
            app_id: Some(w.app_id).filter(|a| !a.is_empty()),
            title: Some(w.title).filter(|t| !t.is_empty()),
            class,
            app_trust,
            app_irreversible_capable: irreversible_capable,
            node: None,
            url: None,
            irreversible: None,
            provenance: ProvenanceFacts {
                empty: req.provenance_empty,
                trusts: [false; 4],
                sources: &[],
            },
        };
        let d = check::after_defer(table, &ctx, "", Some(DeferAnswer::Fallthrough));
        (d.outcome == Outcome::Allow).then(|| d.rule.to_owned())
    })
}

/// `policyd` answered deferral `req`.
pub fn defer_answer(state: &mut AbyssState, req: u64, answer: DeferAnswer) {
    resolve_defer(state, req, Some(answer));
}

/// `policyd` minted (or refused to mint) a grant for a prompt answer. The
/// request it was for has already run on the human's "allow"; a minted
/// grant only widens what later requests may do without asking, and is
/// admitted like any other (verified against the pinned key).
pub fn minted(state: &mut AbyssState, req: u64, grant: Option<Vec<u8>>) {
    let agent = req >> 32;
    let Some(grant) = grant else {
        tracing::info!(agent, "policyd declined to mint a grant for a prompt answer");
        return;
    };
    let key = crate::policy::table::live(state)
        .then_some(state.policy_key)
        .flatten();
    let revoked = &state.revoked;
    let r = state.agents.with_agent(agent, |a| {
        a.add_grant(&grant, key.as_ref(), now_ms(), |g| revoked.covers(g.task_id))
    });
    match r {
        Some(Ok(())) => tracing::info!(agent, "minted grant added"),
        Some(Err(e)) => tracing::warn!(agent, ?e, "minted grant refused"),
        None => {}
    }
}

/// Every prompt answer since the last call, carried through. Called by the
/// consent prompt after each answer or withdrawal.
pub fn drain(state: &mut AbyssState) {
    for r in state.trusted_ui.consent.take_resolved() {
        let Ok(req_id) = u32::try_from(r.req) else {
            continue;
        };
        let k = key(r.agent, req_id);
        let Some(i) = state.enforce.prompts.iter().position(|(x, ..)| *x == k) else {
            continue;
        };
        let (_, req, asked_class) = state.enforce.prompts.remove(i);
        match r.verdict {
            consent::Verdict::Proceed { mint } => {
                let Some(principal) = state.agents.principal_of(req.agent) else {
                    continue;
                };
                // COMP-11 §4 step 5, COMP-08 §10 step 8b: re-validate.
                let window = match resolve(state, &req) {
                    Ok(w) => w,
                    Err((s, d)) => {
                        reply(state, &req, s, d);
                        continue;
                    }
                };
                // S-05 §5 rule 3: the human approved an act on a window of
                // one class; if it has risen since, the approval is not for
                // what is there now.
                if scene::class_of(state, &window) > asked_class {
                    reply(state, &req, Status::ClassChanged, String::new());
                    continue;
                }
                if let Some(m) = mint {
                    crate::policy::link::send(
                        state,
                        &ToPolicyd::Mint {
                            req: k,
                            principal: principal.clone(),
                            scope: m.scope,
                            unattended: m.unattended,
                        },
                    );
                }
                if let Submitted::Answered(s, d) = execute(state, &principal, &req, &window) {
                    crate::protocols::agent::seat::reply(state, req.agent, req.req_id, s, &d);
                }
            }
            consent::Verdict::Denied => reply(state, &req, Status::PromptDenied, String::new()),
            consent::Verdict::DeniedAndPause => {
                crate::policy::lifecycle::pause(state, req.agent);
                reply(state, &req, Status::PromptDenied, String::new());
            }
            consent::Verdict::TimedOut => reply(state, &req, Status::PromptTimeout, String::new()),
            consent::Verdict::Withdrawn => {}
        }
    }
}

/// Drop everything pending for `agent` (it went away).
pub fn forget(state: &mut AbyssState, agent: u64) {
    crate::policy::batch::forget(state, agent);
    state.enforce.prompts.retain(|(_, r, _)| r.agent != agent);
    state.enforce.defers.retain(|(_, r, _)| r.agent != agent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_act_names_a_seat_capability() {
        let acts = [
            Act::Focus { handle: 1 },
            Act::Key {
                keycode: 30,
                pressed: true,
                mods: 0,
            },
            Act::Keysym {
                keysym: 0x61,
                pressed: true,
            },
            Act::Text {
                handle: 1,
                text: "a".into(),
            },
            Act::PointerAbs { x: 0, y: 0 },
            Act::PointerRel { dx: 1.0, dy: 1.0 },
            Act::Button {
                button: 0x110,
                pressed: true,
            },
            Act::Axis {
                axis: 0,
                value: 1.0,
                discrete: 0,
                source: 0,
            },
            Act::TouchDown { id: 0, x: 0, y: 0 },
            Act::TouchUp { id: 0 },
            Act::TouchMotion { id: 0, x: 0, y: 0 },
            Act::Click {
                handle: 1,
                x: 0,
                y: 0,
                button: 0x110,
            },
        ];
        for a in acts {
            assert!(crate::policy::SEAT_CAPS.contains(&a.capability()), "{a:?}");
        }
    }

    #[test]
    fn malformed_acts_are_invalid() {
        assert!(!Act::Text {
            handle: 1,
            text: String::new()
        }
        .valid());
        assert!(!Act::Text {
            handle: 1,
            text: "x".repeat(MAX_TEXT + 1)
        }
        .valid());
        assert!(!Act::Focus { handle: 0 }.valid());
        assert!(!Act::PointerRel {
            dx: f64::NAN,
            dy: 0.0
        }
        .valid());
    }

    #[test]
    fn keys_and_text_are_never_audited_by_content() {
        let text = Act::Text {
            handle: 3,
            text: "hunter2".into(),
        };
        assert!(!format!("{:?}", text.audit_args()).contains("hunter2"));
        assert!(matches!(
            Act::Key {
                keycode: 30,
                pressed: true,
                mods: 0
            }
            .audit_args(),
            audit::Args::None
        ));
    }
}
