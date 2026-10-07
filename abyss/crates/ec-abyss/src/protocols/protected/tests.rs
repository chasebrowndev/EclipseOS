// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_protected_surface_v1` against the in-process harness (COMP-19 §9):
//! the protocol errors and limits, draft coalescing, the origin matrix, Enter
//! isolation and the slot rectangle, all with a real `wayland-client` that
//! records what the compositor delivers to it.

use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ec_protocols::protected::client::{
    eclipse_commit_slot_v1::{self, EclipseCommitSlotV1},
    eclipse_protected_surface_manager_v1::EclipseProtectedSurfaceManagerV1,
    eclipse_protected_surface_v1::{self, EclipseProtectedSurfaceV1, Kind as CKind},
};
use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
use smithay::input::keyboard::Keycode;
use smithay::input::pointer::AxisFrame;
use smithay::utils::{Logical, Point};
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_pointer::{self, WlPointer},
        wl_registry,
        wl_seat::{self, WlSeat},
        wl_shm,
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use super::calls::Call;
use super::*;
use crate::input::Origin;
use crate::shell::focus::state_tests::{harness, Harness};

const KEY_ENTER: u32 = 28;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_A: u32 = 30;
const BTN_LEFT: u32 = 0x110;

/// Buffer size of a mapped window: roomy enough for a slot.
const WIN: (i32, i32) = (560, 400);

// error codes
const E_ALREADY_PROTECTED: u32 = 0;
const E_SLOT_EXISTS: u32 = 1;
const E_BAD_KIND: u32 = 3;
const E_DRAFT_TOO_LARGE: u32 = 4;

/// What the compositor delivered to the client.
#[derive(Default)]
struct Seen {
    globals: Vec<(u32, String, u32)>,
    has_kb: bool,
    has_ptr: bool,
    keys: Vec<(u32, bool)>,
    kb_enters: u32,
    ptr_enters: u32,
    ptr_leaves: u32,
    ptr_motions: u32,
    buttons: Vec<(u32, bool)>,
    axes: u32,
    refused: Vec<u32>,
    geometry: Vec<(i32, i32)>,
    states: Vec<(u32, u32)>,
    cancelled: u32,
    committed: Vec<String>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &wl_registry::WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            s.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        e: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        _: &mut Self,
        xdg: &XdgSurface,
        e: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(s: &mut Self, seat: &WlSeat, e: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = e
        {
            if caps.contains(wl_seat::Capability::Keyboard) && !s.has_kb {
                s.has_kb = true;
                seat.get_keyboard(qh, ());
            }
            if caps.contains(wl_seat::Capability::Pointer) && !s.has_ptr {
                s.has_ptr = true;
                seat.get_pointer(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Enter { .. } => s.kb_enters += 1,
            wl_keyboard::Event::Key { key, state, .. } => {
                s.keys
                    .push((key, state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            wl_pointer::Event::Enter { .. } => s.ptr_enters += 1,
            wl_pointer::Event::Leave { .. } => s.ptr_leaves += 1,
            wl_pointer::Event::Motion { .. } => s.ptr_motions += 1,
            wl_pointer::Event::Button { button, state, .. } => {
                s.buttons
                    .push((button, state == WEnum::Value(wl_pointer::ButtonState::Pressed)));
            }
            wl_pointer::Event::Axis { .. } => s.axes += 1,
            _ => {}
        }
    }
}

impl Dispatch<EclipseProtectedSurfaceV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseProtectedSurfaceV1,
        e: eclipse_protected_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_protected_surface_v1::Event::InputRefused { count } = e {
            s.refused.push(count);
        }
    }
}

impl Dispatch<EclipseCommitSlotV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseCommitSlotV1,
        e: eclipse_commit_slot_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use eclipse_commit_slot_v1::Event;
        match e {
            Event::Geometry { width, height } => s.geometry.push((width, height)),
            Event::State { state, reason } => {
                if let (WEnum::Value(st), WEnum::Value(r)) = (state, reason) {
                    s.states.push((st as u32, r as u32));
                }
            }
            Event::Committed { task_id } => s.committed.push(task_id),
            Event::Cancelled => s.cancelled += 1,
            _ => {}
        }
    }
}

delegate_noop!(Seen: WlCompositor);
delegate_noop!(Seen: WlSubcompositor);
delegate_noop!(Seen: ignore WlSubsurface);
delegate_noop!(Seen: WlShmPool);
delegate_noop!(Seen: ignore wl_shm::WlShm);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore XdgToplevel);
delegate_noop!(Seen: EclipseProtectedSurfaceManagerV1);

/// A mapped toplevel's client objects.
struct Win {
    surface: WlSurface,
    _toplevel: XdgToplevel,
    _xdg: XdgSurface,
}

struct Cli {
    conn: Connection,
    queue: EventQueue<Seen>,
    seen: Seen,
    compositor: WlCompositor,
    subcompositor: WlSubcompositor,
    shm: wl_shm::WlShm,
    wm: XdgWmBase,
    manager: EclipseProtectedSurfaceManagerV1,
}

