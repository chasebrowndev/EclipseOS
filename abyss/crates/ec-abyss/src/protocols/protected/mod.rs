// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_protected_surface_v1` (COMP-19): a client marks one of its own
//! surfaces human-input-only and may host a compositor-drawn commit slot in
//! it. Not TCB. This module is the Wayland side and the registry; the slot's
//! card, preview, arming and commit live in `trusted_ui/` and are reached
//! through the seams in [`hooks`], and what the compositor does with a
//! protected surface (capture exclusion, agent-scene exclusion) is theirs,
//! asking [`is_protected`].
//!
//! - **The global** is on the public display only; the agent socket's display
//!   never holds it.
//! - **State** is handle-based: protected surfaces keyed by a `u64` the
//!   Wayland resource carries, slots keyed by their own `u64`, both in
//!   [`Protected`] under `AbyssState`. The compositor-held [`Draft`] lives in
//!   the slot; the client cannot read it back.
//! - **Input** (COMP-19 §3, §6) is [`gate`]: physical input only reaches a
//!   protected surface, an unmodified Enter never reaches the client while a
//!   slot exists, and nothing inside the slot's rectangle does.
//! - **Rules** (§2, §8): one protected object per `wl_surface`, one slot per
//!   protected surface, at most [`MAX_SLOTS_PER_CLIENT`] slots per client
//!   (the fifth is the protocol error `slot_exists`), `statement` at most
//!   [`STATEMENT_MAX`] characters, `set_draft` at 10/s with the latest
//!   draft winning, flushed on a calloop timer.
//! - **Destroy** takes effect at the surface's next commit (§2); the slot is
//!   cancelled at once.

use std::time::{Duration, Instant};

use ec_protocols::protected::server::{
    eclipse_commit_slot_v1::{self, EclipseCommitSlotV1},
    eclipse_protected_surface_manager_v1::{self, EclipseProtectedSurfaceManagerV1},
    eclipse_protected_surface_v1::{self, EclipseProtectedSurfaceV1},
};
use smithay::desktop::{find_popup_root_surface, PopupManager};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::{
    backend::{ClientId, GlobalId},
    protocol::wl_surface::WlSurface,
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::{get_parent, with_states, SubsurfaceCachedState};

use crate::state::AbyssState;

pub mod gate;
pub mod hooks;

/// Every call out to the seams in [`hooks`] goes through here, so the tests
/// can see what the TCB would have been told without depending on what the
/// TCB's bodies do.
pub(crate) mod calls {
    use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

    use super::hooks;
    use crate::input::Origin;
    use crate::state::AbyssState;

    #[cfg(test)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Call {
        Draft(u64),
        Moved(u64),
        Gone(u64),
        Enter(u64),
        Click(u64, i32, i32),
        Audit(Origin),
    }

    pub fn draft_changed(state: &mut AbyssState, slot: u64) {
        #[cfg(test)]
        state.protected.log.push(Call::Draft(slot));
        hooks::draft_changed(state, slot);
    }

    pub fn slot_moved(state: &mut AbyssState, slot: u64) {
        #[cfg(test)]
        state.protected.log.push(Call::Moved(slot));
        hooks::slot_moved(state, slot);
    }

    pub fn slot_gone(state: &mut AbyssState, slot: u64) {
        #[cfg(test)]
        state.protected.log.push(Call::Gone(slot));
        hooks::slot_gone(state, slot);
    }

    pub fn enter(state: &mut AbyssState, slot: u64, now_ms: u32) {
        #[cfg(test)]
        state.protected.log.push(Call::Enter(slot));
        hooks::enter(state, slot, now_ms);
    }

    pub fn click(state: &mut AbyssState, slot: u64, x: i32, y: i32, now_ms: u32) {
        #[cfg(test)]
        state.protected.log.push(Call::Click(slot, x, y));
        hooks::click(state, slot, x, y, now_ms);
    }

    pub fn audit_refused(state: &mut AbyssState, origin: Origin, surface: &WlSurface) {
        #[cfg(test)]
        state.protected.log.push(Call::Audit(origin));
        hooks::audit_refused(state, origin, surface);
    }
}

