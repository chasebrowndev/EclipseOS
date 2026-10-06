// SPDX-License-Identifier: AGPL-3.0-only
//! Atomic batches (COMP-04 §4, COMP-08 §4). Not TCB, and decides nothing:
//! every step still goes through `policy::enforce::submit` when the batch is
//! committed.
//!
//! # Shape
//!
//! - `begin_atomic` opens a batch on the agent. While it is open, acting
//!   requests are *queued*, not run: nothing is mutated and no event reaches
//!   a client, so there is nothing to undo when a batch is abandoned.
//! - Each queued request keeps the window its handle named when it arrived,
//!   if the agent could see one.
//! - `commit_atomic` checks the whole batch first ([`check`]): paused, a
//!   window gone or unmapped, a step that needs the seat's focus when none is
//!   left, a `text` for a window that will not hold focus, a stale
//!   `expected_generation`. Any failure answers **every** queued request and
//!   the commit with that cause (`focus_lost`, `stale_generation`,
//!   `paused`) and runs nothing: no partial application.
//! - Then the steps run in order, in this one dispatch, so no human input and
//!   no focus change can come between them (COMP-04 §4 "no human input
//!   interleaved"; "within `max_frames`" is trivially met by running at 0).
//! - The commit's own `result` is `ok` with the step count as detail.
//!
//! An open batch also notices a focus loss as it happens: every arriving
//! request re-runs the check, so a target that disappears mid-batch aborts
//! it then, not only at commit. A batch left open is abandoned after
//! [`OPEN_TTL`].
//!
//! # What this module cannot do
//!
//! The policy decision for each step is made by `policy::enforce` (TCB) at
//! the moment the step runs, and that module has no "decide without
//! executing" entry. So a step the policy refuses *after* earlier steps ran
//! leaves those applied: the commit answers that step's status with detail
//! `partial:<n>`. `TCB-HOOK` in [`commit`] marks where an all-steps dry run
//! (steps 1 to 8 for every request, nothing executed) goes. The same gap
//! means a step that parks behind a prompt ends the batch: later steps are
//! answered `invalid_argument` `batch_interrupted`.

use std::time::{Duration, Instant};

use ec_policy_eval::check::GrantFact;
use ec_protocols::agent::server::eclipse_agent_v1::Status;
use smithay::desktop::Window;
use smithay::utils::IsAlive;

use super::{generation, seat};
use crate::policy::enforce::{Act, Aim, Request, Submitted};
use crate::policy::scene;
use crate::state::AbyssState;

/// COMP-04 §4 default.
pub const DEFAULT_FRAMES: u32 = 4;
/// The most frames a batch may ask for.
pub const MAX_FRAMES: u32 = 60;
/// Steps one batch may queue.
pub const MAX_STEPS: usize = 256;
/// How long a batch may stay open. Not in the spec: `max_frames` bounds
/// delivery, and an agent's round trips between `begin` and `commit` are
/// far longer than a few frames, so this bounds how long a forgotten batch
/// holds the agent's seat in batch mode.
pub const OPEN_TTL: Duration = Duration::from_secs(5);

/// One queued request.
#[derive(Debug)]
pub struct Queued {
    pub req: Request,
    pub expected: u32,
    /// The window `req`'s handle named when it arrived, if it was visible.
    pub window: Option<Window>,
}

#[derive(Debug)]
pub struct Batch {
    max_frames: u32,
    opened: Instant,
    /// The agent seat's focus when the batch opened.
    focus_at_begin: Option<Window>,
    queued: Vec<Queued>,
    /// Set when the batch was aborted while open: queued requests have
    /// already been answered, and the commit repeats the cause.
    aborted: Option<(Status, String)>,
}

fn now_ms() -> u64 {
    super::now_ms()
}

/// Whether a live grant of `agent` names `cap`. For capabilities the table
/// of per-capability views does not cover (`seat.atomic`, `seat.compat_lock`,
/// `seat.action`): the scope is not evaluated here.
pub(super) fn held(state: &mut AbyssState, agent: u64, cap: &str) -> bool {
    // Drops expired grants first (S-01 §4: no grace).
    let _ = state.agents.with_agent(agent, |a| a.view_at(now_ms()).is_some());
    let Some(a) = state.agents.peek_agent(agent) else {
        return false;
    };
    let mut facts = [GrantFact {
        capability: "",
        unattended: false,
    }; 32];
    let n = a.grant_facts(&mut facts);
    facts[..n].iter().any(|f| f.capability == cap)
}

