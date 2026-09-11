// SPDX-License-Identifier: AGPL-3.0-only
//! Client for the abyss human control socket (COMP-13 §2).
//!
//! The compositor speaks line-delimited JSON-RPC 2.0 on
//! `$XDG_RUNTIME_DIR/eclipse/abyss.sock`. Every part of the desktop — bar,
//! control centre, settings — talks to it through this crate, so there is one
//! place that knows the wire format and one place to fix when it changes.
//!
//! Three properties are deliberate:
//!
//! * **No authority.** This is the asking half. It has no capability of its
//!   own; the compositor's gate decides, and a [`Error::Denied`] is a normal
//!   outcome to render, not an error to retry around.
//! * **No runtime.** The socket is an fd. [`Client::as_raw_fd`] hands it to
//!   the caller's own `calloop` loop, which is how every DE binary is built.
//! * **No blocking reads on the event path.** [`Client::poll_event`] returns
//!   `None` rather than waiting, so a bar that is also drawing never stalls
//!   on the compositor.
//!
//! A call *does* block, on a socket the compositor services between frames.
//! That is the right trade for a settings toggle and the wrong one for a
//! render loop; see [`Client::call`].

use std::{
    io::{ErrorKind, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    time::Duration,
};

use serde_json::{json, Value};

/// Event kinds the compositor will send (COMP-13 §2.1). Mirrors `EVENTS` in
/// `abyss::ipc::methods`; an unknown kind is refused by the server, so this
/// list existing as a type saves a round trip to find out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Workspace,
    Window,
    Focus,
    Output,
    AgentActivity,
    ConfigError,
}

impl EventKind {
    pub const ALL: &'static [EventKind] = &[
        EventKind::Workspace,
        EventKind::Window,
        EventKind::Focus,
        EventKind::Output,
        EventKind::AgentActivity,
        EventKind::ConfigError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Workspace => "workspace",
            EventKind::Window => "window",
            EventKind::Focus => "focus",
            EventKind::Output => "output",
            EventKind::AgentActivity => "agent-activity",
            EventKind::ConfigError => "config-error",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        EventKind::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

/// One notification from the compositor.
#[derive(Debug, Clone)]
pub struct Event {
    pub kind: EventKind,
    /// The notification's `params`, whatever shape that kind carries.
    pub data: Value,
}

#[derive(Debug)]
pub enum Error {
    /// The socket is not there, or not ours. A shell binary started before
    /// the compositor sees this and should retry, not fail.
    Connect(String),
    Io(std::io::Error),
    /// The compositor refused. `code` is the JSON-RPC code — `-32000` is the
    /// gate saying no, which is a thing to show the user, not a bug.
    Rpc {
        code: i32,
        message: String,
    },
    /// Something arrived that is not the protocol.
    Protocol(String),
}

impl Error {
    /// True when the gate refused (COMP-13 §2.2). Worth distinguishing: a
    /// denial is a policy answer, and a GUI shows it rather than retrying.
    pub fn is_denied(&self) -> bool {
        matches!(self, Error::Rpc { code: -32000, .. })
    }

    /// True when the method is not implemented yet (`-32001`). A pane can
    /// grey itself out instead of showing an error.
    pub fn is_unimplemented(&self) -> bool {
        matches!(self, Error::Rpc { code: -32001, .. })
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connect(m) => write!(f, "cannot reach the compositor: {m}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Rpc { code, message } => write!(f, "{message} (code {code})"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

type Result<T> = std::result::Result<T, Error>;

/// Longest line accepted from the compositor. Matches the server's own cap;
/// anything longer is a bug on one side and we drop the connection rather
/// than grow a buffer to meet it.
const MAX_LINE: usize = 64 * 1024;

/// The default socket path. `$XDG_RUNTIME_DIR` is per-user and mode 0700,
/// which is what makes an unauthenticated socket acceptable here.
pub fn default_socket() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("eclipse/abyss.sock"))
}

pub struct Client {
    sock: UnixStream,
    inbuf: Vec<u8>,
    next_id: u64,
    /// Events that arrived while a call was waiting for its reply. The
    /// compositor interleaves them freely, and dropping them because the user
    /// happened to click something is how a bar goes stale.
    pending: std::collections::VecDeque<Event>,
}

impl Client {
    /// Connect to the default socket.
    pub fn connect() -> Result<Client> {
        let path = default_socket().ok_or_else(|| Error::Connect("XDG_RUNTIME_DIR is not set".into()))?;
        Client::connect_to(&path)
    }

    pub fn connect_to(path: &std::path::Path) -> Result<Client> {
        let sock =
            UnixStream::connect(path).map_err(|e| Error::Connect(format!("{}: {e}", path.display())))?;
        // The server checks our uid; we check its. A socket at this path
        // owned by someone else is either a mistake or someone waiting to be
        // handed a settings change, and either way we do not talk to it.
        let peer = peer_uid(&sock)?;
        // SAFETY: getuid is always safe; it reads a process property.
        let us = unsafe { libc::getuid() };
        if peer != us {
            return Err(Error::Connect(format!(
                "{} is owned by uid {peer}, not {us}",
                path.display()
            )));
        }
        sock.set_nonblocking(true)?;
        Ok(Client {
            sock,
            inbuf: Vec::new(),
            next_id: 1,
            pending: std::collections::VecDeque::new(),
        })
    }

    /// The socket, for the caller's own event loop. Register it readable and
    /// call [`Client::poll_event`] until it returns `None`.
    pub fn as_raw_fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }

    /// Send a request and wait for its reply, queueing any events that arrive
    /// first.
    ///
    /// This blocks. The compositor dispatches this socket between frames on
    /// its own loop, so the wait is bounded by a frame in practice — fine for
    /// a button press, wrong inside a paint. Anything on a hot path should
    /// subscribe instead of polling.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_line(&req.to_string())?;

        loop {
            let line = self.read_line_blocking()?;
            let msg: Value =
                serde_json::from_str(&line).map_err(|e| Error::Protocol(format!("{e}: {line:?}")))?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(Error::Rpc {
                        code: err.get("code").and_then(Value::as_i64).unwrap_or(0) as i32,
                        message: err
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                    });
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            if let Some(ev) = as_event(&msg) {
                self.pending.push_back(ev);
            }
            // Anything else is a reply to a request we no longer care about.
            // Dropping it is correct: ids are never reused.
        }
    }

    /// Ask for these event kinds. Replaces any previous subscription, which
    /// is the server's semantics — `subs` is assigned, not extended.
    pub fn subscribe(&mut self, kinds: &[EventKind]) -> Result<()> {
        let names: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        self.call("subscribe", json!({"events": names}))?;
        Ok(())
    }

    pub fn unsubscribe(&mut self) -> Result<()> {
        self.call("unsubscribe", json!({}))?;
        Ok(())
    }

    /// Next event, or `None` when the socket has nothing right now.
    ///
    /// Never blocks. `Err` here means the connection is gone and the client
    /// should be rebuilt — the compositor restarting is an ordinary thing for
    /// a bar to survive.
    pub fn poll_event(&mut self) -> Result<Option<Event>> {
        if let Some(ev) = self.pending.pop_front() {
            return Ok(Some(ev));
        }
        loop {
            let Some(line) = self.read_line_nonblocking()? else {
                return Ok(None);
            };
            let msg: Value =
                serde_json::from_str(&line).map_err(|e| Error::Protocol(format!("{e}: {line:?}")))?;
            if let Some(ev) = as_event(&msg) {
                return Ok(Some(ev));
            }
        }
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        // The write side stays blocking in effect: a request is small enough
        // that a partial write means the compositor is wedged, and looping
        // here is simpler than a second outbound queue in every client.
        self.sock.set_nonblocking(false)?;
        let r = self
            .sock
            .write_all(line.as_bytes())
            .and_then(|()| self.sock.write_all(b"\n"))
            .and_then(|()| self.sock.flush());
        self.sock.set_nonblocking(true)?;
        r.map_err(Error::Io)
    }

    fn take_line(&mut self) -> Option<String> {
        let pos = self.inbuf.iter().position(|b| *b == b'\n')?;
        let line = self.inbuf.drain(..=pos).collect::<Vec<_>>();
        Some(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned())
    }

    fn read_line_nonblocking(&mut self) -> Result<Option<String>> {
        if let Some(l) = self.take_line() {
            return Ok(Some(l));
        }
        let mut buf = [0u8; 4096];
        loop {
            match self.sock.read(&mut buf) {
                Ok(0) => return Err(Error::Connect("the compositor closed the socket".into())),
                Ok(n) => {
                    if self.inbuf.len() + n > MAX_LINE {
                        return Err(Error::Protocol("line too long".into()));
                    }
                    self.inbuf.extend_from_slice(&buf[..n]);
                    if let Some(l) = self.take_line() {
                        return Ok(Some(l));
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e)),
            }
        }
    }

    fn read_line_blocking(&mut self) -> Result<String> {
        loop {
            if let Some(l) = self.read_line_nonblocking()? {
                return Ok(l);
            }
            // Wait on the fd rather than spinning. No timeout: the socket is
            // the compositor, and if it is gone the read returns 0 and we
            // report a closed connection.
            wait_readable(self.sock.as_raw_fd(), Duration::from_millis(500))?;
        }
    }
}

/// A JSON-RPC notification (no `id`) whose method names an event kind.
fn as_event(msg: &Value) -> Option<Event> {
    if msg.get("id").is_some() {
        return None;
    }
    let kind = EventKind::parse(msg.get("method")?.as_str()?)?;
    Some(Event {
        kind,
        data: msg.get("params").cloned().unwrap_or(Value::Null),
    })
}

fn peer_uid(sock: &UnixStream) -> Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are correctly sized for SO_PEERCRED on a
    // connected AF_UNIX socket, which is what `sock` is.
    let rc = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    Ok(cred.uid)
}

fn wait_readable(fd: RawFd, timeout: Duration) -> Result<()> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, count 1.
    let rc = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis() as libc::c_int) };
    if rc < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(Error::Io(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_kind_round_trips_through_its_wire_name() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(*k));
        }
        assert_eq!(EventKind::parse("nonsense"), None);
    }

    #[test]
    fn a_reply_is_not_mistaken_for_an_event() {
        let reply = json!({"jsonrpc": "2.0", "id": 7, "result": {}});
        assert!(as_event(&reply).is_none());
        let ev = json!({"jsonrpc": "2.0", "method": "focus", "params": {"window": 3}});
        assert_eq!(as_event(&ev).unwrap().kind, EventKind::Focus);
        // A notification for something we do not model is ignored rather than
        // guessed at, so a server that grows a kind does not crash a bar.
        let unknown = json!({"jsonrpc": "2.0", "method": "future-thing", "params": {}});
        assert!(as_event(&unknown).is_none());
    }

    #[test]
    fn a_denial_is_distinguishable_from_a_failure() {
        let denied = Error::Rpc {
            code: -32000,
            message: "denied".into(),
        };
        assert!(denied.is_denied());
        assert!(!denied.is_unimplemented());
        assert!(!Error::Protocol("x".into()).is_denied());
    }
}
