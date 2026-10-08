// SPDX-License-Identifier: AGPL-3.0-only
//! Client glue for `eclipse_protected_surface_v1` (COMP-19 §2) that works from
//! a display and a surface somebody else owns.
//!
//! The console's window is created by winit/iced, which owns the `wl_display`
//! connection and reads its socket. This module attaches to that display by
//! pointer (`raw-window-handle` hands both out), makes a private event queue
//! for the protected-surface objects, and never reads the socket itself after
//! start-up: the host's own loop reads, libwayland files our events into our
//! queue, and [`Protected::pump`] (call it from the UI tick) hands them on.
//!
//! The client draws nothing here. The compositor draws the slot and the
//! client only keeps the geometry it reports free (A-08 §8).
//!
//! A protocol error kills the whole connection, which is the console's own
//! window. So every request that could raise one (a second slot, a draft on an
//! unpause slot) is refused here first.

use std::{
    ffi::c_void,
    sync::mpsc::{channel, Receiver, Sender},
};

use wayland_backend::client::{Backend, ObjectId};
use wayland_client::{
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_registry::WlRegistry, wl_surface::WlSurface},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};

#[allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
#[allow(non_upper_case_globals, non_snake_case, unused_imports)]
#[allow(missing_docs, clippy::all)]
mod proto {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/eclipse-protected-surface-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/eclipse-protected-surface-v1.xml");
}

use proto::{
    eclipse_commit_slot_v1::{self, EclipseCommitSlotV1},
    eclipse_protected_surface_manager_v1::EclipseProtectedSurfaceManagerV1,
    eclipse_protected_surface_v1::{self, EclipseProtectedSurfaceV1},
};

#[derive(Debug)]
pub enum Error {
    /// A null or unusable display / surface pointer.
    BadHandle(&'static str),
    /// The compositor has no `eclipse_protected_surface_manager_v1` global:
    /// not abyss, or the feature is off. The console then has no slot and must
    /// say so.
    NoManager,
    Wayland(String),
    /// Refused client-side because the compositor would raise a protocol error.
    Refused(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadHandle(w) => write!(f, "bad {w} pointer"),
            Error::NoManager => write!(f, "the compositor offers no protected surfaces"),
            Error::Wayland(m) => write!(f, "wayland: {m}"),
            Error::Refused(m) => write!(f, "{m}"),
        }
    }
}

/// Slot kind (COMP-19 §2): the only two at v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// Dispatch a task, or resume a session.
    TaskCommit = 1,
    TaskUnpause = 2,
}

/// The draft the compositor previews (`set_draft`). `narrowing` is a CBOR scope
/// list that may only shrink; `continuation` and `resumes` are a task id or
/// empty, at most one non-empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub package: String,
    pub statement: String,
    pub deadline_s: u32,
    pub narrowing: Vec<u8>,
    pub continuation: String,
    pub resumes: String,
    pub workspace: u32,
}

/// The account rule: empty, or 1 to 32 of `[A-Za-z0-9_-]`.
fn account_name_ok(a: &str) -> bool {
    a.len() <= 32
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Draft {
    fn check(&self) -> Result<(), Error> {
        if !self.continuation.is_empty() && !self.resumes.is_empty() {
            return Err(Error::Refused("continuation and resumes are exclusive"));
        }
        if self.statement.chars().count() > 1000 {
            return Err(Error::Refused("statement is over 1000 characters"));
        }
        let nul = |s: &str| s.as_bytes().contains(&0);
        if nul(&self.package) || nul(&self.statement) || nul(&self.continuation) || nul(&self.resumes) {
            return Err(Error::Refused("draft strings may not contain NUL"));
        }
        Ok(())
    }
}

/// `state` event, by name. The numeric order is the spec's list
/// (previewing=0 .. refused=4, matching abyss); an unrecognised
/// value is kept, not guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    Previewing,
    Armed,
    Disarmed,
    Suspended,
    Refused,
    Unknown(u32),
}

impl SlotState {
    pub fn from_wire(n: u32) -> SlotState {
        match n {
            0 => SlotState::Previewing,
            1 => SlotState::Armed,
            2 => SlotState::Disarmed,
            3 => SlotState::Suspended,
            4 => SlotState::Refused,
            other => SlotState::Unknown(other),
        }
    }
}

/// `state` event reason (COMP-19, abyss numbering). Never a rule id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotReason {
    None,
    Overflow,
    PolicyUnavailable,
    PreviewStale,
    NotFocused,
    Occluded,
    Locked,
    Modal,
    Fullscreen,
    Refused,
    AgentdUnavailable,
    Unsupported,
    Unknown(u32),
}

impl SlotReason {
    pub fn from_wire(n: u32) -> SlotReason {
        match n {
            0 => SlotReason::None,
            1 => SlotReason::Overflow,
            2 => SlotReason::PolicyUnavailable,
            3 => SlotReason::PreviewStale,
            4 => SlotReason::NotFocused,
            5 => SlotReason::Occluded,
            6 => SlotReason::Locked,
            7 => SlotReason::Modal,
            8 => SlotReason::Fullscreen,
            9 => SlotReason::Refused,
            10 => SlotReason::AgentdUnavailable,
            11 => SlotReason::Unsupported,
            other => SlotReason::Unknown(other),
        }
    }
}

