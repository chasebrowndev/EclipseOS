// SPDX-License-Identifier: AGPL-3.0-only
//! Human control socket (COMP-13 §2).
//!
//! A line-delimited JSON-RPC 2.0 server on `$XDG_RUNTIME_DIR/eclipse/abyss.sock`,
//! mode 0600, for bars, launchers, scripts and `eclipse-ctl`.
//!
//! It is an ordinary calloop source on the compositor's own loop, not a
//! thread: reads are non-blocking and a request is dispatched between two
//! frames like any other event, so a slow or hostile client cannot stall the
//! display. Connection state lives in [`IpcState`] inside `AbyssState`,
//! addressed by `u64` handle — no locks, no `Rc<RefCell<_>>`.
//!
//! Authorisation is [`gate::check`], which runs before any state is touched.
//! This socket deliberately has none of the agent protocol's reach: no
//! `get_tree`, no capture, no grant manipulation (COMP-13 §2).

pub(crate) mod config_rpc;
pub mod gate;
pub(crate) mod methods;

use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
};

use serde_json::{json, Value};
use smithay::{
    desktop::Window,
    reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction, RegistrationToken},
};

use crate::state::AbyssState;
pub use gate::{Decision, Peer};

/// Longest single JSON-RPC line accepted. A request larger than this is a
/// bug or an attack; either way the connection is dropped rather than grown.
const MAX_LINE: usize = 64 * 1024;
/// Bound on the per-connection write queue (COMP-14 §3: no unbounded queues).
/// Events are dropped oldest-first once this is reached; a connection that
/// still cannot make room is disconnected.
const MAX_OUT: usize = 256 * 1024;

/// JSON-RPC error codes. The negative 32xxx range is the standard's; -32000
/// down is ours.
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
pub(crate) const INVALID_PARAMS: i32 = -32602;
pub(crate) const DENIED: i32 = -32000;
const NOT_IMPLEMENTED: i32 = -32001;

/// One connected client.
pub struct Conn {
    pub id: u64,
    pub peer: Peer,
    /// A second descriptor for the same socket, so a reply can be written
    /// from anywhere without borrowing the source's own handle.
    write: UnixStream,
    inbuf: Vec<u8>,
    outbuf: VecDeque<u8>,
    /// Event kinds this connection asked for; empty means not subscribed.
    subs: Vec<String>,
    /// Events discarded because the write queue was full.
    dropped: u64,
    dead: bool,
    token: Option<RegistrationToken>,
}

impl Conn {
    fn enqueue(&mut self, line: &str, droppable: bool) {
        if self.outbuf.len() + line.len() + 1 > MAX_OUT {
            self.flush();
        }
        if self.outbuf.len() + line.len() + 1 > MAX_OUT {
            if droppable {
                // Drop-oldest (COMP-13 §5 decision 1): shed whole lines from
                // the front until the new one fits.
                while self.outbuf.len() + line.len() + 1 > MAX_OUT {
                    match self.outbuf.iter().position(|b| *b == b'\n') {
                        Some(i) => drop(self.outbuf.drain(..=i)),
                        None => break,
                    }
                }
                self.dropped += 1;
            }
            if self.outbuf.len() + line.len() + 1 > MAX_OUT {
                tracing::warn!(conn = self.id, "control client is not reading; disconnecting");
                self.dead = true;
                return;
            }
        }
        self.outbuf.extend(line.as_bytes());
        self.outbuf.push_back(b'\n');
        self.flush();
    }

    fn flush(&mut self) {
        while !self.outbuf.is_empty() {
            let (front, _) = self.outbuf.as_slices();
            let chunk = if front.is_empty() {
                self.outbuf.make_contiguous();
                self.outbuf.as_slices().0
            } else {
                front
            };
            match self.write.write(chunk) {
                Ok(0) => {
                    self.dead = true;
                    return;
                }
                Ok(n) => drop(self.outbuf.drain(..n)),
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => return,
                Err(_) => {
                    self.dead = true;
                    return;
                }
            }
        }
    }
}

