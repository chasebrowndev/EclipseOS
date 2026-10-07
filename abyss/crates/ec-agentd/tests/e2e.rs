// SPDX-License-Identifier: AGPL-3.0-only
//! agentd against a fake `policyd` (an in-process SEQPACKET listener), a
//! console client and a fake agent speaking MCP.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ec_agentd::{start, Config, Daemon};
use ec_policy_eval::audit::{Emission, Kind as AuditKind};
use ec_policy_eval::cbor::{enc, MapBuilder, Reader};
use ec_policy_eval::link::{self, CancelMode, FromPolicyd, ToPolicyd};
use ec_policy_eval::Ulid;
use rustix::net::{self, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags};
use serde_json::{json, Value};

// ---- fake policyd ----------------------------------------------------------

fn seq_listener(path: &Path) -> OwnedFd {
    let _ = std::fs::remove_file(path);
    let fd = net::socket_with(
        net::AddressFamily::UNIX,
        net::SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    net::bind(&fd, &SocketAddrUnix::new(path).unwrap()).unwrap();
    net::listen(&fd, 4).unwrap();
    fd
}

struct Fake {
    fd: OwnedFd,
    emissions: Vec<Emission>,
}

impl Fake {
    fn accept(listener: &OwnedFd) -> Fake {
        let fd = net::accept_with(listener, SocketFlags::CLOEXEC).unwrap();
        net::sockopt::set_socket_timeout(&fd, net::sockopt::Timeout::Recv, Some(Duration::from_secs(5)))
            .unwrap();
        let mut m = MapBuilder::new();
        m.insert("key", enc(|w| w.bytes(&[7u8; 32])));
        m.insert("v", enc(|w| w.u64(1)));
        net::send(&fd, &m.finish(), SendFlags::NOSIGNAL).unwrap();
        Fake {
            fd,
            emissions: Vec::new(),
        }
    }

    fn send(&self, m: &FromPolicyd) {
        net::send(&self.fd, &m.encode(), SendFlags::NOSIGNAL).unwrap();
    }

    fn recv_raw(&self) -> Vec<u8> {
        let mut buf = vec![0u8; 70_000];
        let (n, _) = net::recv(&self.fd, &mut buf[..], RecvFlags::empty()).expect("agentd sent nothing");
        buf.truncate(n);
        buf
    }

    /// The next link message from agentd; audit emissions met on the way are
    /// kept in `emissions`.
    fn next_msg(&mut self) -> ToPolicyd {
        loop {
            let b = self.recv_raw();
            if link::is_message(&b) {
                return ToPolicyd::decode(&b).expect("a decodable message");
            }
            self.emissions
                .push(Emission::decode(&b).expect("a decodable emission"));
        }
    }

    /// The next emission, from the ones kept or the wire.
    fn next_emission(&mut self) -> Emission {
        if !self.emissions.is_empty() {
            return self.emissions.remove(0);
        }
        let b = self.recv_raw();
        assert!(!link::is_message(&b), "expected an audit record");
        Emission::decode(&b).unwrap()
    }
}

/// `{msg_id, size, chain_hash, op}` out of a channel record body.
fn channel_body(e: &Emission) -> (u64, u64, usize, String) {
    let mut r = Reader::new(&e.body);
    let n = r.map_begin().unwrap();
    let (mut id, mut size, mut hash, mut op) = (0, 0, 0, String::new());
    for _ in 0..n {
        match r.key().unwrap() {
            "msg_id" => id = r.u64().unwrap(),
            "size" => size = r.u64().unwrap(),
            "chain_hash" => hash = r.bytes().unwrap().len(),
            "op" => op = r.text().unwrap().to_owned(),
            k => panic!("unexpected key {k}"),
        }
    }
    (id, size, hash, op)
}

// ---- JSON-RPC client -------------------------------------------------------

struct Cl {
    r: BufReader<UnixStream>,
    w: UnixStream,
    ev: VecDeque<(String, Value)>,
    next: u64,
}

impl Cl {
    fn connect(path: &Path) -> Cl {
        let t = Instant::now();
        let s = loop {
            match UnixStream::connect(path) {
                Ok(s) => break s,
                Err(e) if t.elapsed() < Duration::from_secs(5) => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("cannot connect to {}: {e}", path.display()),
            }
        };
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Cl {
            r: BufReader::new(s.try_clone().unwrap()),
            w: s,
            ev: VecDeque::new(),
            next: 1,
        }
    }

    fn raw(&mut self, line: &str) {
        // A refused peer may already be gone; that is the test's business.
        let _ = writeln!(self.w, "{line}");
    }

    fn read(&mut self) -> Option<Value> {
        let mut l = String::new();
        match self.r.read_line(&mut l) {
            Ok(0) => None,
            Ok(_) => Some(serde_json::from_str(&l).unwrap()),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => None,
            Err(e) => panic!("read: {e}"),
        }
    }

    fn stash(&mut self, v: &Value) -> bool {
        if v.get("method").and_then(Value::as_str) == Some("event") {
            let p = &v["params"];
            self.ev
                .push_back((p["event"].as_str().unwrap().to_owned(), p["data"].clone()));
            true
        } else {
            false
        }
    }

    /// The whole response to a call, events met on the way kept in order.
    fn call(&mut self, m: &str, p: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.raw(&json!({"jsonrpc": "2.0", "id": id, "method": m, "params": p}).to_string());
        loop {
            let v = self.read().expect("connection closed");
            if v.get("id") == Some(&json!(id)) {
                return v;
            }
            self.stash(&v);
        }
    }

    fn ok(&mut self, m: &str, p: Value) -> Value {
        let v = self.call(m, p);
        assert!(v.get("error").is_none(), "{m}: {v}");
        v["result"].clone()
    }

    fn next_event(&mut self) -> (String, Value) {
        loop {
            if let Some(e) = self.ev.pop_front() {
                return e;
            }
            let v = self.read().expect("connection closed");
            self.stash(&v);
        }
    }

    fn event(&mut self, name: &str) -> Value {
        let (n, d) = self.next_event();
        assert_eq!(n, name, "event order ({d})");
        d
    }

    /// An MCP tool call: `(isError, structuredContent)`.
    fn tool(&mut self, name: &str, args: Value) -> (bool, Value) {
        let r = self.ok("tools/call", json!({"name": name, "arguments": args}));
        (r["isError"].as_bool().unwrap(), r["structuredContent"].clone())
    }
}

// ---- the rig ---------------------------------------------------------------

struct Opts {
    allow_console: bool,
    human: Option<PathBuf>,
    launch: bool,
    packages: Vec<(PathBuf, String)>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            allow_console: true,
            human: None,
            launch: false,
            packages: vec![],
        }
    }
}

const T0: u64 = 1_000_000_000_000;

struct Rig {
    dir: PathBuf,
    d: Option<Daemon>,
    listener: OwnedFd,
    fake: Option<Fake>,
    clock: Arc<AtomicU64>,
}

impl Rig {
    fn new(tag: &str) -> Rig {
        Rig::with(ec_agentd::scratch_dir(tag), T0, Opts::default())
    }

    fn with(dir: PathBuf, now: u64, o: Opts) -> Rig {
        let clock = Arc::new(AtomicU64::new(now));
        let c2 = clock.clone();
        let psock = dir.join("p.sock");
        let listener = seq_listener(&psock);
        let allow = o.allow_console;
        let cfg = Config {
            runtime_dir: dir.join("run"),
            state_dir: dir.join("state"),
            policyd_socket: psock,
            human_socket: o.human,
            package_roots: o.packages,
            retention_ms: ec_agentd::RETENTION_DAYS * 24 * 3600 * 1000,
            launch: o.launch,
            console_policy: Arc::new(move |_| allow),
            clock: Arc::new(move || c2.load(Ordering::Relaxed)),
        };
        let d = start(cfg).unwrap();
        let fake = Fake::accept(&listener);
        let mut r = Rig {
            dir,
            d: Some(d),
            listener,
            fake: Some(fake),
            clock,
        };
        if allow {
            r.wait_available(true);
        }
        r
    }