/// Everything the compositor tells the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Keep this rect (surface-local logical px) free of content.
    Geometry {
        width: i32,
        height: i32,
    },
    State {
        state: SlotState,
        reason: SlotReason,
    },
    Committed {
        task_id: String,
    },
    Cancelled,
    /// Non-physical input was dropped (coalesced, at most 1/s).
    InputRefused {
        count: u32,
    },
}

struct State {
    tx: Sender<Event>,
    /// Set by `committed` / `cancelled`; the slot is destroyed on the next
    /// pump so a new `get_slot` is possible.
    slot_ended: bool,
}

pub struct Protected {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    manager: EclipseProtectedSurfaceManagerV1,
    surface: EclipseProtectedSurfaceV1,
    slot: Option<(EclipseCommitSlotV1, SlotKind)>,
    rx: Receiver<Event>,
}

impl Protected {
    /// Protect `surface` on `display`.
    ///
    /// # Safety
    /// `display` must be a live `wl_display*` and `surface` a live
    /// `wl_surface*` created on it, and both must outlive the returned value.
    /// winit's `raw-window-handle` display and surface handles satisfy this for
    /// the lifetime of the window.
    pub unsafe fn new(display: *mut c_void, surface: *mut c_void) -> Result<Protected, Error> {
        if display.is_null() {
            return Err(Error::BadHandle("display"));
        }
        if surface.is_null() {
            return Err(Error::BadHandle("surface"));
        }
        // SAFETY: caller contract above.
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        // SAFETY: caller contract above; the interface check inside `from_ptr`
        // rejects a pointer that is not a wl_surface.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }
            .map_err(|_| Error::BadHandle("surface"))?;
        let wl_surface = WlSurface::from_id(&conn, id).map_err(|_| Error::BadHandle("surface"))?;

        let (tx, rx) = channel();
        let (globals, queue) =
            registry_queue_init::<State>(&conn).map_err(|e| Error::Wayland(e.to_string()))?;
        let qh = queue.handle();
        let manager: EclipseProtectedSurfaceManagerV1 =
            globals.bind(&qh, 1..=2, ()).map_err(|_| Error::NoManager)?;
        let surface = manager.protect(&wl_surface, &qh, ());
        conn.flush().map_err(|e| Error::Wayland(e.to_string()))?;
        Ok(Protected {
            conn,
            queue,
            qh,
            state: State {
                tx,
                slot_ended: false,
            },
            manager,
            surface,
            slot: None,
            rx,
        })
    }

    /// Events from the compositor, in order.
    pub fn events(&self) -> &Receiver<Event> {
        &self.rx
    }

    /// Hand on whatever the host's loop has already read for us, and flush our
    /// requests. Never blocks. Call from the UI tick.
    pub fn pump(&mut self) -> Result<(), Error> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| Error::Wayland(e.to_string()))?;
        if std::mem::take(&mut self.state.slot_ended) {
            if let Some((slot, _)) = self.slot.take() {
                slot.destroy();
            }
        }
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))
    }

    pub fn has_slot(&self) -> bool {
        self.slot.is_some()
    }

    /// Create the one slot, its top-left at surface-local `(x, y)`.
    pub fn get_slot(&mut self, kind: SlotKind, x: i32, y: i32) -> Result<(), Error> {
        if self.slot.is_some() {
            return Err(Error::Refused("a slot already exists on this surface"));
        }
        let slot = self.surface.get_slot(
            match kind {
                SlotKind::TaskCommit => eclipse_protected_surface_v1::Kind::TaskCommit,
                SlotKind::TaskUnpause => eclipse_protected_surface_v1::Kind::TaskUnpause,
            },
            x,
            y,
            &self.qh,
            (),
        );
        self.slot = Some((slot, kind));
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))
    }

    fn slot_of(&self, kind: SlotKind) -> Result<&EclipseCommitSlotV1, Error> {
        match &self.slot {
            Some((s, k)) if *k == kind => Ok(s),
            Some(_) => Err(Error::Refused("wrong slot kind for this request")),
            None => Err(Error::Refused("no slot")),
        }
    }

    /// Replace the draft of a `task.commit` slot. Disarms it.
    pub fn set_draft(&self, d: &Draft) -> Result<(), Error> {
        d.check()?;
        let slot = self.slot_of(SlotKind::TaskCommit)?;
        slot.set_draft(
            d.package.clone(),
            d.statement.clone(),
            d.deadline_s,
            d.narrowing.clone(),
            d.continuation.clone(),
            d.resumes.clone(),
            d.workspace,
        );
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))
    }

    /// Name the account a `task.commit` slot's task runs under (ADR 0077):
    /// empty for the default, else 1 to 32 of `[A-Za-z0-9_-]`. Part of the
    /// draft; the compositor re-previews and disarms like `set_draft`.
    ///
    /// Returns `Ok(true)` when sent, and `Ok(false)` without sending anything
    /// when the compositor speaks protocol v1 (it has no accounts; the task
    /// runs under the default one). A bad name or a missing / wrong-kind slot
    /// is `Err`, refused here so the compositor never raises `bad_account`.
    pub fn set_account(&self, account: &str) -> Result<bool, Error> {
        if !account_name_ok(account) {
            return Err(Error::Refused("account is not 1 to 32 of A-Za-z0-9_-"));
        }
        let slot = self.slot_of(SlotKind::TaskCommit)?;
        if slot.version() < 2 {
            return Ok(false);
        }
        slot.set_account(account.to_owned());
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))?;
        Ok(true)
    }

    /// Name the task a `task.unpause` slot would unpause.
    pub fn set_unpause(&self, task_id: &str) -> Result<(), Error> {
        if task_id.as_bytes().contains(&0) {
            return Err(Error::Refused("task id may not contain NUL"));
        }
        let slot = self.slot_of(SlotKind::TaskUnpause)?;
        slot.set_unpause(task_id.to_owned());
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))
    }

    /// Move the slot. Disarms it.
    pub fn move_to(&self, x: i32, y: i32) -> Result<(), Error> {
        let Some((slot, _)) = &self.slot else {
            return Err(Error::Refused("no slot"));
        };
        slot._move(x, y);
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))
    }

    pub fn cancel(&self) -> Result<(), Error> {
        let Some((slot, _)) = &self.slot else {
            return Err(Error::Refused("no slot"));
        };
        slot.cancel();
        self.conn.flush().map_err(|e| Error::Wayland(e.to_string()))
    }
}

