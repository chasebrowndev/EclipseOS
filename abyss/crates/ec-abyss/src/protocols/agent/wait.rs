// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_scene_v1.wait_for` and `cancel_wait` (COMP-08 §3). Not TCB.
//!
//! A wait is one KDL predicate node, parsed once into a [`Pred`] that holds
//! the windows it names (resolved through the agent's `scene.list` view when
//! the request arrives, so an unknown, hidden or out-of-scope handle is the
//! same `invalid_argument` "handle" as everywhere, F-07). It is evaluated
//! when the request arrives and then on a 25 ms calloop timer that runs only
//! while some wait is pending. Every evaluation goes through the agent's
//! *current* view again: a window that has since become invisible to the
//! agent satisfies nothing (except `unmapped`, for a window it did see), so a
//! wait is no oracle on windows the agent lost sight of.
//!
//! The answer is one `waited` event whose `generation` is the toplevel's
//! generation at the moment the predicate held (P-07 §5.3), read by the same
//! code that produced the satisfaction, not at request time or delivery.
//!
//! Predicates that need the semantic tree (`node`, `text_contains`,
//! `node_gone`) or a terminal channel (`command_finished`) are refused
//! `invalid_argument` "unsupported" at the request, never silently never
//! satisfied.

use std::time::{Duration, Instant};

use ec_policy_eval::SceneView;
use ec_protocols::agent::server::{eclipse_agent_v1::Status, eclipse_scene_v1::EclipseSceneV1};
use kdl::{KdlDocument, KdlNode};
use smithay::desktop::Window;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::IsAlive;

use super::{generation, seat};
use crate::policy::scene;
use crate::state::AbyssState;

/// How often pending waits are re-evaluated.
const POLL: Duration = Duration::from_millis(25);
/// The longest a wait may be.
pub const MAX_TIMEOUT_MS: u32 = 600_000;
/// Pending waits one agent may hold.
pub const MAX_WAITS: usize = 16;
const MAX_PREDICATE: usize = 1024;
const MAX_REGEX: usize = 256;

#[derive(Debug)]
pub enum Seat {
    Human,
    Agent(u64),
}

#[derive(Debug)]
pub enum Pred {
    Appears {
        app_id: Option<String>,
        title: Option<String>,
    },
    TitleMatches {
        handle: u64,
        window: Window,
        re: regex::Regex,
    },
    Focus {
        seat: Seat,
        handle: u64,
        window: Window,
    },
    GenerationGt {
        handle: u64,
        window: Window,
        n: u32,
    },
    Unmapped {
        handle: u64,
        window: Window,
    },
    Idle {
        handle: u64,
        window: Window,
        ms: u64,
    },
}

#[derive(Debug)]
struct Wait {
    agent: u64,
    req_id: u32,
    scene: EclipseSceneV1,
    pred: Pred,
    deadline: Instant,
}

#[derive(Debug, Default)]
pub struct Waits {
    waits: Vec<Wait>,
    timer: Option<RegistrationToken>,
}

impl Waits {
    /// Waits pending.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.waits.len()
    }
}

fn prop<'a>(node: &'a KdlNode, key: &str) -> Option<&'a kdl::KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_some_and(|n| n.value() == key))
        .map(|e| e.value())
}