/// A physical press inside a slot's rectangle, after the caller's own
/// click-to-focus (COMP-19 §6). `x`, `y` are relative to the slot.
pub(crate) fn on_click(state: &mut AbyssState, slot: u64, x: i32, y: i32, now_ms: u32) {
    calls::click(state, slot, x, y, now_ms);
}

#[cfg(test)]
mod tests;

pub use eclipse_commit_slot_v1::{Reason, State};
pub use eclipse_protected_surface_v1::Kind;

const VERSION: u32 = 1;
/// COMP-19 §8: slots one client may hold.
pub const MAX_SLOTS_PER_CLIENT: usize = 4;
/// COMP-19 §8: the longest `statement`, in characters.
pub const STATEMENT_MAX: usize = 1_000;
/// COMP-19 §8: `set_draft` at 10/s.
pub const DRAFT_INTERVAL: Duration = Duration::from_millis(100);
/// COMP-19 §2: `input_refused` at most once a second.
pub const REFUSED_INTERVAL: Duration = Duration::from_secs(1);
/// How far up a surface's parents to look. Subsurface trees are shallow; the
/// bound only keeps a malformed one from looping.
const MAX_DEPTH: usize = 32;

// The protocol error codes (the XML's `error` enum, shared by all three
// interfaces).
const ALREADY_PROTECTED: u32 = 0;
const SLOT_EXISTS: u32 = 1;
const NOT_OWNER: u32 = 2;
const BAD_KIND: u32 = 3;
const DRAFT_TOO_LARGE: u32 = 4;

/// The compositor's copy of a task draft (COMP-19 §2 `set_draft`). Human
/// text: never logged.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub package: String,
    pub statement: String,
    pub deadline_s: u32,
    /// CBOR scope list; may only shrink (checked by the previewer, not here).
    pub narrowing: Vec<u8>,
    /// A task id or empty. At most one of this and `resumes` is non-empty.
    pub continuation: String,
    pub resumes: String,
    pub workspace: u32,
}

impl std::fmt::Debug for Draft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The statement is human input.
        f.debug_struct("Draft")
            .field("statement_chars", &self.statement.chars().count())
            .field("deadline_s", &self.deadline_s)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// One protected `wl_surface`.
struct PSurface {
    handle: u64,
    surface: WlSurface,
    resource: EclipseProtectedSurfaceV1,
    /// `destroy` was sent: still protected until the surface's next commit.
    ending: bool,
    /// The slot this surface hosts, by id.
    slot: Option<u64>,
    /// Refused input not yet reported, and when the next report may go.
    refused: u32,
    refused_ok_at: Instant,
    refused_timer: bool,
}

/// One commit slot.
struct Slot {
    id: u64,
    /// The hosting [`PSurface::handle`].
    host: u64,
    client: ClientId,
    resource: EclipseCommitSlotV1,
    kind: Kind,
    /// Surface-local logical position of the slot's top left.
    pos: (i32, i32),
    draft: Draft,
    /// The `task_unpause` task id.
    unpause: String,
    /// Bumped on every change the preview depends on.
    revision: u64,
    /// The latest draft held back by the rate limit.
    pending: Option<Draft>,
    last_applied: Option<Instant>,
    timer: bool,
}

/// The global, every protected surface and every slot.
pub struct Protected {
    #[allow(dead_code)] // holds the global alive
    global: GlobalId,
    surfaces: Vec<PSurface>,
    slots: Vec<Slot>,
    next_handle: u64,
    next_slot: u64,
    /// Held pointer buttons whose press was consumed by a slot, so the
    /// release is too. Bit `button & 31`.
    consumed_buttons: u32,
    /// Bit 0: Return, bit 1: KP_Enter, press consumed (release owed).
    consumed_enter: u8,
    /// The pointer's last position came from an origin that may reach a
    /// protected surface, so a scene-driven refresh may show it one.
    pointer_trusted: bool,
    /// Every seam call made, for the tests.
    #[cfg(test)]
    pub(crate) log: Vec<calls::Call>,
}

