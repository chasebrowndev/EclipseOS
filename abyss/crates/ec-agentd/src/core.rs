// SPDX-License-Identifier: AGPL-3.0-only
//! The agentd core: one thread owns every task, conversation and connection.
//!
//! Socket threads (`net.rs`), the `policyd` link (`link.rs`), the human-socket
//! relay (`human.rs`) and agent reapers (`launcher.rs`) only move messages;
//! they reach the core by channel as [`Msg`] and never share its state, so
//! there are no locks here.
//!
//! Authority: this process holds none. It cannot create a task (policyd
//! provisions them, A-08 §5.2); the only `ToPolicyd` messages it builds are
//! pause, cancel and exited.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use ec_policy_eval::audit::{self, Emission, Kind as AuditKind};
use ec_policy_eval::cbor::{enc, MapBuilder};
use ec_policy_eval::link::{CancelMode, FromPolicyd, ToPolicyd};
use serde_json::{json, Value};

use crate::launcher::{self, Running};
use crate::rpc::{self, p_str, p_u64, Parsed, Request, RpcError};
use crate::sanitize::{self, MAX_TEXT};
use crate::session::{self, Sessions};
use crate::store::{Kind, Message, Store, Task};
use crate::{net, packages, router, sandbox, ulid_from_text, Config};
use ec_inference_wire::{Completion, Failure, Request as InferRequest, ToRouter};

/// A-03 §7 defaults, counted on the task.
pub const MSGS_PER_MIN: usize = 60;
pub const BYTES_PER_MIN: u64 = 1024 * 1024;
const WINDOW_MS: u64 = 60_000;
/// How long a pause or cancel waits for policyd before the console is told.
const PENDING_MS: u64 = 5_000;
const SWEEP_EVERY_MS: u64 = 3_600_000;
const AUDIT_QUEUE: usize = 4096;
/// The MCP tool a resumed task alone has (F-23).
pub const RESTORE_TOOL: &str = "session.restore";
/// The MCP tool a task whose package declares `inference { }` alone has
/// (ADR 0076).
pub const INFERENCE_TOOL: &str = "inference.complete";

#[derive(Debug, Clone, PartialEq)]
pub enum Surface {
    Console,
    /// An agent's MCP socket; the task is fixed by the socket, never named by
    /// the caller.
    Mcp(String),
}

pub enum Msg {
    Open {
        conn: u64,
        surface: Surface,
        tx: Sender<String>,
        stream: UnixStream,
    },
    Line {
        conn: u64,
        line: String,
    },
    Closed {
        conn: u64,
    },
    LinkUp(Sender<Vec<u8>>),
    LinkDown,
    Link(FromPolicyd),
    Decisions(u64),
    ProcExit {
        task: String,
        status: String,
    },
    HumanReply {
        conn: u64,
        id: Option<Value>,
        result: Result<Value, RpcError>,
    },
    /// The router's answer to one `inference.complete`, or its failure.
    Inference {
        id: u64,
        result: Result<Completion, Failure>,
    },
    Tick,
    Shutdown,
}

/// An `inference.complete` waiting on the router: where its answer goes.
struct InferCall {
    task: String,
    conn: u64,
    mcp_id: Value,
}

struct Conn {
    surface: Surface,
    tx: Sender<String>,
    stream: UnixStream,
    subscribed: bool,
}

enum Action {
    Pause,
    Cancel(CancelMode),
}

struct Pending {
    conn: u64,
    id: Option<Value>,
    deadline: u64,
    task: String,
    action: Action,
}

enum Outcome {
    Now(Result<Value, RpcError>),
    Deferred,
}

struct Prov {
    task: String,
    principal: String,
    package: String,
    version: String,
    statement: String,
    deadline_ms: u64,
    continuation: String,
    resumes: String,
}

pub struct Core {
    cfg: Config,
    tx: Sender<Msg>,
    ids: Arc<AtomicU64>,
    store: Store,
    sessions: Sessions,
    tasks: BTreeMap<String, Task>,
    conns: HashMap<u64, Conn>,
    link: Option<Sender<Vec<u8>>>,
    audit_q: VecDeque<Vec<u8>>,
    pending: HashMap<u64, Pending>,
    next_req: u64,
    decisions: u64,
    last_show_ms: Option<u64>,
    procs: HashMap<String, Running>,
    mcp_stop: HashMap<String, Arc<AtomicBool>>,
    last_sweep_ms: u64,
    /// To the inference router satellite (`router.rs`).
    router: Sender<ToRouter>,
    next_infer: u64,
    infer_pending: HashMap<u64, InferCall>,
    /// Task -> its one in-flight call.
    infer_by_task: HashMap<String, u64>,
    /// Tasks found open on disk at startup: policyd is told `Exited` once the
    /// link is up.
    unreported: Vec<String>,
}

fn log(m: &str) {
    eprintln!("ec-agentd: {m}");
}

impl Core {
    pub fn new(cfg: Config, tx: Sender<Msg>, ids: Arc<AtomicU64>) -> Core {
        let store = Store::new(&cfg.state_dir);
        let state_dir = cfg.state_dir.clone();
        let runtime_dir = cfg.runtime_dir.clone();
        let tx_core = tx.clone();
        let now = (cfg.clock)();
        let mut tasks = BTreeMap::new();
        let mut unreported = Vec::new();
        for mut t in store.load_all() {
            // A task still open on disk lost its agent with the last agentd.
            if !t.is_closed() {
                t.state = "closed".into();
                t.reason = "agentd_restart".into();
                t.closed_ms = Some(now);
                t.awaiting_reply = false;
                unreported.push(t.id.clone());
                if let Err(e) = store.write_meta(&t) {
                    log(&format!("cannot persist task {}: {e}", t.id));
                }
            }
            tasks.insert(t.id.clone(), t);
        }
        let mut c = Core {
            cfg,
            tx,
            ids,
            store,
            sessions: Sessions::new(&state_dir),
            tasks,
            conns: HashMap::new(),
            link: None,
            audit_q: VecDeque::new(),
            pending: HashMap::new(),
            next_req: 1,
            decisions: 0,
            last_show_ms: None,
            procs: HashMap::new(),
            mcp_stop: HashMap::new(),
            last_sweep_ms: now,
            router: router::spawn(runtime_dir.join("inferenced.sock"), tx_core),
            next_infer: 1,
            infer_pending: HashMap::new(),
            infer_by_task: HashMap::new(),
            unreported,
        };
        c.sweep();
        c
    }