/// The window `handle` names, if the agent's `cap` view reaches it. The same
/// answers `policy::enforce` gives a handle-aimed act.
pub(super) fn visible_window(
    state: &mut AbyssState,
    agent: u64,
    cap: &str,
    handle: u64,
) -> Result<Window, (Status, String)> {
    let Some(mut a) = state.agents.take_agent(agent) else {
        return Err((Status::NoCapability, cap.to_owned()));
    };
    let r = match a.act_view(now_ms(), cap) {
        None => Err((Status::NoCapability, cap.to_owned())),
        Some(view) => scene::resolve(state, view, handle).ok_or((Status::OutOfScope, cap.to_owned())),
    };
    state.agents.put_agent(agent, a);
    r
}

fn paused(state: &AbyssState, agent: u64) -> bool {
    state.policy_key.is_none() || crate::policy::lifecycle::is_paused(state, agent)
}

/// In the space and still alive: what "mapped" means for a batch target.
fn mapped(state: &AbyssState, w: &Window) -> bool {
    w.alive() && state.space.element_geometry(w).is_some()
}

/// Whether `agent` has a batch open (queueing), aborted or not.
pub(super) fn is_open(state: &mut AbyssState, agent: u64) -> bool {
    state.agents.aux_mut(agent).is_some_and(|x| x.batch.is_some())
}

/// `begin_atomic`. The answer for the caller to send.
pub(super) fn begin(state: &mut AbyssState, agent: u64, max_frames: u32) -> (Status, String) {
    if paused(state, agent) {
        return (Status::Paused, String::new());
    }
    // TCB-HOOK: run `check()` for capability `seat.atomic` here (step 7), and
    // add `seat.atomic` to `policy::SEAT_CAPS` so its scope is compiled. Until
    // then only the capability being held is tested, which is enough because
    // every queued step is still decided on its own at commit.
    if !held(state, agent, "seat.atomic") {
        return (Status::NoCapability, "seat.atomic".to_owned());
    }
    let frames = match max_frames {
        0 => DEFAULT_FRAMES,
        n if n <= MAX_FRAMES => n,
        _ => return (Status::InvalidArgument, "max_frames".to_owned()),
    };
    let focus = seat::focused_window(state, agent);
    let Some(aux) = state.agents.aux_mut(agent) else {
        return (Status::NoCapability, "seat.atomic".to_owned());
    };
    if aux.batch.is_some() {
        return (Status::InvalidArgument, "nested".to_owned());
    }
    aux.batch = Some(Batch {
        max_frames: frames,
        opened: Instant::now(),
        focus_at_begin: focus,
        queued: Vec::new(),
        aborted: None,
    });
    (Status::Ok, String::new())
}

/// Queue `req` on the open batch. Answers now only if the batch is (or just
/// became) aborted.
pub(super) fn queue(state: &mut AbyssState, agent: u64, req: Request, expected: u32) {
    let req_id = req.req_id;
    let window = match req.act.aim() {
        Aim::Handle(h) if h != 0 => visible_window(state, agent, req.act.capability(), h).ok(),
        _ => None,
    };
    let Some(aux) = state.agents.aux_mut(agent) else {
        return;
    };
    let Some(b) = aux.batch.as_mut() else {
        return;
    };
    if let Some((s, d)) = b.aborted.clone() {
        seat::reply(state, agent, req_id, s, &d);
        return;
    }
    b.queued.push(Queued {
        req,
        expected,
        window,
    });
    if b.queued.len() > MAX_STEPS {
        abort(state, agent, Status::QuotaExceeded, "batch");
        return;
    }
    if let Err((s, d)) = check(state, agent) {
        abort(state, agent, s, &d);
    }
}

/// Answer every queued request with `status` and mark the batch aborted;
/// the commit (or abort) that follows repeats the cause.
fn abort(state: &mut AbyssState, agent: u64, status: Status, detail: &str) {
    let Some(b) = state.agents.aux_mut(agent).and_then(|x| x.batch.as_mut()) else {
        return;
    };
    let ids: Vec<u32> = b.queued.drain(..).map(|q| q.req.req_id).collect();
    b.aborted = Some((status, detail.to_owned()));
    for id in ids {
        seat::reply(state, agent, id, status, detail);
    }
}

/// The pre-commit check of the open batch: everything that can be known
/// without running a step. First failure wins.
fn check(state: &mut AbyssState, agent: u64) -> Result<(), (Status, String)> {
    if paused(state, agent) {
        return Err((Status::Paused, String::new()));
    }
    // The queue is moved out so generations can be read with `&mut state`.
    let Some(b) = state.agents.aux_mut(agent).and_then(|x| x.batch.take()) else {
        return Ok(());
    };
    let r = check_batch(state, &b);
    if let Some(x) = state.agents.aux_mut(agent) {
        x.batch = Some(b);
    }
    r
}