impl Protected {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<AbyssState, EclipseProtectedSurfaceManagerV1, _>(VERSION, ());
        Self {
            global,
            surfaces: Vec::new(),
            slots: Vec::new(),
            next_handle: 1,
            next_slot: 1,
            consumed_buttons: 0,
            consumed_enter: 0,
            pointer_trusted: true,
            #[cfg(test)]
            log: Vec::new(),
        }
    }

    /// Nothing is protected, so every gate is a no-op.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// How many protected surfaces there are (live or awaiting their commit).
    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    fn slot(&self, id: u64) -> Option<&Slot> {
        self.slots.iter().find(|s| s.id == id)
    }

    fn slot_mut(&mut self, id: u64) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    fn surface(&self, handle: u64) -> Option<&PSurface> {
        self.surfaces.iter().find(|p| p.handle == handle)
    }

    fn surface_mut(&mut self, handle: u64) -> Option<&mut PSurface> {
        self.surfaces.iter_mut().find(|p| p.handle == handle)
    }
}

// ------------------------------------------------------------ surface tree

/// `surface`'s parent: the subsurface parent, or for a popup the surface it
/// ultimately hangs off.
fn parent_of(popups: &PopupManager, surface: &WlSurface) -> Option<WlSurface> {
    if let Some(p) = get_parent(surface) {
        return Some(p);
    }
    let kind = popups.find_popup(surface)?;
    find_popup_root_surface(&kind).ok().filter(|r| r != surface)
}

