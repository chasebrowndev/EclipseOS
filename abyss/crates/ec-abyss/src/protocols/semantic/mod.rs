// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_semantic_v1` (COMP-09), COMP-16 milestone 22: a client publishes
//! a live semantic tree for one of its toplevels, and the compositor holds
//! the authoritative copy.
//!
//! Not TCB. This module stores and serves what a client says, treating all
//! of it as data (T2). What it does *not* do is decide how sensitive a
//! window is: that is the scene filter's (`policy::scene`, `policy::classes`)
//! and is only consumed here. The one classification this module owns is the
//! per-node, raise-only one the client declares ([`tree`]).
//!
//! - [`tree`] is the state-free model and the published [`tree::View`].
//! - This file is the Wayland side: the global on the public display (the
//!   agent socket's display never holds it), request dispatch with the
//!   COMP-09 §2 protocol errors, the 1 s retention after unmap (§3), and
//!   the action round trip (§4).
//! - Three seams are for the TCB and the agent protocol, each marked
//!   `TCB-HOOK`: [`capture_facts`] (what `render/capture.rs`'s
//!   `semantics_for` returns), [`read_tree`] (what `eclipse_scene_v1.get_tree`
//!   serves), and [`request_action`] / [`take_done`] (what
//!   `eclipse_agent_seat_v1.action` drives). The taxonomy check for
//!   `ext.irreversible` is a fourth ([`taxonomy_known`]).
//!
//! Handles, not references: entries live in [`SemanticState`] under
//! `AbyssState`, keyed by a `u64` the Wayland resource carries.

use std::time::{Duration, Instant};

use ec_policy_eval::{Class, SceneView};
use ec_protocols::semantic::server::{
    eclipse_semantic_manager_v1::{self, EclipseSemanticManagerV1},
    eclipse_semantic_surface_v1::{self, EclipseSemanticSurfaceV1},
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Logical, Rectangle};

use crate::policy::scene;
use crate::render::capture::SemanticTree;
use crate::state::AbyssState;

pub mod tree;

#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod refclient;
#[cfg(test)]
mod tests;

use tree::{ExtOutcome, Model, Snapshot, View, Violation, FALLBACK_SIZE};

const VERSION: u32 = 1;
/// `capabilities`: ACTIONS | DIFF | SENSITIVITY.
const CAPABILITIES: u32 = 1 | 2 | 4;
/// COMP-09 §3: a tree outlives its surface this long, so an in-flight query
/// answers `client_gone`.
pub const RETENTION: Duration = Duration::from_secs(1);
/// COMP-09 §2: the client must ack within this.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(2);
/// Actions in flight per surface.
const MAX_PENDING: usize = 64;
const ARGS_MAX: usize = 3_072;
/// Finished actions waiting to be collected.
const MAX_DONE: usize = 1_024;

/// The wire status of an `ack_action`, plus the two the compositor makes up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionStatus {
    Ok,
    Failed,
    Unsupported,
    InvalidArgs,
    /// No ack inside [`ACTION_TIMEOUT`] (COMP-09 §2).
    ClientTimeout,
    /// The surface went away with the action in flight.
    ClientGone,
}

impl ActionStatus {
    /// A client's ack. Codes this compositor does not define count as
    /// failed: a client cannot claim the compositor's own statuses.
    fn from_ack(code: u32) -> Self {
        match code {
            0 => Self::Ok,
            2 => Self::Unsupported,
            3 => Self::InvalidArgs,
            _ => Self::Failed,
        }
    }
}

/// Why [`request_action`] refused (COMP-09 §4 steps 2 and 3, and the cases
/// before them). The enforcement check (step 1) is the caller's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionError {
    /// The surface publishes no tree.
    NoTree,
    /// The surface or its publisher is gone (`client_gone`).
    ClientGone,
    /// `expected_generation` is not the tree's generation.
    StaleGeneration,
    /// No such node, or one an agent may not name (secret).
    NoSuchNode,
    /// The node does not offer the verb.
    NoSuchAction,
    /// Too many actions in flight on this surface.
    Busy,
}