impl Cli {
    fn connect(h: &mut Harness) -> Self {
        let (server, client) = UnixStream::pair().expect("socket pair");
        h.state
            .display_handle
            .insert_client(server, crate::state::client_state())
            .expect("insert client");
        let conn = Connection::from_socket(client).expect("connect");
        let mut q = conn.new_event_queue();
        let qh = q.handle();
        let registry = conn.display().get_registry(&qh, ());
        let mut seen = Seen::default();
        let pump = |seen: &mut Seen, q: &mut EventQueue<Seen>, h: &mut Harness| {
            for _ in 0..8 {
                let _ = conn.flush();
                h.dispatch();
                if let Some(g) = conn.prepare_read() {
                    let _ = g.read();
                }
                let _ = q.dispatch_pending(seen);
            }
        };
        pump(&mut seen, &mut q, h);
        let find = |seen: &Seen, name: &str| {
            seen.globals
                .iter()
                .find(|(_, i, _)| i == name)
                .map(|(n, _, v)| (*n, *v))
                .unwrap_or_else(|| panic!("{name} not advertised"))
        };
        let (n, v) = find(&seen, "wl_compositor");
        let compositor = registry.bind(n, v.min(5), &qh, ());
        let (n, _) = find(&seen, "wl_subcompositor");
        let subcompositor = registry.bind(n, 1, &qh, ());
        let (n, _) = find(&seen, "wl_shm");
        let shm = registry.bind(n, 1, &qh, ());
        let (n, _) = find(&seen, "xdg_wm_base");
        let wm = registry.bind(n, 1, &qh, ());
        let (n, v) = find(&seen, "wl_seat");
        let _seat: WlSeat = registry.bind(n, v.min(5), &qh, ());
        let (n, v) = find(&seen, "eclipse_protected_surface_manager_v1");
        assert_eq!(v, 1);
        let manager = registry.bind(n, 1, &qh, ());
        pump(&mut seen, &mut q, h);
        Self {
            conn,
            queue: q,
            seen,
            compositor,
            subcompositor,
            shm,
            wm,
            manager,
        }
    }

    fn pump(&mut self, h: &mut Harness) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            h.dispatch();
            if let Some(g) = self.conn.prepare_read() {
                let _ = g.read();
            }
            let _ = self.queue.dispatch_pending(&mut self.seen);
        }
    }

    /// The protocol error the compositor killed this client with: the
    /// interface and the code.
    fn error(&self) -> Option<(String, u32)> {
        self.conn.protocol_error().map(|e| (e.object_interface, e.code))
    }

    fn buffer(&self, w: i32, h: i32) -> WlBuffer {
        let size = w * h * 4;
        // SAFETY: an anonymous memfd; the name is a valid C string.
        let fd = unsafe { libc::memfd_create(c"abyss-protected-test".as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create");
        // SAFETY: `fd` was just created and nothing else owns it.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.set_len(size as u64).expect("size memfd");
        let qh = self.queue.handle();
        let pool = self.shm.create_pool(file.as_fd(), size, &qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        buffer
    }

    /// An xdg toplevel with a [`WIN`]-sized buffer, mapped.
    fn map(&mut self, h: &mut Harness) -> Win {
        let qh = self.queue.handle();
        let surface = self.compositor.create_surface(&qh, ());
        let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        surface.commit();
        self.pump(h);
        surface.attach(Some(&self.buffer(WIN.0, WIN.1)), 0, 0);
        surface.commit();
        self.pump(h);
        Win {
            surface,
            _toplevel: toplevel,
            _xdg: xdg,
        }
    }

    /// A subsurface of `parent`, with a small buffer, at (`x`, `y`).
    fn subsurface(
        &mut self,
        h: &mut Harness,
        parent: &WlSurface,
        x: i32,
        y: i32,
    ) -> (WlSurface, WlSubsurface) {
        let qh = self.queue.handle();
        let child = self.compositor.create_surface(&qh, ());
        let sub = self.subcompositor.get_subsurface(&child, parent, &qh, ());
        sub.set_position(x, y);
        child.attach(Some(&self.buffer(100, 100)), 0, 0);
        child.commit();
        parent.commit();
        self.pump(h);
        (child, sub)
    }

    fn protect(&mut self, h: &mut Harness, surface: &WlSurface) -> EclipseProtectedSurfaceV1 {
        let qh = self.queue.handle();
        let p = self.manager.protect(surface, &qh, ());
        self.pump(h);
        p
    }

    fn slot(
        &mut self,
        h: &mut Harness,
        p: &EclipseProtectedSurfaceV1,
        kind: CKind,
        x: i32,
        y: i32,
    ) -> EclipseCommitSlotV1 {
        let qh = self.queue.handle();
        let s = p.get_slot(kind, x, y, &qh, ());
        self.pump(h);
        s
    }
}

fn say(slot: &EclipseCommitSlotV1, statement: &str) {
    slot.set_draft(
        "pkg".into(),
        statement.into(),
        60,
        Vec::new(),
        String::new(),
        String::new(),
        0,
    );
}

/// The compositor-side surface of the `n`th toplevel.
fn server_surface(
    h: &Harness,
    n: usize,
) -> smithay::reexports::wayland_server::protocol::wl_surface::WlSurface {
    h.state.xdg_shell_state.toplevel_surfaces()[n]
        .wl_surface()
        .clone()
}

/// The global rectangle of the `n`th toplevel's surface origin.
fn origin(h: &Harness, n: usize) -> Point<i32, Logical> {
    let s = server_surface(h, n);
    surface_origin(&h.state, &s).expect("mapped")
}

fn pt(x: i32, y: i32) -> Point<i32, Logical> {
    (x, y).into()
}

fn now(h: &Harness) -> u32 {
    h.state.start_time.elapsed().as_millis() as u32
}

// ---- server-side event drivers: the compositor's own entry points

fn key(h: &mut Harness, origin: Origin, code: u32, pressed: bool) {
    let t = now(h);
    h.state.with_origin(origin, |s| {
        s.keyboard_key(
            Keycode::new(code + 8),
            if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            },
            t,
        )
    });
}

fn motion(h: &mut Harness, origin: Origin, at: Point<i32, Logical>) {
    let t = now(h);
    let pos: Point<f64, Logical> = (f64::from(at.x), f64::from(at.y)).into();
    h.state.with_origin(origin, |s| s.pointer_moved(pos, t));
}

fn button(h: &mut Harness, origin: Origin, pressed: bool) {
    let t = now(h);
    h.state.with_origin(origin, |s| {
        s.pointer_button(
            BTN_LEFT,
            if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            },
            t,
        )
    });
}

