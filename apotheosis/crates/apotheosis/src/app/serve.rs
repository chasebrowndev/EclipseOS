// SPDX-License-Identifier: AGPL-3.0-only
//! The control socket on the main loop (T-12): the listener and every client are calloop
//! sources; requests are decoded, gated on the capability table and answered in order;
//! slow reads (`pane.text`, `pane.processes`) run on the worker pool and come back through
//! the main channel. Nothing a client sends or receives is logged, only methods, codes and
//! pids.

use std::collections::BTreeMap;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use apoth_action::{Action, ClassContext, Event, EventKind, FieldValue, OverrideClass, Refusal};
use apoth_proto::params::{Extent, Launch};
use apoth_proto::results::{
    ForegroundProc, HelloReply, LaunchReply, PaneNode, PaneState, PaneTextReply, ProcessInfo,
    ProcessesReply, SubscribeReply, TabNode, TreeReply, VersionReply, WindowNode,
};
use apoth_proto::{
    Cap, ErrorCode, PROTO_OLDEST, PROTO_VERSION, Params, Reply, Request, WireError, WireEvent,
    negotiate,
};
use apoth_pty::ProcSource;
use calloop::generic::Generic;
use calloop::{Interest, Mode, PostAction, RegistrationToken};

use super::{App, PaneKey};
use crate::ctl::caps::{self, Policy, Verdict};
use crate::ctl::conn::Conn;
use crate::ctl::peer::{self, Peer, Refused};
use crate::ctl::target::{self, Target};
use crate::ctl::text::{TEXT_BUDGET, pane_text};
use crate::pane::MainMsg;

/// Connections accepted per listener wakeup.
const ACCEPT_BUDGET: usize = 16;
/// Clients at once; further connections are refused.
const MAX_CLIENTS: usize = 64;
/// Features offered in `hello`.
const FEATURES: &[&str] = &["events", "launch-wait"];
/// `argv` bytes read from `/proc` per process.
const MAX_ARGV_BYTES: usize = 4096;
/// Processes listed by `pane.processes`.
const MAX_PROCS: usize = 256;

/// The socket's state on [`App`].
pub(super) struct Ctl {
    listener: UnixListener,
    /// The socket path (exported to panes as `APOTHEOSIS_SOCKET`).
    pub(super) path: PathBuf,
    clients: BTreeMap<u64, Client>,
    next: u64,
    /// `launch --wait` replies owed when a pane ends.
    waits: Vec<(u64, u64, PaneKey)>,
}

struct Client {
    conn: Conn,
    peer: Peer,
    tokens: Vec<RegistrationToken>,
    write: Option<RegistrationToken>,
    /// Close once the queue is written (after a framing error).
    closing: bool,
    dead: bool,
}

impl Drop for Ctl {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn err(code: ErrorCode, msg: &str) -> WireError {
    WireError::new(code, msg)
}

/// What the class table is told about macros and config keys.
struct Ctx<'a> {
    app: &'a App,
}

impl ClassContext for Ctx<'_> {
    fn override_class(&self, key: &str) -> Option<OverrideClass> {
        apoth_config::schema::key(key).map(|k| OverrideClass::parse_fail_closed(k.class.name()))
    }
    fn macro_members(&self, name: &str) -> Option<&[Action]> {
        self.app
            .keymap
            .macros
            .get(name)
            .map(|m| m.members.as_slice())
    }
}