    fn now(&self) -> u64 {
        (self.cfg.clock)()
    }

    pub fn run(mut self, rx: Receiver<Msg>) {
        for m in rx {
            if !self.handle(m) {
                break;
            }
        }
        self.stop_everything();
    }

    /// Returns false on shutdown.
    pub fn handle(&mut self, m: Msg) -> bool {
        match m {
            Msg::Open {
                conn,
                surface,
                tx,
                stream,
            } => {
                if let Surface::Mcp(t) = &surface {
                    if self.tasks.get(t).is_none_or(Task::is_closed) {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return true;
                    }
                }
                self.conns.insert(
                    conn,
                    Conn {
                        surface,
                        tx,
                        stream,
                        subscribed: false,
                    },
                );
            }
            Msg::Line { conn, line } => self.on_line(conn, &line),
            Msg::Closed { conn } => {
                self.conns.remove(&conn);
                self.pending.retain(|_, p| p.conn != conn);
                // Its answer would have nowhere to go; the router's reply, if
                // it comes, finds no entry.
                self.infer_pending.retain(|_, c| c.conn != conn);
                self.infer_by_task
                    .retain(|_, id| self.infer_pending.contains_key(id));
            }
            Msg::LinkUp(tx) => {
                self.link = Some(tx);
                let q: Vec<Vec<u8>> = self.audit_q.drain(..).collect();
                for b in q {
                    self.send_audit_bytes(b);
                }
                // policyd still counts leftover tasks live; tell it they are not.
                while let Some(task) = self.unreported.pop() {
                    let req = self.req_id();
                    if !self.send_policyd(&ToPolicyd::Exited {
                        req,
                        task: task.clone(),
                        reason: "failed".into(),
                    }) {
                        self.unreported.push(task);
                        break;
                    }
                }
            }
            Msg::LinkDown => {
                self.link = None;
                let reqs: Vec<u64> = self.pending.keys().copied().collect();
                for r in reqs {
                    self.pending_reply(r, Err(RpcError::unavailable()));
                }
            }
            Msg::Link(l) => self.on_link(l),
            Msg::Decisions(n) => {
                if n != self.decisions {
                    self.decisions = n;
                    self.event("decisions_pending", json!({"count": n}));
                }
            }
            Msg::ProcExit { task, status } => {
                self.procs.remove(&task);
                if self.tasks.get(&task).is_some_and(|t| !t.is_closed()) {
                    let req = self.req_id();
                    self.send_policyd(&ToPolicyd::Exited {
                        req,
                        task,
                        reason: status,
                    });
                }
            }
            Msg::HumanReply { conn, id, result } => {
                if let Some(id) = id {
                    self.send(
                        conn,
                        match result {
                            Ok(v) => rpc::ok(&id, v),
                            Err(e) => rpc::err(&id, &e),
                        },
                    );
                }
            }
            Msg::Inference { id, result } => self.on_inference(id, &result),
            Msg::Tick => self.tick(),
            Msg::Shutdown => return false,
        }
        true
    }

    // ---- plumbing -------------------------------------------------------

    fn req_id(&mut self) -> u64 {
        let r = self.next_req;
        self.next_req += 1;
        r
    }

    fn send(&self, conn: u64, line: String) {
        if let Some(c) = self.conns.get(&conn) {
            let _ = c.tx.send(line);
        }
    }

    /// One event to every subscribed console connection, in call order.
    fn event(&self, name: &str, data: Value) {
        let line =
            json!({"jsonrpc": "2.0", "method": "event", "params": {"event": name, "data": data}}).to_string();
        for c in self.conns.values() {
            if c.surface == Surface::Console && c.subscribed {
                let _ = c.tx.send(line.clone());
            }
        }
    }

    fn send_policyd(&mut self, m: &ToPolicyd) -> bool {
        match &self.link {
            Some(l) if l.send(m.encode()).is_ok() => true,
            Some(_) => {
                self.link = None;
                false
            }
            None => false,
        }
    }

    fn send_audit_bytes(&mut self, b: Vec<u8>) {
        if let Some(l) = &self.link {
            if l.send(b.clone()).is_ok() {
                return;
            }
            self.link = None;
        }
        if self.audit_q.len() >= AUDIT_QUEUE {
            self.audit_q.pop_front();
        }
        self.audit_q.push_back(b);
    }

    /// A `channel` record (A-08 §11): id, size and chain hash, never the text.
    fn channel_audit(&mut self, task: &str, principal: &str, msg: Option<&Message>, size: usize, op: &str) {
        let (msg_id, hash) = match msg {
            Some(m) => (
                m.msg_id,
                blake3::hash(format!("{}\0{}", m.min_trust, m.head).as_bytes()),
            ),
            None => (0, blake3::hash(b"")),
        };
        let mut b = MapBuilder::new();
        b.insert("chain_hash", enc(|w| w.bytes(hash.as_bytes())));
        b.insert("msg_id", enc(|w| w.u64(msg_id)));
        b.insert("op", enc(|w| w.text(op)));
        b.insert("size", enc(|w| w.u64(size as u64)));
        let principal = if audit::principal_ok(principal) {
            principal.to_owned()
        } else {
            "human".to_owned()
        };
        let e = Emission {
            kind: AuditKind::Channel,
            principal,
            grant_id: None,
            task_id: ulid_from_text(task),
            chain_id: None,
            req_id: None,
            serial: None,
            body: b.finish(),
        };
        self.send_audit_bytes(e.encode());
    }