/// A scroll frame the way a virtual pointer or the harness injects one.
fn scroll(h: &mut Harness, origin: Origin) {
    let t = now(h);
    h.state.with_origin(origin, |s| {
        s.inject_pointer_axis(
            AxisFrame::new(t)
                .source(AxisSource::Wheel)
                .value(Axis::Vertical, 10.0),
        )
    });
}

/// A client with one mapped, keyboard- and pointer-focused window, protected.
struct Rig {
    h: Harness,
    c: Cli,
    win: Win,
    prot: EclipseProtectedSurfaceV1,
}

fn rig() -> Rig {
    let mut h = harness();
    let mut c = Cli::connect(&mut h);
    let win = c.map(&mut h);
    let prot = c.protect(&mut h, &win.surface);
    let mut r = Rig { h, c, win, prot };
    r.focus();
    r
}

impl Rig {
    fn pump(&mut self) {
        self.c.pump(&mut self.h);
    }

    /// Focus the window and put the pointer on it, as the human would.
    fn focus(&mut self) {
        let surface = server_surface(&self.h, 0);
        let window = crate::shell::window_for_surface(&self.h.state, &surface).expect("window");
        self.h.state.with_origin(Origin::Physical, |s| {
            crate::shell::focus::focus_window(s, &window)
        });
        let o = origin(&self.h, 0);
        motion(&mut self.h, Origin::Physical, o + pt(20, 20));
        self.pump();
        assert!(self.c.seen.kb_enters >= 1, "keyboard focus reached the client");
        assert!(self.c.seen.ptr_enters >= 1, "pointer focus reached the client");
    }

    fn at(&self, x: i32, y: i32) -> Point<i32, Logical> {
        origin(&self.h, 0) + pt(x, y)
    }
}

// ------------------------------------------------------------ protocol errors

#[test]
fn a_second_protect_is_already_protected() {
    let mut h = harness();
    let mut c = Cli::connect(&mut h);
    let win = c.map(&mut h);
    let _a = c.protect(&mut h, &win.surface);
    assert_eq!(c.error(), None);
    let _b = c.protect(&mut h, &win.surface);
    assert_eq!(
        c.error(),
        Some(("eclipse_protected_surface_manager_v1".into(), E_ALREADY_PROTECTED))
    );
}

#[test]
fn a_second_slot_is_slot_exists() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 10, 10);
    assert_eq!(r.c.error(), None);
    let _t = r.c.slot(&mut r.h, &r.prot, CKind::TaskUnpause, 10, 10);
    assert_eq!(
        r.c.error(),
        Some(("eclipse_protected_surface_v1".into(), E_SLOT_EXISTS))
    );
}

#[test]
fn the_fifth_slot_of_a_client_is_slot_exists() {
    let mut h = harness();
    let mut c = Cli::connect(&mut h);
    let mut keep = Vec::new();
    for _ in 0..MAX_SLOTS_PER_CLIENT {
        let win = c.map(&mut h);
        let p = c.protect(&mut h, &win.surface);
        let s = c.slot(&mut h, &p, CKind::TaskCommit, 0, 0);
        keep.push((win, p, s));
    }
    assert_eq!(c.error(), None, "four are allowed");
    assert_eq!(slots(&h.state).count(), MAX_SLOTS_PER_CLIENT);
    let win = c.map(&mut h);
    let p = c.protect(&mut h, &win.surface);
    let _s = c.slot(&mut h, &p, CKind::TaskCommit, 0, 0);
    assert_eq!(
        c.error(),
        Some(("eclipse_protected_surface_v1".into(), E_SLOT_EXISTS))
    );
}

#[test]
fn set_draft_needs_a_commit_slot_and_set_unpause_an_unpause_slot() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskUnpause, 0, 0);
    say(&s, "x");
    r.pump();
    assert_eq!(
        r.c.error(),
        Some(("eclipse_commit_slot_v1".into(), E_BAD_KIND)),
        "set_draft on task_unpause"
    );

    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    s.set_unpause("01TASK".into());
    r.pump();
    assert_eq!(
        r.c.error(),
        Some(("eclipse_commit_slot_v1".into(), E_BAD_KIND)),
        "set_unpause on task_commit"
    );
}

#[test]
fn a_statement_over_1000_characters_is_draft_too_large() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    say(&s, &"a".repeat(STATEMENT_MAX));
    r.pump();
    assert_eq!(r.c.error(), None, "exactly 1000 is fine");
    say(&s, &"a".repeat(STATEMENT_MAX + 1));
    r.pump();
    assert_eq!(
        r.c.error(),
        Some(("eclipse_commit_slot_v1".into(), E_DRAFT_TOO_LARGE))
    );
}

/// The limit counts characters, not bytes.
#[test]
fn the_statement_limit_counts_characters() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    say(&s, &"\u{e9}".repeat(STATEMENT_MAX));
    r.pump();
    assert_eq!(r.c.error(), None);
}

#[test]
fn the_pointer_on_a_drawn_card_gets_the_compositors_arrow() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 4, 5);
    r.pump();
    crate::trusted_ui::commit::tick(&mut r.h.state);
    let rects = crate::trusted_ui::commit::drawn(&r.h.state.trusted_ui.cards);
    let card = *rects.first().expect("the card is drawn");
    let centre = card.loc + Point::from((card.size.w / 2, card.size.h / 2));
    r.h.state.pointer_location = centre.to_f64();
    assert!(crate::trusted_ui::pointer_on_card(&r.h.state));
    r.h.state.pointer_location = (card.loc - Point::from((1, 1))).to_f64();
    assert!(!crate::trusted_ui::pointer_on_card(&r.h.state));
}