/// A finished action, for the agent protocol to turn into its `result`
/// (COMP-09 §4 step 5; the audit record of step 6 is the caller's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDone {
    /// The opaque value [`request_action`] was given.
    pub requester: u64,
    pub serial: u32,
    pub node: u32,
    pub verb: u32,
    pub status: ActionStatus,
    /// The tree's generation when the action finished.
    pub generation: u64,
    /// Request to ack (or to timeout).
    pub latency: Duration,
}

struct Pending {
    serial: u32,
    requester: u64,
    node: u32,
    verb: u32,
    started: Instant,
    deadline: Instant,
}

struct Entry {
    sid: u64,
    resource: EclipseSemanticSurfaceV1,
    toplevel: XdgToplevel,
    surface: WlSurface,
    model: Model,
    view: Option<View>,
    /// Set when the surface or its publisher went away; the tree is kept
    /// for [`RETENTION`] from then.
    gone_at: Option<Instant>,
    pending: Vec<Pending>,
    next_serial: u32,
}

/// The global and every surface's tree.
pub struct SemanticState {
    #[allow(dead_code)] // holds the global alive
    global: GlobalId,
    entries: Vec<Entry>,
    next_sid: u64,
    done: Vec<ActionDone>,
    /// Surfaces whose tree went away holding a secret. A tree that
    /// vanishes, or is replaced, must not talk the compositor out of a
    /// secret it already knew about (the ratchet), so the surface stays
    /// covered whole until it dies.
    tombs: Vec<WlSurface>,
}

impl SemanticState {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<AbyssState, EclipseSemanticManagerV1, _>(VERSION, ());
        Self {
            global,
            entries: Vec::new(),
            next_sid: 1,
            done: Vec::new(),
            tombs: Vec::new(),
        }
    }

    /// Surfaces with a live or retained entry.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Mark `sid` gone: fail what it had in flight and remember a secret.
    fn retire(&mut self, sid: u64, now: Instant) {
        let Self {
            entries, done, tombs, ..
        } = self;
        let Some(e) = entries.iter_mut().find(|e| e.sid == sid) else {
            return;
        };
        if e.gone_at.is_some() {
            return;
        }
        e.gone_at = Some(now);
        let generation = e.view.as_ref().map_or(0, |v| v.generation);
        for p in e.pending.drain(..) {
            push_done(done, &p, ActionStatus::ClientGone, generation, now);
        }
        let known = e.view.as_ref().is_some_and(|v| v.secret_facts(now).known) || e.model.pending_secret(now);
        if known && !tombs.contains(&e.surface) {
            tombs.push(e.surface.clone());
        }
    }

    /// Notice what died, time out what was not acked, drop what was kept
    /// long enough. Cheap; called from every entry point that has `&mut`.
    pub fn sweep(&mut self, now: Instant) {
        let dead: Vec<u64> = self
            .entries
            .iter()
            .filter(|e| e.gone_at.is_none() && (!e.resource.is_alive() || !e.toplevel.is_alive()))
            .map(|e| e.sid)
            .collect();
        for sid in dead {
            self.retire(sid, now);
        }
        let Self { entries, done, .. } = self;
        for e in entries.iter_mut() {
            let generation = e.view.as_ref().map_or(0, |v| v.generation);
            let mut i = 0;
            while i < e.pending.len() {
                if e.pending[i].deadline <= now {
                    let p = e.pending.remove(i);
                    push_done(done, &p, ActionStatus::ClientTimeout, generation, now);
                } else {
                    i += 1;
                }
            }
        }
        self.entries
            .retain(|e| e.gone_at.is_none_or(|t| now.duration_since(t) < RETENTION));
        self.tombs.retain(|s| s.is_alive());
    }
}

fn push_done(done: &mut Vec<ActionDone>, p: &Pending, status: ActionStatus, generation: u64, now: Instant) {
    if done.len() >= MAX_DONE {
        done.remove(0);
    }
    done.push(ActionDone {
        requester: p.requester,
        serial: p.serial,
        node: p.node,
        verb: p.verb,
        status,
        generation,
        latency: now.saturating_duration_since(p.started),
    });
}