    fn persist(&self, id: &str) {
        if let Some(t) = self.tasks.get(id) {
            if let Err(e) = self.store.write_meta(t) {
                log(&format!("cannot persist task {id}: {e}"));
            }
        }
    }

    // ---- policyd --------------------------------------------------------

    fn on_link(&mut self, l: FromPolicyd) {
        match l {
            FromPolicyd::Provision {
                task,
                principal,
                package,
                version,
                statement,
                deadline_ms,
                continuation,
                resumes,
                ..
            } => self.provision(Prov {
                task,
                principal,
                package,
                version,
                statement,
                deadline_ms,
                continuation,
                resumes,
            }),
            FromPolicyd::TaskState { task, state, reason } => self.task_state(&task, &state, &reason),
            FromPolicyd::Revoked { principal } => {
                let ids: Vec<String> = self
                    .tasks
                    .values()
                    .filter(|t| t.principal == principal && !t.is_closed())
                    .map(|t| t.id.clone())
                    .collect();
                for id in ids {
                    self.task_state(&id, "closed", "revoked");
                }
            }
            FromPolicyd::Done { req } => {
                if let Some(p) = self.pending.get(&req) {
                    if let Action::Cancel(CancelMode::Drain) = p.action {
                        let id = p.task.clone();
                        if let Some(t) = self.tasks.get_mut(&id) {
                            t.drain_requested = true;
                        }
                        self.persist(&id);
                    }
                }
                self.pending_reply(req, Ok(json!({"ok": true})));
            }
            FromPolicyd::Refused { req, reason } => {
                self.pending_reply(req, Err(RpcError::new(rpc::DENIED, &reason, "policyd refused")));
            }
            _ => {}
        }
    }

    fn pending_reply(&mut self, req: u64, r: Result<Value, RpcError>) {
        let Some(p) = self.pending.remove(&req) else {
            return;
        };
        if let Some(id) = p.id {
            self.send(
                p.conn,
                match r {
                    Ok(v) => rpc::ok(&id, v),
                    Err(e) => rpc::err(&id, &e),
                },
            );
        }
    }

    fn provision(&mut self, p: Prov) {
        if self.tasks.contains_key(&p.task) {
            return;
        }
        if ulid_from_text(&p.task).is_none()
            || !packages::safe_component(&p.package)
            || !packages::safe_component(&p.version)
            || (!p.resumes.is_empty() && ulid_from_text(&p.resumes).is_none())
        {
            log("refusing to provision a task with a malformed id");
            let req = self.req_id();
            self.send_policyd(&ToPolicyd::Exited {
                req,
                task: p.task,
                reason: "failed".into(),
            });
            return;
        }
        let now = self.now();
        let mut t = Task::new(&p.task, now);
        t.principal = if audit::principal_ok(&p.principal) {
            p.principal
        } else {
            format!("agent:{}", p.package)
        };
        t.package = p.package;
        t.version = p.version;
        t.statement = sanitize::clean(&p.statement);
        t.deadline_ms = p.deadline_ms;
        t.continuation = p.continuation;
        // An unreadable or invalid block gives no tool: fail closed.
        if let Some(dir) = packages::find_dir(&self.cfg.package_roots, &t.package, &t.version) {
            match packages::inference(&dir) {
                Ok(i) => t.inference = i,
                Err(e) => log(&format!("task {}: inference block ignored: {e}", t.id)),
            }
        }
        if !p.resumes.is_empty() {
            // A resume carries the old chain's trust (A-08 §5.4): it starts
            // where the old task ended, never cleaner. An old task agentd no
            // longer knows is treated as untrusted (the ratchet only tightens).
            t.min_trust = match self.tasks.get(&p.resumes) {
                Some(old) => old.min_trust.clone(),
                None => "untrusted".to_owned(),
            };
            t.resumes = p.resumes;
        }
        if let Err(e) = self.store.write_meta(&t) {
            log(&format!("cannot persist task {}: {e}", t.id));
            let req = self.req_id();
            self.send_policyd(&ToPolicyd::Exited {
                req,
                task: t.id,
                reason: "failed".into(),
            });
            return;
        }
        let id = t.id.clone();
        let mut started = json!({"task_id": id, "package": t.package, "statement": t.statement, "deadline_ms": t.deadline_ms});
        if !t.resumes.is_empty() {
            started["resumes"] = json!(t.resumes);
        }
        self.event("task_started", started);
        self.tasks.insert(id.clone(), t);
        if let Err(e) = self.launch(&id) {
            log(&format!("task {id}: agent not started: {e}"));
            let req = self.req_id();
            self.send_policyd(&ToPolicyd::Exited {
                req,
                task: id,
                reason: "failed".into(),
            });
        }
    }