#[test]
fn a_slot_is_told_its_size_and_the_unpause_task_is_held() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskUnpause, 4, 5);
    assert_eq!(r.c.seen.geometry, vec![hooks::slot_size(Kind::TaskUnpause)]);
    s.set_unpause("01TASK".into());
    r.pump();
    let id = slots(&r.h.state).next().expect("slot");
    assert_eq!(unpause_task(&r.h.state, id), Some("01TASK"));
    assert_eq!(kind_of(&r.h.state, id), Some(Kind::TaskUnpause));
}

#[test]
fn both_continuation_and_resumes_is_refused_not_stored() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    s.set_draft(
        "pkg".into(),
        "do it".into(),
        0,
        Vec::new(),
        "A".into(),
        "B".into(),
        0,
    );
    r.pump();
    assert_eq!(r.c.error(), None);
    // The trusted side also reports the slot's arming state; the refusal is
    // what matters here.
    assert!(r
        .c
        .seen
        .states
        .contains(&(State::Refused as u32, Reason::Refused as u32)));
    let id = slots(&r.h.state).next().expect("slot");
    assert_eq!(draft(&r.h.state, id).map(|d| d.statement.as_str()), Some(""));
}

// ------------------------------------------------------------ the registry

#[test]
fn the_compositor_holds_the_draft() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 1, 2);
    s.set_draft(
        "org.example.pkg".into(),
        "summarise the inbox".into(),
        90,
        vec![1, 2, 3],
        String::new(),
        "01RESUME".into(),
        2,
    );
    r.pump();
    let id = slots(&r.h.state).next().expect("slot");
    let d = draft(&r.h.state, id).expect("draft");
    assert_eq!(d.package, "org.example.pkg");
    assert_eq!(d.statement, "summarise the inbox");
    assert_eq!(d.deadline_s, 90);
    assert_eq!(d.narrowing, vec![1, 2, 3]);
    assert_eq!(d.resumes, "01RESUME");
    assert_eq!(d.continuation, "");
    assert_eq!(d.workspace, 2);
    assert_eq!(host_of(&r.h.state, id), Some(server_surface(&r.h, 0)));
    assert!(is_protected(&r.h.state, &server_surface(&r.h, 0)));
    let rect = slot_rect(&r.h.state, id).expect("rect");
    let (w, h) = hooks::slot_size(Kind::TaskCommit);
    assert_eq!(rect.size, (w, h).into());
    assert_eq!(rect.loc, origin(&r.h, 0) + pt(1, 2));
    s._move(30, 40);
    r.pump();
    assert_eq!(
        slot_rect(&r.h.state, id).map(|r| r.loc),
        Some(origin(&r.h, 0) + pt(30, 40))
    );
    assert!(r.h.state.protected.log.contains(&Call::Moved(id)));
}

#[test]
fn a_subsurface_of_a_protected_surface_is_protected() {
    let mut h = harness();
    let mut c = Cli::connect(&mut h);
    let win = c.map(&mut h);
    let (child, _sub) = c.subsurface(&mut h, &win.surface, 10, 10);
    let _p = c.protect(&mut h, &win.surface);
    // Resolve the child on the compositor's side: it is the only subsurface.
    let root = server_surface(&h, 0);
    let mut found = None;
    let _ = &child;
    // The child's own surface is reached through the root's surface tree.
    smithay::wayland::compositor::with_surface_tree_downward(
        &root,
        (),
        |_, _, _| smithay::wayland::compositor::TraversalAction::DoChildren(()),
        |s, _, _| {
            if s != &root {
                found = Some(s.clone());
            }
        },
        |_, _, _| true,
    );
    let sub = found.expect("subsurface");
    assert!(is_protected(&h.state, &sub));
    assert!(is_protected(&h.state, &root));
}

#[test]
fn destroying_the_protected_object_ends_protection_at_the_next_commit() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let _ = &s;
    let surface = server_surface(&r.h, 0);
    r.prot.destroy();
    r.pump();
    assert!(
        is_protected(&r.h.state, &surface),
        "still protected until the commit"
    );
    assert_eq!(r.c.seen.cancelled, 1, "the slot was cancelled at once");
    assert_eq!(slots(&r.h.state).count(), 0);
    r.win.surface.commit();
    r.pump();
    assert!(!is_protected(&r.h.state, &surface));
    assert!(r.h.state.protected.is_empty());
}

#[test]
fn the_client_going_away_ends_protection_and_the_slot() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let Rig { mut h, c, win, prot } = r;
    drop(prot);
    drop(win);
    drop(c);
    for _ in 0..4 {
        h.dispatch();
    }
    assert!(h.state.protected.is_empty());
    assert_eq!(slots(&h.state).count(), 0);
}

// ------------------------------------------------------------ rate limit

#[test]
fn set_draft_is_rate_limited_and_the_latest_draft_wins() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let id = slots(&r.h.state).next().expect("slot");
    for i in 0..20 {
        say(&s, &format!("draft {i}"));
    }
    r.pump();
    assert_eq!(
        draft(&r.h.state, id).map(|d| d.statement.as_str()),
        Some("draft 0"),
        "the first is applied at once"
    );
    assert!(has_pending_draft(&r.h.state, id));
    let applied =
        r.h.state
            .protected
            .log
            .iter()
            .filter(|c| **c == Call::Draft(id))
            .count();
    assert_eq!(applied, 1, "nineteen were coalesced");
    // The flush is a calloop timer.
    std::thread::sleep(DRAFT_INTERVAL + Duration::from_millis(60));
    r.pump();
    assert_eq!(
        draft(&r.h.state, id).map(|d| d.statement.as_str()),
        Some("draft 19"),
        "only the latest was kept"
    );
    assert!(!has_pending_draft(&r.h.state, id));
    let applied =
        r.h.state
            .protected
            .log
            .iter()
            .filter(|c| **c == Call::Draft(id))
            .count();
    assert_eq!(
        applied, 2,
        "10/s: two applications for twenty drafts in one burst"
    );
    assert_eq!(revision(&r.h.state, id), Some(2));
}