/// The surface's size as the capture path sees it (`window.bbox()`), which
/// is what rectangles are clamped to and what a resize is measured by.
fn surface_size(state: &AbyssState, surface: &WlSurface) -> (i32, i32) {
    crate::shell::window_for_surface(state, surface)
        .map(|w| w.bbox().size)
        .filter(|s| s.w > 0 && s.h > 0)
        .map_or(FALLBACK_SIZE, |s| (s.w, s.h))
}

/// Whether `id` is an `ext.irreversible` taxonomy id (S-06 §2).
///
/// TCB-HOOK: the compiled taxonomy lives in the enforcement table (COMP-11),
/// which has none until the S-06 matcher (M18) lands. Needed:
/// `policy::table::taxonomy_has(state: &AbyssState, id: &str) -> bool`, true
/// only for an id in the live table's taxonomy; false with no table. Until
/// then this is the fail-closed stub: nothing is known, so every
/// `ext.irreversible` is dropped with a `budget_exceeded` warning (A-07)
/// and no client can invent a category.
fn taxonomy_known(_state: &AbyssState, _id: &str) -> bool {
    false
}

// ------------------------------------------------------------ public seams

/// What the capture path needs for `surface`: its tree, the surface's
/// current generation, and whether a `secret` node is known (COMP-02 §7,
/// A-10).
///
/// TCB-HOOK: this has the signature of `render/capture.rs`'s private
/// `semantics_for` on purpose. Replace its body with
/// `crate::protocols::semantic::capture_facts(state, surface)`; that turns
/// on node-level redaction for live trees (the redaction suite's live-tree
/// arm, COMP-16 M22) and nothing else changes there.
///
/// Fail-closed throughout; the arms are in `tests.rs`:
/// - no tree and no secret ever known: `Absent`, not known;
/// - a secret that cannot be placed (no rectangle, clamped to nothing, too
///   many, dropped to the node budget, removed within the grace, declared
///   but not yet committed, or a tree that vanished holding one): `Present`
///   with no rectangles and `known`, which `resolve_nodes` turns into the
///   whole surface;
/// - the surface resized since the tree was published while a secret is
///   known: the surface generation is one ahead, so the tree is stale.
pub fn capture_facts(state: &AbyssState, surface: Option<&WlSurface>) -> (SemanticTree, u64, bool) {
    let Some(surface) = surface else {
        return (SemanticTree::Absent, 0, false);
    };
    let now = Instant::now();
    let sem = &state.semantic;
    let tomb = sem.tombs.contains(surface);
    let Some(e) = sem.entries.iter().find(|e| &e.surface == surface) else {
        return (SemanticTree::Absent, 0, tomb);
    };
    let pending = e.model.pending_secret(now);
    let Some(v) = e.view.as_ref() else {
        return (SemanticTree::Absent, 0, tomb || pending);
    };
    let facts = v.secret_facts(now);
    let known = facts.known || tomb || pending;
    let secret = if facts.unplaced || tomb || pending {
        Vec::new()
    } else {
        facts
            .rects
            .iter()
            .map(|r| Rectangle::<i32, Logical>::new((r.x, r.y).into(), (r.w, r.h).into()))
            .collect()
    };
    // A resize leaves rectangles describing content that is no longer on
    // screen. It only matters while something is secret.
    let generation = if known && surface_size(state, surface) != v.size {
        v.generation + 1
    } else {
        v.generation
    };
    (
        SemanticTree::Present {
            generation: v.generation,
            secret,
        },
        generation,
        known,
    )
}

/// What an agent's read of a surface's tree gets.
#[derive(Debug, PartialEq)]
pub enum ReadOutcome {
    /// Not a window this agent can see: the same answer as one that never
    /// existed (F-07).
    NotFound,
    /// A visible window that publishes no tree.
    NoTree,
    /// The publisher is gone and the retained tree is not served
    /// (COMP-09 §3: `client_gone`).
    ClientGone,
    Tree(Snapshot),
}