    /// Creates the task's MCP socket, then starts its agent.
    fn launch(&mut self, id: &str) -> Result<(), String> {
        let dir = self.cfg.runtime_dir.join("agents").join(id);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| e.to_string())?;
        let sock = dir.join("mcp.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        net::serve(
            listener,
            Surface::Mcp(id.to_owned()),
            net::mcp_policy(),
            self.tx.clone(),
            stop.clone(),
            self.ids.clone(),
        );
        self.mcp_stop.insert(id.to_owned(), stop);
        if !self.cfg.launch {
            return Ok(());
        }
        let t = self.tasks.get(id).ok_or("task vanished")?;
        let pkg_dir = packages::find_dir(&self.cfg.package_roots, &t.package, &t.version)
            .ok_or("package is not installed")?;
        // TODO(ec-manifest): use the shared parser.
        let entry = packages::entrypoint(&pkg_dir)?;
        // Fail closed before anything starts (A-01 §6: no sandbox without its
        // filters): an unreadable or unmet `sandbox { }` request, a
        // `net.egress` (no egress proxy until M20), or a kernel without
        // Landlock refuses the launch; the task then ends `Exited{failed}`.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let decl = packages::sandbox_decl(&pkg_dir)?.resolve(home.as_deref())?;
        sandbox::preflight()?;
        let init_exe =
            std::env::current_exe().map_err(|e| format!("cannot find agentd's own binary: {e}"))?;
        let run = launcher::start(
            &launcher::Spec {
                task: id,
                package: &t.package,
                pkg_dir: &pkg_dir,
                entrypoint: &entry,
                mcp_sock: &sock,
                decl: &decl,
                init_exe: &init_exe,
            },
            self.tx.clone(),
        )
        .map_err(|e| e.to_string())?;
        self.procs.insert(id.to_owned(), run);
        Ok(())
    }

    fn task_state(&mut self, id: &str, state: &str, reason: &str) {
        if !matches!(state, "active" | "paused" | "draining" | "closed") {
            return;
        }
        let now = self.now();
        let Some(t) = self.tasks.get_mut(id) else {
            return;
        };
        if t.is_closed() || (t.state == state && t.reason == reason) {
            return;
        }
        t.state = state.to_owned();
        t.reason = reason.to_owned();
        let was_awaiting = t.awaiting_reply;
        let closing = state == "closed";
        if closing {
            t.closed_ms = Some(now);
            t.awaiting_reply = false;
        }
        self.persist(id);
        self.event(
            "task_state",
            json!({"task_id": id, "state": state, "reason": reason}),
        );
        if closing {
            if was_awaiting {
                self.event("awaiting_reply", json!({"task_id": id, "awaiting": false}));
            }
            self.finish_close(id, reason);
        }
    }

    /// The task is closed: stop its agent and socket, then say so.
    fn finish_close(&mut self, id: &str, reason: &str) {
        if let Some(r) = self.procs.remove(id) {
            launcher::stop(&r);
        }
        if let Some(s) = self.mcp_stop.remove(id) {
            s.store(true, Ordering::Relaxed);
        }
        let dir = self.cfg.runtime_dir.join("agents").join(id);
        let _ = std::fs::remove_file(dir.join("mcp.sock"));
        let _ = std::fs::remove_file(launcher::cfg_path(&dir.join("mcp.sock")));
        let _ = std::fs::remove_dir(&dir);
        let gone: Vec<u64> = self
            .conns
            .iter()
            .filter(|(_, c)| c.surface == Surface::Mcp(id.to_owned()))
            .map(|(k, _)| *k)
            .collect();
        for k in gone {
            if let Some(c) = self.conns.remove(&k) {
                let _ = c.stream.shutdown(std::net::Shutdown::Both);
            }
        }
        self.pending.retain(|_, p| p.task != id);
        // A call still waiting is dropped, and the router told so it can let
        // go of anything it holds for the task.
        self.infer_pending.retain(|_, c| c.task != id);
        self.infer_by_task.remove(id);
        let _ = self.router.send(ToRouter::Close { task: id.to_owned() });
        self.event("task_closed", json!({"task_id": id, "reason": reason}));
    }

    fn tick(&mut self) {
        let now = self.now();
        let late: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(r, _)| *r)
            .collect();
        for r in late {
            self.pending_reply(r, Err(RpcError::unavailable()));
        }
        if now.saturating_sub(self.last_sweep_ms) >= SWEEP_EVERY_MS {
            self.last_sweep_ms = now;
            self.sweep();
        }
    }

    /// Retention (A-08 §11): closed tasks older than the window go, transcript
    /// and all. Audit is unaffected.
    pub fn sweep(&mut self) {
        let now = self.now();
        let old: Vec<String> = self
            .tasks
            .values()
            .filter(|t| {
                t.closed_ms
                    .is_some_and(|c| c.saturating_add(self.cfg.retention_ms) <= now)
            })
            .map(|t| t.id.clone())
            .collect();
        for id in old {
            self.store.delete(&id);
            self.sessions.delete(&id);
            self.tasks.remove(&id);
        }
    }

    fn stop_everything(&mut self) {
        for (_, r) in self.procs.drain() {
            launcher::stop(&r);
        }
        for (_, s) in self.mcp_stop.drain() {
            s.store(true, Ordering::Relaxed);
        }
    }

    // ---- request dispatch -----------------------------------------------

    fn on_line(&mut self, conn: u64, line: &str) {
        let Some(surface) = self.conns.get(&conn).map(|c| c.surface.clone()) else {
            return;
        };
        let req = match rpc::parse(line) {
            Parsed::Reply(s) => {
                self.send(conn, s);
                return;
            }
            Parsed::Req(r) => r,
        };
        let out = match &surface {
            Surface::Console => self.console_call(conn, &req),
            Surface::Mcp(task) => {
                self.record_request(task, &req);
                match self.inference_call(conn, task, &req) {
                    Some(Outcome::Now(r)) => {
                        self.record_response(task, &req, &r);
                        Outcome::Now(r)
                    }
                    // Recorded when the router answers (`on_inference`).
                    Some(Outcome::Deferred) => Outcome::Deferred,
                    None => {
                        let r = self.mcp_call(task, &req);
                        self.record_response(task, &req, &r);
                        Outcome::Now(r)
                    }
                }
            }
        };
        if let (Some(id), Outcome::Now(r)) = (&req.id, out) {
            self.send(
                conn,
                match r {
                    Ok(v) => rpc::ok(id, v),
                    Err(e) => rpc::err(id, &e),
                },
            );
        }
    }

    /// Appends to the task's session record (A-08 §5.4). A failure is logged
    /// by id only, never by content, and does not fail the call: the record
    /// is history, not a gate.
    fn record(&mut self, task: &str, kind: &str, body: Value) {
        let e = session::entry(kind, self.now(), body);
        match self.sessions.append(task, &e) {
            Ok(true) => {}
            Ok(false) => log(&format!("task {task}: session record is full; entries dropped")),
            Err(err) => log(&format!("task {task}: cannot write the session record: {err}")),
        }
    }

    fn record_request(&mut self, task: &str, req: &Request) {
        self.record(
            task,
            "mcp_request",
            json!({"method": req.method, "params": req.params}),
        );
    }

    fn record_response(&mut self, task: &str, req: &Request, r: &Result<Value, RpcError>) {
        // Notifications get no response.
        if req.id.is_none() {
            return;
        }
        // `session.restore` is recorded like any other result: it is what the
        // model saw, and a later resume of this task needs it to hand on the
        // whole history, not only this task's part.
        let body = match r {
            Ok(v) => json!({"method": req.method, "result": v}),
            Err(e) => json!({"method": req.method, "error": e.to_json()}),
        };
        self.record(task, "mcp_response", body);
    }

    /// A conversation message, as `conversation.v1`.
    fn record_message(&mut self, task: &str, m: &Message) {
        self.record(task, "message", json!({"message": m.to_json()}));
    }

    fn task_ref(&self, id: &str) -> Result<&Task, RpcError> {
        // Unknown and not-ours are the same answer (A-08 §7); there is only
        // one owner, so there is only one case.
        self.tasks.get(id).ok_or_else(RpcError::not_found)
    }

    fn console_call(&mut self, conn: u64, req: &Request) -> Outcome {
        let p = &req.params;
        let r = match req.method.as_str() {
            "list_packages" => Ok(json!({
                "packages": packages::list(&self.cfg.package_roots).iter().map(packages::Package::to_json).collect::<Vec<_>>()
            })),
            "list_tasks" => self.m_list_tasks(p),
            "get_task" => self.m_get_task(p),
            "conversation_read" => self.m_read(p),
            "conversation_post" => self.m_post(p),
            "pause_task" | "cancel_task" => return self.m_pause_cancel(conn, req),
            "show_decisions" => return self.m_show(conn, req),
            "list_sessions" => self.m_list_sessions(p),
            "delete_session" => self.m_delete_session(p),
            "subscribe" => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.subscribed = true;
                }
                Ok(json!({
                    "subscribed": true,
                    "events": ["task_started", "task_state", "message", "awaiting_reply", "decisions_pending", "task_closed"],
                    "available": self.link.is_some(),
                    "decisions_pending": self.decisions,
                }))
            }
            "unsubscribe" => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.subscribed = false;
                }
                Ok(json!({}))
            }
            _ => Err(RpcError::new(
                rpc::METHOD_NOT_FOUND,
                "method_not_found",
                "unknown method",
            )),
        };
        Outcome::Now(r)
    }

    fn m_list_tasks(&self, p: &Value) -> Result<Value, RpcError> {
        let want = match p.get("state") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_str() {
                Some(s @ ("active" | "paused" | "draining" | "closed")) => Some(s),
                _ => {
                    return Err(RpcError::invalid_params(
                        "state must be active, paused, draining or closed",
                    ))
                }
            },
        };
        let mut v: Vec<&Task> = self
            .tasks
            .values()
            .filter(|t| match want {
                Some(s) => t.state == s,
                None => !t.is_closed(),
            })
            .collect();
        v.sort_by(|a, b| (a.started_ms, &a.id).cmp(&(b.started_ms, &b.id)));
        Ok(json!({
            "tasks": v.iter().map(|t| t.to_json()).collect::<Vec<_>>(),
            "available": self.link.is_some(),
            "decisions_pending": self.decisions,
        }))
    }

    fn m_get_task(&self, p: &Value) -> Result<Value, RpcError> {
        let t = self.task_ref(p_str(p, "task_id")?)?;
        let mut v = t.to_json();
        v["subtree"] = json!([]);
        v["available"] = json!(self.link.is_some());
        Ok(v)
    }

    fn m_read(&mut self, p: &Value) -> Result<Value, RpcError> {
        let id = p_str(p, "task_id")?;
        let since = p_u64(p, "since")?.unwrap_or(0);
        let t = self.task_ref(id)?;
        let msgs: Vec<Message> = t
            .msgs
            .iter()
            .filter(|m| m.msg_id > since)
            .take(500)
            .cloned()
            .collect();
        let out = json!({
            "messages": msgs.iter().map(Message::to_json).collect::<Vec<_>>(),
            "closed": t.is_closed(),
            "awaiting_reply": t.awaiting_reply,
        });
        for m in &msgs {
            self.channel_audit(id, "human", Some(m), m.text.len(), "read");
        }
        Ok(out)
    }

    fn m_post(&mut self, p: &Value) -> Result<Value, RpcError> {
        let id = p_str(p, "task_id")?;
        let raw = p_str(p, "text")?;
        let reply_to = p_u64(p, "reply_to")?;
        let now = self.now();
        let (msg, cleared) = {
            let t = self.tasks.get_mut(id).ok_or_else(RpcError::not_found)?;
            if t.is_closed() {
                return Err(RpcError::closed());
            }
            let text = sanitize::clean(raw);
            if text.trim().is_empty() {
                return Err(RpcError::invalid_params("text is empty"));
            }
            if text.len() > MAX_TEXT {
                return Err(RpcError::invalid_params("text is over 16 KiB"));
            }
            if reply_to.is_some_and(|r| !t.msgs.iter().any(|m| m.msg_id == r)) {
                return Err(RpcError::invalid_params("reply_to names no message"));
            }
            // A human post informs; it does not authorise (A-08 §6): the
            // chain is stamped HumanClient at `standard`, never `human`.
            let msg = Message {
                msg_id: t.next_msg,
                time_ms: now,
                kind: Kind::Human,
                text,
                reply_to,
                min_trust: "standard".into(),
                head: "human_client".into(),
            };
            self.store
                .append(id, &msg)
                .map_err(|_| RpcError::new(rpc::INTERNAL, "io", "cannot store the message"))?;
            t.next_msg += 1;
            t.msgs.push(msg.clone());
            t.counters.human_posts += 1;
            let cleared = t.awaiting_reply;
            t.awaiting_reply = false;
            (msg, cleared)
        };
        self.persist(id);
        self.record_message(id, &msg);
        self.channel_audit(id, "human", Some(&msg), msg.text.len(), "post");
        self.event("message", message_event(id, &msg));
        if cleared {
            self.event("awaiting_reply", json!({"task_id": id, "awaiting": false}));
        }
        Ok(json!({"msg_id": msg.msg_id, "time_ms": msg.time_ms}))
    }

    fn m_pause_cancel(&mut self, conn: u64, req: &Request) -> Outcome {
        let p = &req.params;
        let parsed = (|| -> Result<(String, Action), RpcError> {
            let id = p_str(p, "task_id")?.to_owned();
            let action = if req.method == "pause_task" {
                Action::Pause
            } else {
                match p_str(p, "mode")? {
                    "drain" => Action::Cancel(CancelMode::Drain),
                    "immediate" => Action::Cancel(CancelMode::Immediate),
                    _ => return Err(RpcError::invalid_params("mode must be drain or immediate")),
                }
            };
            Ok((id, action))
        })();
        let (id, action) = match parsed {
            Ok(x) => x,
            Err(e) => return Outcome::Now(Err(e)),
        };
        match self.task_ref(&id) {
            Err(e) => return Outcome::Now(Err(e)),
            Ok(t) if t.is_closed() => return Outcome::Now(Err(RpcError::closed())),
            Ok(_) => {}
        }
        let r = self.req_id();
        let msg = match &action {
            Action::Pause => ToPolicyd::PauseTask {
                req: r,
                task: id.clone(),
            },
            Action::Cancel(mode) => ToPolicyd::CancelTask {
                req: r,
                task: id.clone(),
                mode: *mode,
            },
        };
        if !self.send_policyd(&msg) {
            return Outcome::Now(Err(RpcError::unavailable()));
        }
        let deadline = self.now() + PENDING_MS;
        self.pending.insert(
            r,
            Pending {
                conn,
                id: req.id.clone(),
                deadline,
                task: id,
                action,
            },
        );
        Outcome::Deferred
    }

    fn m_show(&mut self, conn: u64, req: &Request) -> Outcome {
        let now = self.now();
        if self.last_show_ms.is_some_and(|l| now.saturating_sub(l) < 1000) {
            return Outcome::Now(Err(RpcError::new(
                rpc::RATE_LIMITED,
                "rate_limited",
                "show_decisions is limited to once a second",
            )));
        }
        let Some(path) = self.cfg.human_socket.clone() else {
            return Outcome::Now(Err(RpcError::unavailable()));
        };
        self.last_show_ms = Some(now);
        let tx = self.tx.clone();
        let id = req.id.clone();
        std::thread::spawn(move || {
            let result = crate::human::show_decisions(&path);
            let _ = tx.send(Msg::HumanReply { conn, id, result });
        });
        Outcome::Deferred
    }

    fn m_list_sessions(&self, p: &Value) -> Result<Value, RpcError> {
        let since = p_u64(p, "since")?.unwrap_or(0);
        let mut v: Vec<&Task> = self
            .tasks
            .values()
            .filter(|t| t.closed_ms.is_some_and(|c| c > since))
            .collect();
        v.sort_by(|a, b| (b.closed_ms, &b.id).cmp(&(a.closed_ms, &a.id)));
        Ok(json!({"sessions": v.iter().map(|t| json!({
            "task_id": t.id,
            "package": t.package,
            "statement": t.statement,
            "reason": t.reason,
            "closed_ms": t.closed_ms,
            "min_trust": t.min_trust,
            "resumes": (!t.resumes.is_empty()).then(|| t.resumes.clone()),
            "eligible": self.eligible(t),
        })).collect::<Vec<_>>()}))
    }

    /// Whether History may offer Resume (A-08 §5.4): closed, its package is
    /// still installed and `resumable` (F-24), and its session record is on
    /// disk. S-11 I3/I5/I6 closures are not excluded yet: agentd is not told
    /// the close was an incident (docs/KNOWNBUGS.md AGENTD-01).
    fn eligible(&self, t: &Task) -> bool {
        t.is_closed()
            && self.sessions.exists(&t.id)
            && packages::find(&self.cfg.package_roots, &t.package, &t.version)
                .is_some_and(|(dir, publisher)| packages::resumable(&dir, &publisher))
    }

    fn m_delete_session(&mut self, p: &Value) -> Result<Value, RpcError> {
        let id = p_str(p, "task_id")?;
        if !self.task_ref(id)?.is_closed() {
            return Err(RpcError::invalid_params(
                "only a closed task has a session to delete",
            ));
        }
        self.store.delete(id);
        self.sessions.delete(id);
        self.tasks.remove(id);
        Ok(json!({"deleted": true}))
    }

    // ---- MCP ------------------------------------------------------------

    fn mcp_call(&mut self, task: &str, req: &Request) -> Result<Value, RpcError> {
        match req.method.as_str() {
            "initialize" => {
                let t = self.task_ref(task)?;
                let version = req
                    .params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-06-18");
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "ec-agentd", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": t.statement,
                }))
            }
            m if m.starts_with("notifications/") => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let t = self.task_ref(task)?;
                Ok(tools_list(t.resumes.is_empty(), t.inference.is_some()))
            }
            "tools/call" => self.mcp_tool(task, &req.params),
            _ => Err(RpcError::new(
                rpc::METHOD_NOT_FOUND,
                "method_not_found",
                "unknown method",
            )),
        }
    }

    /// `tools/call inference.complete` on a task whose package declares
    /// inference. `None` for anything else, so the ordinary path answers (an
    /// undeclared task gets the unknown-tool error of a name that never
    /// existed).
    ///
    /// backend and model are the manifest's, stamped here; the arguments may
    /// not carry them. The core does not wait: the request goes to the router
    /// satellite and the answer comes back as `Msg::Inference`.
    fn inference_call(&mut self, conn: u64, task: &str, req: &Request) -> Option<Outcome> {
        if req.method != "tools/call"
            || req.params.get("name").and_then(Value::as_str) != Some(INFERENCE_TOOL)
        {
            return None;
        }
        let t = self.tasks.get(task)?;
        let decl = t.inference.clone()?;
        let package = t.package.clone();
        // A notification has no one to answer; do not spend a model call on it.
        let Some(mcp_id) = req.id.clone() else {
            return Some(Outcome::Now(Ok(Value::Null)));
        };
        if t.is_closed() {
            return Some(Outcome::Now(Err(RpcError::closed())));
        }
        let empty = json!({});
        let args = match router::parse_args(req.params.get("arguments").unwrap_or(&empty)) {
            Ok(a) => a,
            Err(m) => return Some(Outcome::Now(Err(RpcError::invalid_params(m)))),
        };
        let fail = |kind: &str, msg: &str| {
            Some(Outcome::Now(Ok(router::tool_result(&Err(router::failure(
                kind, msg,
            ))))))
        };
        if self.infer_by_task.contains_key(task) {
            return fail("rate_limited", "one inference call at a time per task");
        }
        let id = self.next_infer;
        self.next_infer += 1;
        let sent = self.router.send(ToRouter::Complete(InferRequest {
            id,
            task: task.to_owned(),
            package,
            backend: decl.backend,
            model: decl.model,
            system: args.system,
            messages: args.messages,
            tools: args.tools,
            max_tokens: args.max_tokens,
        }));
        if sent.is_err() {
            return fail("backend_unavailable", router::DOWN_MESSAGE);
        }
        self.infer_by_task.insert(task.to_owned(), id);
        self.infer_pending.insert(
            id,
            InferCall {
                task: task.to_owned(),
                conn,
                mcp_id,
            },
        );
        Some(Outcome::Deferred)
    }

    /// The router answered (or failed) a call. The answer goes through the
    /// same session-record path as every other MCP response. Its content is
    /// recorded there and nowhere else: the audit chain's `Model` provenance
    /// links are not stamped yet (docs/KNOWNBUGS.md AGENTD-01).
    fn on_inference(&mut self, id: u64, result: &Result<Completion, Failure>) {
        let Some(call) = self.infer_pending.remove(&id) else {
            return;
        };
        self.infer_by_task.remove(&call.task);
        let v = router::tool_result(result);
        self.record(
            &call.task,
            "mcp_response",
            json!({"method": "tools/call", "result": v}),
        );
        self.send(call.conn, rpc::ok(&call.mcp_id, v));
    }

    fn mcp_tool(&mut self, task: &str, p: &Value) -> Result<Value, RpcError> {
        let name = p_str(p, "name")?;
        let empty = json!({});
        let args = p.get("arguments").unwrap_or(&empty);
        let result = match name {
            // Note the absence of any task parameter: the task is the socket's.
            "task.say" => self.agent_post(task, Kind::Say, p_str(args, "text")?),
            "task.ask" => self.agent_post(task, Kind::Ask, p_str(args, "question")?),
            "task.inbox" => self.inbox(task, p_u64(args, "since")?.unwrap_or(0)),
            // F-23: only a task created with `resumes` has this tool; for any
            // other it is an unknown tool, like a name that never existed.
            RESTORE_TOOL if self.tasks.get(task).is_some_and(|t| !t.resumes.is_empty()) => self.restore(task),
            _ => return Err(RpcError::invalid_params("unknown tool")),
        };
        Ok(match result {
            Ok(v) => json!({
                "content": [{"type": "text", "text": v.to_string()}],
                "structuredContent": v,
                "isError": false,
            }),
            Err(reason) => {
                let v = json!({"error": reason});
                json!({
                    "content": [{"type": "text", "text": v.to_string()}],
                    "structuredContent": v,
                    "isError": true,
                })
            }
        })
    }

    /// An agent's `say` or `ask` (A-08 §6, quotas A-03 §7).
    fn agent_post(&mut self, id: &str, kind: Kind, raw: &str) -> Result<Value, &'static str> {
        let now = self.now();
        enum Res {
            Posted(Message, bool),
            Quota(usize),
        }
        let (res, principal) = {
            let t = self.tasks.get_mut(id).ok_or("not_found")?;
            if t.is_closed() {
                return Err("closed");
            }
            let text = sanitize::clean(raw);
            if text.trim().is_empty() {
                return Err("empty");
            }
            if text.len() > MAX_TEXT {
                return Err("too_large");
            }
            let len = text.len() as u64;
            while t
                .window
                .front()
                .is_some_and(|(at, _)| now.saturating_sub(*at) >= WINDOW_MS)
            {
                t.window.pop_front();
            }
            let bytes: u64 = t.window.iter().map(|w| w.1).sum();
            let principal = t.principal.clone();
            if t.window.len() >= MSGS_PER_MIN || bytes + len > BYTES_PER_MIN {
                t.counters.quota_exceeded += 1;
                (Res::Quota(text.len()), principal)
            } else {
                let msg = Message {
                    msg_id: t.next_msg,
                    time_ms: now,
                    kind,
                    text,
                    reply_to: None,
                    // The agent's chain, as far as agentd knows it: the task's
                    // summary, headed by the agent principal.
                    min_trust: t.min_trust.clone(),
                    head: principal.clone(),
                };
                self.store.append(id, &msg).map_err(|_| "io")?;
                t.next_msg += 1;
                t.msgs.push(msg.clone());
                t.window.push_back((now, len));
                t.counters.messages_posted += 1;
                t.counters.bytes_posted += len;
                let mut changed = false;
                if kind == Kind::Ask && !t.awaiting_reply {
                    t.awaiting_reply = true;
                    changed = true;
                }
                (Res::Posted(msg, changed), principal)
            }
        };
        self.persist(id);
        match res {
            Res::Quota(size) => {
                self.channel_audit(id, &principal, None, size, "quota_exceeded");
                Err("quota_exceeded")
            }
            Res::Posted(msg, changed) => {
                self.record_message(id, &msg);
                self.channel_audit(id, &principal, Some(&msg), msg.text.len(), "post");
                self.event("message", message_event(id, &msg));
                if changed {
                    self.event("awaiting_reply", json!({"task_id": id, "awaiting": true}));
                }
                Ok(json!({"msg_id": msg.msg_id}))
            }
        }
    }

    /// `session.restore` (A-06 §7, F-23): the old session's record, then the
    /// boundary marker. Answers once.
    fn restore(&mut self, id: &str) -> Result<Value, &'static str> {
        let t = self.tasks.get(id).ok_or("not_found")?;
        if t.restore_taken {
            return Err("already_restored");
        }
        let old = t.resumes.clone();
        let mut entries = self.sessions.read(&old).ok_or("session_unavailable")?;
        let restored = entries.len();
        entries.push(session::boundary());
        if let Some(t) = self.tasks.get_mut(id) {
            t.restore_taken = true;
        }
        Ok(json!({"entries": entries, "restored": restored, "resumes": old}))
    }

    /// Human and context messages after a cursor, and whether the task is
    /// closed (or draining).
    fn inbox(&mut self, id: &str, since: u64) -> Result<Value, &'static str> {
        let t = self.tasks.get(id).ok_or("not_found")?;
        let msgs: Vec<Message> = t
            .msgs
            .iter()
            .filter(|m| m.msg_id > since && matches!(m.kind, Kind::Human | Kind::Context))
            .take(100)
            .cloned()
            .collect();
        let closed = t.inbox_closed();
        let principal = t.principal.clone();
        let cursor = msgs.last().map_or(since, |m| m.msg_id);
        for m in &msgs {
            self.channel_audit(id, &principal, Some(m), m.text.len(), "read");
        }
        Ok(json!({
            "messages": msgs.iter().map(Message::to_json).collect::<Vec<_>>(),
            "closed": closed,
            "cursor": cursor,
        }))
    }
}

