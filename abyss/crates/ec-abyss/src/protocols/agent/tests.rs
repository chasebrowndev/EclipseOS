// SPDX-License-Identifier: AGPL-3.0-only
//! The agent socket and `eclipse_agent_v1`, driven by a real
//! `wayland-client` against the in-process harness (COMP-16 M11, F-03).

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;

use ec_policy_eval::grant::{cose_sign1, protected_header, sig_structure};
use ec_policy_eval::{Capability, Constraints, Grant, Ulid};
use ec_protocols::agent::client::{
    eclipse_agent_manager_v1::{self, EclipseAgentManagerV1},
    eclipse_agent_v1::{self, EclipseAgentV1},
    eclipse_scene_v1::{self, EclipseSceneV1},
};
use ed25519_dalek::{Signer, SigningKey};
use wayland_client::{
    protocol::wl_registry::{self, WlRegistry},
    Connection, Dispatch, EventQueue, QueueHandle,
};

use crate::addons::Hook;
use crate::shell::focus::state_tests::{harness, Harness};
use crate::state::ClientState;

pub(super) const MANAGER: &str = "eclipse_agent_manager_v1";
const PAUSED: u32 = 14;
pub(super) const INVALID_ARGUMENT: u32 = 15;

#[derive(Default)]
pub(super) struct Seen {
    globals: Vec<(u32, String)>,
    dedupe: Option<u32>,
    pub(super) toplevels: Vec<(u32, u32)>,
    pub(super) done: Vec<u32>,
    pub(super) hits: Vec<(u32, u32)>,
    pub(super) results: Vec<(u32, u32, String)>,
    /// Every `eclipse_scene_v1` event, whole, in arrival order.
    pub(super) scene: Vec<eclipse_scene_v1::Event>,
}

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &WlRegistry,
        e: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = e {
            s.globals.push((name, interface));
        }
    }
}

impl Dispatch<EclipseAgentManagerV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseAgentManagerV1,
        e: eclipse_agent_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_agent_manager_v1::Event::DedupeWindow { seconds } = e {
            s.dedupe = Some(seconds);
        }
    }
}

impl Dispatch<EclipseAgentV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseAgentV1,
        e: eclipse_agent_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_agent_v1::Event::Result {
            req_id,
            status,
            detail,
            ..
        } = e
        {
            s.results.push((req_id, status, detail));
        }
    }
}

impl Dispatch<EclipseSceneV1, ()> for Seen {
    fn event(
        s: &mut Self,
        _: &EclipseSceneV1,
        e: eclipse_scene_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use eclipse_scene_v1::Event;
        match &e {
            Event::Toplevel { req_id, handle, .. } => s.toplevels.push((*req_id, *handle)),
            Event::ToplevelsDone { req_id } => s.done.push(*req_id),
            Event::Hit { req_id, handle, .. } => s.hits.push((*req_id, *handle)),
            Event::Result {
                req_id,
                status,
                detail,
                ..
            } => s.results.push((*req_id, *status, detail.clone())),
            _ => {}
        }
        s.scene.push(e);
    }
}

pub(super) struct Peer {
    conn: Connection,
    queue: EventQueue<Seen>,
    pub(super) seen: Seen,
    registry: WlRegistry,
}

impl Peer {
    fn new(h: &mut Harness, client: UnixStream) -> Self {
        let conn = Connection::from_socket(client).expect("connect");
        let queue = conn.new_event_queue();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut p = Peer {
            conn,
            queue,
            seen: Seen::default(),
            registry,
        };
        p.pump(h);
        p
    }

    /// A client inserted directly where its socket would put it: an agent
    /// on the agent display, flagged as `accept` flags it; anyone else on
    /// the human display.
    pub(super) fn inserted(h: &mut Harness, agent: bool) -> Self {
        if !agent {
            return Self::on_human(h, false);
        }
        let (server, client) = UnixStream::pair().expect("pair");
        let data = Arc::new(ClientState {
            agent: true,
            ..Default::default()
        });
        h.state
            .agents
            .display
            .clone()
            .expect("agent display")
            .insert_client(server, data)
            .expect("insert");
        Self::new(h, client)
    }

    /// A client on the human display, with the agent flag as given.
    fn on_human(h: &mut Harness, agent: bool) -> Self {
        let (server, client) = UnixStream::pair().expect("pair");
        let data = Arc::new(ClientState {
            agent,
            ..Default::default()
        });
        h.state
            .display_handle
            .insert_client(server, data)
            .expect("insert");
        Self::new(h, client)
    }

    pub(super) fn pump(&mut self, h: &mut Harness) {
        for _ in 0..8 {
            let _ = self.conn.flush();
            h.dispatch();
            if let Some(guard) = self.conn.prepare_read() {
                let _ = guard.read();
            }
            if self.queue.dispatch_pending(&mut self.seen).is_err() {
                return;
            }
        }
    }