#[test]
fn a_draft_after_the_interval_is_applied_at_once() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let id = slots(&r.h.state).next().expect("slot");
    say(&s, "one");
    r.pump();
    std::thread::sleep(DRAFT_INTERVAL + Duration::from_millis(20));
    say(&s, "two");
    r.pump();
    assert_eq!(draft(&r.h.state, id).map(|d| d.statement.as_str()), Some("two"));
    assert!(!has_pending_draft(&r.h.state, id));
}

// ------------------------------------------------------------ origin matrix

/// Every origin but `physical` is dropped before delivery, `agent_compat`
/// included even though it arrives on the human seat; `physical` is delivered.
#[test]
fn only_physical_input_reaches_a_protected_surface() {
    let mut r = rig();
    // Baseline: physical input is delivered.
    key(&mut r.h, Origin::Physical, KEY_A, true);
    key(&mut r.h, Origin::Physical, KEY_A, false);
    button(&mut r.h, Origin::Physical, true);
    button(&mut r.h, Origin::Physical, false);
    scroll(&mut r.h, Origin::Physical);
    r.pump();
    assert_eq!(r.c.seen.keys, vec![(KEY_A, true), (KEY_A, false)]);
    assert_eq!(r.c.seen.buttons, vec![(BTN_LEFT, true), (BTN_LEFT, false)]);
    assert_eq!(r.c.seen.axes, 1);

    let mut origins = vec![
        Origin::AgentSeat,
        Origin::AgentCompat,
        Origin::Virtual,
        Origin::Scripted,
    ];
    if !Origin::Injected.reaches_protected() {
        origins.push(Origin::Injected);
    }
    for origin in origins {
        let (keys, buttons, axes, motions) = (
            r.c.seen.keys.len(),
            r.c.seen.buttons.len(),
            r.c.seen.axes,
            r.c.seen.ptr_motions,
        );
        key(&mut r.h, origin, KEY_A, true);
        key(&mut r.h, origin, KEY_A, false);
        // The pointer is on the window, so button and scroll aim at it.
        button(&mut r.h, origin, true);
        button(&mut r.h, origin, false);
        scroll(&mut r.h, origin);
        let to = r.at(60, 60);
        motion(&mut r.h, origin, to);
        r.pump();
        assert_eq!(r.c.seen.keys.len(), keys, "{origin:?} key");
        assert_eq!(r.c.seen.buttons.len(), buttons, "{origin:?} button");
        assert_eq!(r.c.seen.axes, axes, "{origin:?} axis");
        assert_eq!(r.c.seen.ptr_motions, motions, "{origin:?} motion");
        // Put the pointer back where the human had it.
        let back = r.at(20, 20);
        motion(&mut r.h, Origin::Physical, back);
        r.pump();
    }
    // Physical is still delivered afterwards.
    key(&mut r.h, Origin::Physical, KEY_A, true);
    r.pump();
    assert_eq!(r.c.seen.keys.last(), Some(&(KEY_A, true)));
}

/// Agent origins are audited (the hook is TCB's), others are not.
#[test]
fn agent_origins_are_audited_and_the_others_are_not() {
    let mut r = rig();
    key(&mut r.h, Origin::AgentCompat, KEY_A, true);
    key(&mut r.h, Origin::AgentSeat, KEY_A, true);
    key(&mut r.h, Origin::Virtual, KEY_A, true);
    key(&mut r.h, Origin::Scripted, KEY_A, true);
    let audits: Vec<_> =
        r.h.state
            .protected
            .log
            .iter()
            .filter_map(|c| match c {
                Call::Audit(o) => Some(*o),
                _ => None,
            })
            .collect();
    assert_eq!(audits, vec![Origin::AgentCompat, Origin::AgentSeat]);
}

/// A scripted or injected key with nothing tagging it is `Injected`, which a
/// build without `wlcs` refuses (COMP-19 §3, "release build").
#[cfg(not(feature = "wlcs"))]
#[test]
fn injected_input_is_refused_in_a_build_without_wlcs() {
    let mut r = rig();
    assert!(!Origin::Injected.reaches_protected());
    // Untagged, as the conformance harness and the tests drive it.
    let t = now(&r.h);
    r.h.state
        .keyboard_key(Keycode::new(KEY_A + 8), KeyState::Pressed, t);
    r.h.state
        .keyboard_key(Keycode::new(KEY_A + 8), KeyState::Released, t);
    r.h.state.inject_pointer_button(BTN_LEFT, true, t);
    r.h.state.inject_pointer_button(BTN_LEFT, false, t);
    key(&mut r.h, Origin::Injected, KEY_A, true);
    r.pump();
    assert!(r.c.seen.keys.is_empty(), "{:?}", r.c.seen.keys);
    assert!(r.c.seen.buttons.is_empty(), "{:?}", r.c.seen.buttons);
}

/// With `wlcs` the same input is delivered, so the conformance suite can run.
#[cfg(feature = "wlcs")]
#[test]
fn injected_input_is_delivered_with_wlcs() {
    let mut r = rig();
    let t = now(&r.h);
    r.h.state
        .keyboard_key(Keycode::new(KEY_A + 8), KeyState::Pressed, t);
    r.pump();
    assert_eq!(r.c.seen.keys, vec![(KEY_A, true)]);
}