    fn console_path(&self) -> PathBuf {
        self.dir.join("run/console.sock")
    }

    fn console(&self) -> Cl {
        Cl::connect(&self.console_path())
    }

    fn fake(&mut self) -> &mut Fake {
        self.fake.as_mut().unwrap()
    }

    fn wait_available(&mut self, want: bool) {
        let mut c = self.console();
        let t = Instant::now();
        loop {
            let r = c.ok("list_tasks", json!({}));
            if r["available"] == json!(want) {
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "link never became {want}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn mcp(&self, task: &str) -> Cl {
        let mut c = Cl::connect(&self.dir.join("run/agents").join(task).join("mcp.sock"));
        let init = c.ok("initialize", json!({"protocolVersion": "2025-06-18"}));
        assert!(init["serverInfo"]["name"].is_string());
        c
    }

    /// Shuts the daemon down, keeping the directories.
    fn stop(mut self) -> (PathBuf, Arc<AtomicU64>) {
        self.d.take().unwrap().shutdown();
        (self.dir.clone(), self.clock.clone())
    }

    #[allow(dead_code)]
    fn listener(&self) -> &OwnedFd {
        &self.listener
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(d) = self.d.take() {
            d.shutdown();
        }
    }
}

fn tid(n: u64) -> String {
    Ulid::from_parts(1_700_000_000_000 + n, [n as u8; 10]).to_text()
}

fn prov(task: &str, principal: &str) -> FromPolicyd {
    FromPolicyd::Provision {
        task: task.to_owned(),
        principal: principal.to_owned(),
        package: "ref".into(),
        version: "1".into(),
        statement: "Triage the invoices".into(),
        deadline_ms: 7_200_000,
        grant: vec![1, 2, 3],
        continuation: String::new(),
        resumes: String::new(),
    }
}

fn prov_resumes(task: &str, principal: &str, resumes: &str) -> FromPolicyd {
    let FromPolicyd::Provision {
        task,
        principal,
        package,
        version,
        statement,
        deadline_ms,
        grant,
        continuation,
        ..
    } = prov(task, principal)
    else {
        unreachable!()
    };
    FromPolicyd::Provision {
        task,
        principal,
        package,
        version,
        statement,
        deadline_ms,
        grant,
        continuation,
        resumes: resumes.to_owned(),
    }
}

fn state(task: &str, s: &str, reason: &str) -> FromPolicyd {
    FromPolicyd::TaskState {
        task: task.to_owned(),
        state: s.to_owned(),
        reason: reason.to_owned(),
    }
}

// ---- tests -----------------------------------------------------------------

#[test]
fn json_rpc_conformance() {
    let rig = Rig::new("conf");
    let mut c = rig.console();
    c.raw("{nope");
    assert_eq!(c.read().unwrap()["error"]["code"], -32700);
    c.raw("[1,2]");
    assert_eq!(c.read().unwrap()["error"]["code"], -32600);
    c.raw(r#"{"jsonrpc":"2.0","id":9,"method":"list_tasks","params":[1]}"#);
    let r = c.read().unwrap();
    assert_eq!(
        (r["error"]["code"].clone(), r["id"].clone()),
        (json!(-32602), json!(9))
    );
    let r = c.call("no_such_method", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    assert_eq!(r["jsonrpc"], "2.0");
    // A notification gets no reply: the next line is the next call's answer.
    c.raw(r#"{"jsonrpc":"2.0","method":"list_tasks"}"#);
    let r = c.call("list_tasks", json!({}));
    assert!(r["result"]["tasks"].is_array());
    // String ids come back as sent.
    c.raw(r#"{"jsonrpc":"2.0","id":"abc","method":"list_packages"}"#);
    let r = c.read().unwrap();
    assert_eq!(r["id"], "abc");
    assert!(r["result"]["packages"].is_array());
}

#[test]
fn the_console_socket_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new("perm");
    let m = std::fs::metadata(rig.console_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(m, 0o600);
}

#[test]
fn a_refused_peer_is_closed() {
    let dir = ec_agentd::scratch_dir("peer");
    let rig = Rig::with(
        dir,
        T0,
        Opts {
            allow_console: false,
            ..Opts::default()
        },
    );
    let mut s = Cl::connect(&rig.console_path());
    s.raw(r#"{"jsonrpc":"2.0","id":1,"method":"list_tasks"}"#);
    assert!(s.read().is_none(), "a refused peer must get no answer");
}

#[test]
fn provision_to_close_with_event_order_and_audit() {
    let mut rig = Rig::new("flow");
    let mut c = rig.console();
    let sub = c.ok("subscribe", json!({}));
    assert_eq!(sub["available"], true);
    let id = tid(1);
    let principal = "agent:ref";
    rig.fake().send(&prov(&id, principal));
    let started = c.event("task_started");
    assert_eq!(
        started,
        json!({"task_id": id, "package": "ref", "statement": "Triage the invoices", "deadline_ms": 7_200_000})
    );

    let t = c.ok("get_task", json!({"task_id": id}));
    assert_eq!(t["state"], "active");
    assert_eq!(t["package"], "ref");
    assert_eq!(t["awaiting_reply"], false);
    assert!(t.get("grant").is_none());

    // The fake agent.
    let mut m = rig.mcp(&id);
    let init = m.ok("initialize", json!({}));
    assert_eq!(init["instructions"], "Triage the invoices");
    let tools = m.ok("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["task.say", "task.ask", "task.inbox"]);

    let (err, r) = m.tool("task.say", json!({"text": "Found 3 invoices"}));
    assert!(!err, "{r}");
    let ev = c.event("message");
    assert_eq!(ev["kind"], "say");
    assert_eq!(ev["text"], "Found 3 invoices");
    assert_eq!(ev["trust"], json!({"min_trust": "standard", "head": principal}));

    // The audit record carries id, size and a hash, never the text.
    let e = rig.fake().next_emission();
    assert_eq!(e.kind, AuditKind::Channel);
    assert_eq!(e.principal, principal);
    assert_eq!(e.task_id, ec_agentd::ulid_from_text(&id));
    let (mid, size, hash, op) = channel_body(&e);
    assert_eq!((mid, size, hash, op.as_str()), (1, 16, 32, "post"));
    assert!(!e.body.windows(5).any(|w| w == b"Found"));

    let (err, _) = m.tool("task.ask", json!({"question": "Which folder?"}));
    assert!(!err);
    assert_eq!(c.event("message")["kind"], "ask");
    assert_eq!(
        c.event("awaiting_reply"),
        json!({"task_id": id, "awaiting": true})
    );
    let tasks = c.ok("list_tasks", json!({}));
    assert_eq!(tasks["tasks"][0]["awaiting_reply"], true);

    // A human reply: standard trust, never `human`; it clears awaiting_reply.
    let r = c.ok(
        "conversation_post",
        json!({"task_id": id, "text": "The shared one", "reply_to": 2}),
    );
    assert_eq!(r["msg_id"], 3);
    let ev = c.event("message");
    assert_eq!(ev["kind"], "human");
    assert_eq!(ev["reply_to"], 2);
    assert_eq!(
        ev["trust"],
        json!({"min_trust": "standard", "head": "human_client"})
    );
    assert_eq!(
        c.event("awaiting_reply"),
        json!({"task_id": id, "awaiting": false})
    );

    let (_, inbox) = m.tool("task.inbox", json!({}));
    assert_eq!(inbox["closed"], false);
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["messages"][0]["text"], "The shared one");
    assert_eq!(inbox["messages"][0]["trust"]["min_trust"], "standard");
    let cursor = inbox["cursor"].as_u64().unwrap();
    let (_, again) = m.tool("task.inbox", json!({"since": cursor}));
    assert_eq!(again["messages"], json!([]));

    // Reading the transcript.
    let read = c.ok("conversation_read", json!({"task_id": id}));
    assert_eq!(read["messages"].as_array().unwrap().len(), 3);
    assert_eq!(read["closed"], false);
    let after = c.ok("conversation_read", json!({"task_id": id, "since": 2}));
    assert_eq!(after["messages"].as_array().unwrap().len(), 1);

    // Pause goes to policyd and the answer comes back on the same call.
    c.raw(
        &json!({"jsonrpc": "2.0", "id": 100, "method": "pause_task", "params": {"task_id": id}}).to_string(),
    );
    let ToPolicyd::PauseTask { req, task } = next_non_audit(rig.fake()) else {
        panic!("expected pause_task")
    };
    assert_eq!(task, id);
    rig.fake().send(&FromPolicyd::Done { req });
    rig.fake().send(&state(&id, "paused", "human"));
    loop {
        let v = c.read().unwrap();
        if v.get("id") == Some(&json!(100)) {
            assert_eq!(v["result"], json!({"ok": true}));
            break;
        }
        c.stash(&v);
    }
    assert_eq!(
        c.event("task_state"),
        json!({"task_id": id, "state": "paused", "reason": "human"})
    );

    // Drain cancel: told to policyd; the agent's inbox reports closed.
    c.raw(
        &json!({"jsonrpc": "2.0", "id": 101, "method": "cancel_task",
                  "params": {"task_id": id, "mode": "drain"}})
        .to_string(),
    );
    let ToPolicyd::CancelTask { req, task, mode } = next_non_audit(rig.fake()) else {
        panic!("expected cancel_task")
    };
    assert_eq!((task.as_str(), mode), (id.as_str(), CancelMode::Drain));
    rig.fake().send(&FromPolicyd::Done { req });
    loop {
        let v = c.read().unwrap();
        if v.get("id") == Some(&json!(101)) {
            break;
        }
        c.stash(&v);
    }
    let (_, inbox) = m.tool("task.inbox", json!({"since": cursor}));
    assert_eq!(inbox["closed"], true);

    // Closed: state, then close, then no more posts.
    rig.fake().send(&state(&id, "closed", "cancelled"));
    assert_eq!(
        c.event("task_state"),
        json!({"task_id": id, "state": "closed", "reason": "cancelled"})
    );
    assert_eq!(
        c.event("task_closed"),
        json!({"task_id": id, "reason": "cancelled"})
    );
    let r = c.call("conversation_post", json!({"task_id": id, "text": "late"}));
    assert_eq!(r["error"]["code"], -32006);
    assert!(!rig.dir.join("run/agents").join(&id).join("mcp.sock").exists());

    // History survives a restart (the task is closed, so it is a session).
    let (dir, clock) = rig.stop_keep();
    let rig2 = Rig::with(dir, clock.load(Ordering::Relaxed), Opts::default());
    let mut c2 = rig2.console();
    let s = c2.ok("list_sessions", json!({}));
    assert_eq!(s["sessions"][0]["task_id"], id);
    assert_eq!(s["sessions"][0]["reason"], "cancelled");
    let read = c2.ok("conversation_read", json!({"task_id": id}));
    assert_eq!(read["messages"].as_array().unwrap().len(), 3);
    assert_eq!(read["closed"], true);
    assert!(c2.ok("list_tasks", json!({}))["tasks"]
        .as_array()
        .unwrap()
        .is_empty());

    // Files are private.
    use std::os::unix::fs::PermissionsExt;
    let conv = rig2.dir.join("state/conversations").join(&id);
    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(conv.clone()), 0o700);
    assert_eq!(mode(conv.join("messages.jsonl")), 0o600);
    assert_eq!(mode(conv.join("meta.json")), 0o600);

    // Delete removes the session and its transcript.
    assert_eq!(c2.ok("delete_session", json!({"task_id": id}))["deleted"], true);
    assert!(!conv.exists());
    assert_eq!(
        c2.call("get_task", json!({"task_id": id}))["error"]["code"],
        -32002
    );
}

impl Rig {
    fn stop_keep(self) -> (PathBuf, Arc<AtomicU64>) {
        self.stop()
    }
}

fn next_non_audit(f: &mut Fake) -> ToPolicyd {
    f.next_msg()
}

#[test]
fn another_task_and_a_missing_task_look_the_same() {
    let mut rig = Rig::new("iso");
    let (a, b) = (tid(1), tid(2));
    rig.fake().send(&prov(&a, "agent:a"));
    rig.fake().send(&prov(&b, "agent:b"));
    let mut c = rig.console();
    // Wait for both.
    let t = Instant::now();
    while c.ok("list_tasks", json!({}))["tasks"].as_array().unwrap().len() < 2 {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    // Console: every unknown id answers the same error, whatever its shape.
    let missing = [
        tid(99),
        "../../etc/passwd".to_owned(),
        String::new(),
        "x".repeat(300),
    ];
    let mut shapes: Vec<Value> = Vec::new();
    for m in ["get_task", "conversation_read"] {
        for id in &missing {
            shapes.push(c.call(m, json!({"task_id": id}))["error"].clone());
        }
    }
    for id in &missing {
        shapes.push(c.call("conversation_post", json!({"task_id": id, "text": "hi"}))["error"].clone());
        shapes.push(c.call("pause_task", json!({"task_id": id}))["error"].clone());
        shapes.push(c.call("cancel_task", json!({"task_id": id, "mode": "drain"}))["error"].clone());
        shapes.push(c.call("delete_session", json!({"task_id": id}))["error"].clone());
    }
    for e in &shapes {
        assert_eq!(e["code"], -32002, "{e}");
        assert_eq!(e, &shapes[0]);
    }

    // MCP: no parameter names a task. An agent that tries lands in its own.
    let mut ma = rig.mcp(&a);
    let (err, _) = ma.tool(
        "task.say",
        json!({"text": "mine", "task_id": b, "channel": format!("conversation/{b}")}),
    );
    assert!(!err);
    let ra = c.ok("conversation_read", json!({"task_id": a}));
    let rb = c.ok("conversation_read", json!({"task_id": b}));
    assert_eq!(ra["messages"].as_array().unwrap().len(), 1);
    assert_eq!(rb["messages"].as_array().unwrap().len(), 0);
    // Unknown tools are protocol errors.
    let r = ma.call(
        "tools/call",
        json!({"name": "task.read_other", "arguments": {"task_id": b}}),
    );
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn agent_quota_is_sixty_messages_a_minute() {
    let mut rig = Rig::new("quota");
    let id = tid(1);
    rig.fake().send(&prov(&id, "agent:q"));
    let mut c = rig.console();
    let mut m = rig.mcp(&id);
    for i in 0..60 {
        let (err, r) = m.tool("task.say", json!({"text": format!("m{i}")}));
        assert!(!err, "{i}: {r}");
    }
    let (err, r) = m.tool("task.say", json!({"text": "one too many"}));
    assert!(err);
    assert_eq!(r["error"], "quota_exceeded");
    assert_eq!(
        c.ok("get_task", json!({"task_id": id}))["counters"]["quota_exceeded"],
        1
    );
    // The breach is audited, without a body.
    let mut last = None;
    for _ in 0..61 {
        last = Some(rig.fake().next_emission());
    }
    let (mid, _, _, op) = channel_body(&last.unwrap());
    assert_eq!((mid, op.as_str()), (0, "quota_exceeded"));
    // The human is not quota-limited by this, and the window slides.
    rig.clock.fetch_add(61_000, Ordering::Relaxed);
    let (err, _) = m.tool("task.say", json!({"text": "later"}));
    assert!(!err);
}

#[test]
fn text_is_sanitised_and_bounded() {
    let mut rig = Rig::new("san");
    let id = tid(1);
    let mut c = rig.console();
    c.ok("subscribe", json!({}));
    rig.fake().send(&prov(&id, "agent:s"));
    let mut m = rig.mcp(&id);
    c.event("task_started");
    let (err, _) = m.tool(
        "task.say",
        json!({"text": "a\u{1b}[31mred\u{202E}gnp.exe\u{0}\tz\nend"}),
    );
    assert!(!err);
    assert_eq!(c.event("message")["text"], "a[31mredgnp.exe\tz\nend");
    // Human posts too.
    c.ok(
        "conversation_post",
        json!({"task_id": id, "text": "x\u{7}y\u{2066}z"}),
    );
    assert_eq!(c.event("message")["text"], "xyz");
    // Bounds.
    let big = "x".repeat(16 * 1024 + 1);
    assert_eq!(
        c.call("conversation_post", json!({"task_id": id, "text": big}))["error"]["code"],
        -32602
    );
    assert_eq!(
        c.call("conversation_post", json!({"task_id": id, "text": "\u{7}"}))["error"]["code"],
        -32602
    );
    let (err, r) = m.tool("task.say", json!({"text": big}));
    assert!(err);
    assert_eq!(r["error"], "too_large");
    let ok = "y".repeat(16 * 1024);
    assert!(!m.tool("task.say", json!({"text": ok})).0);
}

#[test]
fn revocation_closes_the_task() {
    let mut rig = Rig::new("revoke");
    let id = tid(1);
    let mut c = rig.console();
    c.ok("subscribe", json!({}));
    rig.fake().send(&prov(&id, "agent:r"));
    c.event("task_started");
    let mut m = rig.mcp(&id);
    assert!(!m.tool("task.ask", json!({"question": "q"})).0);
    c.event("message");
    c.event("awaiting_reply");
    rig.fake().send(&FromPolicyd::Revoked {
        principal: "agent:r".into(),
    });
    assert_eq!(c.event("task_state")["reason"], "revoked");
    assert_eq!(c.event("awaiting_reply")["awaiting"], false);
    assert_eq!(c.event("task_closed")["reason"], "revoked");
    // The agent's connection is cut.
    assert!(m.read().is_none());
}

#[test]
fn policyd_down_is_visible_and_redial_recovers() {
    let mut rig = Rig::new("down");
    let id = tid(1);
    rig.fake().send(&prov(&id, "agent:d"));
    let mut c = rig.console();
    while c.ok("list_tasks", json!({}))["tasks"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.fake = None; // policyd goes away
    rig.wait_available(false);
    assert_eq!(
        c.call("pause_task", json!({"task_id": id}))["error"]["code"],
        -32003
    );
    assert_eq!(
        c.call("cancel_task", json!({"task_id": id, "mode": "immediate"}))["error"]["code"],
        -32003
    );
    // Reading still works.
    assert!(c.ok("get_task", json!({"task_id": id}))["available"] == false);
    // It comes back and agentd redials.
    rig.fake = Some(Fake::accept(&rig.listener));
    rig.wait_available(true);
    c.raw(&json!({"jsonrpc": "2.0", "id": 7, "method": "pause_task", "params": {"task_id": id}}).to_string());
    assert!(matches!(rig.fake().next_msg(), ToPolicyd::PauseTask { .. }));
}

#[test]
fn audit_queued_while_down_is_sent_after_redial() {
    let mut rig = Rig::new("queue");
    let id = tid(1);
    rig.fake().send(&prov(&id, "agent:d"));
    let mut m = rig.mcp(&id);
    rig.fake = None;
    rig.wait_available(false);
    assert!(!m.tool("task.say", json!({"text": "while down"})).0);
    rig.fake = Some(Fake::accept(&rig.listener));
    let e = rig.fake().next_emission();
    assert_eq!(channel_body(&e).3, "post");
}

#[test]
fn pause_that_policyd_refuses_says_so() {
    let mut rig = Rig::new("refuse");
    let id = tid(1);
    rig.fake().send(&prov(&id, "agent:p"));
    let mut c = rig.console();
    while c.ok("list_tasks", json!({}))["tasks"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    c.raw(&json!({"jsonrpc": "2.0", "id": 5, "method": "pause_task", "params": {"task_id": id}}).to_string());
    let ToPolicyd::PauseTask { req, .. } = rig.fake().next_msg() else {
        panic!()
    };
    rig.fake().send(&FromPolicyd::Refused {
        req,
        reason: "not_active".into(),
    });
    let v = c.read().unwrap();
    assert_eq!(v["id"], 5);
    assert_eq!(v["error"]["data"]["reason"], "not_active");
}

#[test]
fn an_open_task_does_not_survive_a_restart() {
    let mut rig = Rig::new("restart");
    let id = tid(1);
    rig.fake().send(&prov(&id, "agent:x"));
    let mut c = rig.console();
    while c.ok("list_tasks", json!({}))["tasks"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let (dir, clock) = rig.stop();
    let mut rig2 = Rig::with(dir, clock.load(Ordering::Relaxed), Opts::default());
    // policyd is told the leftover task is gone.
    let ToPolicyd::Exited { task, reason, .. } = rig2.fake().next_msg() else {
        panic!("expected exited")
    };
    assert_eq!((task, reason.as_str()), (id.clone(), "failed"));
    let mut c2 = rig2.console();
    let t = c2.ok("get_task", json!({"task_id": id}));
    assert_eq!(t["state"], "closed");
    assert_eq!(t["reason"], "agentd_restart");
}

#[test]
fn retention_sweeps_old_sessions() {
    let mut rig = Rig::new("retain");
    let id = tid(1);
    rig.fake().send(&prov(&id, "agent:x"));
    let mut c = rig.console();
    while c.ok("list_tasks", json!({}))["tasks"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    rig.fake().send(&state(&id, "closed", "completed"));
    while c.ok("list_sessions", json!({}))["sessions"]
        .as_array()
        .unwrap()
        .is_empty()
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let (dir, clock) = rig.stop();
    let day = 24 * 3600 * 1000;
    // 29 days on: kept. 31 days on: gone, transcript and all.
    let rig2 = Rig::with(
        dir.clone(),
        clock.load(Ordering::Relaxed) + 29 * day,
        Opts::default(),
    );
    assert_eq!(
        rig2.console().ok("list_sessions", json!({}))["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (dir, _) = rig2.stop();
    let rig3 = Rig::with(
        dir.clone(),
        clock.load(Ordering::Relaxed) + 31 * day,
        Opts::default(),
    );
    assert!(rig3.console().ok("list_sessions", json!({}))["sessions"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!dir.join("state/conversations").join(&id).exists());
}

#[test]
fn list_packages_reads_manifests() {
    let dir = ec_agentd::scratch_dir("pk");
    let root = dir.join("agents");
    let v = root.join("triage/0.3.1");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("manifest.kdl"),
        "agent {\n id \"triage\"\n name \"Invoice Triage\"\n version \"0.3.1\"\n entrypoint \"bin/run\"\n}\n",
    )
    .unwrap();
    let rig = Rig::with(
        dir,
        T0,
        Opts {
            packages: vec![(root, "local".into())],
            ..Opts::default()
        },
    );
    let r = rig.console().ok("list_packages", json!({}));
    assert_eq!(
        r["packages"],
        json!([{"id": "triage", "name": "Invoice Triage", "publisher": "local", "version": "0.3.1"}])
    );
}

#[test]
fn show_decisions_is_forwarded_rate_limited_and_the_count_is_relayed() {
    let dir = ec_agentd::scratch_dir("dec");
    let hs = dir.join("abyss.sock");
    let l = UnixListener::bind(&hs).unwrap();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                for line in BufReader::new(s).lines().map_while(Result::ok) {
                    let v: Value = serde_json::from_str(&line).unwrap();
                    let id = v["id"].clone();
                    match v["method"].as_str().unwrap() {
                        "subscribe" => {
                            writeln!(w, "{}", json!({"jsonrpc": "2.0", "id": id, "result": {}})).unwrap();
                            writeln!(
                                w,
                                "{}",
                                json!({"jsonrpc": "2.0", "method": "event",
                                       "params": {"event": "decisions_pending", "data": {"count": 2}}})
                            )
                            .unwrap();
                        }
                        "show_decisions" => {
                            writeln!(
                                w,
                                "{}",
                                json!({"jsonrpc": "2.0", "id": id, "result": {"opened": true}})
                            )
                            .unwrap();
                        }
                        _ => {}
                    }
                }
            });
        }
    });
    let rig = Rig::with(
        dir,
        T0,
        Opts {
            human: Some(hs),
            ..Opts::default()
        },
    );
    let mut c = rig.console();
    let t = Instant::now();
    while c.ok("list_tasks", json!({}))["decisions_pending"] != json!(2) {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(30));
    }
    let first = c.call("show_decisions", json!({}));
    assert_eq!(first["result"], json!({"opened": true}), "{first}");
    let second = c.call("show_decisions", json!({}));
    assert_eq!(second["error"]["code"], -32004);
}

// ---- session records and restore (A-08 §5.4, F-21, F-23) ---------------------

fn wait_task(c: &mut Cl, id: &str) {
    let t = Instant::now();
    loop {
        if c.call("get_task", json!({ "task_id": id }))
            .get("error")
            .is_none()
        {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "task never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_closed(c: &mut Cl, id: &str) {
    let t = Instant::now();
    loop {
        if c.ok("list_sessions", json!({}))["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["task_id"] == json!(id))
        {
            return;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "never closed");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// An installed package `ref` 1 for the resume tests; `extra` is manifest text.
fn ref_package(root: &Path, extra: &str) {
    let v = root.join("ref/1");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("manifest.kdl"),
        format!(
            "agent {{\n id \"ref\"\n name \"Ref\"\n version \"1\"\n entrypoint \"./run.sh\"\n{extra}\n}}\n"
        ),
    )
    .unwrap();
}

fn tool_names(c: &mut Cl) -> Vec<String> {
    c.ok("tools/list", json!({}))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

const BOUNDARY: &str = "The environment changed: every handle and revision from the restored session is invalid. Re-observe before acting.";

#[test]
fn a_resumed_task_restores_the_old_history_once_and_a_plain_task_cannot() {
    let dir = ec_agentd::scratch_dir("resume");
    let root = dir.join("agents");
    ref_package(&root, "");
    let mut rig = Rig::with(
        dir.clone(),
        T0,
        Opts {
            packages: vec![(root, "local".into())],
            ..Opts::default()
        },
    );
    let mut c = rig.console();
    let (old, new) = (tid(1), tid(2));
    rig.fake().send(&prov(&old, "agent:ref"));
    wait_task(&mut c, &old);
    let mut a = rig.mcp(&old);
    let (err, _) = a.tool("task.say", json!({"text": "first words"}));
    assert!(!err);
    c.ok(
        "conversation_post",
        json!({"task_id": old, "text": "human words"}),
    );

    // A task not created with `resumes` has no such tool, and a call errors.
    assert!(!tool_names(&mut a).iter().any(|n| n == "session.restore"));
    let e = a.call("tools/call", json!({"name": "session.restore", "arguments": {}}));
    assert_eq!(e["error"]["code"], -32602, "{e}");

    // The record is private on disk.
    use std::os::unix::fs::PermissionsExt;
    let rec = dir.join("state/sessions").join(&old);
    assert_eq!(
        std::fs::metadata(&rec).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(rec.join("record.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    rig.fake().send(&state(&old, "closed", "completed"));
    wait_closed(&mut c, &old);
    let sessions = c.ok("list_sessions", json!({}));
    assert_eq!(sessions["sessions"][0]["eligible"], true);

    rig.fake().send(&prov_resumes(&new, "agent:ref", &old));
    wait_task(&mut c, &new);
    let t = c.ok("get_task", json!({"task_id": new}));
    assert_eq!(t["resumes"], json!(old));
    assert_eq!(c.ok("get_task", json!({"task_id": old}))["resumes"], Value::Null);
    let mut b = rig.mcp(&new);
    assert!(tool_names(&mut b).iter().any(|n| n == "session.restore"));
    let (err, v) = b.tool("session.restore", json!({}));
    assert!(!err, "{v}");
    let entries = v["entries"].as_array().unwrap();
    // The boundary marker is last, and only it is a boundary.
    assert_eq!(
        entries.last().unwrap(),
        &json!({"kind": "boundary", "text": BOUNDARY})
    );
    assert_eq!(v["restored"], json!(entries.len() - 1));
    assert!(entries[..entries.len() - 1]
        .iter()
        .all(|e| e["kind"] != "boundary"));
    // The old tool call, its result and both conversation messages, in order.
    let pos = |f: &dyn Fn(&Value) -> bool| {
        entries
            .iter()
            .position(f)
            .unwrap_or_else(|| panic!("missing in {entries:?}"))
    };
    let call = pos(&|e| e["kind"] == "mcp_request" && e["params"]["arguments"]["text"] == "first words");
    let said = pos(&|e| e["kind"] == "message" && e["message"]["text"] == "first words");
    let reply = pos(&|e| e["kind"] == "mcp_response" && e["result"]["structuredContent"]["msg_id"] == 1);
    let human = pos(&|e| e["kind"] == "message" && e["message"]["text"] == "human words");
    assert!(call < said && said < reply && reply < human);
    // Once only.
    assert_eq!(
        b.tool("session.restore", json!({})),
        (true, json!({"error": "already_restored"}))
    );
    // The new conversation is the new task's own.
    let r = c.ok("conversation_read", json!({"task_id": new}));
    assert!(r["messages"].as_array().unwrap().is_empty());
}

#[test]
fn a_resumed_task_starts_at_the_old_trust() {
    let mut rig = Rig::new("resume-trust");
    let mut c = rig.console();
    let old = tid(1);
    rig.fake().send(&prov(&old, "agent:x"));
    wait_task(&mut c, &old);
    rig.fake().send(&state(&old, "closed", "completed"));
    wait_closed(&mut c, &old);
    let (dir, clock) = rig.stop();
    // Nothing feeds a chain into agentd yet, so make the old task untrusted
    // on disk, as a real chain would have left it.
    let meta = dir.join("state/conversations").join(&old).join("meta.json");
    let text = std::fs::read_to_string(&meta).unwrap();
    assert!(text.contains("\"min_trust\":\"standard\""));
    std::fs::write(
        &meta,
        text.replace("\"min_trust\":\"standard\"", "\"min_trust\":\"untrusted\""),
    )
    .unwrap();
    let mut rig = Rig::with(dir, clock.load(Ordering::Relaxed), Opts::default());
    let mut c = rig.console();
    let (new, ghost) = (tid(2), tid(3));
    rig.fake().send(&prov_resumes(&new, "agent:x", &old));
    rig.fake().send(&prov_resumes(&ghost, "agent:x", &tid(9)));
    wait_task(&mut c, &new);
    wait_task(&mut c, &ghost);
    assert_eq!(
        c.ok("get_task", json!({"task_id": new}))["min_trust"],
        "untrusted"
    );
    // A session agentd no longer knows cannot be shown clean either.
    assert_eq!(
        c.ok("get_task", json!({"task_id": ghost}))["min_trust"],
        "untrusted"
    );
    // And an agent post carries it.
    let mut m = rig.mcp(&new);
    assert!(!m.tool("task.say", json!({"text": "hi"})).0);
    let r = c.ok("conversation_read", json!({"task_id": new}));
    assert_eq!(r["messages"][0]["trust"]["min_trust"], "untrusted");
    // An unrelated fresh task is unaffected.
    let fresh = tid(4);
    rig.fake().send(&prov(&fresh, "agent:x"));
    wait_task(&mut c, &fresh);
    assert_eq!(
        c.ok("get_task", json!({"task_id": fresh}))["min_trust"],
        "standard"
    );
}

#[test]
fn delete_session_and_retention_remove_the_record() {
    let dir = ec_agentd::scratch_dir("rec-del");
    let root = dir.join("agents");
    ref_package(&root, "resumable #false");
    let mut rig = Rig::with(
        dir.clone(),
        T0,
        Opts {
            packages: vec![(root, "local".into())],
            ..Opts::default()
        },
    );
    let mut c = rig.console();
    let (a, b) = (tid(1), tid(2));
    for id in [&a, &b] {
        rig.fake().send(&prov(id, "agent:ref"));
        wait_task(&mut c, id);
        let mut m = rig.mcp(id);
        assert!(!m.tool("task.say", json!({"text": "x"})).0);
        rig.fake().send(&state(id, "closed", "completed"));
        wait_closed(&mut c, id);
    }
    let rec = |id: &str| dir.join("state/sessions").join(id);
    assert!(rec(&a).exists() && rec(&b).exists());
    // `resumable #false` is listed, not eligible.
    let l = c.ok("list_sessions", json!({}));
    assert!(l["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["eligible"] == false));
    c.ok("delete_session", json!({"task_id": a}));
    assert!(!rec(&a).exists() && !dir.join("state/conversations").join(&a).exists());
    assert!(rec(&b).exists());
    // Retention takes the other one with the transcript.
    let (d2, clock) = rig.stop();
    let day = 24 * 3600 * 1000;
    let rig2 = Rig::with(d2, clock.load(Ordering::Relaxed) + 31 * day, Opts::default());
    assert!(!rec(&b).exists());
    drop(rig2);
}

#[test]
fn a_session_without_a_record_is_not_eligible() {
    let dir = ec_agentd::scratch_dir("rec-elig");
    let root = dir.join("agents");
    ref_package(&root, "");
    let mut rig = Rig::with(
        dir,
        T0,
        Opts {
            packages: vec![(root, "local".into())],
            ..Opts::default()
        },
    );
    let mut c = rig.console();
    let (with, without) = (tid(1), tid(2));
    for id in [&with, &without] {
        rig.fake().send(&prov(id, "agent:ref"));
        wait_task(&mut c, id);
    }
    // Only this one talks over MCP, so only it has a record.
    assert!(!rig.mcp(&with).tool("task.say", json!({"text": "x"})).0);
    for id in [&with, &without] {
        rig.fake().send(&state(id, "closed", "completed"));
        wait_closed(&mut c, id);
    }
    let l = c.ok("list_sessions", json!({}));
    let el = |id: &str| {
        l["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["task_id"] == json!(id))
            .unwrap()["eligible"]
            .clone()
    };
    assert_eq!((el(&with), el(&without)), (json!(true), json!(false)));
}

#[cfg(feature = "dev-unsandboxed")]
mod launched {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn package(root: &Path, script: &str) {
        package_with(root, script, "");
    }

    fn package_with(root: &Path, script: &str, extra: &str) {
        let v = root.join("ref/1");
        std::fs::create_dir_all(&v).unwrap();
        std::fs::write(
            v.join("manifest.kdl"),
            format!("agent {{\n id \"ref\"\n name \"Ref\"\n version \"1\"\n entrypoint \"./run.sh\"\n{extra}\n}}\n"),
        )
        .unwrap();
        std::fs::write(v.join("run.sh"), script).unwrap();
        std::fs::set_permissions(v.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn wait_for(p: &Path) -> String {
        let t = Instant::now();
        loop {
            if let Ok(s) = std::fs::read_to_string(p) {
                if s.ends_with('\n') {
                    return s;
                }
            }
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "{} never appeared",
                p.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_agent_gets_its_socket_and_dies_with_the_task() {
        let dir = ec_agentd::scratch_dir("launch");
        let root = dir.join("agents");
        let out = dir.join("out.txt");
        package(
            &root,
            &format!(
                "#!/bin/sh\necho \"$$ $ECLIPSE_TASK_ID $ECLIPSE_MCP_SOCKET\" > {out}.tmp\nmv {out}.tmp {out}\nexec sleep 300\n",
                out = out.display()
            ),
        );
        let mut rig = Rig::with(
            dir,
            T0,
            Opts {
                launch: true,
                packages: vec![(root, "local".into())],
                ..Opts::default()
            },
        );
        let id = tid(1);
        rig.fake().send(&prov(&id, "agent:ref"));
        let line = wait_for(&out);
        let mut it = line.split_whitespace();
        let pid: u32 = it.next().unwrap().parse().unwrap();
        assert_eq!(it.next().unwrap(), id);
        let sock = PathBuf::from(it.next().unwrap());
        assert!(sock.exists(), "the MCP socket exists when the agent starts");
        assert!(Path::new(&format!("/proc/{pid}")).exists());
        rig.fake().send(&state(&id, "closed", "cancelled"));
        let t = Instant::now();
        while Path::new(&format!("/proc/{pid}")).exists() {
            assert!(t.elapsed() < Duration::from_secs(5), "the agent was not stopped");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn an_agent_that_exits_is_reported_to_policyd() {
        let dir = ec_agentd::scratch_dir("exit");
        let root = dir.join("agents");
        package(&root, "#!/bin/sh\nexit 0\n");
        let mut rig = Rig::with(
            dir,
            T0,
            Opts {
                launch: true,
                packages: vec![(root, "local".into())],
                ..Opts::default()
            },
        );
        let id = tid(1);
        rig.fake().send(&prov(&id, "agent:ref"));
        let ToPolicyd::Exited { task, reason, .. } = rig.fake().next_msg() else {
            panic!("expected exited")
        };
        assert_eq!((task, reason.as_str()), (id, "completed"));
    }

    #[test]
    fn a_missing_package_fails_the_task() {
        let mut rig = Rig::with(
            ec_agentd::scratch_dir("nopkg"),
            T0,
            Opts {
                launch: true,
                ..Opts::default()
            },
        );
        let id = tid(1);
        rig.fake().send(&prov(&id, "agent:ref"));
        let ToPolicyd::Exited { reason, .. } = rig.fake().next_msg() else {
            panic!("expected exited")
        };
        assert_eq!(reason, "failed");
    }

    /// The real reference agent (`ec-ref-agent`) behind agentd's MCP socket,
    /// driven from the console socket: the agent acknowledges the statement,
    /// answers a human post, asks back on a question, and exits when the
    /// task closes. Skipped when the binary has not been built alongside.
    #[test]
    fn the_reference_agent_talks_through_the_console() {
        let exe = std::env::current_exe().unwrap();
        let bin = exe
            .parent()
            .and_then(Path::parent)
            .map(|d| d.join("ec-ref-agent"));
        let Some(bin) = bin.filter(|b| b.exists()) else {
            eprintln!("ec-ref-agent is not built; skipped (cargo build -p ec-ref-agent)");
            return;
        };
        let dir = ec_agentd::scratch_dir("refagent");
        let root = dir.join("agents");
        package(&root, &format!("#!/bin/sh\nexec {}\n", bin.display()));
        let mut rig = Rig::with(
            dir,
            T0,
            Opts {
                launch: true,
                packages: vec![(root, "local".into())],
                ..Opts::default()
            },
        );
        let mut c = rig.console();
        let id = tid(1);
        rig.fake().send(&prov(&id, "agent:ref"));
        let read = |c: &mut Cl, want: usize| {
            let t = Instant::now();
            loop {
                let r = c.ok("conversation_read", json!({ "task_id": id }));
                let msgs = r["messages"].as_array().cloned().unwrap_or_default();
                if msgs.len() >= want {
                    return (msgs, r);
                }
                assert!(t.elapsed() < Duration::from_secs(10), "only {msgs:?}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let (msgs, _) = read(&mut c, 1);
        assert_eq!(msgs[0]["kind"], "say");
        assert!(msgs[0]["text"].as_str().unwrap().contains("Triage the invoices"));
        assert_eq!(msgs[0]["trust"]["min_trust"], "standard");

        c.ok(
            "conversation_post",
            json!({ "task_id": id, "text": "which folder?" }),
        );
        let (msgs, r) = read(&mut c, 4);
        assert_eq!(msgs[1]["kind"], "human");
        assert!(msgs[2..].iter().any(|m| m["kind"] == "ask"), "{msgs:?}");
        assert_eq!(r["awaiting_reply"], true);

        // policyd closed it, so agentd stops the agent and does not report
        // an exit back; the session moves to History.
        rig.fake().send(&state(&id, "closed", "cancelled"));
        let t = Instant::now();
        loop {
            let r = c.ok("list_sessions", json!({}));
            if r["sessions"]
                .as_array()
                .is_some_and(|s| s.iter().any(|x| x["task_id"] == json!(id)))
            {
                break;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "never closed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A package asking for `net.egress` cannot be sandboxed without the S-09
    /// proxy (M20): the launch is refused and the task ends `Exited{failed}`.
    /// A forbidden `fs.read` is refused the same way, a plain one launches.
    #[test]
    fn unmet_sandbox_requests_refuse_the_launch() {
        for (extra, ok) in [
            ("sandbox {\n net.egress \"api.acme.com:443\"\n}", false),
            ("sandbox {\n fs.read \"~/.ssh\"\n}", false),
            ("sandbox {\n fs.write \"/etc\"\n}", false),
            ("sandbox {\n fs.exec \"/x\"\n}", false),
            ("sandbox {\n fs.write \"/tmp/out\"\n}", true),
        ] {
            let dir = ec_agentd::scratch_dir("sbx-refuse");
            let root = dir.join("agents");
            package_with(&root, "#!/bin/sh\nexit 0\n", extra);
            let mut rig = Rig::with(
                dir,
                T0,
                Opts {
                    launch: true,
                    packages: vec![(root, "local".into())],
                    ..Opts::default()
                },
            );
            rig.fake().send(&prov(&tid(1), "agent:ref"));
            let ToPolicyd::Exited { reason, .. } = rig.fake().next_msg() else {
                panic!("expected exited")
            };
            // The refusals say `failed` at once; the allowed one runs and exits 0.
            assert_eq!(reason, if ok { "completed" } else { "failed" }, "{extra}");
        }
    }

    /// The real reference agent resumes a session: the second task's agent
    /// restores the first one's record and says how many entries came back.
    #[test]
    fn the_reference_agent_resumes_a_session() {
        let exe = std::env::current_exe().unwrap();
        let bin = exe
            .parent()
            .and_then(Path::parent)
            .map(|d| d.join("ec-ref-agent"));
        let Some(bin) = bin.filter(|b| b.exists()) else {
            eprintln!("ec-ref-agent is not built; skipped (cargo build -p ec-ref-agent)");
            return;
        };
        let dir = ec_agentd::scratch_dir("refresume");
        let root = dir.join("agents");
        package(&root, &format!("#!/bin/sh\nexec {}\n", bin.display()));
        let mut rig = Rig::with(
            dir,
            T0,
            Opts {
                launch: true,
                packages: vec![(root, "local".into())],
                ..Opts::default()
            },
        );
        let mut c = rig.console();
        let (old, new) = (tid(1), tid(2));
        let first_say = |c: &mut Cl, id: &str| {
            wait_task(c, id);
            let t = Instant::now();
            loop {
                let r = c.ok("conversation_read", json!({ "task_id": id }));
                if let Some(m) = r["messages"].as_array().and_then(|m| m.first()).cloned() {
                    return m;
                }
                assert!(t.elapsed() < Duration::from_secs(10), "no message from {id}");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        rig.fake().send(&prov(&old, "agent:ref"));
        let m = first_say(&mut c, &old);
        // A fresh task restores nothing.
        assert!(!m["text"].as_str().unwrap().contains("Restored"), "{m}");
        rig.fake().send(&state(&old, "closed", "completed"));
        wait_closed(&mut c, &old);

        rig.fake().send(&prov_resumes(&new, "agent:ref", &old));
        let m = first_say(&mut c, &new);
        let text = m["text"].as_str().unwrap();
        let n: usize = text
            .split("Restored ")
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no restore count in {text:?}"));
        // initialize, tools/list, the ack and the inbox polls of the first run.
        assert!(n >= 6, "{text}");
        assert!(text.contains("Triage the invoices"));
        assert_eq!(c.ok("get_task", json!({"task_id": new}))["resumes"], json!(old));
        rig.fake().send(&state(&new, "closed", "cancelled"));
    }
}

// ---- inference.complete (ADR 0076) ------------------------------------------

/// A fake `ec-inferenced`: a listener at the path agentd dials.
struct FakeRouter {
    listener: UnixListener,
}

struct RouterConn {
    s: UnixStream,
}

impl FakeRouter {
    fn start(rig: &Rig) -> FakeRouter {
        let listener = UnixListener::bind(rig.dir.join("run/inferenced.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        FakeRouter { listener }
    }

    fn accept(&self) -> RouterConn {
        let t = Instant::now();
        loop {
            match self.listener.accept() {
                Ok((s, _)) => {
                    s.set_nonblocking(false).unwrap();
                    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    return RouterConn { s };
                }
                Err(_) if t.elapsed() < Duration::from_secs(5) => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => panic!("agentd never dialled the router: {e}"),
            }
        }
    }
}

impl RouterConn {
    fn recv(&mut self) -> ec_inference_wire::ToRouter {
        let v = ec_inference_wire::read_frame(&mut self.s)
            .expect("router read")
            .expect("agentd closed the router connection");
        ec_inference_wire::ToRouter::from_json(&v).expect("a valid frame")
    }

    fn complete(&mut self) -> ec_inference_wire::Request {
        match self.recv() {
            ec_inference_wire::ToRouter::Complete(r) => r,
            other => panic!("expected a complete, got {other:?}"),
        }
    }

    fn reply(&mut self, r: &ec_inference_wire::Reply) {
        ec_inference_wire::write_frame(&mut self.s, &r.to_json()).unwrap();
    }

    fn ok(&mut self, id: u64, text: &str) {
        self.reply(&ec_inference_wire::Reply {
            id,
            result: Ok(ec_inference_wire::Completion {
                content: json!([{"type": "text", "text": text}]),
                stop_reason: "end_turn".into(),
                model: "claude-test-1".into(),
                input_tokens: 3,
                output_tokens: 1,
                cost_usd: None,
            }),
        });
    }
}

const INF_BLOCK: &str = " inference {\n  backend \"api\"\n  model \"claude-test-1\"\n }";

/// Packages `ref` (with an inference block) and `plain` (without).
fn inference_rig(tag: &str) -> Rig {
    let dir = ec_agentd::scratch_dir(tag);
    let root = dir.join("agents");
    ref_package(&root, INF_BLOCK);
    let v = root.join("plain/1");
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        v.join("manifest.kdl"),
        "agent {\n id \"plain\"\n version \"1\"\n entrypoint \"./run.sh\"\n}\n",
    )
    .unwrap();
    Rig::with(
        dir,
        T0,
        Opts {
            packages: vec![(root, "local".into())],
            ..Opts::default()
        },
    )
}

fn start_task(rig: &mut Rig, n: u64, package: &str) -> (String, Cl) {
    let id = tid(n);
    let mut p = prov(&id, &format!("agent:{package}"));
    if let FromPolicyd::Provision { package: pk, .. } = &mut p {
        *pk = package.to_owned();
    }
    rig.fake().send(&p);
    wait_task(&mut rig.console(), &id);
    let m = rig.mcp(&id);
    (id, m)
}

fn msgs() -> Value {
    json!([{"role": "user", "content": "hi"}])
}

/// Sends a `tools/call inference.complete` without waiting for the answer.
fn send_inf(c: &mut Cl, id: u64, args: Value) {
    c.raw(
        &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "inference.complete", "arguments": args}})
        .to_string(),
    );
}

fn read_resp(c: &mut Cl, id: u64) -> Value {
    loop {
        let v = c.read().expect("connection closed");
        if v.get("id") == Some(&json!(id)) {
            return v;
        }
    }
}

/// `(isError, parsed text)` of a tool-call response.
fn inf_result(v: &Value) -> (bool, Value) {
    let r = &v["result"];
    let text = r["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{v}"));
    (
        r["isError"].as_bool().unwrap(),
        serde_json::from_str(text).unwrap(),
    )
}

#[test]
fn the_inference_tool_is_listed_only_for_a_package_that_declares_it() {
    let mut rig = inference_rig("inf-list");
    let (_, mut with) = start_task(&mut rig, 1, "ref");
    let (_, mut without) = start_task(&mut rig, 2, "plain");
    let tools = with.ok("tools/list", json!({}))["tools"].clone();
    let t = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "inference.complete")
        .expect("listed");
    assert_eq!(t["inputSchema"]["required"], json!(["messages"]));
    assert!(t["inputSchema"]["properties"].get("model").is_none());
    assert!(!tool_names(&mut without).iter().any(|n| n == "inference.complete"));
    // A task without the declaration gets the unknown-tool error, exactly as
    // for a name that never existed.
    let a = without.call(
        "tools/call",
        json!({"name": "inference.complete", "arguments": {"messages": msgs()}}),
    );
    let b = without.call("tools/call", json!({"name": "no.such.tool", "arguments": {}}));
    assert_eq!(a["error"], b["error"]);
    assert_eq!(a["error"]["code"], -32602);
}

#[test]
fn a_call_goes_to_the_router_with_the_manifests_backend_and_model() {
    let mut rig = inference_rig("inf-ok");
    let router = FakeRouter::start(&rig);
    let (id, mut m) = start_task(&mut rig, 1, "ref");

    // Bad arguments never reach the router, and neither does an attempt to
    // choose the model or the backend.
    for bad in [
        json!({}),
        json!({"messages": []}),
        json!({"messages": msgs(), "tools": "x"}),
        json!({"messages": msgs(), "system": 1}),
        json!({"messages": msgs(), "max_tokens": 0}),
        json!({"messages": msgs(), "max_tokens": 64001}),
        json!({"messages": msgs(), "model": "claude-opus-9"}),
        json!({"messages": msgs(), "backend": "claude-code"}),
    ] {
        send_inf(&mut m, 100, bad.clone());
        let v = read_resp(&mut m, 100);
        assert_eq!(v["error"]["code"], -32602, "{bad}: {v}");
    }

    send_inf(
        &mut m,
        101,
        json!({"system": "be brief", "messages": msgs(), "tools": [{"name": "t", "input_schema": {}}], "max_tokens": 777}),
    );
    let mut rc = router.accept();
    let r = rc.complete();
    assert_eq!(r.task, id);
    assert_eq!(r.package, "ref");
    assert_eq!(r.backend, ec_inference_wire::Backend::Api);
    assert_eq!(r.model, "claude-test-1");
    assert_eq!(r.system, "be brief");
    assert_eq!(r.max_tokens, 777);
    assert_eq!(r.messages, msgs());
    rc.ok(r.id, "hello");
    let (err, body) = inf_result(&read_resp(&mut m, 101));
    assert!(!err);
    assert_eq!(body["content"][0]["text"], "hello");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["model"], "claude-test-1");

    // Defaults, a fresh request id, and the same connection.
    send_inf(&mut m, 102, json!({"messages": msgs()}));
    let r2 = rc.complete();
    assert_ne!(r2.id, r.id);
    assert_eq!(r2.max_tokens, 16000);
    assert_eq!(r2.system, "");
    assert_eq!(r2.tools, json!([]));
    rc.reply(&ec_inference_wire::Reply {
        id: r2.id,
        result: Err(ec_inference_wire::Failure {
            kind: "broker_locked".into(),
            message: "unlock with ec-secret unlock".into(),
        }),
    });
    let (err, body) = inf_result(&read_resp(&mut m, 102));
    assert!(err);
    assert_eq!(
        body,
        json!({"kind": "broker_locked", "message": "unlock with ec-secret unlock"})
    );

    // The call and both answers are in the session record, like any MCP traffic.
    let rec = std::fs::read_to_string(rig.dir.join("state/sessions").join(&id).join("record.jsonl")).unwrap();
    let lines: Vec<Value> = rec.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(lines
        .iter()
        .any(|e| e["kind"] == "mcp_request" && e["params"]["name"] == "inference.complete"));
    assert!(lines.iter().any(|e| e["kind"] == "mcp_response"
        && e["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("hello"))));
    assert!(lines
        .iter()
        .any(|e| e["kind"] == "mcp_response" && e["result"]["isError"] == true));
}

#[test]
fn one_inference_call_at_a_time_per_task() {
    let mut rig = inference_rig("inf-single");
    let router = FakeRouter::start(&rig);
    let (_, mut a) = start_task(&mut rig, 1, "ref");
    let (_, mut b) = start_task(&mut rig, 2, "ref");
    send_inf(&mut a, 1, json!({"messages": msgs()}));
    let mut rc = router.accept();
    let first = rc.complete();

    // A second call on the same task is refused at once; the first is not
    // disturbed. Another task is not affected.
    send_inf(&mut a, 2, json!({"messages": msgs()}));
    let (err, body) = inf_result(&read_resp(&mut a, 2));
    assert!(err);
    assert_eq!(body["kind"], "rate_limited");
    assert_eq!(body["message"], "one inference call at a time per task");
    send_inf(&mut b, 1, json!({"messages": msgs()}));
    let other = rc.complete();
    assert_ne!(other.task, first.task);

    rc.ok(first.id, "one");
    assert!(!inf_result(&read_resp(&mut a, 1)).0);
    rc.ok(other.id, "other");
    assert!(!inf_result(&read_resp(&mut b, 1)).0);

    // Once answered the task may call again.
    send_inf(&mut a, 3, json!({"messages": msgs()}));
    let again = rc.complete();
    rc.ok(again.id, "two");
    assert!(!inf_result(&read_resp(&mut a, 3)).0);
}

#[test]
fn a_closing_task_drops_its_call_and_the_router_is_told() {
    let mut rig = inference_rig("inf-close");
    let router = FakeRouter::start(&rig);
    let (id, mut m) = start_task(&mut rig, 1, "ref");
    send_inf(&mut m, 1, json!({"messages": msgs()}));
    let mut rc = router.accept();
    let r = rc.complete();
    // The router stays silent; the task closes.
    rig.fake().send(&state(&id, "closed", "cancelled"));
    match rc.recv() {
        ec_inference_wire::ToRouter::Close { task } => assert_eq!(task, id),
        other => panic!("expected close, got {other:?}"),
    }
    // A late answer for the dropped call is ignored, and agentd carries on.
    rc.ok(r.id, "too late");
    let (id2, mut m2) = start_task(&mut rig, 2, "ref");
    send_inf(&mut m2, 1, json!({"messages": msgs()}));
    let r2 = rc.complete();
    assert_eq!(r2.task, id2);
    rc.ok(r2.id, "fine");
    assert!(!inf_result(&read_resp(&mut m2, 1)).0);
}

#[test]
fn an_absent_router_fails_fast_and_a_returning_one_is_redialled() {
    let mut rig = inference_rig("inf-down");
    let (_, mut m) = start_task(&mut rig, 1, "ref");
    let t = Instant::now();
    send_inf(&mut m, 1, json!({"messages": msgs()}));
    let (err, body) = inf_result(&read_resp(&mut m, 1));
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(err);
    assert_eq!(body["kind"], "backend_unavailable");
    assert_eq!(
        body["message"],
        "the inference router (ec-inferenced) is not running"
    );

    // The router appears: the next call dials it.
    let router = FakeRouter::start(&rig);
    send_inf(&mut m, 2, json!({"messages": msgs()}));
    let mut rc = router.accept();
    let _ = rc.complete();
    // It dies with the call in flight: the call fails, it does not hang.
    drop(rc);
    let (err, body) = inf_result(&read_resp(&mut m, 2));
    assert!(err);
    assert_eq!(body["kind"], "backend_unavailable");

    // And comes back again.
    send_inf(&mut m, 3, json!({"messages": msgs()}));
    let mut rc = router.accept();
    let r = rc.complete();
    rc.ok(r.id, "back");
    assert!(!inf_result(&read_resp(&mut m, 3)).0);
}

#[test]
fn a_long_inference_call_fits_on_a_task_socket_but_not_the_console() {
    let mut rig = inference_rig("inf-long");
    let router = FakeRouter::start(&rig);
    let (_, mut m) = start_task(&mut rig, 1, "ref");
    let big = "x".repeat(1024 * 1024);
    send_inf(&mut m, 1, json!({"messages": [{"role": "user", "content": big}]}));
    let mut rc = router.accept();
    let r = rc.complete();
    assert_eq!(r.messages[0]["content"].as_str().unwrap().len(), 1024 * 1024);
    rc.ok(r.id, "ok");
    assert!(!inf_result(&read_resp(&mut m, 1)).0);

    // The console keeps the 64 KiB cap: an over-long line ends the connection.
    let mut c = rig.console();
    let pad = "y".repeat(100 * 1024);
    c.raw(&json!({"jsonrpc": "2.0", "id": 1, "method": "list_tasks", "params": {"pad": pad}}).to_string());
    assert!(c.read().is_none());
}