/// The predicate text as a [`Pred`]. `Err` is the `invalid_argument` detail.
pub fn parse(state: &AbyssState, view: &SceneView, text: &str) -> Result<Pred, &'static str> {
    if text.len() > MAX_PREDICATE {
        return Err("predicate");
    }
    let doc: KdlDocument = text.parse().map_err(|_| "predicate")?;
    let [node] = doc.nodes() else {
        return Err("predicate");
    };
    if node.children().is_some() {
        return Err("predicate");
    }
    let name = node.name().value();
    let allowed: &[&str] = match name {
        "toplevel_appears" => &["app_id", "title"],
        "title_matches" => &["handle", "regex"],
        "focus" => &["seat", "handle"],
        "generation_gt" => &["handle", "n"],
        "unmapped" => &["handle"],
        "idle" => &["handle", "ms"],
        "node" | "text_contains" | "node_gone" | "command_finished" => return Err("unsupported"),
        _ => return Err("predicate"),
    };
    // Properties only, each known, none repeated.
    let mut seen: Vec<&str> = Vec::new();
    for e in node.entries() {
        let Some(k) = e.name().map(|n| n.value()) else {
            return Err("predicate");
        };
        if !allowed.contains(&k) || seen.contains(&k) {
            return Err("predicate");
        }
        seen.push(k);
    }
    let string = |k: &str| -> Result<Option<String>, &'static str> {
        match prop(node, k) {
            None => Ok(None),
            Some(v) => v.as_string().map(|s| Some(s.to_owned())).ok_or("predicate"),
        }
    };
    let uint = |k: &str| -> Result<Option<u64>, &'static str> {
        match prop(node, k) {
            None => Ok(None),
            Some(v) => v
                .as_integer()
                .and_then(|i| u64::try_from(i).ok())
                .map(Some)
                .ok_or("predicate"),
        }
    };
    let window_of =
        |handle: u64| -> Result<Window, &'static str> { scene::resolve(state, view, handle).ok_or("handle") };
    let need = |v: Option<u64>| v.ok_or("predicate");
    Ok(match name {
        "toplevel_appears" => {
            let (app_id, title) = (string("app_id")?, string("title")?);
            if app_id.is_none() && title.is_none() {
                return Err("predicate");
            }
            Pred::Appears { app_id, title }
        }
        "title_matches" => {
            let handle = need(uint("handle")?)?;
            let src = string("regex")?.ok_or("predicate")?;
            if src.len() > MAX_REGEX {
                return Err("predicate");
            }
            let re = regex::RegexBuilder::new(&src)
                .size_limit(1 << 18)
                .build()
                .map_err(|_| "predicate")?;
            Pred::TitleMatches {
                handle,
                window: window_of(handle)?,
                re,
            }
        }
        "focus" => {
            let handle = need(uint("handle")?)?;
            let seat = match string("seat")?.ok_or("predicate")?.as_str() {
                "human" => Seat::Human,
                s => Seat::Agent(
                    s.strip_prefix("agent-")
                        .and_then(|n| n.parse().ok())
                        .ok_or("predicate")?,
                ),
            };
            Pred::Focus {
                seat,
                handle,
                window: window_of(handle)?,
            }
        }
        "generation_gt" => {
            let handle = need(uint("handle")?)?;
            let n = u32::try_from(need(uint("n")?)?).map_err(|_| "predicate")?;
            Pred::GenerationGt {
                handle,
                window: window_of(handle)?,
                n,
            }
        }
        "unmapped" => {
            let handle = need(uint("handle")?)?;
            Pred::Unmapped {
                handle,
                window: window_of(handle)?,
            }
        }
        _ => {
            let handle = need(uint("handle")?)?;
            let ms = need(uint("ms")?)?;
            Pred::Idle {
                handle,
                window: window_of(handle)?,
                ms,
            }
        }
    })
}

/// Whether `pred` holds now for an agent seeing through `view`: the
/// toplevel that satisfied it and its generation at this moment.
pub fn eval(state: &mut AbyssState, view: &SceneView, pred: &Pred) -> Option<(u64, u32)> {
    match pred {
        Pred::Appears { app_id, title } => {
            for (handle, w) in scene::list(state, view) {
                let (a, t) = super::identity(&w);
                if app_id.as_ref().is_some_and(|x| *x != a) || title.as_ref().is_some_and(|x| *x != t) {
                    continue;
                }
                return Some((handle, generation::of(state, &w)));
            }
            None
        }
        Pred::Unmapped { handle, window } => {
            if !window.alive() {
                return Some((*handle, 0));
            }
            if state.space.element_geometry(window).is_some() {
                return None;
            }
            scene::visible(state, view, window).then(|| (*handle, generation::of(state, window)))
        }
        Pred::TitleMatches { handle, window, re } => {
            if !scene::visible(state, view, window) {
                return None;
            }
            let (_, t) = super::identity(window);
            re.is_match(&t).then(|| (*handle, generation::of(state, window)))
        }
        Pred::Focus { seat, handle, window } => {
            if !scene::visible(state, view, window) {
                return None;
            }
            let on = match seat {
                Seat::Human => state.focus.as_ref() == Some(window),
                Seat::Agent(id) => seat::focused_window(state, *id).as_ref() == Some(window),
            };
            on.then(|| (*handle, generation::of(state, window)))
        }
        Pred::GenerationGt { handle, window, n } => {
            if !scene::visible(state, view, window) {
                return None;
            }
            let g = generation::of(state, window);
            (g > *n).then_some((*handle, g))
        }
        Pred::Idle { handle, window, ms } => {
            if !scene::visible(state, view, window) {
                return None;
            }
            let (g, since) = generation::observe(state, window);
            (since.elapsed() >= Duration::from_millis(*ms)).then_some((*handle, g))
        }
    }
}

/// Evaluate `pred` for `agent` through its live view. `None` when it does
/// not hold, or the agent has no view at all.
fn eval_for(state: &mut AbyssState, agent: u64, pred: &Pred) -> Option<(u64, u32)> {
    let mut a = state.agents.take_agent(agent)?;
    let r = a.view_at(super::now_ms()).and_then(|v| eval(state, v.list, pred));
    state.agents.put_agent(agent, a);
    r
}

fn send(scene: &EclipseSceneV1, req_id: u32, satisfied: bool, handle: u64, generation: u32) {
    scene.waited(
        req_id,
        u32::from(satisfied),
        u32::try_from(handle).unwrap_or(0),
        0,
        generation,
    );
}