/// The server half of `eclipse_scene_v1.get_tree`: the tree of the window
/// `handle` names, as `view` may see it.
///
/// Visibility and the window's class are the scene filter's:
/// [`scene::resolve`] decides whether the window exists for this agent and
/// [`scene::class`] gives the floor every node is raised to. This module
/// adds only the per-node rule (a secret node, and what is under it, is
/// absent from the snapshot, never redacted in place).
///
/// TCB-HOOK: `get_tree` is not in the `eclipse_agent_v1` XML yet. Needed
/// there (COMP-08 §3, owner review because it is an agent read path):
/// `eclipse_scene_v1.get_tree(req_id, handle)` answered by
/// `scene.read` scope check, then this function, then the `tree`/`node`
/// events built from the [`Snapshot`] and a `provenance` link stamped with
/// the generation and node-id hash (A-09). `ClientGone` maps to the
/// `client_gone` status, `NotFound` to `invalid_argument` detail `handle`.
pub fn read_tree(state: &AbyssState, view: &SceneView, handle: u64, now: Instant) -> ReadOutcome {
    let Some(window) = scene::resolve(state, view, handle) else {
        return ReadOutcome::NotFound;
    };
    let Some(surface) = crate::shell::window_surface(&window) else {
        return ReadOutcome::NoTree;
    };
    let Some(e) = state.semantic.entries.iter().find(|e| e.surface == surface) else {
        return ReadOutcome::NoTree;
    };
    let gone = e.gone_at.is_some() || !e.resource.is_alive() || !e.toplevel.is_alive();
    if gone {
        return ReadOutcome::ClientGone;
    }
    match e.view.as_ref() {
        None => ReadOutcome::NoTree,
        Some(v) => ReadOutcome::Tree(v.snapshot(scene::class(state, &window), now)),
    }
}

/// Step 2-4 of COMP-09 §4: forward a semantic action to the publisher.
/// Returns the serial the ack will carry.
///
/// The caller has already run `check()` and the enforcement table (step 1;
/// no state is mutated before that returns `Allow`) and resolved the window
/// through the scene filter. `requester` is an opaque value handed back in
/// [`ActionDone`].
///
/// TCB-HOOK: `eclipse_agent_seat_v1.action(handle, node, verb, args,
/// expected_generation)` must call this after its enforcement `Allow`, map
/// [`ActionError`] to `client_gone` / `stale_generation` /
/// `no_such_action` (`NoSuchNode` and `NoTree` too: `no_such_action`), and
/// answer from [`take_done`] with the post-action generation and focus.
pub fn request_action(
    state: &mut AbyssState,
    surface: &WlSurface,
    requester: u64,
    node: u32,
    verb: u32,
    args: &str,
    expected_generation: u64,
) -> Result<u32, ActionError> {
    let now = Instant::now();
    state.semantic.sweep(now);
    let e = state
        .semantic
        .entries
        .iter_mut()
        .find(|e| &e.surface == surface)
        .ok_or(ActionError::NoTree)?;
    if e.gone_at.is_some() {
        return Err(ActionError::ClientGone);
    }
    let v = e.view.as_ref().ok_or(ActionError::NoTree)?;
    if v.generation != expected_generation {
        return Err(ActionError::StaleGeneration);
    }
    // A node an agent cannot read is a node it cannot name.
    match v.class_of(node, now) {
        None | Some(Class::Secret) => return Err(ActionError::NoSuchNode),
        Some(_) => {}
    }
    if !v.node(node).is_some_and(|n| n.actions.contains(&verb)) {
        return Err(ActionError::NoSuchAction);
    }
    if e.pending.len() >= MAX_PENDING {
        return Err(ActionError::Busy);
    }
    let mut serial = e.next_serial.wrapping_add(1);
    if serial == 0 {
        serial = 1;
    }
    e.next_serial = serial;
    e.pending.push(Pending {
        serial,
        requester,
        node,
        verb,
        started: now,
        deadline: now + ACTION_TIMEOUT,
    });
    e.resource
        .action_requested(serial, node, verb, tree::clean(args, ARGS_MAX, true));
    Ok(serial)
}

/// Finished actions, oldest first: acks, timeouts and gone publishers.
pub fn take_done(state: &mut AbyssState) -> Vec<ActionDone> {
    state.semantic.sweep(Instant::now());
    std::mem::take(&mut state.semantic.done)
}