impl App {
    /// Bind `$XDG_RUNTIME_DIR/apotheosis/<pid>.sock` and listen. Failure leaves the
    /// terminal without a socket (logged), never without a window.
    pub(super) fn start_ctl(&mut self) {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let Some(dir) = crate::ctl::path::socket_dir(runtime.as_deref()) else {
            tracing::warn!("XDG_RUNTIME_DIR is unset: no control socket");
            return;
        };
        let (listener, path) = match crate::ctl::path::bind(&dir, &std::process::id().to_string()) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "cannot create the control socket");
                return;
            }
        };
        let fd = match listener.as_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                tracing::warn!(error = %e, "cannot watch the control socket");
                return;
            }
        };
        if let Err(e) = self.lh.insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, _, app: &mut App| {
                app.ctl_accept();
                Ok(PostAction::Continue)
            },
        ) {
            tracing::warn!(error = %e.error, "cannot watch the control socket");
            return;
        }
        self.ctl = Some(Ctl {
            listener,
            path,
            clients: BTreeMap::new(),
            next: 1,
            waits: Vec::new(),
        });
    }

    /// The socket path panes export.
    pub(super) fn ctl_path(&self) -> Option<&std::path::Path> {
        self.ctl.as_ref().map(|c| c.path.as_path())
    }

    fn pane_pids(&self) -> Vec<(u64, i32)> {
        self.panes
            .iter()
            .filter_map(|(id, p)| p.pty.as_ref().map(|t| (*id, t.pid())))
            .collect()
    }

    fn ctl_accept(&mut self) {
        for _ in 0..ACCEPT_BUDGET {
            let Some(ctl) = self.ctl.as_ref() else { return };
            let stream = match ctl.listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) => {
                    tracing::debug!(error = %e, "accept failed");
                    return;
                }
            };
            if ctl.clients.len() >= MAX_CLIENTS {
                tracing::debug!("too many socket clients; connection refused");
                continue;
            }
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            let interpreters = self.reloader.config().list("socket.interpreters").to_vec();
            let peer = match peer::identify(&stream, &self.pane_pids(), &interpreters) {
                Ok(p) => p,
                Err(why) => {
                    let (why, kind) = match why {
                        Refused::Uid => ("another uid", None),
                        Refused::Gone => ("gone or recycled pid", None),
                        Refused::Io(e) => ("unreadable", Some(e.kind())),
                    };
                    tracing::debug!(why, ?kind, "socket peer refused");
                    continue;
                }
            };
            self.ctl_register(Conn::new(stream), peer);
        }
    }

    fn ctl_register(&mut self, conn: Conn, peer: Peer) {
        let Some(ctl) = self.ctl.as_mut() else { return };
        let cid = ctl.next;
        ctl.next += 1;
        let mut tokens = Vec::new();
        let read_fd: Option<OwnedFd> = conn.stream().as_fd().try_clone_to_owned().ok();
        let pidfd: Option<OwnedFd> = peer.pidfd.try_clone().ok();
        if let Some(fd) = read_fd
            && let Ok(t) = self.lh.insert_source(
                Generic::new(fd, Interest::READ, Mode::Level),
                move |_, _, app: &mut App| Ok(app.ctl_readable(cid)),
            )
        {
            tokens.push(t);
        }
        if let Some(fd) = pidfd
            && let Ok(t) = self.lh.insert_source(
                Generic::new(fd, Interest::READ, Mode::Level),
                move |_, _, app: &mut App| {
                    // The peer exited: its connection goes with it (T-12 §1).
                    app.ctl_kill(cid);
                    Ok(PostAction::Remove)
                },
            )
        {
            tokens.push(t);
        }
        tracing::debug!(
            pid = peer.pid,
            agent = peer.facts.agent,
            descendant = peer.facts.pane.is_some(),
            "socket client connected"
        );
        if let Some(ctl) = self.ctl.as_mut() {
            ctl.clients.insert(
                cid,
                Client {
                    conn,
                    peer,
                    tokens,
                    write: None,
                    closing: false,
                    dead: false,
                },
            );
        }
    }

    /// Mark a client gone; its sources are removed after this dispatch (a source cannot
    /// remove itself while it runs).
    fn ctl_kill(&mut self, cid: u64) {
        let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) else {
            return;
        };
        if c.dead {
            return;
        }
        c.dead = true;
        self.lh.insert_idle(move |app: &mut App| app.ctl_drop(cid));
    }

    fn ctl_drop(&mut self, cid: u64) {
        let Some(ctl) = self.ctl.as_mut() else { return };
        ctl.waits.retain(|w| w.0 != cid);
        let had = ctl.clients.contains_key(&cid);
        if let Some(c) = ctl.clients.remove(&cid) {
            for t in c.tokens.into_iter().chain(c.write) {
                self.lh.remove(t);
            }
            tracing::debug!(pid = c.peer.pid, "socket client disconnected");
        }
        if had {
            self.media_drop_client(cid);
        }
    }

    fn ctl_readable(&mut self, cid: u64) -> PostAction {
        let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) else {
            return PostAction::Remove;
        };
        if c.dead {
            return PostAction::Continue;
        }
        let input = match c.conn.read() {
            Ok(i) => i,
            Err(_) => {
                self.ctl_kill(cid);
                return PostAction::Continue;
            }
        };
        for line in input.lines {
            match line {
                Ok(l) => self.ctl_line(cid, &l),
                Err(e) => {
                    // An oversize line: answer, then close (T-12 §1).
                    self.ctl_send(cid, &Reply::Err { id: None, error: e });
                    if let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) {
                        c.closing = true;
                    }
                    self.ctl_flush(cid);
                    return PostAction::Continue;
                }
            }
        }
        if input.eof {
            self.ctl_kill(cid);
        }
        PostAction::Continue
    }

    /// Queue one line for a client and write what the socket takes.
    fn ctl_queue(&mut self, cid: u64, line: &[u8]) {
        let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) else {
            return;
        };
        if c.dead {
            return;
        }
        if !c.conn.queue(line) {
            tracing::debug!(pid = c.peer.pid, "socket client too slow; disconnected");
            self.ctl_kill(cid);
            return;
        }
        self.ctl_flush(cid);
    }

    pub(super) fn ctl_send(&mut self, cid: u64, r: &Reply) {
        let line = r.to_line().unwrap_or_else(|e| {
            Reply::Err {
                id: match r {
                    Reply::Ok { id, .. } => Some(*id),
                    Reply::Err { id, .. } => *id,
                },
                error: e,
            }
            .to_line()
            .unwrap_or_default()
        });
        self.ctl_queue(cid, &line);
    }

    fn ctl_flush(&mut self, cid: u64) {
        let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) else {
            return;
        };
        match c.conn.flush() {
            Ok(true) => {
                if c.closing {
                    self.ctl_kill(cid);
                }
            }
            Ok(false) => {
                if c.write.is_some() {
                    return;
                }
                let Ok(fd) = c.conn.stream().as_fd().try_clone_to_owned() else {
                    return;
                };
                match self.lh.insert_source(
                    Generic::new(fd, Interest::WRITE, Mode::Level),
                    move |_, _, app: &mut App| Ok(app.ctl_writable(cid)),
                ) {
                    Ok(t) => {
                        if let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) {
                            c.write = Some(t);
                        }
                    }
                    Err(_) => self.ctl_kill(cid),
                }
            }
            Err(_) => self.ctl_kill(cid),
        }
    }

    fn ctl_writable(&mut self, cid: u64) -> PostAction {
        let Some(c) = self.ctl.as_mut().and_then(|c| c.clients.get_mut(&cid)) else {
            return PostAction::Remove;
        };
        match c.conn.flush() {
            Ok(false) => PostAction::Continue,
            Ok(true) => {
                c.write = None;
                if c.closing {
                    self.ctl_kill(cid);
                }
                PostAction::Remove
            }
            Err(_) => {
                c.write = None;
                self.ctl_kill(cid);
                PostAction::Remove
            }
        }
    }

    /// Deliver an event to every subscription that takes it, and queue it for the rules
    /// (T-11 §5). Content fields are withheld from agent peers (T-12 §4).
    pub(super) fn ctl_emit(&mut self, mut ev: Event, pane: Option<PaneKey>) {
        if let Some(p) = pane {
            let _ = ev.set("pane", FieldValue::Int(p as i64));
            if let Some(t) = self.session.tab_of(p) {
                let _ = ev.set("tab", FieldValue::Int(t as i64));
            }
        }
        let _ = ev.set("window", FieldValue::Int(self.mux_win as i64));
        self.auto_enqueue(&ev, pane);
        let Some(ctl) = self.ctl.as_mut() else { return };
        if ctl.clients.is_empty() {
            return;
        }
        let mut out = Vec::new();
        for (&cid, c) in &mut ctl.clients {
            if c.dead || c.conn.proto.is_none() {
                continue;
            }
            let Some(sub) = c.conn.wants(ev.kind, pane) else {
                continue;
            };
            c.conn.seq += 1;
            let w = WireEvent::from_event(c.conn.seq, Some(sub), &ev, c.peer.facts.agent);
            if let Ok(line) = apoth_proto::encode_line(&w) {
                out.push((cid, line));
            }
        }
        for (cid, line) in out {
            self.ctl_queue(cid, &line);
        }
    }

    /// A pane ended: answer `launch --wait` requests for it.
    pub(super) fn ctl_pane_gone(&mut self, id: PaneKey, code: Option<i32>) {
        let Some(ctl) = self.ctl.as_mut() else { return };
        let (done, keep): (Vec<_>, Vec<_>) = ctl.waits.drain(..).partition(|w| w.2 == id);
        ctl.waits = keep;
        for (cid, rid, pane) in done {
            let r = Reply::ok(
                rid,
                &LaunchReply {
                    pane,
                    window: self.mux_win,
                    exit: Some(code.unwrap_or(0)),
                },
            )
            .unwrap_or_else(|e| Reply::Err {
                id: Some(rid),
                error: e,
            });
            self.ctl_send(cid, &r);
        }
    }

    // -------------------------------------------------------------- requests

    fn ctl_line(&mut self, cid: u64, line: &[u8]) {
        let req = match Request::decode(line) {
            Ok(r) => r,
            Err(d) => return self.ctl_send(cid, &d.reply()),
        };
        let id = req.id;
        let reply = match self.ctl_handle(cid, req) {
            Ok(Some(ok)) => Reply::Ok { id, ok },
            Ok(None) => return, // answered later (worker or launch --wait)
            Err(e) => Reply::Err {
                id: Some(id),
                error: e,
            },
        };
        self.ctl_send(cid, &reply);
    }

    fn client(&self, cid: u64) -> Option<&Client> {
        self.ctl.as_ref()?.clients.get(&cid)
    }

    /// Resolve a request's target to a pane (T-12 §3; `self` by default).
    fn resolve(&self, cid: u64, t: Option<&str>) -> Result<PaneKey, WireError> {
        let me = self.client(cid).and_then(|c| c.peer.facts.pane);
        let t = target::parse(t.unwrap_or(apoth_proto::DEFAULT_TARGET))
            .map_err(|m| err(ErrorCode::BadTarget, m))?;
        let id = match t {
            Target::SelfPane => me.ok_or_else(|| {
                err(
                    ErrorCode::NoTarget,
                    "the client is not inside a pane; name a target",
                )
            })?,
            Target::Id(n) => n,
            Target::Focused => self
                .focused()
                .ok_or_else(|| err(ErrorCode::NoTarget, "no pane has focus"))?,
        };
        if self.panes.contains_key(&id) {
            Ok(id)
        } else {
            Err(err(ErrorCode::NoTarget, "no such pane"))
        }
    }

    /// Check every class the request needs against the table.
    fn gate(&self, cid: u64, params: &Params, target: Option<PaneKey>) -> Result<(), WireError> {
        let Some(c) = self.client(cid) else {
            return Err(err(ErrorCode::Internal, "no client"));
        };
        let cls = params.classify(&Ctx { app: self });
        if let Some(r) = cls.refused {
            let m = match r {
                Refusal::GrantKey => "grant keys are never writable over the socket",
                _ => "refused by the capability rules",
            };
            return Err(err(ErrorCode::Denied, m));
        }
        let cfg = self.reloader.config();
        let grants = cfg
            .get("socket.grants")
            .and_then(apoth_config::Value::as_map)
            .unwrap_or(&[]);
        let policy = Policy {
            grants,
            key_stream: cfg.bool("socket.key-stream"),
        };
        for class in cls.classes.iter() {
            match caps::decide(&c.peer.facts, c.peer.exe.as_deref(), class, target, &policy) {
                Verdict::Grant => {}
                Verdict::Ask => {
                    // HOOK(consent): the consent bar (T-11 §1) is not built yet, so a class
                    // that needs the user's answer is denied (reported gap).
                    return Err(WireError::new(
                        ErrorCode::Denied,
                        format!(
                            "`{}` needs consent, which is not available yet",
                            class.name()
                        ),
                    ));
                }
                Verdict::Deny(m) => return Err(err(ErrorCode::Denied, m)),
            }
        }
        Ok(())
    }

    fn ctl_handle(
        &mut self,
        cid: u64,
        req: Request,
    ) -> Result<Option<serde_json::Value>, WireError> {
        let Some(c) = self.client(cid) else {
            return Err(err(ErrorCode::Internal, "no client"));
        };
        let agent = c.peer.facts.agent;
        let ready = c.conn.proto.is_some();
        let method = req.method();
        let params = req.params;
        if let Params::Hello(h) = &params {
            if ready {
                return Err(err(ErrorCode::NotReady, "hello was already sent"));
            }
            let proto = negotiate(h.proto)?;
            let me = c.peer.facts.pane;
            let cfg = self.reloader.config();
            let grants = cfg
                .get("socket.grants")
                .and_then(apoth_config::Value::as_map)
                .unwrap_or(&[]);
            let policy = Policy {
                grants,
                key_stream: cfg.bool("socket.key-stream"),
            };
            let granted: Vec<Cap> = h
                .want
                .iter()
                .copied()
                .filter(|w| {
                    caps::decide(&c.peer.facts, c.peer.exe.as_deref(), w.0, me, &policy)
                        == Verdict::Grant
                })
                .collect();
            tracing::debug!(proto, "socket hello");
            if let Some(c) = self.ctl.as_mut().and_then(|x| x.clients.get_mut(&cid)) {
                c.conn.proto = Some(proto);
            }
            return json(&HelloReply {
                proto,
                version: env!("CARGO_PKG_VERSION").into(),
                granted,
                features: FEATURES.iter().map(|s| (*s).to_owned()).collect(),
            });
        }
        if !ready {
            return Err(err(ErrorCode::NotReady, "send hello first"));
        }
        if method.info().r2 {
            return Err(err(ErrorCode::Unsupported, "reserved for a later release"));
        }
        // Registry-backed methods: a bad action is its own error before any class check.
        if let Some(Err(e)) = params.to_action() {
            let code = match e.kind {
                apoth_action::ErrorKind::UnknownAction => ErrorCode::UnknownAction,
                _ => ErrorCode::BadRequest,
            };
            return Err(WireError::new(code, e.message));
        }
        let target = if method.info().targeted {
            match &params {
                // The whole tree needs no pane.
                Params::Tree(p) if p.target.is_none() => None,
                _ => Some(self.resolve(cid, params.target())?),
            }
        } else {
            None
        };
        if let Params::Subscribe(s) = &params
            && let Some(m) = &s.match_expr
        {
            // The match narrows events to one pane; resolve it like a target.
            let _ = self.resolve(cid, Some(m))?;
        }
        self.gate(cid, &params, target)?;
        tracing::debug!(method = method.name(), "socket request");
        match params {
            Params::Hello(_) => unreachable!("handled above"),
            Params::Version(_) => json(&VersionReply {
                proto: PROTO_VERSION,
                oldest: PROTO_OLDEST,
                version: env!("CARGO_PKG_VERSION").into(),
            }),
            Params::Tree(_) => json(&self.tree(agent)),
            Params::PaneInfo(_) => {
                let id = target.unwrap_or_default();
                json(&self.pane_node(id, agent))
            }
            Params::PaneText(p) => {
                if p.extent == Extent::Selection {
                    return Err(err(
                        ErrorCode::Unsupported,
                        "selection text is not available yet",
                    ));
                }
                if p.sgr {
                    return Err(err(ErrorCode::Unsupported, "sgr text is not available yet"));
                }
                let id = target.unwrap_or_default();
                self.pane_text_job(cid, req.id, id, p.extent, p.wrap)?;
                Ok(None)
            }
            Params::PaneProcesses(_) => {
                let id = target.unwrap_or_default();
                self.processes_job(cid, req.id, id, agent)?;
                Ok(None)
            }
            Params::ConfigGet(p) => {
                let v = self
                    .reloader
                    .config()
                    .get(&p.key)
                    .ok_or_else(|| err(ErrorCode::BadRequest, "unknown config key"))?;
                Ok(Some(config_json(v)))
            }
            Params::ConfigReload(_) => {
                self.start_reload();
                Ok(Some(serde_json::Value::Null))
            }
            Params::Subscribe(s) => {
                let kinds: Vec<EventKind> = s
                    .events
                    .iter()
                    .filter_map(|e| EventKind::parse(e))
                    .collect();
                let pane = match &s.match_expr {
                    Some(m) => Some(self.resolve(cid, Some(m))?),
                    None => None,
                };
                let c = self
                    .ctl
                    .as_mut()
                    .and_then(|x| x.clients.get_mut(&cid))
                    .ok_or_else(|| err(ErrorCode::Internal, "no client"))?;
                let sub = c
                    .conn
                    .subscribe(kinds, pane)
                    .ok_or_else(|| err(ErrorCode::BadRequest, "too many subscriptions"))?;
                json(&SubscribeReply { subscription: sub })
            }
            Params::Unsubscribe(u) => {
                let c = self
                    .ctl
                    .as_mut()
                    .and_then(|x| x.clients.get_mut(&cid))
                    .ok_or_else(|| err(ErrorCode::Internal, "no client"))?;
                if c.conn.unsubscribe(u.subscription) {
                    Ok(Some(serde_json::Value::Null))
                } else {
                    Err(err(ErrorCode::BadRequest, "no such subscription"))
                }
            }
            Params::ActionRun(p) => {
                let a =
                    Action::parse(&p.action).map_err(|e| err(ErrorCode::BadRequest, &e.message))?;
                if !super::handles(a.id()) {
                    return Err(err(
                        ErrorCode::Unsupported,
                        "that action is not available yet",
                    ));
                }
                self.run_targeted(a, target.unwrap_or_default());
                Ok(Some(serde_json::Value::Null))
            }
            Params::InputSendKeys(p) => {
                let id = target.unwrap_or_default();
                let mut chords = Vec::with_capacity(p.keys.len());
                for k in &p.keys {
                    chords.push(
                        apoth_action::KeyChord::parse(k)
                            .map_err(|e| err(ErrorCode::BadRequest, &e.message))?,
                    );
                }
                // Encoded straight to the pane: never through bind lookup (T-12 §2).
                self.send_chords(id, &chords)
                    .map_err(|m| err(ErrorCode::BadRequest, m))?;
                Ok(Some(serde_json::Value::Null))
            }
            Params::Launch(l) => self.ctl_launch(cid, req.id, *l),
            Params::MediaWindowOpen(p) => self.media_open(cid, p),
            Params::MediaWindowWrite(p) => self.media_write(cid, p),
            Params::MediaWindowWait(p) => self.media_wait(cid, req.id, p),
            Params::PaneBlocks(_)
            | Params::ConfigEffective(_)
            | Params::KeyboardState(_)
            | Params::GraphicsList(_)
            | Params::MediaList(_)
            | Params::WaylandInfo(_)
            | Params::RenderStatus(_)
            | Params::PaneWatch(_)
            | Params::ConfigSet(_)
            | Params::ConfigReset(_)
            | Params::ConfigPersist(_)
            | Params::ConfigSaveOverride(_)
            | Params::Notify(_) => Err(err(ErrorCode::Unsupported, "not available yet")),
        }
    }

    fn ctl_launch(
        &mut self,
        cid: u64,
        rid: u64,
        l: Launch,
    ) -> Result<Option<serde_json::Value>, WireError> {
        if l.hold || !l.overrides.is_empty() || l.layout.is_some() || l.app_id.is_some() {
            return Err(err(
                ErrorCode::Unsupported,
                "hold, overrides, layouts and app ids are not available yet",
            ));
        }
        let argv = (!l.argv.is_empty()).then_some(l.argv);
        let cwd = l.cwd.map(PathBuf::from);
        let (argv, cwd, profile_env) = match l.profile.as_deref() {
            None => (argv, cwd, Vec::new()),
            Some(name) => self
                .profile_launch(name, argv, cwd)
                .map_err(|()| err(ErrorCode::BadRequest, "unknown profile"))?,
        };
        // The launcher's environment, less what the pane sets itself (its identity and the
        // terminal it runs in).
        let env: Vec<(String, String)> = l
            .env
            .into_iter()
            .filter(|(k, _)| {
                !k.starts_with("APOTHEOSIS_")
                    && !matches!(
                        k.as_str(),
                        "TERM" | "COLORTERM" | "TERMINFO" | "TERM_PROGRAM" | "TERM_PROGRAM_VERSION"
                    )
            })
            .chain(profile_env)
            .collect();
        let Some(id) = self.new_tab_with(argv, cwd, env) else {
            return Err(err(ErrorCode::Internal, "the pane could not start"));
        };
        if let Some(p) = &l.profile {
            self.set_explicit_profile(id, p);
        }
        if let Some(t) = l.title
            && let Some(tid) = self.session.tab_of(id)
        {
            let _ = self
                .session
                .rename_tab(tid, super::clip(&t, super::MAX_TITLE));
            self.send_layout();
        }
        if l.wait {
            if let Some(ctl) = self.ctl.as_mut() {
                ctl.waits.push((cid, rid, id));
            }
            return Ok(None);
        }
        json(&LaunchReply {
            pane: id,
            window: self.mux_win,
            exit: None,
        })
    }

    /// Run a socket action for `target`. Input and close go to the target directly; any
    /// other action acts on the focused pane, so the target is focused first (reported
    /// gap: targeted actions without focus need the dispatcher to take an acting pane).
    fn run_targeted(&mut self, a: Action, target: PaneKey) {
        use Action as A;
        match a {
            A::InputSendText { text } => self.send_unit(target, text.into_bytes()),
            A::InputSendBytes { bytes } => self.send_unit(target, bytes.0),
            A::InputSendKeys { keys } => {
                let _ = self.send_chords(target, &keys.0);
            }
            A::PaneClose { .. } => self.close_pane(target),
            other => {
                if self.focused() != Some(target) {
                    self.reveal(target);
                }
                self.dispatch(vec![other], 0);
            }
        }
    }

    /// Show `id`'s tab and focus it.
    pub(super) fn reveal(&mut self, id: PaneKey) {
        let Some(tid) = self.session.tab_of(id) else {
            return;
        };
        if let Some(w) = self.session.window(self.mux_win)
            && let Some(i) = w.tabs.iter().position(|t| *t == tid)
            && w.active != Some(tid)
        {
            let _ = self
                .session
                .focus_tab(self.mux_win, apoth_mux::TabTarget::Index(i));
        }
        if let Some(tab) = self.session.tab_mut(tid) {
            let _ = tab.focus(id);
        }
        self.relayout();
    }

    fn tree(&self, redact: bool) -> TreeReply {
        let mut tabs = Vec::new();
        if let Some(w) = self.session.window(self.mux_win) {
            for &tid in &w.tabs {
                let Some(tab) = self.session.tab(tid) else {
                    continue;
                };
                tabs.push(TabNode {
                    id: tid,
                    title: (!redact).then(|| self.tab_title(tid)),
                    panes: tab
                        .panes()
                        .into_iter()
                        .map(|p| self.pane_node(p, redact))
                        .collect(),
                });
            }
        }
        TreeReply {
            windows: vec![WindowNode {
                id: self.mux_win,
                tabs,
            }],
        }
    }

    fn pane_node(&self, id: PaneKey, redact: bool) -> PaneNode {
        let p = self.panes.get(&id);
        let (rows, cols) = p.map_or((0, 0), |p| p.dims);
        let fg = p
            .and_then(|p| p.pty.as_ref())
            .and_then(|t| t.foreground_pgid().ok());
        PaneNode {
            id,
            title: p.filter(|_| !redact).map(|p| p.title.clone()),
            cwd: p
                .filter(|_| !redact)
                .and_then(|p| {
                    p.cwd
                        .clone()
                        .or_else(|| p.pty.as_ref()?.foreground_cwd().ok())
                })
                .map(|c| c.to_string_lossy().into_owned()),
            foreground: fg.map(|pid| ForegroundProc {
                pid: pid as u32,
                argv: if redact { None } else { read_argv(pid) },
            }),
            cols: cols as u32,
            rows: rows as u32,
            state: if p.is_some_and(|p| p.exit.is_some()) {
                PaneState::Exited
            } else {
                PaneState::Running
            },
        }
    }

    fn submit(&self, job: crate::workers::Job) -> Result<(), WireError> {
        self.workers
            .submit(job)
            .map_err(|_| err(ErrorCode::Internal, "busy; try again"))
    }

    fn pane_text_job(
        &self,
        cid: u64,
        rid: u64,
        id: PaneKey,
        extent: Extent,
        wrap: bool,
    ) -> Result<(), WireError> {
        let p = self
            .panes
            .get(&id)
            .ok_or_else(|| err(ErrorCode::NoTarget, "no such pane"))?;
        let shared = std::sync::Arc::clone(&p.shared);
        let tx = self.to_main.clone();
        self.submit(Box::new(move || {
            let (text, truncated) = {
                let t = shared.lock();
                pane_text(t.grid(), extent, wrap, TEXT_BUDGET)
            };
            let r =
                Reply::ok(rid, &PaneTextReply { text, truncated }).unwrap_or_else(|e| Reply::Err {
                    id: Some(rid),
                    error: e,
                });
            let _ = tx.send(MainMsg::Ctl(cid, r));
        }))
    }

    fn processes_job(
        &self,
        cid: u64,
        rid: u64,
        id: PaneKey,
        redact: bool,
    ) -> Result<(), WireError> {
        let pty = self
            .panes
            .get(&id)
            .and_then(|p| p.pty.as_ref())
            .ok_or_else(|| err(ErrorCode::NoTarget, "no such pane"))?;
        let shell = pty.pid();
        let fg = pty.foreground_pgid().unwrap_or(shell);
        let tx = self.to_main.clone();
        self.submit(Box::new(move || {
            let r =
                Reply::ok(rid, &list_processes(shell, fg, redact)).unwrap_or_else(|e| Reply::Err {
                    id: Some(rid),
                    error: e,
                });
            let _ = tx.send(MainMsg::Ctl(cid, r));
        }))
    }
}

