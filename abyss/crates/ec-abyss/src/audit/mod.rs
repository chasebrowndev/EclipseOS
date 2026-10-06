// SPDX-License-Identifier: AGPL-3.0-only
//! Provenance emission (COMP-12). TCB.
//!
//! abyss writes no audit store. It hands `policyd` one [`Emission`] per
//! SEQPACKET message on the same `policyd.sock` connection the key offer
//! came in on (COMP-12 §1), and `policyd` is the only writer (S-04 §4).
//!
//! The channel is lossless, and who waits when it is full is the point:
//!
//! - An agent request goes ahead only once its `request` record is in the
//!   socket and nothing is queued ahead of it ([`begin`]). Otherwise the
//!   request is answered `paused` and nothing happens: the agent stalls, it
//!   is not let through unjournalled. The records that follow from an
//!   admitted request ([`agent`]) are queued if the socket is full, never
//!   dropped; at most one admitted request per agent is ever in flight
//!   behind a queue, so that queue is bounded by the agents, not by load.
//! - A human-side record ([`human`]) never waits. While the socket is full
//!   or `policyd` is down it goes to a ring of [`RING`] records, flushed in
//!   order on reconnect; past that, records are counted and a gap marker
//!   takes their place (COMP-12 §1).
//!
//! Elision happens here, before the write (COMP-12 §4, S-04 §3): the
//! builders below take handles, names and counts, never content. Human
//! input is recorded only as `focus` records.
//!
//! A record is written straight into one reused buffer and sent from it
//! (COMP-12 §1). It is copied out only when it has to wait in the queue, so
//! a focus change on a click allocates nothing.
//!
//! Records are emitted only while the agents hook is on. With it off there is
//! no `policyd` link and no agent for a record to be accountable to (ADR
//! 0069; COMP-01 §6: off is the normal state, not degraded mode).

use std::collections::VecDeque;
use std::os::fd::OwnedFd;

use ec_policy_eval::audit::{Kind, VERSION};
use ec_policy_eval::cbor::Writer;
use ec_policy_eval::Ulid;
use rustix::net::{send, SendFlags};
use smithay::reexports::calloop::{generic::Generic, Interest, Mode, PostAction, RegistrationToken};

use crate::addons::Hook;
use crate::state::AbyssState;

#[cfg(test)]
use ec_policy_eval::audit::Emission;

/// Human-side records held while `policyd` cannot take them (COMP-12 §7
/// open decision 1, settled at 4096 by VOL1 F-14).
pub const RING: usize = 4096;

/// The emitter's state, owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct Audit {
    /// The link, while it is up. A dup of the descriptor the link reads.
    fd: Option<OwnedFd>,
    /// The edge-triggered writable source that drains [`Audit::pending`].
    writable: Option<RegistrationToken>,
    /// Encoded records not yet in the socket, oldest first.
    pending: VecDeque<Vec<u8>>,
    /// Human records refused by a full ring since the last gap marker.
    dropped: u64,
    /// The window the last `focus` record moved focus to.
    last_focus: Option<u64>,
    /// The record being sent. Cleared, never freed, between records.
    buf: Writer,
    /// Tests: where records go instead of a socket. Each is decoded as
    /// `policyd` would, so a record it would refuse fails the test.
    #[cfg(test)]
    pub(crate) sink: Option<Vec<Emission>>,
}

impl Audit {
    /// Tests: write to `fd` as if the link were up on it.
    #[cfg(test)]
    pub(crate) fn link_for_test(&mut self, fd: OwnedFd) {
        self.sink = None;
        self.fd = Some(fd);
    }

    /// Whether anything is waiting to be written.
    pub fn backlogged(&self) -> bool {
        !self.pending.is_empty()
    }

    fn try_send(&mut self, bytes: &[u8]) -> bool {
        #[cfg(test)]
        if let Some(sink) = &mut self.sink {
            sink.push(Emission::decode(bytes).expect("an emission policyd accepts"));
            return true;
        }
        let Some(fd) = &self.fd else {
            return false;
        };
        // A short write cannot happen on SEQPACKET: the message is taken
        // whole or not at all.
        send(fd, bytes, SendFlags::DONTWAIT | SendFlags::NOSIGNAL).is_ok()
    }