/// All control-socket state. Owned by `AbyssState`.
#[derive(Default)]
pub struct IpcState {
    /// Where the socket lives, so it can be unlinked on a clean exit.
    pub path: Option<PathBuf>,
    conns: Vec<Conn>,
    next_conn: u64,
    /// Stable `u64` names for windows, handed out on first sight.
    handles: Vec<(u64, Window)>,
    next_handle: u64,
    /// The connection currently inside its own read callback, if any. Its
    /// calloop source must not be removed from underneath it.
    current: Option<u64>,
}

impl IpcState {
    fn conn_mut(&mut self, id: u64) -> Option<&mut Conn> {
        self.conns.iter_mut().find(|c| c.id == id)
    }

    /// The handle for `window`, minting one if this is the first time it has
    /// been named over IPC.
    pub fn handle_for(&mut self, window: &Window) -> u64 {
        if let Some((h, _)) = self.handles.iter().find(|(_, w)| w == window) {
            return *h;
        }
        self.next_handle += 1;
        let h = self.next_handle;
        self.handles.push((h, window.clone()));
        h
    }

    pub fn window_for(&self, handle: u64) -> Option<Window> {
        self.handles
            .iter()
            .find(|(h, _)| *h == handle)
            .map(|(_, w)| w.clone())
    }

    /// Forget handles for windows that are gone. Called on every window
    /// listing, which bounds the table by the number of live windows.
    fn gc(&mut self) {
        use smithay::utils::IsAlive;
        self.handles.retain(|(_, w)| w.alive());
    }
}

fn socket_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(base).join("eclipse"))
}

/// Bind the control socket and add it to the loop. A failure here is not
/// fatal to the compositor: it comes up without a control socket and says so.
pub fn start(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    let Some(dir) = socket_dir() else {
        tracing::warn!("XDG_RUNTIME_DIR unset; no control socket");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(path = %dir.display(), %e, "creating the control socket directory");
        return;
    }
    // Own-user-only, both the directory and the socket (COMP-13 §2).
    if let Err(e) = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)) {
        tracing::warn!(path = %dir.display(), %e, "tightening the control socket directory");
        return;
    }
    let path = dir.join("abyss.sock");
    if path.exists() {
        // A socket that still answers belongs to a live compositor. Never
        // steal it; a second abyss simply runs without a control socket.
        if UnixStream::connect(&path).is_ok() {
            tracing::error!(path = %path.display(), "control socket already in use; not binding");
            return;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(path = %path.display(), %e, "binding the control socket");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::error!(path = %path.display(), %e, "tightening the control socket; removing it");
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Err(e) = listener.set_nonblocking(true) {
        tracing::warn!(%e, "control socket non-blocking");
        return;
    }

    let inserted = handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => accept(state, stream),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::warn!(%e, "control socket accept");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    );
    if let Err(e) = inserted {
        tracing::warn!(%e, "inserting the control socket source");
        let _ = std::fs::remove_file(&path);
        return;
    }
    tracing::info!(path = %path.display(), "control socket listening");
    state.ipc.path = Some(path);
}

/// Unlink the socket. Called on a clean shutdown so the next start does not
/// have to reason about a stale file.
pub fn cleanup(state: &AbyssState) {
    if let Some(path) = &state.ipc.path {
        let _ = std::fs::remove_file(path);
    }
}

fn owner_uid() -> u32 {
    // SAFETY: `geteuid` is always safe; it cannot fail and touches no memory.
    unsafe { libc::geteuid() }
}