fn json<T: serde::Serialize>(v: &T) -> Result<Option<serde_json::Value>, WireError> {
    serde_json::to_value(v)
        .map(Some)
        .map_err(|_| err(ErrorCode::Internal, "result not serialisable"))
}

/// A config value as JSON.
fn config_json(v: &apoth_config::Value) -> serde_json::Value {
    use apoth_config::Value as V;
    use serde_json::Value as J;
    match v {
        V::Null => J::Null,
        V::Bool(b) => J::from(*b),
        V::Int(i) => J::from(*i),
        V::Float(f) => J::from(*f),
        V::Str(s) => J::from(s.as_str()),
        V::List(l) => J::from(l.clone()),
        V::Color(c) => J::from(format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])),
        V::Map(m) => J::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), J::from(v.clone())))
                .collect(),
        ),
    }
}

/// `argv` of a process from `/proc` (bounded; `None` when unreadable).
fn read_argv(pid: i32) -> Option<Vec<String>> {
    let raw = apoth_pty::ProcFs.cmdline(pid).ok()?;
    let raw = &raw[..raw.len().min(MAX_ARGV_BYTES)];
    Some(
        raw.split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
    )
}

/// The pane's shell and its descendants (bounded walk; runs on a worker).
fn list_processes(shell: i32, fg: i32, redact: bool) -> ProcessesReply {
    let src = apoth_pty::ProcFs;
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(shell, 0usize)]);
    while let Some((pid, depth)) = queue.pop_front() {
        if out.len() >= MAX_PROCS {
            break;
        }
        let Ok(line) = std::fs::read(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(st) = apoth_pty::parse_stat(&line) else {
            continue;
        };
        out.push(ProcessInfo {
            pid: pid as u32,
            ppid: st.ppid.max(0) as u32,
            foreground: st.pgrp == fg,
            argv: if redact { None } else { read_argv(pid) },
        });
        if depth < apoth_pty::MAX_WALK_DEPTH
            && let Ok(kids) = src.children(pid)
        {
            queue.extend(kids.into_iter().map(|k| (k, depth + 1)));
        }
    }
    ProcessesReply { processes: out }
}