    /// Writes `r` into the reused buffer.
    fn encode(&mut self, r: &Rec) {
        self.buf.clear();
        r.write(&mut self.buf);
    }

    /// Sends what [`Audit::encode`] wrote.
    fn send_buf(&mut self) -> bool {
        let buf = std::mem::take(&mut self.buf);
        let sent = self.try_send(buf.as_slice());
        self.buf = buf;
        sent
    }

    /// Writes queued records until the socket is full or the queue is empty.
    fn flush(&mut self) {
        while let Some(front) = self.pending.pop_front() {
            if !self.try_send(&front) {
                self.pending.push_front(front);
                return;
            }
        }
        if self.dropped > 0 {
            self.encode(&gap(self.dropped));
            if self.send_buf() {
                self.dropped = 0;
            }
        }
    }

    fn queue_human(&mut self, bytes: Vec<u8>) {
        if self.pending.len() >= RING {
            self.dropped += 1;
            return;
        }
        if self.dropped > 0 {
            self.pending.push_back(gap(self.dropped).bytes());
            self.dropped = 0;
        }
        self.pending.push_back(bytes);
    }
}

/// One record, borrowed from its caller until it is written.
#[derive(Debug, Clone, Copy)]
pub struct Rec<'a> {
    kind: Kind,
    principal: &'a str,
    task_id: Option<Ulid>,
    req_id: Option<u32>,
    body: Body<'a>,
}

/// What an agent asked with: the `args` of its `request` record.
#[derive(Debug, Clone, Copy)]
pub enum Args<'a> {
    None,
    Filter(&'a str),
    Handle(u64),
    Point(i64, i64),
}

#[derive(Debug, Clone, Copy)]
enum Body<'a> {
    Request {
        interface: &'a str,
        name: &'a str,
        args: Args<'a>,
    },
    Decision {
        allow: bool,
        rule_id: &'a str,
        phase: &'a str,
        latency_us: u64,
    },
    Result {
        status: u32,
        detail: &'a str,
        latency_us: u64,
    },
    Lifecycle {
        event: &'a str,
    },
    /// A table went live: its version and hash (COMP-11 §2 step 4).
    Policy {
        version: u64,
        hash: [u8; 32],
    },
    Gap {
        dropped: u64,
    },
    Focus {
        cause: &'a str,
        from: Option<u64>,
        to: Option<u64>,
    },
}

// Every map below is written with its keys already in canonical order:
// shorter key first, then bytewise. The tests decode each one, and the
// decoder refuses a misordered map.

impl Rec<'_> {
    /// The S-04 §1 emission envelope. `chain_id`, `grant_id` and `serial`
    /// are not set by abyss yet, so they are absent.
    fn write(&self, w: &mut Writer) {
        w.map(4 + usize::from(self.req_id.is_some()) + usize::from(self.task_id.is_some()));
        w.text("v");
        w.u64(VERSION);
        w.text("body");
        self.body.write(w);
        w.text("kind");
        w.text(self.kind.as_str());
        if let Some(r) = self.req_id {
            w.text("req_id");
            w.u64(u64::from(r));
        }
        if let Some(t) = self.task_id {
            w.text("task_id");
            w.bytes(&t.0);
        }
        w.text("principal");
        w.text(self.principal);
    }

    /// The record encoded on its own, for the queue and tests.
    fn bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.write(&mut w);
        w.finish()
    }
}

