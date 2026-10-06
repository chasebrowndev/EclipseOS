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
    eclipse_scene_v1::EclipseSceneV1,
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
    scene: EclipseSceneV1,
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
    let (agent, scene) = peer.admit(&mut h, grant(caps));
    assert_eq!(peer.error(), None, "admitted");
    let seat = peer.get_seat(&mut h, &agent);
    assert_eq!(peer.error(), None, "seat created");
    spy.pump(&mut h);
    World {
        h,
        spy,
        peer,
        agent,
        scene,
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
    w.h.state.policy_table = Some(Table {
        version: 2,
        rules,
        ..Default::default()
    });
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

/// COMP-16 M16 gate: no state mutation on any denied path. Each step of
/// COMP-08 §10 that can refuse is made to refuse in turn; after every one the
/// target's agent-seat input log, the seat's focus and the human's focus are
/// exactly what they were before.
#[test]
fn no_denied_step_changes_anything() {
    use ec_policy_eval::check::{CompiledRule, Phases, Pred, Table};
    use ec_policy_eval::scope::Glob;
    const PAUSED: u32 = 14;
    const OUT_OF_SCOPE: u32 = 2;
    const POLICY_DENIED: u32 = 10;
    const INVALID_ARGUMENT: u32 = 15;

    let mut w = world(
        "seat-nomut.sock",
        &["scene.list", "seat.focus", "seat.key", "seat.text"],
    );
    let f = w.focus();
    assert_eq!(w.status(f), Some((OK, String::new())));
    let seat = w.name();
    let agent = w.h.state.agent_seats[0].0;
    let snapshot = |w: &World| {
        (
            w.spy.data.on(&seat),
            crate::protocols::agent::seat::focused_window(&w.h.state, agent),
            w.h.state.focus.clone(),
        )
    };
    let before = snapshot(&w);

    // Step 1: invalid argument (empty text).
    let id = w.id();
    w.seat.text(id, w.handle, 0, String::new(), 0, 0, vec![0x80]);
    w.pump();
    assert_eq!(w.status(id).map(|s| s.0), Some(INVALID_ARGUMENT));
    assert_eq!(snapshot(&w), before, "invalid argument");

    // Step 3: paused.
    crate::policy::lifecycle::pause(&mut w.h.state, agent);
    let id = w.key(KEY_A, true);
    assert_eq!(w.status(id).map(|s| s.0), Some(PAUSED));
    assert_eq!(snapshot(&w), before, "paused");
    crate::policy::lifecycle::resume(&mut w.h.state, agent);
    w.h.state.audit.sink = Some(Vec::new());

    // Step 4: out of scope (a handle the agent cannot see).
    let id = w.id();
    w.seat.text(id, 9999, 0, "x".into(), 0, 0, vec![0x80]);
    w.pump();
    assert_eq!(w.status(id).map(|s| s.0), Some(OUT_OF_SCOPE));
    assert_eq!(snapshot(&w), before, "out of scope");

    // Step 7: the table denies.
    let mut rules = Phases::default();
    rules.deny.push(CompiledRule {
        id: "no-keys".into(),
        preds: vec![Pred::Capability(vec![Glob::new("seat.key")])],
        unless: Vec::new(),
    });
    w.h.state.policy_table = Some(Table {
        version: 9,
        rules,
        ..Default::default()
    });
    let id = w.key(KEY_A, true);
    assert_eq!(w.status(id), Some((POLICY_DENIED, "no-keys".into())));
    assert_eq!(snapshot(&w), before, "policy denied");

    // Step 7: an empty table (no rule matches) denies too.
    w.h.state.policy_table = Some(Table::default());
    let id = w.key(KEY_A, true);
    assert_eq!(w.status(id).map(|s| s.0), Some(POLICY_DENIED));
    assert_eq!(snapshot(&w), before, "no matching rule");

    // Step 7 defer: policyd answers deny, and then the timeout path (no
    // answer) for a second request.
    let mut rules = Phases::default();
    rules.defer.push(CompiledRule {
        id: "think".into(),
        preds: vec![Pred::Capability(vec![Glob::new("seat.key")])],
        unless: Vec::new(),
    });
    w.h.state.policy_table = Some(Table {
        version: 10,
        rules,
        ..Default::default()
    });
    let id = w.key(KEY_A, true);
    assert_eq!(w.status(id), None, "deferred: waiting on policyd");
    let k = (agent << 32) | u64::from(id);
    crate::policy::enforce::defer_answer(&mut w.h.state, k, ec_policy_eval::check::DeferAnswer::Deny);
    w.pump();
    assert_eq!(w.status(id), Some((POLICY_DENIED, "think".into())));
    assert_eq!(snapshot(&w), before, "defer denied");
    // A fallthrough with no allow rule is still a denial.
    let id = w.key(KEY_A, true);
    let k = (agent << 32) | u64::from(id);
    crate::policy::enforce::defer_answer(&mut w.h.state, k, ec_policy_eval::check::DeferAnswer::Fallthrough);
    w.pump();
    assert_eq!(w.status(id).map(|s| s.0), Some(POLICY_DENIED));
    assert_eq!(snapshot(&w), before, "fallthrough never widens");
}

/// COMP-16 M14: atomic batches, `click`, `wait_for`, dedupe, generations and
/// `compat_lock` (COMP-04 §4, §7; COMP-08 §2.2, §3, §4, §10).
mod m14 {
    use super::*;
    use crate::protocols::agent::generation;
    use crate::xwayland::security::SeatCompat;

    const STALE_GENERATION: u32 = 4;
    const FOCUS_LOST: u32 = 5;
    const NO_SUCH_ACTION: u32 = 8;
    const PAUSED: u32 = 14;
    const INVALID_ARGUMENT: u32 = 15;
    const DUPLICATE: u32 = 17;

    impl World {
        /// Every result for `req`, in arrival order.
        fn all(&self, req: u32) -> Vec<(u32, String)> {
            self.peer
                .seen
                .seat_results
                .iter()
                .filter(|(r, ..)| *r == req)
                .map(|(_, s, d)| (*s, d.clone()))
                .collect()
        }

        /// The keys the target heard on the agent seat.
        fn agent_keys(&self) -> Vec<Ev> {
            let seat = self.name();
            self.spy
                .data
                .on(&seat)
                .into_iter()
                .filter(|e| matches!(e, Ev::Key(..)))
                .collect()
        }

        /// The scene's current generation for the mapped window, read the
        /// way an agent does: `list_toplevels`, last `toplevel` event.
        fn generation(&mut self) -> u32 {
            let id = self.id();
            self.scene.list_toplevels(id, String::new());
            self.pump();
            self.peer
                .seen
                .generations
                .iter()
                .rev()
                .find(|(r, h, _)| *r == id && *h == self.handle)
                .map(|(_, _, g)| *g)
                .expect("a toplevel event with a generation")
        }

        fn begin(&mut self) -> u32 {
            let id = self.id();
            self.seat.begin_atomic(id, 0);
            self.pump();
            id
        }

        fn commit(&mut self) -> u32 {
            let id = self.id();
            self.seat.commit_atomic(id);
            self.pump();
            id
        }

        /// Close the mapped window the way a client does.
        fn close_window(&mut self) {
            let (surface, xdg, top) = self.spy.toplevels.remove(0);
            top.destroy();
            xdg.destroy();
            surface.destroy();
            self.pump();
        }

        fn wait(&mut self, predicate: &str, timeout_ms: u32) -> u32 {
            let id = self.id();
            self.scene.wait_for(id, predicate.into(), timeout_ms);
            self.pump();
            id
        }

        /// Pump until `req` has a `waited` event, for up to two seconds.
        fn until_waited(&mut self, req: u32) -> Option<(u32, u32, u32, u32)> {
            for _ in 0..200 {
                if let Some(e) = self.peer.seen.waited.iter().find(|e| e.0 == req) {
                    return Some(*e);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                self.pump();
            }
            None
        }

        fn retitle(&mut self, title: &str) {
            self.spy.toplevels[0].2.set_title(title.into());
            self.spy.toplevels[0].0.commit();
            self.spy.pump(&mut self.h);
        }
    }

    const BATCH_CAPS: [&str; 5] = ["scene.list", "seat.focus", "seat.key", "seat.atomic", "click"];

    /// Exit gate: the whole batch or nothing.
    #[test]
    fn a_batch_applies_every_step_on_commit_and_none_before() {
        let mut w = world("m14-commit.sock", &BATCH_CAPS);
        let f = w.focus();
        assert_eq!(w.status(f), Some((OK, String::new())));

        let b = w.begin();
        assert_eq!(w.status(b), Some((OK, String::new())));
        let down = w.key(KEY_A, true);
        let up = w.key(KEY_A, false);
        assert_eq!(w.status(down), None, "queued, not answered");
        assert_eq!(w.status(up), None);
        assert!(
            w.agent_keys().is_empty(),
            "nothing reaches the target before commit"
        );

        let c = w.commit();
        assert_eq!(w.status(down), Some((OK, String::new())));
        assert_eq!(w.status(up), Some((OK, String::new())));
        assert_eq!(w.status(c), Some((OK, "2".into())), "detail is the step count");
        assert_eq!(w.agent_keys(), vec![Ev::Key(KEY_A, 1), Ev::Key(KEY_A, 0)]);
    }

    /// Exit gate: a batch aborts cleanly on focus loss, nothing partially
    /// applied. The target goes away after the steps were queued, and again
    /// between two queued steps.
    #[test]
    fn a_batch_aborts_on_focus_loss_with_nothing_applied() {
        for mid_queue in [false, true] {
            let mut w = world("m14-focus-loss.sock", &BATCH_CAPS);
            w.focus();
            let keys_before = w.agent_keys();
            let focus_events = w.peer.seen.seat_focus.len();
            w.begin();
            let a = w.key(KEY_A, true);
            if mid_queue {
                w.close_window();
                // The next request finds the target gone and aborts the
                // whole batch then, answering what was already queued.
                let b = w.key(KEY_A, false);
                assert_eq!(w.status(a).map(|s| s.0), Some(FOCUS_LOST), "queued step answered");
                assert_eq!(w.status(b).map(|s| s.0), Some(FOCUS_LOST));
                let c = w.commit();
                assert_eq!(
                    w.status(c).map(|s| s.0),
                    Some(FOCUS_LOST),
                    "the commit repeats the cause"
                );
            } else {
                let b = w.key(KEY_A, false);
                w.close_window();
                let c = w.commit();
                for r in [a, b, c] {
                    assert_eq!(w.status(r).map(|s| s.0), Some(FOCUS_LOST), "req {r}");
                }
            }
            // Nothing partially applied: no key, no focus change, and the
            // batch is over (a new one may begin).
            assert_eq!(w.agent_keys(), keys_before, "no key was delivered");
            assert_eq!(w.peer.seen.seat_focus.len(), focus_events);
            let b = w.begin();
            assert_eq!(
                w.status(b),
                Some((OK, String::new())),
                "the aborted batch is gone"
            );
        }
    }

    /// A step that needs the seat's focus is judged against the focus the
    /// batch's own earlier steps set.
    #[test]
    fn a_batch_judges_steps_against_the_focus_earlier_steps_set() {
        let mut w = world("m14-focus-sim.sock", &BATCH_CAPS);
        // No focus yet: a lone key would be focus_lost, but a focus step
        // first makes it valid.
        w.begin();
        let f = w.id();
        w.seat.focus(f, w.handle, 0, 0, vec![0x80]);
        w.pump();
        let k = w.key(KEY_A, true);
        let c = w.commit();
        assert_eq!(w.status(f), Some((OK, String::new())));
        assert_eq!(w.status(k), Some((OK, String::new())));
        assert_eq!(w.status(c), Some((OK, "2".into())));

        // A key with no focus anywhere and nothing to set it aborts all.
        let mut w = world("m14-focus-none.sock", &BATCH_CAPS);
        w.begin();
        let k = w.key(KEY_A, true);
        assert_eq!(w.status(k).map(|s| s.0), Some(FOCUS_LOST));
        let c = w.commit();
        assert_eq!(w.status(c).map(|s| s.0), Some(FOCUS_LOST));
        assert!(w.agent_keys().is_empty());
    }

    #[test]
    fn begin_commit_and_abort_have_their_own_errors() {
        let mut w = world("m14-batch-errors.sock", &BATCH_CAPS);
        w.focus();
        // commit and abort with nothing open.
        let c = w.commit();
        assert_eq!(w.status(c), Some((INVALID_ARGUMENT, "no_batch".into())));
        let a = w.id();
        w.seat.abort_atomic(a);
        w.pump();
        assert_eq!(w.status(a), Some((INVALID_ARGUMENT, "no_batch".into())));
        // nested, and max_frames out of range.
        let b = w.begin();
        assert_eq!(w.status(b), Some((OK, String::new())));
        let n = w.begin();
        assert_eq!(w.status(n), Some((INVALID_ARGUMENT, "nested".into())));
        // abort discards every queued step.
        let k = w.key(KEY_A, true);
        let a = w.id();
        w.seat.abort_atomic(a);
        w.pump();
        assert_eq!(w.status(a), Some((OK, String::new())));
        assert_eq!(w.status(k), Some((INVALID_ARGUMENT, "aborted".into())));
        assert!(w.agent_keys().is_empty());
        let id = w.id();
        w.seat.begin_atomic(id, 61);
        w.pump();
        assert_eq!(w.status(id), Some((INVALID_ARGUMENT, "max_frames".into())));
    }

    #[test]
    fn begin_needs_seat_atomic_and_pause_aborts_an_open_batch() {
        let mut w = world("m14-batch-cap.sock", &["scene.list", "seat.focus", "seat.key"]);
        w.focus();
        let b = w.begin();
        assert_eq!(w.status(b), Some((NO_CAPABILITY, "seat.atomic".into())));
        // Without a batch, acts run as before.
        let k = w.key(KEY_A, true);
        assert_eq!(w.status(k), Some((OK, String::new())));

        let mut w = world("m14-batch-paused.sock", &BATCH_CAPS);
        w.focus();
        let agent = w.h.state.agent_seats[0].0;
        w.begin();
        let k = w.key(KEY_A, true);
        assert_eq!(w.status(k), None);
        crate::policy::lifecycle::pause(&mut w.h.state, agent);
        let k2 = w.key(KEY_A, false);
        assert_eq!(
            w.status(k).map(|s| s.0),
            Some(PAUSED),
            "pause aborts the open batch"
        );
        assert_eq!(w.status(k2).map(|s| s.0), Some(PAUSED));
        let c = w.commit();
        assert_eq!(w.status(c).map(|s| s.0), Some(PAUSED));
        assert!(w.agent_keys().is_empty());
    }

    /// Exit gate: a repeated req_id replays the stored result and executes
    /// nothing (COMP-08 §2.2).
    #[test]
    fn a_repeated_req_id_replays_without_executing() {
        let mut w = world("m14-dedupe.sock", &BATCH_CAPS);
        w.focus();
        let id = w.key(KEY_A, true);
        assert_eq!(w.all(id), vec![(OK, String::new())]);
        assert_eq!(w.agent_keys(), vec![Ev::Key(KEY_A, 1)]);

        // The same req_id again, as a client retry would send it.
        w.seat.key(id, KEY_A, 1, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(
            w.all(id),
            vec![(OK, String::new()), (DUPLICATE, "0".into())],
            "replayed as duplicate with the original status"
        );
        assert_eq!(w.agent_keys(), vec![Ev::Key(KEY_A, 1)], "nothing ran twice");

        // A denial is replayed as the denial, and still runs nothing.
        let mut w = world("m14-dedupe-deny.sock", &["scene.list", "seat.focus"]);
        w.focus();
        let k = w.key(KEY_A, true);
        assert_eq!(w.all(k), vec![(NO_CAPABILITY, "seat.key".into())]);
        w.seat.key(k, KEY_A, 1, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.all(k)[1], (DUPLICATE, "1:seat.key".into()));

        // Control requests are deduped too: a retried begin_atomic does not
        // report "nested".
        let mut w = world("m14-dedupe-ctl.sock", &BATCH_CAPS);
        let b = w.begin();
        w.seat.begin_atomic(b, 0);
        w.pump();
        assert_eq!(w.all(b), vec![(OK, String::new()), (DUPLICATE, "0".into())]);
    }

    /// A request parked behind a prompt answers a repeat `pending`, and runs
    /// exactly once when the human allows it.
    #[test]
    fn a_repeat_while_a_prompt_is_open_is_pending_and_runs_once() {
        use ec_policy_eval::check::{CompiledRule, Phases, Pred, Table};
        use ec_policy_eval::scope::Glob;
        use smithay::input::keyboard::Keysym;

        let mut w = world("m14-dedupe-prompt.sock", &BATCH_CAPS);
        w.focus();
        let mut rules = Phases::default();
        rules.prompt.push(CompiledRule {
            id: "ask".into(),
            preds: vec![Pred::Capability(vec![Glob::new("seat.key")])],
            unless: Vec::new(),
        });
        rules.allow.push(CompiledRule {
            id: "default-allow".into(),
            preds: vec![Pred::Capability(vec![Glob::new("*")])],
            unless: Vec::new(),
        });
        w.h.state.policy_table = Some(Table {
            version: 2,
            rules,
            ..Default::default()
        });
        let id = w.key(KEY_A, true);
        assert_eq!(w.status(id), None, "parked");
        w.seat.key(id, KEY_A, 1, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.all(id), vec![(DUPLICATE, "pending".into())]);
        crate::trusted_ui::arm_now(&mut w.h.state);
        crate::trusted_ui::key(&mut w.h.state, Keysym::Tab);
        crate::trusted_ui::key(&mut w.h.state, Keysym::Tab);
        crate::trusted_ui::key(&mut w.h.state, Keysym::space);
        w.pump();
        assert_eq!(
            w.all(id),
            vec![(DUPLICATE, "pending".into()), (OK, String::new())]
        );
        assert_eq!(w.agent_keys(), vec![Ev::Key(KEY_A, 1)], "ran once");
    }

    /// Exit gate: `stale_generation` fires on a changed tree, with nothing
    /// done, and carries the current generation.
    #[test]
    fn stale_generation_fires_when_the_window_changed() {
        let mut w = world("m14-stale.sock", &BATCH_CAPS);
        let g = w.generation();
        assert!(g > 0, "generations start at 1; 0 is unchecked");
        assert_eq!(w.generation(), g, "reading changes nothing");

        // Fresh: the act runs.
        let f = w.id();
        w.seat.focus(f, w.handle, g, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(f), Some((OK, String::new())));
        let focus_events = w.peer.seen.seat_focus.len();
        assert_eq!(w.generation(), g, "focusing is not a change of the window");

        // The window changes under the agent.
        w.retitle("something else");
        let now = w.generation();
        assert!(now > g, "a title change bumps the generation");

        // The stale act is refused with the current generation, nothing runs.
        let stale = w.id();
        w.seat.focus(stale, w.handle, g, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(stale), Some((STALE_GENERATION, now.to_string())));
        assert_eq!(w.peer.seen.seat_focus.len(), focus_events, "nothing was done");

        // Click carries it too; the current one runs; 0 is unchecked.
        let click = w.id();
        w.seat.click(click, w.handle, 0, 1, 1, 0x110, g, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(click), Some((STALE_GENERATION, now.to_string())));
        let ok = w.id();
        w.seat.focus(ok, w.handle, now, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(ok), Some((OK, String::new())));
        let unchecked = w.id();
        w.seat.focus(unchecked, w.handle, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(unchecked), Some((OK, String::new())));
        // The seat's results report the focus window's generation.
        assert!(w
            .peer
            .seen
            .seat_generations
            .iter()
            .any(|(r, g)| *r == unchecked && *g == now));
    }

    /// A stale step in a batch aborts the whole batch before anything runs.
    #[test]
    fn a_stale_step_aborts_the_batch() {
        let mut w = world("m14-batch-stale.sock", &BATCH_CAPS);
        let g = w.generation();
        w.focus();
        w.retitle("changed");
        w.begin();
        let k = w.key(KEY_A, true);
        let stale = w.id();
        w.seat.focus(stale, w.handle, g, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(stale).map(|s| s.0), Some(STALE_GENERATION));
        assert_eq!(
            w.status(k).map(|s| s.0),
            Some(STALE_GENERATION),
            "queued step aborted"
        );
        let c = w.commit();
        assert_eq!(w.status(c).map(|s| s.0), Some(STALE_GENERATION));
        assert!(w.agent_keys().is_empty(), "nothing applied");
    }

    /// COMP-08 §4 `click`: node 0 means (x, y); a nonzero node needs the
    /// semantic tree; the click must land on the window it names.
    #[test]
    fn click_lands_at_the_point_and_refuses_a_node() {
        let mut w = world("m14-click.sock", &["scene.list", "click"]);
        let window = find(&w.h, "seatapp");
        let g = w.h.state.space.element_geometry(&window).expect("mapped");
        let (px, py) = (g.loc.x + 7, g.loc.y + 9);

        let id = w.id();
        w.seat.click(id, w.handle, 0, px, py, 0x110, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(id), Some((OK, String::new())));
        let at = w.h.state.agent_seats[0].1.pointer_position();
        assert_eq!(
            (at.x, at.y),
            (f64::from(px), f64::from(py)),
            "node 0 clicks (x, y)"
        );
        assert_eq!(w.peer.seen.seat_focus, vec![w.handle], "a click focuses first");

        // Handle 0: whatever is topmost and visible at the point.
        let id = w.id();
        w.seat.click(id, 0, 0, px + 1, py, 0x110, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(id), Some((OK, String::new())));

        // A nonzero node has no tree to resolve in.
        let id = w.id();
        w.seat.click(id, w.handle, 5, px, py, 0x110, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(id), Some((INVALID_ARGUMENT, "node".into())));

        // A point off the window is not a click on it, and moves nothing.
        let before = w.h.state.agent_seats[0].1.pointer_position();
        let id = w.id();
        w.seat.click(
            id,
            w.handle,
            0,
            g.loc.x + g.size.w + 50,
            g.loc.y,
            0x110,
            0,
            0,
            vec![0x80],
        );
        w.pump();
        assert_ne!(w.status(id).map(|s| s.0), Some(OK));
        assert_eq!(w.h.state.agent_seats[0].1.pointer_position(), before);
    }

    /// A click is one atomic composite: in a batch it runs at commit.
    #[test]
    fn a_click_in_a_batch_runs_at_commit() {
        let mut w = world("m14-click-batch.sock", &BATCH_CAPS);
        let window = find(&w.h, "seatapp");
        let g = w.h.state.space.element_geometry(&window).expect("mapped");
        w.begin();
        let id = w.id();
        w.seat
            .click(id, w.handle, 0, g.loc.x + 3, g.loc.y + 3, 0x110, 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(id), None);
        assert!(w.peer.seen.seat_focus.is_empty(), "queued: no focus yet");
        let c = w.commit();
        assert_eq!(w.status(id), Some((OK, String::new())));
        assert_eq!(w.status(c), Some((OK, "1".into())));
        assert_eq!(w.peer.seen.seat_focus, vec![w.handle]);
    }

    /// `action` needs the semantic tree, which does not exist yet.
    #[test]
    fn action_is_no_such_action_for_now() {
        let mut w = world("m14-action.sock", &["scene.list", "seat.action"]);
        let id = w.id();
        w.seat.action(id, w.handle, 1, 0, String::new(), 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(id), Some((NO_SUCH_ACTION, "semantic".into())));

        let mut w = world("m14-action-cap.sock", &["scene.list"]);
        let id = w.id();
        w.seat.action(id, w.handle, 1, 0, String::new(), 0, 0, vec![0x80]);
        w.pump();
        assert_eq!(w.status(id), Some((NO_CAPABILITY, "seat.action".into())));
    }

    #[test]
    fn wait_for_answers_now_later_and_on_timeout() {
        let mut w = world("m14-wait.sock", &["scene.list"]);
        let g = w.generation();

        // Already true: answered at once, with the generation at satisfaction.
        let id = w.wait("toplevel_appears app_id=\"seatapp\"", 1000);
        assert_eq!(w.peer.seen.waited, vec![(id, 1, w.handle, g)]);

        // Not yet true: parked, then satisfied when the window appears.
        let late = w.wait("toplevel_appears app_id=\"late\"", 5000);
        assert!(w.peer.seen.waited.iter().all(|e| e.0 != late), "parked");
        w.spy.map(&mut w.h, "late");
        let late_window = find(&w.h, "late");
        // Unset test classes are `secret`, which no agent can see.
        crate::policy::scene::test_class::set(&late_window, ec_policy_eval::Class::Public);
        let (_, satisfied, handle, gen_at) = w.until_waited(late).expect("satisfied");
        assert_eq!(satisfied, 1);
        assert_eq!(u64::from(handle), w.h.state.ipc.handle_for(&late_window));
        assert_eq!(
            gen_at,
            generation::of(&mut w.h.state, &late_window),
            "generation at satisfaction"
        );

        // Timeout: satisfied 0.
        let gt = w.wait(&format!("generation_gt handle={} n=4000000", w.handle), 40);
        let (_, satisfied, ..) = w.until_waited(gt).expect("timed out");
        assert_eq!(satisfied, 0);

        // A generation bump satisfies generation_gt, reporting the new value.
        // (Mapping the second window may have re-tiled this one: read afresh.)
        let g = w.generation();
        let gt = w.wait(&format!("generation_gt handle={} n={g}", w.handle), 5000);
        assert!(w.peer.seen.waited.iter().all(|e| e.0 != gt), "not yet");
        w.retitle("renamed");
        let (_, satisfied, handle, gen_at) = w.until_waited(gt).expect("bumped");
        assert_eq!((satisfied, handle), (1, w.handle));
        assert!(gen_at > g);
    }

    #[test]
    fn wait_for_title_focus_idle_and_unmapped() {
        let mut w = world("m14-wait2.sock", &["scene.list", "seat.focus"]);
        w.retitle("Inbox (3)");
        let t = w.wait(
            &format!("title_matches handle={} regex=\"^Inbox [(][0-9]+[)]$\"", w.handle),
            500,
        );
        assert_eq!(
            w.peer.seen.waited.iter().find(|e| e.0 == t).map(|e| e.1),
            Some(1),
            "{:?}",
            w.peer.seen.results
        );

        // focus on an agent seat: parks until the agent focuses.
        let seat = w.name();
        let f = w.wait(&format!("focus seat=\"{seat}\" handle={}", w.handle), 5000);
        assert!(w.peer.seen.waited.iter().all(|e| e.0 != f));
        w.focus();
        assert_eq!(w.until_waited(f).map(|e| e.1), Some(1));

        // idle: nothing changed for 30 ms.
        let i = w.wait(&format!("idle handle={} ms=30", w.handle), 5000);
        assert_eq!(w.until_waited(i).map(|e| e.1), Some(1));

        // unmapped: the window goes away; it was visible when asked.
        let u = w.wait(&format!("unmapped handle={}", w.handle), 5000);
        assert!(w.peer.seen.waited.iter().all(|e| e.0 != u));
        w.close_window();
        assert_eq!(w.until_waited(u).map(|e| e.1), Some(1));
    }

    #[test]
    fn wait_for_refuses_what_it_cannot_answer_and_can_be_cancelled() {
        let mut w = world("m14-wait3.sock", &["scene.list"]);
        let result = |w: &World, id: u32| {
            w.peer
                .seen
                .results
                .iter()
                .find(|(r, ..)| *r == id)
                .map(|(_, s, d)| (*s, d.clone()))
        };
        for (p, detail) in [
            ("node handle=1 id=2", "unsupported"),
            ("text_contains handle=1 node=2 needle=\"x\"", "unsupported"),
            ("gibberish", "predicate"),
            ("unmapped handle=424242", "handle"),
        ] {
            let id = w.wait(p, 100);
            assert_eq!(result(&w, id), Some((INVALID_ARGUMENT, detail.into())), "{p}");
        }
        let id = w.wait("idle handle=1 ms=1", 0);
        assert_eq!(result(&w, id), Some((INVALID_ARGUMENT, "timeout_ms".into())));

        // A parked wait ends, unsatisfied, when cancelled.
        let id = w.wait("toplevel_appears app_id=\"nope\"", 60_000);
        assert!(w.peer.seen.waited.iter().all(|e| e.0 != id));
        w.scene.cancel_wait(id);
        w.pump();
        assert_eq!(
            w.peer.seen.waited.iter().find(|e| e.0 == id).map(|e| e.1),
            Some(0)
        );
        // Cancelling what is not pending says so.
        let other = w.id();
        w.scene.cancel_wait(other);
        w.pump();
        assert_eq!(result(&w, other), Some((INVALID_ARGUMENT, "req_id".into())));
        assert_eq!(w.h.state.agents.waits.len(), 0, "no waits left");
    }

    /// COMP-04 §7: while an agent holds a compat lock, the human's keys for
    /// that window are queued and delivered, in order, on release.
    #[test]
    fn a_compat_lock_queues_human_keys_and_delivers_them_on_release() {
        use std::cell::Cell;
        let caps = ["scene.list", "seat.focus", "seat.compat_lock"];
        let mut w = world("m14-lock.sock", &caps);
        let window = find(&w.h, "seatapp");

        // Not a compat-flagged app: refused.
        let id = w.id();
        w.seat.compat_lock(id, w.handle, 0);
        w.pump();
        assert_eq!(w.status(id), Some((INVALID_ARGUMENT, "not_compat".into())));

        window
            .user_data()
            .insert_if_missing(|| crate::shell::rules::RuleSeat(Cell::new(SeatCompat::Lock)));
        // The human's keyboard is on the window, and reaches it freely.
        let human_key = |w: &mut World, code: u32, pressed: bool| {
            let st = if pressed {
                KeyState::Pressed
            } else {
                KeyState::Released
            };
            w.h.state.keyboard_key(Keycode::new(code + 8), st, 1);
            w.pump();
        };
        human_key(&mut w, KEY_A, true);
        human_key(&mut w, KEY_A, false);
        let heard = |w: &World| -> Vec<Ev> {
            w.spy
                .data
                .on("seat0")
                .into_iter()
                .filter(|e| matches!(e, Ev::Key(..)))
                .collect()
        };
        assert_eq!(
            heard(&w),
            vec![Ev::Key(KEY_A, 1), Ev::Key(KEY_A, 0)],
            "precondition: the human's keys reach the window"
        );

        let lock = w.id();
        w.seat.compat_lock(lock, w.handle, 0);
        w.pump();
        assert_eq!(w.status(lock), Some((OK, String::new())));
        assert_eq!(w.h.state.agents.locks.active().count(), 1);

        let before = heard(&w).len();
        human_key(&mut w, KEY_ESC, true);
        human_key(&mut w, KEY_ESC, false);
        human_key(&mut w, KEY_A, true);
        human_key(&mut w, KEY_A, false);
        assert_eq!(heard(&w).len(), before, "held while locked, not dropped");

        let un = w.id();
        w.seat.compat_unlock(un);
        w.pump();
        assert_eq!(w.status(un), Some((OK, String::new())));
        assert_eq!(
            heard(&w)[before..],
            [
                Ev::Key(KEY_ESC, 1),
                Ev::Key(KEY_ESC, 0),
                Ev::Key(KEY_A, 1),
                Ev::Key(KEY_A, 0)
            ],
            "delivered in order on release"
        );
        assert_eq!(w.h.state.agents.locks.active().count(), 0);
        let un = w.id();
        w.seat.compat_unlock(un);
        w.pump();
        assert_eq!(w.status(un), Some((INVALID_ARGUMENT, "no_lock".into())));

        // Live again after release.
        human_key(&mut w, KEY_A, true);
        assert_eq!(heard(&w).len(), before + 5);
    }

    /// The lock gives the seat back by itself: on timeout, on pause (the
    /// human override), and when the agent goes.
    #[test]
    fn a_compat_lock_ends_on_timeout_pause_and_teardown() {
        use std::cell::Cell;
        let caps = ["scene.list", "seat.focus", "seat.compat_lock"];
        let take = |sock: &str| {
            let mut w = world(sock, &caps);
            let window = find(&w.h, "seatapp");
            window
                .user_data()
                .insert_if_missing(|| crate::shell::rules::RuleSeat(Cell::new(SeatCompat::Lock)));
            let id = w.id();
            w.seat.compat_lock(id, w.handle, 40);
            w.pump();
            assert_eq!(w.status(id), Some((OK, String::new())));
            w
        };
        let key = |w: &mut World| {
            w.h.state
                .keyboard_key(Keycode::new(KEY_A + 8), KeyState::Pressed, 1);
            w.pump();
        };
        let heard = |w: &World| {
            w.spy
                .data
                .on("seat0")
                .iter()
                .filter(|e| matches!(e, Ev::Key(..)))
                .count()
        };

        // Timeout.
        let mut w = take("m14-lock-timeout.sock");
        key(&mut w);
        assert_eq!(heard(&w), 0);
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            w.pump();
        }
        assert_eq!(w.h.state.agents.locks.active().count(), 0, "expired");
        assert_eq!(heard(&w), 1, "the held key arrived on expiry");

        // Pause.
        let mut w = take("m14-lock-pause.sock");
        let agent = w.h.state.agent_seats[0].0;
        key(&mut w);
        assert_eq!(heard(&w), 0);
        crate::policy::lifecycle::pause(&mut w.h.state, agent);
        key(&mut w);
        assert_eq!(w.h.state.agents.locks.active().count(), 0, "paused: released");
        assert_eq!(heard(&w), 2, "held and live keys both arrived");

        // The agent goes.
        let mut w = take("m14-lock-gone.sock");
        key(&mut w);
        w.agent.destroy();
        w.pump();
        assert_eq!(w.h.state.agents.locks.active().count(), 0);
        assert_eq!(heard(&w), 1);
    }

    #[test]
    fn a_compat_lock_needs_its_capability_and_a_sane_timeout() {
        use std::cell::Cell;
        let mut w = world("m14-lock-cap.sock", &["scene.list", "seat.focus"]);
        let id = w.id();
        w.seat.compat_lock(id, w.handle, 0);
        w.pump();
        assert_eq!(w.status(id), Some((NO_CAPABILITY, "seat.compat_lock".into())));

        let mut w = world("m14-lock-cap2.sock", &["scene.list", "seat.compat_lock"]);
        let id = w.id();
        w.seat.compat_lock(id, w.handle, 0);
        w.pump();
        assert_eq!(
            w.status(id),
            Some((NO_CAPABILITY, "seat.focus".into())),
            "scope comes from seat.focus"
        );

        let mut w = world(
            "m14-lock-big.sock",
            &["scene.list", "seat.focus", "seat.compat_lock"],
        );
        let window = find(&w.h, "seatapp");
        window
            .user_data()
            .insert_if_missing(|| crate::shell::rules::RuleSeat(Cell::new(SeatCompat::Lock)));
        let id = w.id();
        w.seat.compat_lock(id, w.handle, 60_001);
        w.pump();
        assert_eq!(w.status(id), Some((INVALID_ARGUMENT, "timeout_ms".into())));
    }

    /// COMP-16 M11 gate for `wait_for`: a `secret` window is never seen,
    /// named or satisfied on, however the predicate is phrased.
    #[test]
    fn wait_for_never_resolves_on_a_hidden_window() {
        let mut w = world("m14-wait-hidden.sock", &["scene.list"]);
        w.spy.map(&mut w.h, "hidden"); // unset test class: secret
        let hidden = find(&w.h, "hidden");
        let hh = w.h.state.ipc.handle_for(&hidden);

        let id = w.wait("toplevel_appears app_id=\"hidden\"", 60);
        assert_eq!(
            w.until_waited(id).map(|e| e.1),
            Some(0),
            "timed out, never satisfied"
        );
        for p in [format!("unmapped handle={hh}"), format!("idle handle={hh} ms=1")] {
            let id = w.wait(&p, 60);
            let r = w.peer.seen.results.iter().find(|(r, ..)| *r == id).cloned();
            assert_eq!(r, Some((id, INVALID_ARGUMENT, "handle".into())), "{p}");
        }
    }

    /// Batch control writes the same audit trail an act does (COMP-12 §1).
    #[test]
    fn control_requests_are_audited() {
        let mut w = world("m14-audit.sock", &BATCH_CAPS);
        w.focus();
        let before = w.h.state.audit.sink.as_ref().map_or(0, Vec::len);
        w.begin();
        let after = w.h.state.audit.sink.as_ref().map_or(0, Vec::len);
        assert!(after > before, "begin_atomic wrote audit records");
    }
}