#[test]
fn refused_input_is_reported_at_most_once_a_second() {
    let mut r = rig();
    for _ in 0..6 {
        button(&mut r.h, Origin::Virtual, true);
    }
    r.pump();
    assert_eq!(r.c.seen.refused, vec![1], "the first at once, the rest held");
    std::thread::sleep(REFUSED_INTERVAL + Duration::from_millis(100));
    r.pump();
    assert_eq!(r.c.seen.refused, vec![1, 5], "the rest in one report");
    assert!(r.c.seen.buttons.is_empty());
}

#[test]
fn unprotected_surfaces_take_every_origin() {
    let mut h = harness();
    let mut c = Cli::connect(&mut h);
    let _win = c.map(&mut h);
    let surface = server_surface(&h, 0);
    let window = crate::shell::window_for_surface(&h.state, &surface).expect("window");
    h.state.with_origin(Origin::Physical, |s| {
        crate::shell::focus::focus_window(s, &window)
    });
    c.pump(&mut h);
    key(&mut h, Origin::Virtual, KEY_A, true);
    c.pump(&mut h);
    assert_eq!(c.seen.keys, vec![(KEY_A, true)]);
}

// ------------------------------------------------------------ focus

/// Focus follows a virtual pointer onto an ordinary window but not onto a
/// protected one; a physical pointer takes it there (COMP-19 §3).
#[test]
fn virtual_motion_does_not_move_focus_onto_a_protected_window() {
    let mut h = harness();
    h.state.config.general.focus_follows_mouse = true;
    let mut c = Cli::connect(&mut h);
    let a = c.map(&mut h);
    let b = c.map(&mut h);
    let _p = c.protect(&mut h, &b.surface);
    let sa = server_surface(&h, 0);
    let sb = server_surface(&h, 1);
    let wa = crate::shell::window_for_surface(&h.state, &sa).expect("a");
    let wb = crate::shell::window_for_surface(&h.state, &sb).expect("b");
    h.state
        .with_origin(Origin::Physical, |s| crate::shell::focus::focus_window(s, &wa));
    assert_eq!(h.state.focus.as_ref(), Some(&wa));
    let centre_b = {
        let g = h.state.space.element_geometry(&wb).expect("b geometry");
        g.loc + pt(g.size.w / 2, g.size.h / 2)
    };
    motion(&mut h, Origin::Virtual, centre_b);
    assert_eq!(
        h.state.focus.as_ref(),
        Some(&wa),
        "a virtual pointer cannot focus it"
    );
    motion(&mut h, Origin::Physical, centre_b);
    assert_eq!(h.state.focus.as_ref(), Some(&wb), "a physical one can");
    let _ = &a;
}

#[test]
fn focus_requests_from_other_origins_are_refused_for_a_protected_surface() {
    let mut r = rig();
    let surface = server_surface(&r.h, 0);
    for origin in [
        Origin::AgentSeat,
        Origin::AgentCompat,
        Origin::Virtual,
        Origin::Scripted,
    ] {
        let ok =
            r.h.state
                .with_origin(origin, |s| gate::focus_allowed(s, &surface));
        assert!(!ok, "{origin:?}");
    }
    assert!(r
        .h
        .state
        .with_origin(Origin::Physical, |s| gate::focus_allowed(s, &surface)));
}

// ------------------------------------------------------------ Enter isolation

/// While a slot exists no unmodified Enter reaches the client, press or
/// release, whether or not the slot is armed (the TCB decides that); the
/// slot is told.
#[test]
fn an_unmodified_enter_never_reaches_the_client_while_a_slot_exists() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let id = slots(&r.h.state).next().expect("slot");
    for _ in 0..2 {
        key(&mut r.h, Origin::Physical, KEY_ENTER, true);
        key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    }
    r.pump();
    assert!(r.c.seen.keys.is_empty(), "{:?}", r.c.seen.keys);
    let enters =
        r.h.state
            .protected
            .log
            .iter()
            .filter(|c| **c == Call::Enter(id))
            .count();
    assert_eq!(enters, 2, "the slot heard each press");
    // Other keys still reach the client.
    key(&mut r.h, Origin::Physical, KEY_A, true);
    r.pump();
    assert_eq!(r.c.seen.keys, vec![(KEY_A, true)]);
}

#[test]
fn shift_enter_passes_to_the_client() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let id = slots(&r.h.state).next().expect("slot");
    key(&mut r.h, Origin::Physical, KEY_LEFTSHIFT, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    key(&mut r.h, Origin::Physical, KEY_LEFTSHIFT, false);
    r.pump();
    assert!(
        r.c.seen.keys.contains(&(KEY_ENTER, true)) && r.c.seen.keys.contains(&(KEY_ENTER, false)),
        "{:?}",
        r.c.seen.keys
    );
    assert!(!r.h.state.protected.log.contains(&Call::Enter(id)));
}

/// Shift released before Enter: the release still goes to the client that saw
/// the press, and is not mistaken for the slot's.
#[test]
fn a_shift_enter_release_after_shift_is_still_the_clients() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    key(&mut r.h, Origin::Physical, KEY_LEFTSHIFT, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    key(&mut r.h, Origin::Physical, KEY_LEFTSHIFT, false);
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    r.pump();
    assert!(r.c.seen.keys.contains(&(KEY_ENTER, false)), "{:?}", r.c.seen.keys);
}