impl Body<'_> {
    fn write(&self, w: &mut Writer) {
        match *self {
            Body::Request {
                interface,
                name,
                args,
            } => {
                w.map(3);
                w.text("args");
                args.write(w);
                w.text("request");
                w.text(name);
                w.text("interface");
                w.text(interface);
            }
            Body::Decision {
                allow,
                rule_id,
                phase,
                latency_us,
            } => {
                w.map(4);
                w.text("phase");
                w.text(phase);
                w.text("outcome");
                w.text(if allow { "allow" } else { "deny" });
                w.text("rule_id");
                w.text(rule_id);
                w.text("latency_us");
                w.u64(latency_us);
            }
            Body::Result {
                status,
                detail,
                latency_us,
            } => {
                w.map(3);
                w.text("detail");
                w.text(detail);
                w.text("status");
                w.u64(u64::from(status));
                w.text("latency_us");
                w.u64(latency_us);
            }
            Body::Lifecycle { event } => {
                w.map(1);
                w.text("event");
                w.text(event);
            }
            Body::Policy { version, hash } => {
                w.map(2);
                w.text("hash");
                w.bytes(&hash);
                w.text("version");
                w.u64(version);
            }
            Body::Gap { dropped } => {
                w.map(2);
                w.text("event");
                w.text("audit_gap");
                w.text("dropped");
                w.u64(dropped);
            }
            Body::Focus { cause, from, to } => {
                w.map(2 + usize::from(from.is_some()) + usize::from(to.is_some()));
                w.text("seat");
                w.text("seat0");
                w.text("cause");
                w.text(cause);
                if let Some(h) = to {
                    w.text("to_handle");
                    w.u64(h);
                }
                if let Some(h) = from {
                    w.text("from_handle");
                    w.u64(h);
                }
            }
        }
    }
}

impl Args<'_> {
    fn write(&self, w: &mut Writer) {
        match *self {
            Args::None => w.map(0),
            Args::Filter(f) => {
                w.map(1);
                w.text("filter");
                w.text(f);
            }
            Args::Handle(h) => {
                w.map(1);
                w.text("handle");
                w.u64(h);
            }
            Args::Point(x, y) => {
                w.map(2);
                w.text("x");
                w.i64(x);
                w.text("y");
                w.i64(y);
            }
        }
    }
}

/// The gap marker: how many human records the ring had no room for. A
/// `lifecycle` record from the compositor itself (VOL1 F-13).
fn gap(dropped: u64) -> Rec<'static> {
    record(Kind::Lifecycle, "system:abyss", None, Body::Gap { dropped })
}

fn on(state: &AbyssState) -> bool {
    state.addons.hooks.is_on(Hook::Agents)
}

/// The link came up on `fd`: start writing, and drain what queued meanwhile.
pub fn up(state: &mut AbyssState, fd: &OwnedFd) {
    down(state);
    let (Ok(send_fd), Ok(watch_fd)) = (fd.try_clone(), fd.try_clone()) else {
        tracing::warn!("cannot dup the policyd link for audit; records stay queued");
        return;
    };
    let token = state.loop_handle.insert_source(
        Generic::new(watch_fd, Interest::WRITE, Mode::Edge),
        |_, _, state| {
            state.audit.flush();
            Ok(PostAction::Continue)
        },
    );
    match token {
        Ok(t) => state.audit.writable = Some(t),
        Err(e) => tracing::warn!(%e, "adding the audit writer to the loop"),
    }
    state.audit.fd = Some(send_fd);
    state.audit.flush();
}

/// The link went down. Queued records wait for the next one.
pub fn down(state: &mut AbyssState) {
    state.audit.fd = None;
    if let Some(t) = state.audit.writable.take() {
        state.loop_handle.remove(t);
    }
}

/// Admits an agent request: true once its `request` record is in the
/// socket. False means the agent stalls: it is answered `paused` and
/// nothing happens (VOL1 F-12).
pub fn begin(state: &mut AbyssState, r: Rec) -> bool {
    if !on(state) {
        return true;
    }
    state.audit.flush();
    if state.audit.backlogged() {
        return false;
    }
    state.audit.encode(&r);
    state.audit.send_buf()
}

/// A record an agent is accountable for: one that follows from an admitted
/// request, or its own `lifecycle` start/stop. Queued, never dropped, if the
/// socket will not take it now; bounded by the agents, not by load.
pub fn agent(state: &mut AbyssState, r: Rec) {
    if !on(state) {
        return;
    }
    state.audit.flush();
    state.audit.encode(&r);
    if state.audit.backlogged() || !state.audit.send_buf() {
        let bytes = state.audit.buf.as_slice().to_vec();
        state.audit.pending.push_back(bytes);
    }
}

