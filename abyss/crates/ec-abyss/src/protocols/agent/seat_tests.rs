// SPDX-License-Identifier: AGPL-3.0-only
//! The COMP-16 M13 gate pieces for `eclipse_agent_seat_v1`: end to end over
//! the wire, a real client holding the target window, a real agent on the
//! agent display. Seat isolation (COMP-04 §3, §5, §7) is checked here too.

use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use ec_policy_eval::{Capability, Constraints, Grant, Ulid};
use ec_protocols::agent::client::{
    eclipse_agent_seat_v1::EclipseAgentSeatV1, eclipse_agent_v1::EclipseAgentV1,
};
use smithay::backend::input::KeyState;
use smithay::input::keyboard::Keycode;
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

use super::tests::{hooked, signed, sk, Peer};
use crate::shell::focus::state_tests::Harness;
use crate::state::ClientState;

const KEY_A: u32 = 30;
const KEY_ESC: u32 = 1;
const KEY_LEFTMETA: u32 = 125;
const KEY_LEFTSHIFT: u32 = 42;
const NO_CAPABILITY: u32 = 1;
const OK: u32 = 0;

/// What a client's keyboards heard, and on which seat.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Enter,
    Leave,
    /// `(evdev code, 1 pressed / 0 released)`
    Key(u32, u32),
}

#[derive(Default)]
struct SpyData {
    log: Vec<(String, Ev)>,
    seats: Vec<String>,
    list: Vec<(u32, String, u32)>,
}

impl SpyData {
    fn on(&self, seat: &str) -> Vec<Ev> {
        self.log
            .iter()
            .filter(|(s, _)| s == seat)
            .map(|(_, e)| e.clone())
            .collect()
    }
}

/// A client that owns windows and records every keyboard event on every
/// `wl_seat` it can see, by the seat's name.
struct Spy {
    conn: Connection,
    queue: EventQueue<SpyData>,
    data: SpyData,
    compositor: WlCompositor,
    shm: WlShm,
    wm: XdgWmBase,
    toplevels: Vec<(WlSurface, XdgSurface, XdgToplevel)>,
}

impl Dispatch<WlRegistry, ()> for SpyData {
    fn event(
        d: &mut Self,
        registry: &WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Seats can come and go at any time; bind each as it appears.
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = e
        {
            if interface == "wl_seat" {
                let _: WlSeat = registry.bind(name, version.min(5), qh, ());
            } else {
                d.list.push((name, interface, version));
            }
        }
    }
}

impl Dispatch<WlSeat, ()> for SpyData {
    fn event(d: &mut Self, seat: &WlSeat, e: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Name { name } = e {
            d.seats.push(name.clone());
            seat.get_keyboard(qh, name);
        }
    }
}

impl Dispatch<WlKeyboard, String> for SpyData {
    fn event(
        d: &mut Self,
        _: &WlKeyboard,
        e: wl_keyboard::Event,
        seat: &String,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let ev = match e {
            wl_keyboard::Event::Enter { .. } => Ev::Enter,
            wl_keyboard::Event::Leave { .. } => Ev::Leave,
            wl_keyboard::Event::Key { key, state, .. } => Ev::Key(
                key,
                u32::from(state == wayland_client::WEnum::Value(wl_keyboard::KeyState::Pressed)),
            ),
            _ => return,
        };
        d.log.push((seat.clone(), ev));
    }
}

impl Dispatch<XdgWmBase, ()> for SpyData {
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

impl Dispatch<XdgSurface, ()> for SpyData {
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

delegate_noop!(SpyData: WlCompositor);
delegate_noop!(SpyData: WlShmPool);
delegate_noop!(SpyData: ignore WlShm);
delegate_noop!(SpyData: ignore WlBuffer);
delegate_noop!(SpyData: ignore WlSurface);
delegate_noop!(SpyData: ignore XdgToplevel);

impl Spy {
    fn connect(h: &mut Harness) -> Self {
        let (server, client) = UnixStream::pair().expect("pair");
        h.state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .expect("insert");
        let conn = Connection::from_socket(client).expect("connect");
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        let mut data = SpyData::default();
        for _ in 0..4 {
            let _ = conn.flush();
            h.dispatch();
            if let Some(g) = conn.prepare_read() {
                let _ = g.read();
            }
            queue.dispatch_pending(&mut data).expect("dispatch");
        }
        let qh = queue.handle();
        let find = |iface: &str| {
            data.list
                .iter()
                .find(|g| g.1 == iface)
                .map(|g| (g.0, g.2))
                .unwrap_or_else(|| panic!("{iface}"))
        };
        let (n, v) = find("wl_compositor");
        let compositor = registry.bind(n, v.min(5), &qh, ());
        let (n, _) = find("wl_shm");
        let shm = registry.bind(n, 1, &qh, ());
        let (n, _) = find("xdg_wm_base");
        let wm = registry.bind(n, 1, &qh, ());
        let mut s = Spy {
            conn,
            queue,
            data,
            compositor,
            shm,
            wm,
            toplevels: Vec::new(),
        };
        s.pump(h);
        s
    }