/// Is `target` `from` or one of its ancestors (subsurface or popup parents)?
fn chain_contains(popups: &PopupManager, from: &WlSurface, target: &WlSurface) -> bool {
    let mut cur = from.clone();
    for _ in 0..MAX_DEPTH {
        if &cur == target {
            return true;
        }
        match parent_of(popups, &cur) {
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}

/// Is `surface` a protected surface, or a subsurface or popup of one? What
/// capture, the agent scene and the input path all ask (COMP-19 §1). Allocates
/// nothing, and is a single length check while nothing is protected.
pub fn is_protected(state: &AbyssState, surface: &WlSurface) -> bool {
    if state.protected.surfaces.is_empty() {
        return false;
    }
    state
        .protected
        .surfaces
        .iter()
        .any(|p| chain_contains(&state.popups, surface, &p.surface))
}

/// Does `surface` have a protected surface at or below it? A toplevel whose
/// composer is a protected subsurface takes its keys on the toplevel, and the
/// compositor cannot tell which widget the client routes them to, so
/// non-physical keys are refused for the whole toplevel.
pub fn has_protected(state: &AbyssState, surface: &WlSurface) -> bool {
    if state.protected.surfaces.is_empty() {
        return false;
    }
    state
        .protected
        .surfaces
        .iter()
        .any(|p| chain_contains(&state.popups, &p.surface, surface))
}

/// The protected surface (handle) `surface` is, or sits under, or contains.
fn related_protected(state: &AbyssState, surface: &WlSurface) -> Option<u64> {
    state
        .protected
        .surfaces
        .iter()
        .find(|p| {
            chain_contains(&state.popups, surface, &p.surface)
                || chain_contains(&state.popups, &p.surface, surface)
        })
        .map(|p| p.handle)
}

// ------------------------------------------------------------ the TCB's side

/// Every live slot id.
pub fn slots(state: &AbyssState) -> impl Iterator<Item = u64> + '_ {
    state.protected.slots.iter().map(|s| s.id)
}

/// The protected surface hosting `slot`.
pub fn host_of(state: &AbyssState, slot: u64) -> Option<WlSurface> {
    let s = state.protected.slot(slot)?;
    state.protected.surface(s.host).map(|p| p.surface.clone())
}

/// The compositor-held draft of a `task_commit` slot, as last applied. An
/// empty draft until the client's first `set_draft`.
pub fn draft(state: &AbyssState, slot: u64) -> Option<&Draft> {
    state.protected.slot(slot).map(|s| &s.draft)
}

/// The task a `task_unpause` slot names, `""` until `set_unpause`.
pub fn unpause_task(state: &AbyssState, slot: u64) -> Option<&str> {
    state.protected.slot(slot).map(|s| s.unpause.as_str())
}

/// What the slot commits.
pub fn kind_of(state: &AbyssState, slot: u64) -> Option<Kind> {
    state.protected.slot(slot).map(|s| s.kind)
}

/// Bumped each time the draft, the unpause task or the position changes: a
/// preview taken at one revision is stale at another.
pub fn revision(state: &AbyssState, slot: u64) -> Option<u64> {
    state.protected.slot(slot).map(|s| s.revision)
}

/// A newer draft is being held back by the 10/s limit. A commit must not
/// spend the older one while this is true: the client believes it sent the
/// newer.
pub fn has_pending_draft(state: &AbyssState, slot: u64) -> bool {
    state.protected.slot(slot).is_some_and(|s| s.pending.is_some())
}

/// The slot's owner.
pub fn client_of(state: &AbyssState, slot: u64) -> Option<ClientId> {
    state.protected.slot(slot).map(|s| s.client.clone())
}

/// Where `surface`'s top left is in global logical coordinates, for a
/// toplevel window and its subsurfaces. `None` for a popup, a layer surface
/// or anything unmapped.
pub fn surface_origin(state: &AbyssState, surface: &WlSurface) -> Option<Point<i32, Logical>> {
    let mut offset = Point::<i32, Logical>::default();
    let mut cur = surface.clone();
    for _ in 0..MAX_DEPTH {
        match get_parent(&cur) {
            Some(parent) => {
                let loc = with_states(&cur, |s| {
                    s.cached_state.get::<SubsurfaceCachedState>().current().location
                });
                offset += loc;
                cur = parent;
            }
            None => {
                let window = crate::shell::window_for_surface(state, &cur)?;
                let at = state.space.element_location(&window)?;
                let origin = at - window.geometry().loc;
                return Some(origin + offset);
            }
        }
    }
    None
}

/// The slot's rectangle in global logical coordinates: the host's origin,
/// the client's position, the compositor's size ([`hooks::slot_size`]).
pub fn slot_rect(state: &AbyssState, slot: u64) -> Option<Rectangle<i32, Logical>> {
    let s = state.protected.slot(slot)?;
    let host = &state.protected.surface(s.host)?.surface;
    let origin = surface_origin(state, host)?;
    let (w, h) = hooks::slot_size(s.kind);
    Some(Rectangle::new(origin + Point::from(s.pos), (w, h).into()))
}

/// `geometry`: the slot's size changed.
pub fn send_geometry(state: &AbyssState, slot: u64, width: i32, height: i32) {
    if let Some(s) = state.protected.slot(slot) {
        s.resource.geometry(width, height);
    }
}

/// `state`: the slot's arming state changed. Reason codes, never rule ids.
pub fn send_state(state: &AbyssState, slot: u64, st: State, reason: Reason) {
    if let Some(s) = state.protected.slot(slot) {
        s.resource.state(st, reason);
    }
}

/// `committed`: the human committed and the task has this id.
pub fn send_committed(state: &AbyssState, slot: u64, task_id: &str) {
    if let Some(s) = state.protected.slot(slot) {
        s.resource.committed(task_id.to_owned());
    }
}

/// `cancelled`.
pub fn send_cancelled(state: &AbyssState, slot: u64) {
    if let Some(s) = state.protected.slot(slot) {
        s.resource.cancelled();
    }
}

// ------------------------------------------------------------ lifecycle

/// Remove slot `id`, telling the TCB first and, if `notify`, the client.
fn cancel_slot(state: &mut AbyssState, id: u64, notify: bool) {
    if state.protected.slot(id).is_none() {
        return;
    }
    calls::slot_gone(state, id);
    let Some(i) = state.protected.slots.iter().position(|s| s.id == id) else {
        return;
    };
    let slot = state.protected.slots.remove(i);
    if let Some(p) = state.protected.surface_mut(slot.host) {
        p.slot = None;
    }
    if notify && slot.resource.is_alive() {
        slot.resource.cancelled();
    }
}

/// Remove protected surface `handle` and its slot.
fn drop_surface(state: &mut AbyssState, handle: u64, notify: bool) {
    let slot = state.protected.surface(handle).and_then(|p| p.slot);
    if let Some(id) = slot {
        cancel_slot(state, id, notify);
    }
    state.protected.surfaces.retain(|p| p.handle != handle);
}

/// `wl_surface.commit`: a protected object destroyed earlier takes effect.
/// Called by the compositor's commit handler for every commit.
pub fn on_commit(state: &mut AbyssState, surface: &WlSurface) {
    if state.protected.surfaces.is_empty() {
        return;
    }
    let ended: Vec<u64> = state
        .protected
        .surfaces
        .iter()
        .filter(|p| p.ending && &p.surface == surface)
        .map(|p| p.handle)
        .collect();
    for h in ended {
        drop_surface(state, h, false);
    }
}

/// A `wl_surface` was destroyed: its protection and slot go with it.
pub fn surface_destroyed(state: &mut AbyssState, surface: &WlSurface) {
    if state.protected.surfaces.is_empty() {
        return;
    }
    let gone: Vec<u64> = state
        .protected
        .surfaces
        .iter()
        .filter(|p| &p.surface == surface)
        .map(|p| p.handle)
        .collect();
    for h in gone {
        drop_surface(state, h, true);
    }
}

// ------------------------------------------------------------ refused input

/// Count one refused event against the protected surface `surface` belongs
/// to, report it to the client at most once a second, and audit it when an
/// agent sent it. Called by [`gate`] and the agent seat, never with content.
pub fn refuse(state: &mut AbyssState, origin: crate::input::Origin, surface: &WlSurface) {
    if origin.is_agent() {
        calls::audit_refused(state, origin, surface);
    }
    let Some(handle) = related_protected(state, surface) else {
        return;
    };
    let now = Instant::now();
    let Some(p) = state.protected.surface_mut(handle) else {
        return;
    };
    p.refused = p.refused.saturating_add(1);
    if now >= p.refused_ok_at {
        flush_refused(state, handle, now);
    } else if !p.refused_timer {
        p.refused_timer = true;
        let wait = p.refused_ok_at - now;
        let armed = state
            .loop_handle
            .insert_source(Timer::from_duration(wait), move |_, _, st| {
                if let Some(p) = st.protected.surface_mut(handle) {
                    p.refused_timer = false;
                }
                flush_refused(st, handle, Instant::now());
                TimeoutAction::Drop
            });
        if armed.is_err() {
            // No timer: report on the next refusal instead.
            if let Some(p) = state.protected.surface_mut(handle) {
                p.refused_timer = false;
            }
        }
    }
}

fn flush_refused(state: &mut AbyssState, handle: u64, now: Instant) {
    let Some(p) = state.protected.surface_mut(handle) else {
        return;
    };
    if p.refused == 0 || now < p.refused_ok_at {
        return;
    }
    let count = std::mem::take(&mut p.refused);
    p.refused_ok_at = now + REFUSED_INTERVAL;
    if p.resource.is_alive() {
        p.resource.input_refused(count);
    }
}

// ------------------------------------------------------------ drafts

/// Make `draft` the slot's draft and tell the TCB.
fn apply_draft(state: &mut AbyssState, id: u64, draft: Draft, now: Instant) {
    let Some(s) = state.protected.slot_mut(id) else {
        return;
    };
    s.draft = draft;
    s.pending = None;
    s.revision += 1;
    s.last_applied = Some(now);
    calls::draft_changed(state, id);
}

/// Apply the draft the rate limit held back, if one is.
fn flush_draft(state: &mut AbyssState, id: u64) {
    let Some(s) = state.protected.slot_mut(id) else {
        return;
    };
    s.timer = false;
    if let Some(d) = s.pending.take() {
        apply_draft(state, id, d, Instant::now());
    }
}

/// `set_draft` after validation: apply now if the slot's 10/s allows, else
/// keep only the latest and flush it on a timer.
fn submit_draft(state: &mut AbyssState, id: u64, draft: Draft) {
    let now = Instant::now();
    let Some(s) = state.protected.slot_mut(id) else {
        return;
    };
    let wait = match s.last_applied {
        Some(t) if now.duration_since(t) < DRAFT_INTERVAL => Some(DRAFT_INTERVAL - now.duration_since(t)),
        _ => None,
    };
    let Some(wait) = wait else {
        apply_draft(state, id, draft, now);
        return;
    };
    s.pending = Some(draft);
    if s.timer {
        return;
    }
    s.timer = true;
    let armed = state
        .loop_handle
        .insert_source(Timer::from_duration(wait), move |_, _, st| {
            flush_draft(st, id);
            TimeoutAction::Drop
        });
    if armed.is_err() {
        // No timer to flush with: spend the draft now rather than strand it.
        flush_draft(state, id);
    }
}

// ------------------------------------------------------------ dispatch

/// User data on an `eclipse_protected_surface_v1`: the entry's handle. 0 is
/// an inert object (the one a protocol error was posted for).
pub struct SurfaceData(u64);

/// User data on an `eclipse_commit_slot_v1`: the slot's id. 0 is inert.
pub struct SlotData(u64);

impl GlobalDispatch<EclipseProtectedSurfaceManagerV1, ()> for AbyssState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<EclipseProtectedSurfaceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let _ = data_init.init(manager, ());
    }
}

impl Dispatch<EclipseProtectedSurfaceManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        manager: &EclipseProtectedSurfaceManagerV1,
        request: eclipse_protected_surface_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            eclipse_protected_surface_manager_v1::Request::Protect { id, surface } => {
                // Object ownership already confines a client to its own
                // objects; this is belt and braces for COMP-19 §2.
                if surface.client().map(|c| c.id()) != Some(client.id()) {
                    let _ = data_init.init(id, SurfaceData(0));
                    manager.post_error(NOT_OWNER, "not a surface of this client");
                    return;
                }
                // A protected object destroyed and waiting for its commit may
                // be replaced; a live one may not.
                let live = state
                    .protected
                    .surfaces
                    .iter()
                    .any(|p| p.surface == surface && !p.ending);
                if live {
                    let _ = data_init.init(id, SurfaceData(0));
                    manager.post_error(ALREADY_PROTECTED, "the surface is already protected");
                    return;
                }
                let stale: Vec<u64> = state
                    .protected
                    .surfaces
                    .iter()
                    .filter(|p| p.surface == surface)
                    .map(|p| p.handle)
                    .collect();
                for h in stale {
                    drop_surface(state, h, false);
                }
                let handle = state.protected.next_handle;
                state.protected.next_handle += 1;
                let resource = data_init.init(id, SurfaceData(handle));
                state.protected.surfaces.push(PSurface {
                    handle,
                    surface,
                    resource,
                    ending: false,
                    slot: None,
                    refused: 0,
                    refused_ok_at: Instant::now(),
                    refused_timer: false,
                });
                // Protection changes who may be pointed at: re-derive what the
                // pointer is over under the new rules.
                state.refresh_pointer_focus();
            }
            eclipse_protected_surface_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<EclipseProtectedSurfaceV1, SurfaceData> for AbyssState {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &EclipseProtectedSurfaceV1,
        request: eclipse_protected_surface_v1::Request,
        data: &SurfaceData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let handle = data.0;
        match request {
            eclipse_protected_surface_v1::Request::GetSlot { id, kind, x, y } => {
                let live = state.protected.surface(handle).is_some_and(|p| !p.ending);
                let kind = match kind {
                    WEnum::Value(k) if live => k,
                    WEnum::Value(_) => {
                        // Destroyed (or inert): nothing to host a slot.
                        let _ = data_init.init(id, SlotData(0));
                        return;
                    }
                    WEnum::Unknown(_) => {
                        let _ = data_init.init(id, SlotData(0));
                        resource.post_error(BAD_KIND, "unknown slot kind");
                        return;
                    }
                };
                let has_slot = state.protected.surface(handle).is_some_and(|p| p.slot.is_some());
                if has_slot {
                    let _ = data_init.init(id, SlotData(0));
                    resource.post_error(SLOT_EXISTS, "the surface already has a slot");
                    return;
                }
                let owned = state
                    .protected
                    .slots
                    .iter()
                    .filter(|s| s.client == client.id())
                    .count();
                if owned >= MAX_SLOTS_PER_CLIENT {
                    let _ = data_init.init(id, SlotData(0));
                    resource.post_error(SLOT_EXISTS, "too many slots for this client");
                    return;
                }
                let sid = state.protected.next_slot;
                state.protected.next_slot += 1;
                let res = data_init.init(id, SlotData(sid));
                let (w, h) = hooks::slot_size(kind);
                state.protected.slots.push(Slot {
                    id: sid,
                    host: handle,
                    client: client.id(),
                    resource: res.clone(),
                    kind,
                    pos: (x, y),
                    draft: Draft::default(),
                    unpause: String::new(),
                    revision: 0,
                    pending: None,
                    last_applied: None,
                    timer: false,
                });
                if let Some(p) = state.protected.surface_mut(handle) {
                    p.slot = Some(sid);
                }
                res.geometry(w, h);
            }
            eclipse_protected_surface_v1::Request::Destroy => {
                // Protected until the next commit; the slot ends now.
                let slot = match state.protected.surface_mut(handle) {
                    Some(p) => {
                        p.ending = true;
                        p.slot
                    }
                    None => None,
                };
                if let Some(id) = slot {
                    cancel_slot(state, id, true);
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _resource: &EclipseProtectedSurfaceV1,
        data: &SurfaceData,
    ) {
        // After a `destroy` request the surface stays protected until its
        // next commit; any other end (the client went away) is immediate.
        let ending = state.protected.surface(data.0).is_some_and(|p| p.ending);
        if data.0 != 0 && !ending {
            drop_surface(state, data.0, false);
        }
    }
}

impl Dispatch<EclipseCommitSlotV1, SlotData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &EclipseCommitSlotV1,
        request: eclipse_commit_slot_v1::Request,
        data: &SlotData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use eclipse_commit_slot_v1::Request;
        let id = data.0;
        // Inert, or cancelled out from under it.
        let Some(kind) = state.protected.slot(id).map(|s| s.kind) else {
            return;
        };
        match request {
            Request::SetDraft {
                package,
                statement,
                deadline_s,
                narrowing,
                continuation,
                resumes,
                workspace,
            } => {
                if kind != Kind::TaskCommit {
                    resource.post_error(BAD_KIND, "set_draft needs a task_commit slot");
                    return;
                }
                if statement.chars().take(STATEMENT_MAX + 1).count() > STATEMENT_MAX {
                    resource.post_error(DRAFT_TOO_LARGE, "statement over 1000 characters");
                    return;
                }
                if !continuation.is_empty() && !resumes.is_empty() {
                    // COMP-19 §2 forbids it and names no error code: the draft
                    // is refused, not stored, and the card is told why.
                    if let Some(s) = state.protected.slot_mut(id) {
                        s.pending = None;
                        s.draft = Draft::default();
                        s.revision += 1;
                    }
                    calls::draft_changed(state, id);
                    resource.state(State::Refused, Reason::Refused);
                    return;
                }
                submit_draft(
                    state,
                    id,
                    Draft {
                        package,
                        statement,
                        deadline_s,
                        narrowing,
                        continuation,
                        resumes,
                        workspace,
                    },
                );
            }
            Request::SetUnpause { task_id } => {
                if kind != Kind::TaskUnpause {
                    resource.post_error(BAD_KIND, "set_unpause needs a task_unpause slot");
                    return;
                }
                if let Some(s) = state.protected.slot_mut(id) {
                    s.unpause = task_id;
                    s.revision += 1;
                }
                calls::draft_changed(state, id);
            }
            Request::Move { x, y } => {
                if let Some(s) = state.protected.slot_mut(id) {
                    s.pos = (x, y);
                    s.revision += 1;
                }
                calls::slot_moved(state, id);
            }
            Request::Cancel => {
                if let Some(s) = state.protected.slot_mut(id) {
                    s.draft = Draft::default();
                    s.unpause.clear();
                    s.pending = None;
                    s.revision += 1;
                }
                calls::draft_changed(state, id);
                resource.cancelled();
            }
            Request::Destroy => cancel_slot(state, id, false),
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, _resource: &EclipseCommitSlotV1, data: &SlotData) {
        if data.0 != 0 {
            cancel_slot(state, data.0, false);
        }
    }
}