/// A link message to `policyd` (`policy::link::send`). It rides the same
/// ordered queue as agent records, so a request is never seen by `policyd`
/// ahead of the records that led to it, and like them it is queued, never
/// dropped. Sent whatever the agents hook says: only the link carries it.
pub fn message(state: &mut AbyssState, bytes: Vec<u8>) {
    state.audit.pending.push_back(bytes);
    state.audit.flush();
}

/// A human-side record (`focus`). Never stalls; rings when it must
/// (COMP-12 §1).
pub fn human(state: &mut AbyssState, r: Rec) {
    if !on(state) {
        return;
    }
    state.audit.flush();
    state.audit.encode(&r);
    if state.audit.backlogged() || !state.audit.send_buf() {
        let bytes = state.audit.buf.as_slice().to_vec();
        state.audit.queue_human(bytes);
    }
}

// ------------------------------------------------------------ builders

fn record<'a>(kind: Kind, principal: &'a str, req_id: Option<u32>, body: Body<'a>) -> Rec<'a> {
    Rec {
        kind,
        principal,
        task_id: None,
        req_id,
        body,
    }
}

/// `request` (S-04 §1.1). The caller elides `args` per S-04 §3 before it
/// gets here.
pub fn request<'a>(
    principal: &'a str,
    task_id: Option<Ulid>,
    req_id: u32,
    interface: &'a str,
    name: &'a str,
    args: Args<'a>,
) -> Rec<'a> {
    let body = Body::Request {
        interface,
        name,
        args,
    };
    Rec {
        task_id,
        ..record(Kind::Request, principal, Some(req_id), body)
    }
}

/// `decision` (S-04 §1.1). `rule_id` is what decided it: the capability
/// checked.
pub fn decision<'a>(
    principal: &'a str,
    task_id: Option<Ulid>,
    req_id: u32,
    allow: bool,
    rule_id: &'a str,
    phase: &'a str,
    latency_us: u64,
) -> Rec<'a> {
    let body = Body::Decision {
        allow,
        rule_id,
        phase,
        latency_us,
    };
    Rec {
        task_id,
        ..record(Kind::Decision, principal, Some(req_id), body)
    }
}

/// `result` (S-04 §1.1). `status` is the `eclipse_agent_v1` status value.
pub fn result<'a>(principal: &'a str, req_id: u32, status: u32, detail: &'a str, latency_us: u64) -> Rec<'a> {
    let body = Body::Result {
        status,
        detail,
        latency_us,
    };
    record(Kind::Result, principal, Some(req_id), body)
}

/// `policy`: an enforcement table went live in the compositor.
pub fn policy(version: u64, hash: [u8; 32]) -> Rec<'static> {
    record(Kind::Policy, "system:abyss", None, Body::Policy { version, hash })
}

/// `lifecycle` (S-04 §1.1): `start`, `stop`, `pause`, `resume`.
pub fn lifecycle<'a>(principal: &'a str, event: &'a str) -> Rec<'a> {
    record(Kind::Lifecycle, principal, None, Body::Lifecycle { event })
}