    fn pump(&mut self, h: &mut Harness) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            h.dispatch();
            if let Some(g) = self.conn.prepare_read() {
                let _ = g.read();
            }
            self.queue.dispatch_pending(&mut self.data).expect("dispatch");
        }
    }

    /// Map a 100x100 toplevel with `app_id`.
    fn map(&mut self, h: &mut Harness, app_id: &str) {
        let qh = self.queue.handle();
        let surface = self.compositor.create_surface(&qh, ());
        let xdg = self.wm.get_xdg_surface(&surface, &qh, ());
        let top = xdg.get_toplevel(&qh, ());
        top.set_app_id(app_id.into());
        surface.commit();
        self.pump(h);
        // SAFETY: a fresh anonymous memfd, wrapped once.
        let file = unsafe {
            let fd = libc::memfd_create(c"seat-test".as_ptr(), 0);
            assert!(fd >= 0);
            std::fs::File::from_raw_fd(fd)
        };
        file.set_len(100 * 100 * 4).expect("len");
        let pool = self.shm.create_pool(file.as_fd(), 100 * 100 * 4, &qh, ());
        let buf = pool.create_buffer(0, 100, 100, 400, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        surface.attach(Some(&buf), 0, 0);
        surface.commit();
        self.toplevels.push((surface, xdg, top));
        self.pump(h);
    }
}

fn find(h: &Harness, app_id: &str) -> smithay::desktop::Window {
    for e in h.state.outputs.iter() {
        for ws in &e.workspaces {
            for w in ws.all_windows() {
                if crate::ipc::methods::identity_of(&w).0.as_deref() == Some(app_id) {
                    return w;
                }
            }
        }
    }
    panic!("{app_id} not placed");
}

fn cap(name: &str) -> Capability {
    Capability {
        name: name.into(),
        scopes: vec!["workspace:human".into()],
        quota: None,
    }
}

fn grant(caps: &[&str]) -> Vec<u8> {
    signed(&Grant {
        id: Ulid([1; 16]),
        principal: "agent:seat".into(),
        issued_ms: 0,
        expires_ms: u64::MAX,
        issuer: "policyd".into(),
        task_id: Ulid([2; 16]),
        capabilities: caps.iter().map(|c| cap(c)).collect(),
        constraints: Constraints::default(),
        unattended: false,
    })
}

struct World {
    h: Harness,
    spy: Spy,
    peer: Peer,
    agent: EclipseAgentV1,
    seat: EclipseAgentSeatV1,
    /// The mapped window's IPC handle.
    handle: u32,
    next: u32,
}

/// One window mapped by a recording client, one agent holding `caps` with a
/// seat. The window is `public`, so a grant on `workspace:human` sees it.
fn world(sock: &str, caps: &[&str]) -> World {
    let (mut h, _path) = hooked(sock, true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());
    crate::policy::table::install_for_test(&mut h.state);
    crate::policy::scene::test_class::clear();
    let mut spy = Spy::connect(&mut h);
    spy.map(&mut h, "seatapp");
    let w = find(&h, "seatapp");
    crate::policy::scene::test_class::set(&w, ec_policy_eval::Class::Public);
    let handle = u32::try_from(h.state.ipc.handle_for(&w)).expect("handle");
    let mut peer = Peer::inserted(&mut h, true);
    let (agent, _scene) = peer.admit(&mut h, grant(caps));
    assert_eq!(peer.error(), None, "admitted");
    let seat = peer.get_seat(&mut h, &agent);
    assert_eq!(peer.error(), None, "seat created");
    spy.pump(&mut h);
    World {
        h,
        spy,
        peer,
        agent,
        seat,
        handle,
        next: 1,
    }
}

impl World {
    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    fn pump(&mut self) {
        self.peer.pump(&mut self.h);
        self.spy.pump(&mut self.h);
        self.peer.pump(&mut self.h);
    }