    fn global(&self, interface: &str) -> Option<u32> {
        self.seen
            .globals
            .iter()
            .find(|(_, i)| i == interface)
            .map(|(n, _)| *n)
    }

    fn manager(&mut self, h: &mut Harness) -> EclipseAgentManagerV1 {
        let name = self.global(MANAGER).expect("manager advertised");
        let m = self.registry.bind(name, 1, &self.queue.handle(), ());
        self.pump(h);
        m
    }

    pub(super) fn admit(&mut self, h: &mut Harness, grant: Vec<u8>) -> (EclipseAgentV1, EclipseSceneV1) {
        let manager = self.manager(h);
        let qh = self.queue.handle();
        let agent = manager.create_agent(grant, &qh, ());
        let scene = agent.get_scene(&qh, ());
        self.pump(h);
        (agent, scene)
    }

    /// `(interface, code)` of the protocol error that ended the connection.
    pub(super) fn error(&self) -> Option<(String, u32)> {
        self.conn.protocol_error().map(|e| (e.object_interface, e.code))
    }
}

pub(super) fn sk() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn grant(principal: &str) -> Vec<u8> {
    let g = Grant {
        id: Ulid([1; 16]),
        principal: principal.into(),
        issued_ms: 0,
        expires_ms: u64::MAX,
        issuer: "policyd".into(),
        task_id: Ulid([2; 16]),
        capabilities: vec![
            Capability {
                name: "scene.list".into(),
                scopes: vec!["workspace:human".into()],
                quota: None,
            },
            Capability {
                name: "scene.read".into(),
                scopes: vec!["workspace:human".into()],
                quota: None,
            },
        ],
        constraints: Constraints::default(),
        unattended: false,
    };
    signed(&g)
}