fn message_event(task: &str, m: &Message) -> Value {
    let mut v = json!({
        "task_id": task,
        "msg_id": m.msg_id,
        "kind": m.kind.as_str(),
        "text": m.text,
        "trust": {"min_trust": m.min_trust, "head": m.head},
        "time_ms": m.time_ms,
    });
    if let Some(r) = m.reply_to {
        v["reply_to"] = json!(r);
    }
    v
}

fn tools_list(plain: bool, inference: bool) -> Value {
    let mut tools = json!([
        {
            "name": "task.say",
            "description": "Post a message to the human in this task's conversation.",
            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
        },
        {
            "name": "task.ask",
            "description": "Ask the human a question and mark the task as awaiting a reply.",
            "inputSchema": {"type": "object", "properties": {"question": {"type": "string"}}, "required": ["question"]},
        },
        {
            "name": "task.inbox",
            "description": "Read the human's messages after a cursor. `closed` is true once the task is ending.",
            "inputSchema": {"type": "object", "properties": {"since": {"type": "integer"}}},
        },
    ]);
    if !plain {
        tools.as_array_mut().expect("array").push(json!({
            "name": RESTORE_TOOL,
            "description": "Restore the history of the session this task resumes: its recorded entries, then a boundary marker. Callable once. Every handle and revision from that session is invalid afterwards; re-observe before acting.",
            "inputSchema": {"type": "object", "properties": {}},
        }));
    }
    if inference {
        tools.as_array_mut().expect("array").push(json!({
            "name": INFERENCE_TOOL,
            "description": "Ask the model this package is declared to use for one completion (Messages API shape). The model and backend are fixed by the package manifest. One call at a time per task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "system": {"type": "string"},
                    "messages": {"type": "array", "minItems": 1, "items": {"type": "object"}},
                    "tools": {"type": "array", "items": {"type": "object"}},
                    "max_tokens": {"type": "integer", "minimum": 1, "maximum": router::MAX_TOKENS_CAP, "default": router::MAX_TOKENS_DEFAULT},
                },
                "required": ["messages"],
                "additionalProperties": false,
            },
        }));
    }
    json!({"tools": tools})
}