impl Drop for Protected {
    fn drop(&mut self) {
        if let Some((slot, _)) = self.slot.take() {
            slot.destroy();
        }
        self.surface.destroy();
        self.manager.destroy();
        let _ = self.conn.flush();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<EclipseProtectedSurfaceManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &EclipseProtectedSurfaceManagerV1,
        _: proto::eclipse_protected_surface_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<EclipseProtectedSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &EclipseProtectedSurfaceV1,
        event: eclipse_protected_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let eclipse_protected_surface_v1::Event::InputRefused { count } = event;
        {
            let _ = state.tx.send(Event::InputRefused { count });
        }
    }
}

impl Dispatch<EclipseCommitSlotV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &EclipseCommitSlotV1,
        event: eclipse_commit_slot_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use eclipse_commit_slot_v1::Event as E;
        let out = match event {
            E::Geometry { width, height } => Event::Geometry { width, height },
            E::State { state: s, reason } => Event::State {
                state: SlotState::from_wire(s.into()),
                reason: SlotReason::from_wire(reason.into()),
            },
            E::Committed { task_id } => {
                state.slot_ended = true;
                Event::Committed { task_id }
            }
            E::Cancelled => {
                state.slot_ended = true;
                Event::Cancelled
            }
        };
        let _ = state.tx.send(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_state_follows_the_spec_order() {
        let names = [
            SlotState::Previewing,
            SlotState::Armed,
            SlotState::Disarmed,
            SlotState::Suspended,
            SlotState::Refused,
        ];
        for (i, n) in names.iter().enumerate() {
            assert_eq!(SlotState::from_wire(i as u32), *n);
        }
        assert_eq!(SlotState::from_wire(9), SlotState::Unknown(9));
        assert_eq!(SlotReason::from_wire(3), SlotReason::PreviewStale);
        assert_eq!(SlotReason::from_wire(10), SlotReason::AgentdUnavailable);
        assert_eq!(SlotReason::from_wire(99), SlotReason::Unknown(99));
    }

    #[test]
    fn draft_rules_are_checked_before_the_wire() {
        let ok = Draft {
            package: "ec-ref-agent".into(),
            statement: "do it".into(),
            ..Draft::default()
        };
        assert!(ok.check().is_ok());
        let both = Draft {
            continuation: "t1".into(),
            resumes: "t2".into(),
            ..ok.clone()
        };
        assert!(both.check().is_err());
        let nul = Draft {
            statement: "a\0b".into(),
            ..ok
        };
        assert!(nul.check().is_err());
    }

    #[test]
    fn account_names_are_checked_before_the_wire() {
        for bad in ["a.b", "has space", "x\0", &"y".repeat(33)] {
            assert!(!account_name_ok(bad), "{bad:?}");
        }
        for good in ["", "work", "a-b_C9", &"z".repeat(32)] {
            assert!(account_name_ok(good), "{good:?}");
        }
    }

    #[test]
    fn null_pointers_are_refused() {
        // SAFETY: null is rejected before anything is dereferenced.
        let r = unsafe { Protected::new(std::ptr::null_mut(), std::ptr::null_mut()) };
        assert!(matches!(r, Err(Error::BadHandle("display"))));
    }
}