    fn name(&self) -> String {
        format!("agent-{}", self.h.state.agent_seats[0].0)
    }

    fn focus(&mut self) -> u32 {
        let id = self.id();
        self.seat.focus(id, self.handle, 0, 0, vec![0x80]);
        self.pump();
        id
    }

    fn key(&mut self, code: u32, pressed: bool) -> u32 {
        let id = self.id();
        self.seat.key(id, code, u32::from(pressed), 0, 0, vec![0x80]);
        self.pump();
        id
    }

    fn status(&self, req: u32) -> Option<(u32, String)> {
        self.peer
            .seen
            .seat_results
            .iter()
            .find(|(r, ..)| *r == req)
            .map(|(_, s, d)| (*s, d.clone()))
    }
}

/// An agent focuses a window by handle and types a key: the target's keyboard
/// on the *agent* seat hears it, and the human seat hears nothing.
#[test]
fn an_agent_key_reaches_the_target_on_the_agent_seat_only() {
    let mut w = world("seat-e2e.sock", &["scene.list", "seat.focus", "seat.key"]);
    assert_eq!(w.peer.seen.keymaps.len(), 1, "keymap sent on creation");
    assert!(w.peer.seen.keymaps[0] > 0);
    let human_before = w.spy.data.on("seat0").len();

    let f = w.focus();
    assert_eq!(w.status(f), Some((OK, String::new())));
    assert_eq!(w.peer.seen.seat_focus, vec![w.handle]);

    let p = w.key(KEY_A, true);
    let r = w.key(KEY_A, false);
    assert_eq!(w.status(p), Some((OK, String::new())));
    assert_eq!(w.status(r), Some((OK, String::new())));

    let seat = w.name();
    assert!(w.spy.data.seats.contains(&seat), "client sees the agent seat");
    assert_eq!(
        w.spy.data.on(&seat),
        vec![Ev::Enter, Ev::Key(KEY_A, 1), Ev::Key(KEY_A, 0)]
    );
    assert!(
        w.spy.data.on("seat0")[human_before..]
            .iter()
            .all(|e| !matches!(e, Ev::Key(..))),
        "the human seat heard no key"
    );
    // The seat's focus is the agent's own; the human's never moved.
    assert!(crate::protocols::agent::seat::focused_window(&w.h.state, w.h.state.agent_seats[0].0).is_some());
}

/// `keysym` maps through the seat keymap, `text` falls to keysyms without an
/// input method, and a string the keymap cannot type types nothing.
#[test]
fn keysym_and_text_type_through_the_keymap() {
    let mut w = world(
        "seat-text.sock",
        &["scene.list", "seat.focus", "seat.key", "seat.text"],
    );
    w.focus();
    let seat = w.name();
    let id = w.id();
    // 'A' (0x41) needs Shift.
    w.seat.keysym(id, 0x41, 1, 0, vec![0x80]);
    w.pump();
    assert_eq!(w.status(id), Some((OK, String::new())));
    let keys: Vec<Ev> = w
        .spy
        .data
        .on(&seat)
        .into_iter()
        .filter(|e| matches!(e, Ev::Key(..)))
        .collect();
    assert_eq!(keys, vec![Ev::Key(KEY_LEFTSHIFT, 1), Ev::Key(KEY_A, 1)]);

    let before = w.spy.data.on(&seat).len();
    let id = w.id();
    w.seat.text(id, w.handle, 0, "hi".into(), 0, 0, vec![0x80]);
    w.pump();
    assert_eq!(w.status(id), Some((OK, "keysym".into())));
    assert!(w.spy.data.on(&seat).len() > before);

    let before = w.spy.data.on(&seat).len();
    let id = w.id();
    w.seat.text(id, w.handle, 0, "\u{1f600}".into(), 0, 0, vec![0x80]);
    w.pump();
    assert_eq!(w.status(id), Some((15, "unsupported".into())));
    assert_eq!(w.spy.data.on(&seat).len(), before, "nothing typed");
}

/// A request with no matching capability is refused and nothing is delivered.
#[test]
fn an_act_without_its_capability_is_no_capability() {
    let mut w = world("seat-deny.sock", &["scene.list", "seat.focus"]);
    w.focus();
    let seat = w.name();
    let k = w.key(KEY_A, true);
    assert_eq!(w.status(k), Some((NO_CAPABILITY, "seat.key".into())));
    assert!(w.spy.data.on(&seat).iter().all(|e| !matches!(e, Ev::Key(..))));
}

/// Destroying the seat (or the agent) releases every held key, so nothing
/// sticks on the target, and takes the capabilities away.
#[test]
fn removing_a_seat_releases_held_keys() {
    for via_agent in [false, true] {
        let mut w = world("seat-held.sock", &["scene.list", "seat.focus", "seat.key"]);
        w.focus();
        w.key(KEY_LEFTSHIFT, true);
        w.key(KEY_A, true);
        let seat = w.name();
        assert!(w.h.state.agent_seats[0].1.keyboard_pressed() == 2);
        if via_agent {
            w.agent.destroy();
        } else {
            w.seat.destroy();
        }
        w.pump();
        assert!(w.h.state.agent_seats.is_empty(), "seat entry dropped");
        let log = w.spy.data.on(&seat);
        assert!(log.contains(&Ev::Key(KEY_A, 0)), "{log:?}");
        assert!(log.contains(&Ev::Key(KEY_LEFTSHIFT, 0)), "{log:?}");
        assert_eq!(log.last(), Some(&Ev::Leave), "focus left last");
    }
}

/// A second `get_seat` on one agent is the protocol error `seat_exists`.
#[test]
fn a_second_get_seat_is_a_protocol_error() {
    let mut w = world("seat-twice.sock", &["scene.list"]);
    let _second = w.peer.get_seat(&mut w.h, &w.agent);
    assert_eq!(w.peer.error(), Some(("eclipse_agent_v1".into(), 2)));
}

/// Agent keys never reach compositor bindings: Super+Escape would pause every
/// agent and Super+Shift+Escape would end them, if the agent seat had
/// bindings. On the human seat the same chord does act.
#[test]
fn agent_keys_never_trigger_bindings_but_human_keys_do() {
    let mut w = world("seat-bind.sock", &["scene.list", "seat.focus", "seat.key"]);
    w.focus();
    for code in [KEY_LEFTMETA, KEY_LEFTSHIFT, KEY_ESC] {
        w.key(code, true);
    }
    for code in [KEY_ESC, KEY_LEFTSHIFT, KEY_LEFTMETA] {
        w.key(code, false);
    }
    assert!(
        !crate::policy::lifecycle::all_paused(&w.h.state),
        "no binding ran"
    );
    assert_eq!(w.h.state.agents.list().len(), 1, "agent not terminated");
    // The same chord, from the human seat, is the override.
    w.h.state
        .keyboard_key(Keycode::new(KEY_LEFTMETA + 8), KeyState::Pressed, 1);
    w.h.state
        .keyboard_key(Keycode::new(KEY_ESC + 8), KeyState::Pressed, 2);
    assert!(crate::policy::lifecycle::all_paused(&w.h.state));
}

/// COMP-04 §3 property: whatever the human does, nothing reaches the agent
/// seat's client focus. A fixed-seed generator drives human keys, pointer
/// motion and buttons; the agent seat's log must stay exactly what the agent
/// itself caused.
#[test]
fn human_input_never_reaches_the_agent_seat() {
    let mut w = world("seat-iso.sock", &["scene.list", "seat.focus", "seat.key"]);
    w.focus();
    let seat = w.name();
    let baseline = w.spy.data.on(&seat);
    assert_eq!(baseline, vec![Ev::Enter]);

    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    // Letters and digits only: no chord can pause the agents or quit.
    let safe_keys: Vec<u32> = (16..=25).chain(30..=38).chain(44..=50).collect();
    for i in 0..400u32 {
        let r = next();
        match r % 4 {
            0 | 1 => {
                let code = safe_keys[(r >> 8) as usize % safe_keys.len()];
                let st = if r & 0x1_0000 == 0 {
                    KeyState::Pressed
                } else {
                    KeyState::Released
                };
                w.h.state.keyboard_key(Keycode::new(code + 8), st, i);
            }
            2 => {
                let p = (((r >> 8) % 800) as f64, ((r >> 24) % 600) as f64);
                w.h.state.pointer_moved(p.into(), i);
            }
            _ => {
                let st = if r & 0x1_0000 == 0 {
                    smithay::backend::input::ButtonState::Pressed
                } else {
                    smithay::backend::input::ButtonState::Released
                };
                w.h.state.pointer_button(0x110, st, i);
            }
        }
        if i % 50 == 0 {
            w.pump();
        }
    }
    w.pump();
    assert_eq!(w.spy.data.on(&seat), baseline, "the agent seat heard nothing");
    let (_, s) = &w.h.state.agent_seats[0];
    assert!(s.keyboard_pressed() == 0, "no key ever pressed on the agent seat");
    assert!(
        crate::protocols::agent::seat::focused_window(&w.h.state, w.h.state.agent_seats[0].0).is_some(),
        "the agent's focus is untouched"
    );
}

/// `preflight` targets are CBOR uint arrays, zipped; anything else is refused
/// before the policy call.
#[test]
fn preflight_targets_decode_strictly() {
    use super::seat::decode_targets;
    let arr = |v: &[u64]| {
        ec_policy_eval::cbor::enc(|w| {
            w.array(v.len());
            for x in v {
                w.u64(*x);
            }
        })
    };
    assert_eq!(
        decode_targets(&arr(&[1, 2]), &arr(&[0, 7])),
        Some(vec![(1, 0), (2, 7)])
    );
    assert_eq!(decode_targets(&arr(&[1]), &arr(&[1, 2])), None, "lengths differ");
    assert_eq!(decode_targets(&[0xff, 0x01], &arr(&[])), None, "bad CBOR");
    let many: Vec<u64> = (0..1001).collect();
    assert_eq!(decode_targets(&arr(&many), &arr(&many)), None, "over 1000");
    let mut trailing = arr(&[1]);
    trailing.push(0);
    assert_eq!(decode_targets(&trailing, &arr(&[1])), None, "trailing bytes");
}

/// The whole chain behind a consent prompt (COMP-11 §4, COMP-10 §3.2): a
/// table rule makes `seat.key` a `prompt`; the request is parked with no
/// answer; Escape is `prompt_denied` and nothing is typed; "Allow once" runs
/// it, re-validated, on the agent seat only.
#[test]
fn a_prompted_key_waits_for_the_human_and_runs_only_on_allow() {
    use ec_policy_eval::check::{CompiledRule, Phases, Pred, Table};
    use ec_policy_eval::scope::Glob;
    use smithay::input::keyboard::Keysym;
    const PROMPT_DENIED: u32 = 11;

    let mut w = world("seat-prompt.sock", &["scene.list", "seat.focus", "seat.key"]);
    let f = w.focus();
    assert_eq!(w.status(f), Some((OK, String::new())));
    let mut rules = Phases::default();
    rules.prompt.push(CompiledRule {
        id: "ask-before-keys".into(),
        preds: vec![Pred::Capability(vec![Glob::new("seat.key")])],
        unless: Vec::new(),
    });
    rules.allow.push(CompiledRule {
        id: "default-allow".into(),
        preds: vec![Pred::Capability(vec![Glob::new("*")])],
        unless: Vec::new(),
    });
    w.h.state.policy_table = Some(Table { version: 2, rules });
    let seat = w.name();

    // Parked: no result yet, a consent prompt holds the human seat.
    let p = w.key(KEY_A, true);
    assert_eq!(w.status(p), None, "no answer while the human decides");
    assert!(crate::trusted_ui::holds_seat(&w.h.state));
    crate::trusted_ui::key(&mut w.h.state, Keysym::Escape);
    w.pump();
    assert_eq!(w.status(p), Some((PROMPT_DENIED, String::new())));
    assert!(
        !w.spy.data.on(&seat).iter().any(|e| matches!(e, Ev::Key(..))),
        "a denied key is never typed"
    );

    // Asked again; the human picks "Allow once" (Tab from Deny wraps to it;
    // Space activates a grant, Enter never does).
    let q = w.key(KEY_A, true);
    assert_eq!(w.status(q), None);
    crate::trusted_ui::arm_now(&mut w.h.state);
    crate::trusted_ui::key(&mut w.h.state, Keysym::Tab);
    crate::trusted_ui::key(&mut w.h.state, Keysym::Tab);
    crate::trusted_ui::key(&mut w.h.state, Keysym::space);
    w.pump();
    assert_eq!(w.status(q), Some((OK, String::new())));
    assert!(
        w.spy.data.on(&seat).contains(&Ev::Key(KEY_A, 1)),
        "the allowed key reached the target on the agent seat"
    );
    assert!(!crate::trusted_ui::holds_seat(&w.h.state));
}