#[test]
fn enter_is_the_clients_when_there_is_no_slot() {
    let mut r = rig();
    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    r.pump();
    assert_eq!(r.c.seen.keys, vec![(KEY_ENTER, true), (KEY_ENTER, false)]);
    assert!(!r
        .h
        .state
        .protected
        .log
        .iter()
        .any(|c| matches!(c, Call::Enter(_))));
}

/// A slot cancelled between the press and the release still owes the client
/// nothing: it never saw the press.
#[test]
fn the_release_of_a_consumed_enter_is_consumed_even_if_the_slot_went() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    s.destroy();
    r.pump();
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    r.pump();
    assert!(r.c.seen.keys.is_empty(), "{:?}", r.c.seen.keys);
}

// ------------------------------------------------------------ the slot's rectangle

#[test]
fn a_press_inside_the_slot_is_the_slots_and_nothing_inside_reaches_the_client() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 100, 100);
    let id = slots(&r.h.state).next().expect("slot");
    let rect = slot_rect(&r.h.state, id).expect("rect");
    // Outside the slot: delivered.
    let outside = r.at(10, 10);
    motion(&mut r.h, Origin::Physical, outside);
    button(&mut r.h, Origin::Physical, true);
    button(&mut r.h, Origin::Physical, false);
    r.pump();
    assert_eq!(r.c.seen.buttons, vec![(BTN_LEFT, true), (BTN_LEFT, false)]);
    let motions = r.c.seen.ptr_motions;
    let leaves = r.c.seen.ptr_leaves;

    // Inside: the pointer leaves the client, the press is the slot's.
    let inside = rect.loc + pt(30, 20);
    motion(&mut r.h, Origin::Physical, inside);
    button(&mut r.h, Origin::Physical, true);
    button(&mut r.h, Origin::Physical, false);
    scroll(&mut r.h, Origin::Physical);
    r.pump();
    assert_eq!(r.c.seen.buttons.len(), 2, "no new button reached the client");
    assert_eq!(r.c.seen.ptr_motions, motions, "no motion inside the rectangle");
    assert_eq!(r.c.seen.ptr_leaves, leaves + 1, "the client lost the pointer");
    assert_eq!(r.c.seen.axes, 0, "no scroll inside the rectangle");
    assert!(
        r.h.state.protected.log.contains(&Call::Click(id, 30, 20)),
        "click in slot coordinates: {:?}",
        r.h.state.protected.log
    );
    let clicks =
        r.h.state
            .protected
            .log
            .iter()
            .filter(|c| matches!(c, Call::Click(..)))
            .count();
    assert_eq!(clicks, 1, "the release is consumed, not a click");

    // Back outside: the client has the pointer again.
    let enters = r.c.seen.ptr_enters;
    motion(&mut r.h, Origin::Physical, outside);
    r.pump();
    assert_eq!(r.c.seen.ptr_enters, enters + 1);
}

#[test]
fn only_a_physical_press_clicks_a_slot() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 100, 100);
    let id = slots(&r.h.state).next().expect("slot");
    let rect = slot_rect(&r.h.state, id).expect("rect");
    let inside = rect.loc + pt(30, 20);
    motion(&mut r.h, Origin::Physical, inside);
    for origin in [
        Origin::Virtual,
        Origin::AgentCompat,
        Origin::AgentSeat,
        Origin::Scripted,
    ] {
        button(&mut r.h, origin, true);
        button(&mut r.h, origin, false);
    }
    r.pump();
    assert!(!r
        .h
        .state
        .protected
        .log
        .iter()
        .any(|c| matches!(c, Call::Click(..))));
    assert!(r.c.seen.buttons.is_empty());
}

#[test]
fn a_cancelled_slot_stops_taking_enter_and_clicks() {
    let mut r = rig();
    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 100, 100);
    s.destroy();
    r.pump();
    assert_eq!(slots(&r.h.state).count(), 0);
    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    r.pump();
    assert_eq!(r.c.seen.keys, vec![(KEY_ENTER, true), (KEY_ENTER, false)]);
}

#[test]
fn the_tcb_can_answer_the_client() {
    let mut r = rig();
    let _s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    let id = slots(&r.h.state).next().expect("slot");
    send_state(&r.h.state, id, State::Armed, Reason::None);
    send_state(&r.h.state, id, State::Suspended, Reason::NotFocused);
    send_geometry(&r.h.state, id, 400, 160);
    send_committed(&r.h.state, id, "01TASK");
    send_cancelled(&r.h.state, id);
    r.pump();
    assert_eq!(
        r.c.seen.states,
        vec![
            (State::Armed as u32, Reason::None as u32),
            (State::Suspended as u32, Reason::NotFocused as u32)
        ]
    );
    assert_eq!(r.c.seen.geometry.last(), Some(&(400, 160)));
    assert_eq!(r.c.seen.committed, vec!["01TASK".to_string()]);
    assert_eq!(r.c.seen.cancelled, 1);
}

// ------------------------------------------------------------ exclusion (§4, §1)