/// Keyboard focus moved to window `to` (`None`: to no window). A `focus`
/// record (S-04 §1.1) when it is a change. Handles only: what was typed
/// is never recorded (S-04 §2).
pub fn focus(state: &mut AbyssState, to: Option<u64>, cause: &str) {
    if !on(state) || state.audit.last_focus == to {
        return;
    }
    let from = std::mem::replace(&mut state.audit.last_focus, to);
    human(
        state,
        record(Kind::Focus, "human", None, Body::Focus { cause, from, to }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::cbor::Reader;

    fn pair() -> (OwnedFd, OwnedFd) {
        use rustix::net::{socketpair, AddressFamily, SocketFlags, SocketType};
        socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .unwrap()
    }

    fn drain(fd: &OwnedFd) -> Vec<Emission> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; ec_policy_eval::audit::MAX_EMISSION];
        while let Ok((n, _)) = rustix::net::recv(fd, &mut buf[..], rustix::net::RecvFlags::DONTWAIT) {
            out.push(Emission::decode(&buf[..n]).unwrap());
        }
        out
    }

    fn body_text(e: &Emission, key: &str) -> Option<String> {
        let mut r = Reader::new(&e.body);
        let n = r.map_begin().ok()?;
        for _ in 0..n {
            if r.key().ok()? == key {
                return r.text().ok().map(str::to_owned);
            }
            r.skip().ok()?;
        }
        None
    }

    #[test]
    fn a_full_ring_counts_and_leaves_one_gap_marker() {
        let mut a = Audit::default();
        for _ in 0..RING + 3 {
            a.queue_human(lifecycle("human", "x").bytes());
        }
        assert_eq!((a.pending.len(), a.dropped), (RING, 3));

        let (ours, theirs) = pair();
        a.fd = Some(ours);
        // Drain as far as the socket buffer allows, read, repeat.
        let mut got = Vec::new();
        loop {
            a.flush();
            let batch = drain(&theirs);
            if batch.is_empty() {
                break;
            }
            got.extend(batch);
        }
        assert_eq!(got.len(), RING + 1);
        let last = got.last().unwrap();
        assert_eq!(body_text(last, "event").as_deref(), Some("audit_gap"));
        assert_eq!(a.dropped, 0);
    }

    #[test]
    fn records_keep_their_order_through_the_queue() {
        let mut a = Audit::default();
        a.queue_human(lifecycle("human", "first").bytes());
        a.queue_human(lifecycle("human", "second").bytes());
        let (ours, theirs) = pair();
        a.fd = Some(ours);
        a.flush();
        let got: Vec<_> = drain(&theirs)
            .iter()
            .map(|e| body_text(e, "event").unwrap())
            .collect();
        assert_eq!(got, ["first", "second"]);
    }

    #[test]
    fn nothing_is_sent_without_a_link() {
        let mut a = Audit::default();
        assert!(!a.try_send(&lifecycle("human", "x").bytes()));
    }

    #[test]
    fn builders_produce_emissions_policyd_accepts() {
        for r in [
            request(
                "agent:a",
                Some(Ulid([1; 16])),
                7,
                "eclipse_scene_v1",
                "hit_test",
                Args::Point(-3, 4),
            ),
            request(
                "agent:a",
                None,
                7,
                "eclipse_scene_v1",
                "list_toplevels",
                Args::Filter("all"),
            ),
            request(
                "agent:a",
                None,
                7,
                "eclipse_scene_v1",
                "get_toplevel",
                Args::Handle(9),
            ),
            request("agent:a", None, 7, "eclipse_scene_v1", "unknown", Args::None),
            decision("agent:a", Some(Ulid([2; 16])), 7, true, "scene.list", "scope", 3),
            decision("agent:a", None, 7, false, "scene.list", "scope", 300),
            result("agent:a", 7, 0, "", 3),
            result("agent:a", 7, 12, "handle", 70_000),
            lifecycle("agent:a", "start"),
            gap(5000),
            record(
                Kind::Focus,
                "human",
                None,
                Body::Focus {
                    cause: "human",
                    from: None,
                    to: None,
                },
            ),
            record(
                Kind::Focus,
                "human",
                None,
                Body::Focus {
                    cause: "human",
                    from: Some(1),
                    to: Some(300),
                },
            ),
        ] {
            let bytes = r.bytes();
            // The decoder refuses a misordered map, at the envelope and,
            // walked here, in the body.
            let e = Emission::decode(&bytes).unwrap();
            assert!(e.kind.from_compositor());
            let mut body = Reader::new(&e.body);
            body.skip().unwrap();
            body.finish().unwrap();
            // And it is the one canonical encoding.
            assert_eq!(e.encode(), bytes);
        }
    }

    #[test]
    fn the_send_buffer_is_reused_across_records() {
        let mut a = Audit {
            sink: Some(Vec::new()),
            ..Default::default()
        };
        a.encode(&lifecycle("agent:a", "start"));
        assert!(a.send_buf());
        let cap = a.buf.capacity();
        a.encode(&lifecycle("agent:a", "stop"));
        assert!(a.send_buf());
        assert_eq!(a.buf.capacity(), cap);
        assert_eq!(a.sink.as_ref().unwrap().len(), 2);
    }
}