fn accept(state: &mut AbyssState, stream: UnixStream) {
    let peer = match peer_cred(&stream) {
        Some(p) => p,
        None => {
            tracing::warn!("control client has no peer credentials; refused");
            return;
        }
    };
    // Fail-closed at the door as well as at the gate.
    if peer.uid != owner_uid() || peer.pid <= 0 {
        tracing::warn!(uid = peer.uid, "control client is not the session owner; refused");
        return;
    }
    if let Err(e) = stream.set_nonblocking(true) {
        tracing::warn!(%e, "control client non-blocking");
        return;
    }
    let write = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(%e, "duplicating the control client socket");
            return;
        }
    };

    state.ipc.next_conn += 1;
    let id = state.ipc.next_conn;
    state.ipc.conns.push(Conn {
        id,
        peer: peer.clone(),
        write,
        inbuf: Vec::new(),
        outbuf: VecDeque::new(),
        subs: Vec::new(),
        dropped: 0,
        dead: false,
        token: None,
    });

    let token = state.loop_handle.insert_source(
        Generic::new(stream, Interest::READ, Mode::Level),
        move |_, stream, state| Ok(readable(state, id, stream)),
    );
    match token {
        Ok(t) => {
            if let Some(c) = state.ipc.conn_mut(id) {
                c.token = Some(t);
            }
            tracing::info!(conn = id, pid = peer.pid, comm = ?peer.comm, "control client connected");
        }
        Err(e) => {
            tracing::warn!(%e, "inserting a control client source");
            state.ipc.conns.retain(|c| c.id != id);
        }
    }
}

/// `SO_PEERCRED` (COMP-13 §3). The kernel fills this in at `connect` time
/// from the peer's real credentials; it cannot be forged by the peer.
/// `std`'s `peer_cred` is still unstable, hence the raw call.
fn peer_cred(stream: &UnixStream) -> Option<Peer> {
    use std::os::fd::AsRawFd;
    let mut ucred = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `stream` is a live socket and `ucred`/`len` are correctly sized
    // out-parameters for SO_PEERCRED.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 || len as usize != std::mem::size_of::<libc::ucred>() {
        return None;
    }
    Some(Peer {
        uid: ucred.uid,
        pid: ucred.pid,
        comm: comm_of(ucred.pid),
    })
}

/// The peer's process name, for the journal only. Never an authorisation
/// input: `/proc/<pid>/comm` is attacker-controllable by the peer itself.
fn comm_of(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_owned())
}

fn readable(state: &mut AbyssState, id: u64, stream: &UnixStream) -> PostAction {
    let mut buf = [0u8; 8192];
    let mut closed = false;
    loop {
        match (&mut &*stream).read(&mut buf) {
            Ok(0) => {
                closed = true;
                break;
            }
            Ok(n) => {
                let Some(c) = state.ipc.conn_mut(id) else {
                    return PostAction::Remove;
                };
                if c.inbuf.len() + n > MAX_LINE {
                    tracing::warn!(conn = id, "control request over the line limit; disconnecting");
                    c.dead = true;
                    break;
                }
                c.inbuf.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::debug!(conn = id, %e, "control client read");
                closed = true;
                break;
            }
        }
    }

    // Split complete lines out before dispatching: handlers get the whole of
    // `state`, so nothing may stay borrowed out of the connection.
    let mut lines: Vec<Vec<u8>> = Vec::new();
    if let Some(c) = state.ipc.conn_mut(id) {
        while let Some(i) = c.inbuf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = c.inbuf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(line);
        }
    }

    state.ipc.current = Some(id);
    for line in lines {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let reply = handle_line(state, id, &line);
        if let (Some(reply), Some(c)) = (reply, state.ipc.conn_mut(id)) {
            c.enqueue(&reply, false);
        }
    }
    state.ipc.current = None;

    let dead = state
        .ipc
        .conn_mut(id)
        .map(|c| {
            c.flush();
            c.dead
        })
        .unwrap_or(true);
    if closed || dead {
        state.ipc.conns.retain(|c| c.id != id);
        tracing::info!(conn = id, "control client disconnected");
        return PostAction::Remove;
    }
    PostAction::Continue
}