/// `g` as `policyd` would hand it over: COSE_Sign1 under [`sk`].
pub(super) fn signed(g: &Grant) -> Vec<u8> {
    let k = sk();
    let protected = protected_header(&k.verifying_key());
    let payload = g.encode();
    let sig = k.sign(&sig_structure(&protected, &payload));
    cose_sign1(&protected, &payload, &sig.to_bytes())
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("abyss-agent-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

/// The harness with the agent socket at a private path.
pub(super) fn hooked(name: &str, on: bool) -> (Harness, PathBuf) {
    let mut h = harness();
    let path = scratch(name);
    h.state.agents.test_path = Some(path.clone());
    if on {
        h.state.addons.hooks.insert(Hook::Agents);
    }
    super::sync(&mut h.state);
    (h, path)
}

/// F-03: with the hook off, no agent socket file and no agent global
/// anywhere, not even to a client flagged as an agent.
#[test]
fn hook_off_means_no_socket_and_no_global() {
    let (mut h, path) = hooked("off.sock", false);
    assert!(!path.exists());
    assert!(!h.state.agents.listening());
    assert!(h.state.agents.display.is_none(), "no agent display either");
    for agent in [false, true] {
        let p = Peer::on_human(&mut h, agent);
        assert!(p.global(MANAGER).is_none(), "agent={agent}");
        assert!(
            p.global("wl_compositor").is_some(),
            "the human desktop is untouched"
        );
    }
}

/// COMP-01 §5 step 10, COMP-13 §3, C-00 §8.1, F-06.
#[test]
fn hook_on_socket_is_0600_and_only_its_clients_see_the_manager() {
    let (mut h, path) = hooked("on.sock", true);
    let mode = std::fs::metadata(&path)
        .expect("socket exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    // Through the real socket: the uid check passes for us.
    let stream = UnixStream::connect(&path).expect("connect agent socket");
    let mut agent = Peer::new(&mut h, stream);
    agent.pump(&mut h);
    let name = agent.global(MANAGER).expect("manager on the agent socket");
    let _ = agent.manager(&mut h);
    assert_eq!(agent.seen.dedupe, Some(60));
    assert!(agent.error().is_none());

    // A wayland-N client neither sees it nor can bind it by name.
    let mut human = Peer::inserted(&mut h, false);
    assert!(human.global(MANAGER).is_none());
    let _: EclipseAgentManagerV1 = human.registry.bind(name, 1, &human.queue.handle(), ());
    human.pump(&mut h);
    assert!(
        human.error().is_some(),
        "binding a hidden global is a protocol error"
    );

    // Hook off: as "agentd dies", then the socket goes (COMP-01 §6).
    h.state.addons.hooks = crate::addons::HookSet::default();
    super::sync(&mut h.state);
    agent.pump(&mut h);
    assert!(!path.exists());
    assert!(h.state.agents.clients.is_empty());
    // The kill is a hang-up, not a protocol error: reading sees EOF.
    assert!(agent.conn.prepare_read().is_none_or(|g| g.read().is_err()));
}

/// COMP-01 §6: before policyd connects, `create_agent` is
/// `POLICY_UNAVAILABLE`.
#[test]
fn create_agent_without_policyd_is_policy_unavailable() {
    let (mut h, _path) = hooked("nokey.sock", true);
    assert!(h.state.policy_key.is_none());
    let mut p = Peer::inserted(&mut h, true);
    p.admit(&mut h, grant("agent:test"));
    assert_eq!(p.error(), Some((MANAGER.into(), 1)));
    assert!(h.state.agents.slots.is_empty());
}

/// COMP-08 §1: anything that is not a good grant is `INVALID_GRANT`.
#[test]
fn a_bad_grant_is_invalid_grant() {
    for bad in [b"not cbor".to_vec(), grant("human")] {
        let (mut h, _path) = hooked("bad.sock", true);
        h.state.audit.sink = Some(Vec::new());
        h.state.policy_key = Some(sk().verifying_key());
        let mut p = Peer::inserted(&mut h, true);
        p.admit(&mut h, bad);
        assert_eq!(p.error(), Some((MANAGER.into(), 0)));
        assert!(h.state.agents.slots.is_empty());
    }
    // Signed by another key.
    let (mut h, _path) = hooked("bad.sock", true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(SigningKey::from_bytes(&[8u8; 32]).verifying_key());
    let mut p = Peer::inserted(&mut h, true);
    p.admit(&mut h, grant("agent:test"));
    assert_eq!(p.error(), Some((MANAGER.into(), 0)));
}

/// F-08: policyd gone, an existing agent is paused and nothing runs.
#[test]
fn requests_while_policyd_is_down_are_paused() {
    let (mut h, _path) = hooked("paused.sock", true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());
    let mut p = Peer::inserted(&mut h, true);
    let (_agent, scene) = p.admit(&mut h, grant("agent:test"));
    assert!(p.error().is_none());
    assert_eq!(h.state.agents.slots.len(), 1);

    h.state.policy_key = None;
    scene.list_toplevels(1, String::new());
    scene.get_toplevel(2, 1);
    scene.hit_test(3, 10, 10);
    p.pump(&mut h);
    assert_eq!(
        p.seen.results,
        vec![
            (1, PAUSED, String::new()),
            (2, PAUSED, String::new()),
            (3, PAUSED, String::new())
        ]
    );
    assert!(p.seen.done.is_empty() && p.seen.hits.is_empty());

    // Reconnect resumes (COMP-01 §6).
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());
    scene.list_toplevels(4, String::new());
    p.pump(&mut h);
    assert_eq!(p.seen.done, vec![4]);
}

/// COMP-15 audit completeness: every request an agent makes is a
/// `request`, a `decision` and a `result` record, in that order, under its
/// `req_id`, with the agent's principal and task; admission is a
/// `lifecycle` record.
#[test]
fn every_scene_request_is_request_decision_result() {
    use ec_policy_eval::audit::Kind;
    let (mut h, _path) = hooked("audit.sock", true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());
    let mut p = Peer::inserted(&mut h, true);
    let (_agent, scene) = p.admit(&mut h, grant("agent:test"));
    scene.list_toplevels(1, String::new());
    scene.get_toplevel(2, 999);
    scene.hit_test(3, 10, 10);
    p.pump(&mut h);

    let seen = h.state.audit.sink.take().unwrap();
    assert_eq!(seen[0].kind, Kind::Lifecycle);
    assert_eq!(seen[0].principal, "agent:test");
    let rest = &seen[1..];
    assert_eq!(rest.len(), 9, "{rest:?}");
    for (i, chunk) in rest.chunks(3).enumerate() {
        let kinds: Vec<_> = chunk.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [Kind::Request, Kind::Decision, Kind::Result]);
        for e in chunk {
            assert_eq!(e.req_id, Some(i as u64 + 1));
            assert_eq!(e.principal, "agent:test");
        }
        assert!(chunk[0].task_id.is_some() && chunk[0].task_id == chunk[1].task_id);
    }
}

/// COMP-12 §1: when policyd stops reading, the agent stalls (`paused`,
/// nothing done) and is let through again once the socket drains.
#[test]
fn a_full_audit_socket_stalls_the_agent_not_the_human() {
    use rustix::net::{recv, send, socketpair, AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType};
    let (mut h, _path) = hooked("stall.sock", true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());
    let mut p = Peer::inserted(&mut h, true);
    let (_agent, scene) = p.admit(&mut h, grant("agent:test"));

    let (ours, theirs) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )
    .unwrap();
    while send(&ours, &[0u8; 512], SendFlags::DONTWAIT).is_ok() {}
    h.state.audit.link_for_test(ours);

    scene.list_toplevels(1, String::new());
    p.pump(&mut h);
    assert_eq!(p.seen.results, vec![(1, PAUSED, String::new())]);
    assert!(p.seen.done.is_empty());

    // The human side does not wait: a focus change still happens, queued.
    crate::audit::focus(&mut h.state, Some(1), "human");
    assert!(h.state.audit.backlogged());

    let mut buf = [0u8; 1024];
    while recv(&theirs, &mut buf[..], RecvFlags::DONTWAIT).is_ok() {}
    scene.list_toplevels(2, String::new());
    p.pump(&mut h);
    assert_eq!(p.seen.done, vec![2]);
    assert!(!h.state.audit.backlogged());
}

/// S-05 §8 and F-07: with no policy table every window is secret, so the
/// agent sees nothing, and a real handle answers exactly as a bogus one.
#[test]
fn with_a_valid_grant_every_window_is_secret_and_unknown() {
    use crate::shell::focus::state_tests::client::Client;
    let (mut h, _path) = hooked("secret.sock", true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());

    let mut app = Client::connect(&mut h);
    app.map_window(&mut h);
    let window = h.state.space.elements().next().cloned().expect("a mapped window");
    let real = h.state.ipc.handle_for(&window) as u32;
    let centre = h.state.space.element_geometry(&window).expect("placed");
    let (cx, cy) = (centre.loc.x + centre.size.w / 2, centre.loc.y + centre.size.h / 2);

    let mut p = Peer::inserted(&mut h, true);
    let (_agent, scene) = p.admit(&mut h, grant("agent:test"));
    assert!(p.error().is_none());

    scene.list_toplevels(1, String::new());
    scene.get_toplevel(2, real);
    scene.get_toplevel(3, real + 1000);
    scene.hit_test(4, cx, cy);
    scene.list_toplevels(5, "app_id \"x\"".into());
    p.pump(&mut h);

    assert!(p.seen.toplevels.is_empty());
    assert_eq!(p.seen.done, vec![1]);
    assert_eq!(
        p.seen.results,
        vec![
            (2, INVALID_ARGUMENT, "handle".into()),
            (3, INVALID_ARGUMENT, "handle".into()),
            (5, INVALID_ARGUMENT, "filter".into()),
        ]
    );
    assert_eq!(p.seen.hits, vec![(4, 0)]);
}

/// C-00 §8.1, COMP-08 §3: the agent socket is its own display. A client on
/// it sees the manager and nothing of the human desktop, so window titles
/// and app_ids reach it only through `policy::scene`, and it cannot inject
/// human input.
#[test]
fn the_agent_socket_serves_only_the_agent_manager() {
    const HUMAN: [&str; 6] = [
        "wl_compositor",
        "wl_seat",
        "ext_foreign_toplevel_list_v1",
        "zwlr_virtual_pointer_manager_v1",
        "wl_shm",
        "xdg_wm_base",
    ];
    let (mut h, path) = hooked("only.sock", true);

    // The human display does carry them, so their absence below means
    // something.
    let human = Peer::inserted(&mut h, false);
    for i in HUMAN {
        assert!(human.global(i).is_some(), "human display lacks {i}");
    }

    let stream = UnixStream::connect(&path).expect("connect agent socket");
    let mut agent = Peer::new(&mut h, stream);
    agent.pump(&mut h);
    assert!(agent.global(MANAGER).is_some());
    for i in HUMAN {
        assert!(agent.global(i).is_none(), "agent client sees {i}");
    }
    let names: Vec<&str> = agent.seen.globals.iter().map(|(_, i)| i.as_str()).collect();
    assert_eq!(names, vec![MANAGER]);
}

/// A stale socket from a dead compositor is replaced; anything else at the
/// path is left alone and nothing listens.
#[test]
fn only_a_stale_socket_is_unlinked() {
    let mut h = harness();
    let path = scratch("file.sock");
    std::fs::write(&path, b"keep").expect("write");
    h.state.agents.test_path = Some(path.clone());
    h.state.addons.hooks.insert(Hook::Agents);
    super::sync(&mut h.state);
    assert!(!h.state.agents.listening());
    assert_eq!(std::fs::read(&path).expect("still there"), b"keep");

    let mut h = harness();
    let path = scratch("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&path).expect("bind"));
    assert!(path.exists(), "a dead socket file stays behind");
    h.state.agents.test_path = Some(path.clone());
    h.state.addons.hooks.insert(Hook::Agents);
    super::sync(&mut h.state);
    assert!(h.state.agents.listening());
    UnixStream::connect(&path).expect("the new socket answers");
}