fn check_batch(state: &mut AbyssState, b: &Batch) -> Result<(), (Status, String)> {
    if b.opened.elapsed() > OPEN_TTL {
        return Err((Status::InvalidArgument, "batch_timeout".to_owned()));
    }
    // The seat's focus as the steps will leave it, so a step that needs it
    // is judged against what earlier steps set.
    let mut focus: Option<&Window> = b.focus_at_begin.as_ref();
    for q in &b.queued {
        if let Some(w) = &q.window {
            if !mapped(state, w) {
                return Err((Status::FocusLost, String::new()));
            }
        }
        match &q.req.act {
            Act::Focus { .. } | Act::Click { .. } => {
                if let Some(w) = &q.window {
                    focus = Some(w);
                }
            }
            Act::Text { .. } if q.window.is_some() && focus != q.window.as_ref() => {
                return Err((Status::FocusLost, String::new()));
            }
            _ => {}
        }
        if q.req.act.aim() == Aim::SeatFocus && !matches!(focus, Some(w) if mapped(state, w)) {
            return Err((Status::FocusLost, String::new()));
        }
        if q.expected != 0 {
            if let Some(w) = &q.window {
                let cur = generation::of(state, w);
                if cur != q.expected {
                    return Err((Status::StaleGeneration, cur.to_string()));
                }
            }
        }
    }
    Ok(())
}

/// `commit_atomic`: all of the queued steps, or none. Each queued request is
/// answered here; the return is the commit's own answer for the caller to
/// send after them.
pub(super) fn commit(state: &mut AbyssState, agent: u64) -> (Status, String) {
    let Some(batch) = state.agents.aux_mut(agent).and_then(|x| x.batch.take()) else {
        return (Status::InvalidArgument, "no_batch".to_owned());
    };
    if let Some(cause) = batch.aborted {
        return cause;
    }
    // Put back for `check`, which reads the batch in place; taken again below.
    let ids: Vec<u32> = batch.queued.iter().map(|q| q.req.req_id).collect();
    if let Some(x) = state.agents.aux_mut(agent) {
        x.batch = Some(batch);
    }
    let verdict = check(state, agent);
    let Some(batch) = state.agents.aux_mut(agent).and_then(|x| x.batch.take()) else {
        return (Status::InvalidArgument, "no_batch".to_owned());
    };
    if let Err((s, d)) = verdict {
        for id in ids {
            seat::reply(state, agent, id, s, &d);
        }
        return (s, d);
    }
    let _ = batch.max_frames; // delivery below is synchronous: 0 frames.

    // TCB-HOOK: `policy::enforce::dry_run(state, &[Request]) -> Result<(), (usize, Status, String)>`
    // — COMP-08 §10 steps 1 to 8 for every request in order (including the
    // 8b class re-check and the batch-token consumption), executing nothing,
    // mutating nothing. Call it here; on `Err((i, s, d))` answer every queued
    // request and the commit with `(s, d)` exactly as the `check` failure
    // above does. Without it a policy refusal of step N > 0 is only seen once
    // steps before N have run, and is reported as `partial:<N>`.
    let total = batch.queued.len();
    let mut steps = batch.queued.into_iter();
    let mut ran = 0usize;
    let mut failure: Option<(Status, String)> = None;
    for q in steps.by_ref() {
        let id = q.req.req_id;
        match seat::submit_act(state, q.req, q.expected) {
            Submitted::Answered(s, d) => {
                seat::reply(state, agent, id, s, &d);
                if s == Status::Ok {
                    ran += 1;
                } else {
                    failure = Some((s, d));
                }
            }
            Submitted::Pending => {
                // Parked behind a prompt or a deferral: it will answer by
                // itself, later, alone. Nothing after it may run before it.
                if let Some(x) = state.agents.aux_mut(agent) {
                    x.dedupe.suspended(id, Instant::now());
                }
                failure = Some((Status::InvalidArgument, "batch_interrupted".to_owned()));
                ran += 1;
            }
        }
        if failure.is_some() {
            break;
        }
    }
    match failure {
        None => (Status::Ok, total.to_string()),
        Some((s, d)) => {
            // The steps that never ran: the same cause, said so.
            for q in steps {
                seat::reply(state, agent, q.req.req_id, s, "aborted");
            }
            let detail = if ran > 0 && d != "batch_interrupted" {
                format!("partial:{ran}")
            } else {
                d
            };
            (s, detail)
        }
    }
}

/// `abort_atomic`: nothing queued runs. Answers each with
/// `invalid_argument` "aborted".
pub(super) fn abort_by_agent(state: &mut AbyssState, agent: u64) -> (Status, String) {
    let Some(b) = state.agents.aux_mut(agent).and_then(|x| x.batch.take()) else {
        return (Status::InvalidArgument, "no_batch".to_owned());
    };
    for q in b.queued {
        seat::reply(state, agent, q.req.req_id, Status::InvalidArgument, "aborted");
    }
    (Status::Ok, String::new())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_defaults_are_the_spec_ones() {
        assert_eq!(super::DEFAULT_FRAMES, 4, "COMP-04 §4");
    }
}