/// Register a pending wait. `Err` is the `(status, detail)` to refuse with.
pub fn add(
    state: &mut AbyssState,
    agent: u64,
    req_id: u32,
    scene: &EclipseSceneV1,
    pred: Pred,
    timeout_ms: u32,
) -> Result<(), (Status, &'static str)> {
    let mine = state.agents.waits.waits.iter().filter(|w| w.agent == agent);
    if mine.clone().any(|w| w.req_id == req_id) {
        return Err((Status::Duplicate, "wait"));
    }
    if mine.count() >= MAX_WAITS {
        return Err((Status::QuotaExceeded, "waits"));
    }
    if state.agents.waits.timer.is_none() {
        let t = state
            .loop_handle
            .insert_source(Timer::from_duration(POLL), |_, _, st| poll(st));
        match t {
            Ok(t) => state.agents.waits.timer = Some(t),
            Err(_) => return Err((Status::QuotaExceeded, "timer")),
        }
    }
    state.agents.waits.waits.push(Wait {
        agent,
        req_id,
        scene: scene.clone(),
        pred,
        deadline: Instant::now() + Duration::from_millis(u64::from(timeout_ms)),
    });
    Ok(())
}

/// `cancel_wait`: the wait ends unsatisfied. Whether there was one.
pub fn cancel(state: &mut AbyssState, agent: u64, req_id: u32) -> bool {
    let Some(i) = state
        .agents
        .waits
        .waits
        .iter()
        .position(|w| w.agent == agent && w.req_id == req_id)
    else {
        return false;
    };
    let w = state.agents.waits.waits.remove(i);
    send(&w.scene, req_id, false, 0, 0);
    true
}

/// The agent is gone: its waits go with it.
pub fn forget(state: &mut AbyssState, agent: u64) {
    state.agents.waits.waits.retain(|w| w.agent != agent);
}

/// The timer: answer every wait that now holds or has run out.
fn poll(state: &mut AbyssState) -> TimeoutAction {
    let now = Instant::now();
    let waits = std::mem::take(&mut state.agents.waits.waits);
    let mut keep = Vec::with_capacity(waits.len());
    for w in waits {
        if !w.scene.is_alive() || state.agents.peek_agent(w.agent).is_none() {
            continue;
        }
        // Paused agents are not looked at (COMP-04 §6); their clocks run.
        let paused = state.policy_key.is_none() || crate::policy::lifecycle::is_paused(state, w.agent);
        let held = if paused {
            None
        } else {
            eval_for(state, w.agent, &w.pred)
        };
        match held {
            Some((h, g)) => send(&w.scene, w.req_id, true, h, g),
            None if now >= w.deadline => send(&w.scene, w.req_id, false, 0, 0),
            None => keep.push(w),
        }
    }
    state.agents.waits.waits.extend(keep);
    if state.agents.waits.waits.is_empty() {
        state.agents.waits.timer = None;
        TimeoutAction::Drop
    } else {
        TimeoutAction::ToDuration(POLL)
    }
}

/// The initial check a `wait_for` request makes before parking. `Some` when
/// it already holds.
pub fn holds_now(state: &mut AbyssState, view: &SceneView, pred: &Pred) -> Option<(u64, u32)> {
    eval(state, view, pred)
}

/// Answer a satisfied wait at request time.
pub fn answer_now(scene: &EclipseSceneV1, req_id: u32, (handle, generation): (u64, u32)) {
    send(scene, req_id, true, handle, generation);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grammar(text: &str) -> Result<(), &'static str> {
        let h = crate::shell::focus::state_tests::harness();
        let view = SceneView::default();
        parse(&h.state, &view, text).map(|_| ())
    }

    #[test]
    fn predicates_that_need_the_tree_are_refused_not_ignored() {
        for p in [
            "node handle=1 id=2",
            "text_contains handle=1 node=2 needle=\"x\"",
            "node_gone handle=1 id=2",
            "command_finished handle=1",
        ] {
            assert_eq!(grammar(p), Err("unsupported"), "{p}");
        }
    }

    #[test]
    fn malformed_predicates_are_refused() {
        for p in [
            "",
            "toplevel_appears",
            "toplevel_appears \"firefox\"",
            "toplevel_appears app_id=\"x\" extra=1",
            "toplevel_appears app_id=\"x\" app_id=\"y\"",
            "toplevel_appears app_id=3",
            "nonsense a=1",
            "idle handle=1",
            "a b=1; c d=2",
            "toplevel_appears app_id=\"x\" { y }",
            "{{{",
        ] {
            assert_eq!(grammar(p), Err("predicate"), "{p:?}");
        }
        assert!(grammar("toplevel_appears app_id=\"x\" title=\"y\"").is_ok());
        assert!(grammar("toplevel_appears title=\"y\"").is_ok());
    }

    #[test]
    fn a_handle_the_agent_cannot_see_is_the_unknown_handle_answer() {
        assert_eq!(grammar("unmapped handle=999"), Err("handle"));
        assert_eq!(grammar("focus seat=\"human\" handle=1"), Err("handle"));
        assert_eq!(grammar("focus seat=\"nobody\" handle=1"), Err("predicate"));
    }
}