fn error(id: Value, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Parse, authorise, dispatch. Returns the line to write back, or `None` for
/// a JSON-RPC notification (a request with no `id`).
///
/// Request parameters are never logged: they can carry window titles and,
/// with scripted input, keystrokes (COMP-13 §2.2, root invariant on human
/// input).
fn handle_line(state: &mut AbyssState, conn: u64, line: &[u8]) -> Option<String> {
    let req: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(conn, error = %e, "malformed control request");
            return Some(error(Value::Null, PARSE_ERROR, "invalid JSON"));
        }
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let respond = !id.is_null();
    let Some(method) = req.get("method").and_then(Value::as_str) else {
        return respond.then(|| error(id, INVALID_REQUEST, "missing method"));
    };
    let method = method.to_owned();

    let peer = state
        .ipc
        .conns
        .iter()
        .find(|c| c.id == conn)
        .map(|c| c.peer.clone())?;
    // Nothing above this line has read or written compositor state.
    match gate::check(&peer, owner_uid(), &state.config, &method) {
        Decision::Deny(why) => {
            tracing::warn!(conn, pid = peer.pid, comm = ?peer.comm, method = %method, reason = why, "control request denied");
            let code = if gate::lookup(&method).is_none() {
                METHOD_NOT_FOUND
            } else {
                DENIED
            };
            return respond.then(|| error(id, code, why));
        }
        Decision::Allow => {}
    }

    // In the table and authorised, but not built yet. Answered before any
    // state is touched so a caller can tell "refused" from "ignored".
    if !gate::lookup(&method).is_some_and(|e| e.implemented) {
        return respond.then(|| {
            error(
                id,
                NOT_IMPLEMENTED,
                &format!("{method} is specified but not implemented yet"),
            )
        });
    }

    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let result = methods::dispatch(state, conn, &method, &params);
    if !respond {
        return None;
    }
    Some(match result {
        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}).to_string(),
        Err(RpcError { code, message }) => error(id, code, &message),
    })
}

pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn invalid_params(m: &str) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: m.to_owned(),
        }
    }
    pub fn denied(m: &str) -> Self {
        Self {
            code: DENIED,
            message: m.to_owned(),
        }
    }
    pub fn not_implemented(m: &str) -> Self {
        Self {
            code: NOT_IMPLEMENTED,
            message: m.to_owned(),
        }
    }
}

/// Broadcast one event to every subscriber that asked for its kind
/// (COMP-13 §2.1 `subscribe`). Cheap when nobody is listening.
pub fn emit(state: &mut AbyssState, kind: &str, params: Value) {
    if !state.ipc.conns.iter().any(|c| wants(c, kind)) {
        return;
    }
    let line = json!({
        "jsonrpc": "2.0",
        "method": "event",
        "params": {"event": kind, "data": params},
    })
    .to_string();
    for c in state.ipc.conns.iter_mut() {
        if wants(c, kind) {
            c.enqueue(&line, true);
        }
    }
    reap(state);
}

fn wants(c: &Conn, kind: &str) -> bool {
    !c.dead && c.subs.iter().any(|s| s == kind)
}

/// Drop connections that died outside their own callback, taking their
/// calloop source with them. The connection currently being dispatched is
/// skipped: its own callback removes it.
fn reap(state: &mut AbyssState) {
    let current = state.ipc.current;
    let doomed: Vec<(u64, Option<RegistrationToken>)> = state
        .ipc
        .conns
        .iter()
        .filter(|c| c.dead && Some(c.id) != current)
        .map(|c| (c.id, c.token))
        .collect();
    for (id, token) in doomed {
        if let Some(t) = token {
            state.loop_handle.remove(t);
        }
        state.ipc.conns.retain(|c| c.id != id);
        tracing::info!(conn = id, "control client disconnected");
    }
}