/// COMP-19 §9 "Agent scene" and "Capture": a window holding a protected
/// surface does not exist for an agent whose scope covers it, and is left
/// out of every capture pass list; its unprotected neighbour is unaffected.
#[test]
fn a_protected_window_is_absent_from_agents_and_from_capture() {
    use ec_policy_eval::grant::{Capability, Constraints, Grant};
    use ec_policy_eval::scope::SceneView;
    use ec_policy_eval::task::Ulid;

    let mut r = rig();
    let _other = r.c.map(&mut r.h);
    let grant = Grant {
        id: Ulid([1; 16]),
        principal: "agent:x".into(),
        issued_ms: 0,
        expires_ms: u64::MAX,
        issuer: "policyd".into(),
        task_id: Ulid([2; 16]),
        capabilities: vec![Capability {
            name: "scene.list".into(),
            scopes: vec!["workspace:human".into(), "class:private".into()],
            quota: None,
        }],
        constraints: Constraints::default(),
        unattended: false,
    };
    let view = SceneView::compile([&grant]);
    let window =
        |h: &Harness, n| crate::shell::window_for_surface(&h.state, &server_surface(h, n)).expect("window");
    let (protected, plain) = (window(&r.h, 0), window(&r.h, 1));
    for w in [&protected, &plain] {
        crate::policy::scene::test_class::set(w, ec_policy_eval::Class::Public);
    }
    assert!(!crate::policy::scene::visible(&r.h.state, &view, &protected));
    assert!(crate::policy::scene::visible(&r.h.state, &view, &plain));
    let listed: Vec<_> = crate::policy::scene::list(&mut r.h.state, &view)
        .into_iter()
        .map(|(_, w)| w)
        .collect();
    assert_eq!(listed, vec![plain.clone()]);
    // The capture pass asks exactly this of every window and layer surface.
    assert!(has_protected(&r.h.state, &server_surface(&r.h, 0)));
    assert!(!has_protected(&r.h.state, &server_surface(&r.h, 1)));
    let src = include_str!("../../render/capture.rs");
    assert_eq!(
        src.matches("protocols::protected::has_protected").count(),
        2,
        "windows and layers"
    );
}

// ------------------------------------------------------------ the slot, end to end

/// A-08 §5.2 through the compositor: a draft is previewed by policyd, the
/// card arms only after the delay, a physical Enter commits exactly the
/// preview on screen, an injected one never does, and the client learns
/// only `committed`.
#[test]
fn a_draft_previews_arms_and_commits_on_physical_enter_only() {
    use ec_policy_eval::cbor::{enc, MapBuilder};
    use ec_policy_eval::link::{FromPolicyd, ToPolicyd};
    use rustix::net::{recv, socketpair, AddressFamily, RecvFlags, SocketFlags, SocketType};

    let mut r = rig();
    r.h.state.policy_key = Some(ed25519_dalek::SigningKey::from_bytes(&[5; 32]).verifying_key());
    crate::policy::table::install_for_test(&mut r.h.state);
    let (ours, theirs) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )
    .unwrap();
    r.h.state.audit.sink = None;
    r.h.state.audit.link_for_test(ours);
    let sent = |theirs: &std::os::fd::OwnedFd| {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 65536];
        while let Ok((n, _)) = recv(theirs, &mut buf[..], RecvFlags::DONTWAIT) {
            if ec_policy_eval::link::is_message(&buf[..n]) {
                out.push(ToPolicyd::decode(&buf[..n]).unwrap());
            }
        }
        out
    };

    let s = r.c.slot(&mut r.h, &r.prot, CKind::TaskCommit, 0, 0);
    s.set_draft(
        "ec-ref-agent".into(),
        "Say hello".into(),
        0,
        Vec::new(),
        String::new(),
        String::new(),
        0,
    );
    r.pump();
    let req = match sent(&theirs).as_slice() {
        [ToPolicyd::PreviewTask {
            req,
            package,
            statement,
            ..
        }] => {
            assert_eq!(
                (package.as_str(), statement.as_str()),
                ("ec-ref-agent", "Say hello")
            );
            *req
        }
        other => panic!("{other:?}"),
    };
    let mut d = MapBuilder::new();
    for (k, v) in [
        ("package_name", "Reference agent"),
        ("package", "ec-ref-agent"),
        ("publisher", "eclipse"),
        ("statement", "Say hello"),
        ("continuation", ""),
    ] {
        d.insert(k, enc(|w| w.text(v)));
    }
    d.insert("deadline_ms", enc(|w| w.u64(7_200_000)));
    d.insert("could", enc(|w| w.array(0)));
    d.insert("narrowed", enc(|w| w.bool(false)));
    d.insert("untrusted_predecessor", enc(|w| w.bool(false)));
    crate::policy::link::deliver_for_test(
        &mut r.h.state,
        FromPolicyd::Preview {
            req,
            preview: 9,
            display: d.finish(),
        },
    );

    // Not armed yet: Enter is dropped, not queued, and nothing is sent.
    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    r.pump();
    assert!(sent(&theirs).is_empty(), "Enter before the arming delay");
    std::thread::sleep(std::time::Duration::from_millis(
        crate::trusted_ui::slot::ARM_DEFAULT_MS + 100,
    ));
    crate::trusted_ui::commit::tick(&mut r.h.state);
    r.pump();
    assert!(
        r.c.seen
            .states
            .contains(&(State::Armed as u32, Reason::None as u32)),
        "{:?}",
        r.c.seen.states
    );

    // An injected Enter never reaches the slot.
    key(&mut r.h, Origin::Injected, KEY_ENTER, true);
    key(&mut r.h, Origin::Injected, KEY_ENTER, false);
    assert!(sent(&theirs).is_empty(), "injected Enter");

    key(&mut r.h, Origin::Physical, KEY_ENTER, true);
    key(&mut r.h, Origin::Physical, KEY_ENTER, false);
    r.pump();
    let creq = match sent(&theirs).as_slice() {
        [ToPolicyd::CreateTask { req, preview: 9, .. }] => *req,
        other => panic!("{other:?}"),
    };
    crate::policy::link::deliver_for_test(
        &mut r.h.state,
        FromPolicyd::TaskCreated {
            req: creq,
            task: "01TASK".into(),
        },
    );
    r.pump();
    assert_eq!(r.c.seen.committed, vec!["01TASK".to_owned()]);
    assert!(
        r.c.seen.keys.iter().all(|(k, _)| *k != KEY_ENTER),
        "no Enter reached the client"
    );
}