/// `focus_hint`: tell the publisher which node an agent seat is targeting
/// for a text commit (COMP-09 §2). Nothing is sent for a node the committed
/// tree does not have, or one an agent may not name.
///
/// TCB-HOOK: called by the agent seat's `text_commit` path once it names a
/// node.
pub fn focus_hint(state: &AbyssState, surface: &WlSurface, node: u32) {
    let now = Instant::now();
    let Some(e) = state.semantic.entries.iter().find(|e| &e.surface == surface) else {
        return;
    };
    if e.gone_at.is_some() {
        return;
    }
    if e.view
        .as_ref()
        .and_then(|v| v.class_of(node, now))
        .is_some_and(|c| c != Class::Secret)
    {
        e.resource.focus_hint(node);
    }
}

/// An `xdg_toplevel` was destroyed: its tree is retained for [`RETENTION`]
/// and in-flight actions fail with `ClientGone`.
pub fn toplevel_destroyed(state: &mut AbyssState, toplevel: &XdgToplevel) {
    let now = Instant::now();
    let sids: Vec<u64> = state
        .semantic
        .entries
        .iter()
        .filter(|e| &e.toplevel == toplevel)
        .map(|e| e.sid)
        .collect();
    for sid in sids {
        state.semantic.retire(sid, now);
    }
}

/// Run the time-based parts (timeouts, retention). Safe to call from a
/// timer or the frame callback; every entry point also sweeps.
pub fn tick(state: &mut AbyssState) {
    state.semantic.sweep(Instant::now());
}

// ------------------------------------------------------------ dispatch

/// User data on an `eclipse_semantic_surface_v1`: the entry's id. 0 is an
/// inert object (the second `get_semantic_surface` for a toplevel, which
/// already got its protocol error).
pub struct SurfaceData(u64);

impl GlobalDispatch<EclipseSemanticManagerV1, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<EclipseSemanticManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(manager, ()).capabilities(CAPABILITIES);
    }
}

impl Dispatch<EclipseSemanticManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &EclipseSemanticManagerV1,
        request: eclipse_semantic_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            eclipse_semantic_manager_v1::Request::GetSemanticSurface { id, toplevel } => {
                let now = Instant::now();
                state.semantic.sweep(now);
                // A client can only name its own objects, so this is
                // belt-and-braces for COMP-09 §1.
                let owner = toplevel.client().map(|c| c.id()) == Some(client.id());
                let surface = state
                    .xdg_shell_state
                    .get_toplevel(&toplevel)
                    .map(|t| t.wl_surface().clone());
                let (surface, code, why) = match surface {
                    Some(s) if owner => {
                        let taken = state
                            .semantic
                            .entries
                            .iter()
                            .any(|e| e.surface == s && e.gone_at.is_none() && e.resource.is_alive());
                        if taken {
                            (None, 3u32, "the toplevel already has a semantic surface")
                        } else {
                            (Some(s), 0, "")
                        }
                    }
                    _ => (None, 4u32, "not a toplevel of this client"),
                };
                let Some(surface) = surface else {
                    // The new object must still be initialised; sid 0 is inert.
                    let _ = data_init.init(id, SurfaceData(0));
                    manager.post_error(code, why);
                    return;
                };
                // A re-publish after a destroy replaces the retained entry.
                // `retire` first, so a secret it held is remembered.
                let old: Vec<u64> = state
                    .semantic
                    .entries
                    .iter()
                    .filter(|e| e.surface == surface)
                    .map(|e| e.sid)
                    .collect();
                for sid in old {
                    state.semantic.retire(sid, now);
                }
                state.semantic.entries.retain(|e| e.surface != surface);
                let sid = state.semantic.next_sid;
                state.semantic.next_sid += 1;
                let resource = data_init.init(id, SurfaceData(sid));
                state.semantic.entries.push(Entry {
                    sid,
                    resource,
                    toplevel,
                    surface,
                    model: Model::new(),
                    view: None,
                    gone_at: None,
                    pending: Vec::new(),
                    next_serial: 0,
                });
            }
            eclipse_semantic_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

/// Post `v` as the protocol error it is.
fn fail(resource: &EclipseSemanticSurfaceV1, v: Violation) {
    resource.post_error(v.code(), v.message());
}

impl Dispatch<EclipseSemanticSurfaceV1, SurfaceData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &EclipseSemanticSurfaceV1,
        request: eclipse_semantic_surface_v1::Request,
        data: &SurfaceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use eclipse_semantic_surface_v1::Request;
        let sid = data.0;
        let now = Instant::now();
        let Some(surface) = state
            .semantic
            .entries
            .iter()
            .find(|e| e.sid == sid)
            .map(|e| e.surface.clone())
        else {
            // Inert, or retired out from under it.
            return;
        };
        // Facts that need the whole state, taken before the entry is
        // borrowed out of it.
        let size = matches!(request, Request::Commit).then(|| surface_size(state, &surface));
        let known = match &request {
            Request::SetExt { key, value, .. } => {
                key.strip_prefix("ext.").unwrap_or(key) == "irreversible" && taxonomy_known(state, value)
            }
            _ => false,
        };
        let SemanticState { entries, done, .. } = &mut state.semantic;
        let Some(e) = entries.iter_mut().find(|e| e.sid == sid) else {
            return;
        };
        if e.gone_at.is_some() {
            return;
        }
        let m = &mut e.model;
        let r: Result<(), Violation> = match request {
            Request::SetRoot { node } => m.set_root(node),
            Request::AddNode { node, parent, index } => m.add_node(node, parent, index),
            Request::RemoveNode { node } => m.remove_node(node, now),
            Request::MoveNode {
                node,
                new_parent,
                index,
            } => m.move_node(node, new_parent, index),
            Request::SetRole { node, role } => m.set_role(node, role),
            Request::SetName { node, name } => m.set_name(node, &name),
            Request::SetDescription { node, desc } => m.set_description(node, &desc),
            Request::SetRect { node, x, y, w, h } => m.set_rect(node, x, y, w, h),
            Request::SetStates { node, states } => m.set_states(node, tree::words(&states)),
            Request::SetValueText {
                node,
                text,
                cursor,
                sel_start,
                sel_end,
            } => m.set_value_text(node, &text, cursor, sel_start, sel_end),
            Request::SetValueNumber {
                node,
                value,
                min,
                max,
                step,
            } => m.set_value_number(node, value, min, max, step),
            Request::SetValueUrl { node, href } => m.set_value_url(node, &href),
            Request::ClearValue { node } => m.clear_value(node),
            Request::SetActions { node, verbs } => m.set_actions(node, tree::words(&verbs)),
            Request::SetActionLabel { node, verb, label } => m.set_action_label(node, verb, &label),
            Request::SetSensitivity { node, class } => m.set_sensitivity(node, class),
            Request::SetExt { node, key, value } => match m.set_ext(node, &key, &value, now, known) {
                Ok(ExtOutcome::Applied) => Ok(()),
                // An unknown taxonomy id: dropped, and the client told the
                // way an over-budget tree is (COMP-09 §3).
                Ok(ExtOutcome::Dropped) => {
                    e.resource.budget_exceeded();
                    Ok(())
                }
                Err(v) => Err(v),
            },
            Request::Commit => {
                let size = size.unwrap_or(FALLBACK_SIZE);
                match e.model.commit(size, now) {
                    Ok(None) => Ok(()),
                    Ok(Some((c, view))) => {
                        e.view = Some(view);
                        if c.budget_exceeded {
                            e.resource.budget_exceeded();
                        }
                        Ok(())
                    }
                    Err(v) => Err(v),
                }
            }
            Request::AckAction { serial, status } => {
                // A late ack, after the timeout already answered, is a race
                // and not an error.
                if let Some(i) = e.pending.iter().position(|p| p.serial == serial) {
                    let p = e.pending.remove(i);
                    let generation = e.view.as_ref().map_or(0, |v| v.generation);
                    push_done(done, &p, ActionStatus::from_ack(status), generation, now);
                }
                Ok(())
            }
            Request::Destroy => {
                if e.model.dirty() {
                    Err(Violation::UncommittedDestroy)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        };
        if let Err(v) = r {
            fail(resource, v);
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _resource: &EclipseSemanticSurfaceV1,
        data: &SurfaceData,
    ) {
        if data.0 != 0 {
            state.semantic.retire(data.0, Instant::now());
        }
    }
}
