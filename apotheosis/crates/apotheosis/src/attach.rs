// SPDX-License-Identifier: AGPL-3.0-only
//! The interface as a view of the session daemon (T-21 §3): `apotheosis --attach`, and
//! every window while `sessions.server` is `on`.
//!
//! The handshake (hello, attach, the pane list) runs on main before the window exists. Then
//! a demultiplexer thread reads the server's records: it rebuilds each pane's term from its
//! snapshot (`Term::restore`, off main), checks the stream's sequence numbers (a gap asks
//! for a resync), and hands every pane its stream over a `SOCK_SEQPACKET` socket pair the
//! pane's io thread reads as it would read a PTY ([`PKT_OUTPUT`], [`PKT_RESIZE`],
//! [`PKT_SNAPSHOT`]). A writer thread sends the view's records (input, resizes, spawns), so
//! main never blocks on the server.
//!
//! Replies the view's terms produce are dropped: the server answered them. Nothing here
//! logs input or pane content.
//!
//! A mux domain (T-22 §5, `domain { ssh "host"; mux #true }`) reaches a remote daemon the
//! same way: the system `ssh` runs `apoth proxy` on the host with its stdin and stdout on one
//! end of a socket pair, so the records cross ssh stdio with no listener and no port. Its
//! handshake runs on the demultiplexer thread (a password may be asked on the askpass sheet,
//! which needs the window), and when the link drops the thread reconnects with back-off and
//! the server's fresh snapshots replace each pane's term: output and processes are the
//! server's, so nothing is lost but the input typed while the link was down (dropped, not
//! queued, so nothing typed blind lands later).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use apoth_proto::mux::{
    self, Decoded, MAX_CHUNK, MUX_MAJOR, MUX_MINOR, PaneEntry, RecordDecoder, ServerMsg, ViewMsg,
};
use apoth_term::{Term, TermConfig, TermImage};
use rustix::net::{AddressFamily, SendFlags, SocketFlags, SocketType};

use crate::pane::{Mailbox, MainMsg, PaneKey};

/// Packet tag: PTY output follows.
pub(crate) const PKT_OUTPUT: u8 = 0;
/// Packet tag: rows and columns (u16 LE each) at this point of the stream.
pub(crate) const PKT_RESIZE: u8 = 1;
/// Packet tag: a restored term waits in the mailbox; swap it in.
pub(crate) const PKT_SNAPSHOT: u8 = 2;

/// How long the handshake may take.
const HANDSHAKE: Duration = Duration::from_secs(5);
/// How long an ssh handshake may take (it may wait for a password on the sheet).
const SSH_HANDSHAKE: Duration = Duration::from_secs(120);
/// Reconnect back-off: first wait and cap.
const BACKOFF: (Duration, Duration) = (Duration::from_millis(500), Duration::from_secs(30));
/// `apoth proxy` exit: no session daemon on the host.
const PROXY_NO_SERVER: i32 = 4;
/// A shell's "command not found".
const NOT_FOUND: i32 = 127;
/// Pages one snapshot may carry (both screens).
const MAX_SNAP_PAGES: usize = 2 * apoth_term::snapshot::SNAP_PAGES_MAX;

/// A pane's end of its stream, as main receives it.
pub(crate) struct NewPane {
    /// The server's pane id (`None` until a spawn is answered).
    pub(crate) sid: Option<u64>,
    /// The io thread reads this (non-blocking).
    pub(crate) rx: OwnedFd,
    pub(crate) mailbox: Mailbox,
}

/// The demultiplexer's end.
struct Slot {
    tx: OwnedFd,
    mailbox: Mailbox,
}

/// What the demultiplexer tells main.
pub(crate) enum Event {
    /// The server has a pane this view has no tab for: open one.
    Added(NewPane),
    /// A spawn (token = the local pane id) got its server pane.
    Bound { token: PaneKey, sid: u64 },
    /// A spawn was refused or failed.
    SpawnFailed { token: PaneKey },
    /// The pane's process exited on the server.
    Exited { sid: u64, status: Option<i32> },
    /// The stream ended: detached by `apoth detach`, refused, or the server is gone.
    Lost(String),
    /// A mux domain's first connection bound the window's first pane to server pane `sid`.
    FirstBound { sid: u64 },
    /// A mux domain's link dropped; reconnecting (`true`) or back (`false`).
    Reconnecting(bool),
}

/// Where a mux domain's daemon is (T-22 §5).
#[derive(Clone, Debug)]
pub(crate) struct SshTarget {
    /// The `ssh` destination (validated by the config).
    pub(crate) dest: String,
    /// The remote session name.
    pub(crate) session: String,
}

/// What `app::run` attaches to.
pub(crate) enum Attach {
    /// A local daemon, already greeted (M35).
    Local(Handshake),
    /// A remote daemon over ssh; connected on the demultiplexer thread.
    Ssh(SshTarget),
}

/// An outgoing record; input is dropped while a link is down.
enum Out {
    Rec(Vec<u8>),
    Input(Vec<u8>),
}

/// The writer's current stream (swapped on reconnect).
#[derive(Default)]
struct Wire {
    stream: Mutex<Option<UnixStream>>,
    cv: Condvar,
    /// The window is going: stop reconnecting, stop waiting.
    closed: AtomicBool,
}

impl Wire {
    fn set(&self, s: Option<UnixStream>) {
        *self
            .stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = s;
        self.cv.notify_all();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.cv.notify_all();
    }
}

/// Main's handle on the attachment.
pub(crate) struct Link {
    tx: mpsc::Sender<Out>,
    wire: Arc<Wire>,
    /// A mux domain's host (for the reconnect notice).
    pub(crate) host: Option<String>,
    tokens: Arc<Mutex<HashMap<PaneKey, Slot>>>,
    pub(crate) session: String,
    /// The window is closing: panes are left running, never closed.
    pub(crate) detaching: bool,
    /// Server pane id to local pane id.
    pub(crate) sids: HashMap<u64, PaneKey>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.wire.close();
    }
}

impl Link {
    fn send(&self, m: &ViewMsg) {
        let _ = self.tx.send(Out::Rec(m.encode()));
    }

    /// Bytes for a server pane, in records of at most one chunk. Never logged.
    pub(crate) fn input(&self, sid: u64, bytes: &[u8]) {
        for c in bytes.chunks(MAX_CHUNK) {
            let rec = ViewMsg::Input {
                pane: sid,
                bytes: c.to_vec(),
            };
            let _ = self.tx.send(Out::Input(rec.encode()));
        }
    }

    pub(crate) fn resize(&self, sid: u64, (rows, cols): (usize, usize), cell: (u32, u32)) {
        let sat = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        let px = |n: u32| u16::try_from(n).unwrap_or(0);
        self.send(&ViewMsg::Resize {
            pane: sid,
            rows: sat(rows),
            cols: sat(cols),
            cell_w: px(cell.0),
            cell_h: px(cell.1),
        });
    }

    pub(crate) fn close(&self, sid: u64) {
        self.send(&ViewMsg::Close { pane: sid });
    }

    pub(crate) fn detach(&mut self) {
        if !self.detaching {
            self.detaching = true;
            self.send(&ViewMsg::Detach);
            // No reconnect after this; the queued `Detach` still goes out on a live link.
            self.wire.closed.store(true, Ordering::Release);
        }
    }

    /// Close every pane of the session and end its server (T-21 §3 kill, tmux
    /// `kill-session`).
    pub(crate) fn kill_session(&mut self) {
        self.send(&ViewMsg::KillSession);
    }

    /// Ask the server for a new pane; its stream arrives on the returned end.
    pub(crate) fn spawn(
        &self,
        token: PaneKey,
        argv: Vec<String>,
        cwd: Option<&Path>,
        (rows, cols): (usize, usize),
    ) -> io::Result<NewPane> {
        let (slot, np) = pair(None)?;
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(token, slot);
        let sat = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        self.send(&ViewMsg::Spawn {
            token,
            argv,
            cwd: cwd.and_then(|p| p.to_str()).map(str::to_owned),
            rows: sat(rows),
            cols: sat(cols),
        });
        Ok(np)
    }
}

/// A stream end pair for one pane.
fn pair(sid: Option<u64>) -> io::Result<(Slot, NewPane)> {
    let (tx, rx) = rustix::net::socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::io::ioctl_fionbio(&rx, true)?;
    let mailbox: Mailbox = Arc::new(Mutex::new(None));
    Ok((
        Slot {
            tx,
            mailbox: Arc::clone(&mailbox),
        },
        NewPane { sid, rx, mailbox },
    ))
}

/// A finished handshake: the stream, what was read past the pane list, and the panes.
pub(crate) struct Handshake {
    stream: UnixStream,
    dec: RecordDecoder,
    pub(crate) session: String,
    pub(crate) panes: Vec<PaneEntry>,
}

/// `$XDG_RUNTIME_DIR/apotheosis/server-<session>.sock`.
pub(crate) fn socket(session: &str) -> Option<PathBuf> {
    crate::server::socket_path(session)
}

/// Connect, say hello and attach. `Err` is the user-facing reason.
pub(crate) fn connect(session: &str) -> Result<Handshake, String> {
    let path = socket(session).ok_or("XDG_RUNTIME_DIR is not set")?;
    let stream =
        UnixStream::connect(&path).map_err(|e| format!("no session server {session:?} ({e})"))?;
    greet(stream, session, HANDSHAKE, &mut || None)
}

/// Hello, attach, wait for the pane list. `gone` reports a transport that ended early
/// (an ssh child that exited) with its reason.
fn greet(
    mut stream: UnixStream,
    session: &str,
    within: Duration,
    gone: &mut dyn FnMut() -> Option<String>,
) -> Result<Handshake, String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| e.to_string())?;
    let hello = ViewMsg::Hello {
        major: MUX_MAJOR,
        minor: MUX_MINOR,
        caps: Vec::new(),
        client: "apotheosis".into(),
    };
    stream
        .write_all(&hello.encode())
        .and_then(|()| stream.write_all(&ViewMsg::Attach.encode()))
        .map_err(|e| gone().unwrap_or_else(|| e.to_string()))?;
    let mut dec = RecordDecoder::new();
    let until = Instant::now() + within;
    let mut welcomed = false;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        while let Some(r) = dec.next_record() {
            let m = match r.map(ServerMsg::decode) {
                Ok(Ok(Decoded::Msg(m))) => m,
                Ok(Ok(Decoded::Skipped(_))) => continue,
                _ => return Err("the session server sent a malformed record".into()),
            };
            match m {
                ServerMsg::Welcome { major, minor, .. } => {
                    mux::check_version(major, minor)?;
                    welcomed = true;
                }
                ServerMsg::Refused { reason } => return Err(reason),
                ServerMsg::Panes(panes) if welcomed => {
                    stream.set_read_timeout(None).map_err(|e| e.to_string())?;
                    return Ok(Handshake {
                        stream,
                        dec,
                        session: session.to_owned(),
                        panes,
                    });
                }
                _ => return Err("the session server answered out of order".into()),
            }
        }
        if Instant::now() > until {
            return Err("the session server does not answer".into());
        }
        match stream.read(&mut buf) {
            Ok(0) => {
                return Err(
                    gone().unwrap_or_else(|| "the session server closed the connection".into())
                );
            }
            Ok(n) => dec.feed(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                if let Some(why) = gone() {
                    return Err(why);
                }
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Why an ssh link ended, from ssh's exit status; `true` when reconnecting cannot help.
#[must_use]
pub(crate) fn ssh_exit_reason(dest: &str, code: Option<i32>) -> (String, bool) {
    match code {
        Some(PROXY_NO_SERVER) => (format!("the session on {dest} has ended"), true),
        Some(NOT_FOUND) => (
            format!(
                "{dest} has no Apotheosis (apoth proxy was not found); install it there with the package manager"
            ),
            true,
        ),
        Some(255) => (format!("ssh could not reach {dest}"), false),
        Some(c) => (format!("the link to {dest} ended (ssh exit {c})"), false),
        None => (format!("the link to {dest} ended"), false),
    }
}

/// The remote command: `apoth proxy` for `session`, never starting a daemon on reconnect.
#[must_use]
pub(crate) fn proxy_command(session: &str, first: bool) -> String {
    if first {
        format!("apoth proxy --session {session}")
    } else {
        format!("apoth proxy --session {session} --no-start")
    }
}

/// An ssh link being opened: the stream end and the child.
struct SshConn {
    stream: UnixStream,
    child: Child,
}

/// Start `ssh … DEST apoth proxy` with stdio on a socket pair (T-22 §5). The private master
/// is reused (§4); askpass is armed for this ssh child when `sink` is given (§3).
fn spawn_ssh(
    t: &SshTarget,
    first: bool,
    sink: Option<&dyn Fn() -> crate::askpass::Sink>,
) -> io::Result<SshConn> {
    use std::os::unix::process::CommandExt;
    let (ours, theirs) = UnixStream::pair()?;
    let out = theirs.try_clone()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let dir = runtime.as_deref().and_then(|rt| {
        apoth_pty::ssh::prepare_control_dir(rt, rustix::process::getuid().as_raw()).ok()
    });
    let master = apoth_pty::ssh::master_args(dir.as_deref());
    if dir.is_some() {
        let _ = apoth_pty::ssh::probe_master(
            std::ffi::OsStr::new("ssh"),
            &master,
            &t.dest,
            apoth_pty::ssh::PROBE_TIMEOUT,
        );
    }
    let mut c = Command::new("ssh");
    c.args(&master)
        .args(["-T", "--", &t.dest])
        .arg(proxy_command(&t.session, first))
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .stdout(Stdio::from(OwnedFd::from(out)))
        .stderr(Stdio::null())
        .env_remove(apoth_proto::askpass::ENV_SOCK)
        .env_remove(apoth_proto::askpass::ENV_TOKEN)
        .process_group(0);
    let armed = match (sink, crate::ctl::path::socket_dir(runtime.as_deref())) {
        (Some(_), Some(d)) => crate::askpass::Prepared::new(&d, &t.dest).ok(),
        _ => None,
    };
    if let (Some(a), Some(me)) = (&armed, apoth_exe()) {
        c.env("SSH_ASKPASS", me)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(apoth_proto::askpass::ENV_SOCK, a.socket())
            .env(apoth_proto::askpass::ENV_TOKEN, a.token_hex());
    }
    let child = c.spawn()?;
    drop(c);
    if let (Some(a), Some(mk)) = (armed, sink)
        && let Ok(pid) = i32::try_from(child.id())
        && let Err(e) = a.start(pid, mk())
    {
        tracing::debug!(error = %e, "askpass not armed for the mux link");
    }
    Ok(SshConn {
        stream: ours,
        child,
    })
}

/// The `apoth` binary next to ours, else by `PATH`.
fn apoth_exe() -> Option<PathBuf> {
    let sib = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("apoth")))
        .filter(|p| p.is_file());
    Some(sib.unwrap_or_else(|| PathBuf::from("apoth")))
}

/// Reap an ssh child (bounded), its exit code if it has one.
fn reap(child: &mut Child, within: Duration) -> Option<i32> {
    let until = Instant::now() + within;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.code(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                // Our own child, by its handle.
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Make sure the session's server runs (`sessions.server on`): start this binary as one
/// when nobody listens, and wait for its socket (bounded).
pub(crate) fn ensure_server(session: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let path = socket(session).ok_or("XDG_RUNTIME_DIR is not set")?;
    if UnixStream::connect(&path).is_ok() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(exe)
        .args(["--server", "--session", session])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start the session server: {e}"))?;
    // Reap it whenever it ends (it normally outlives this window).
    let _ = std::thread::Builder::new()
        .name("apoth-server-reap".into())
        .spawn(move || {
            let _ = child.wait();
        });
    let until = Instant::now() + Duration::from_secs(2);
    while UnixStream::connect(&path).is_err() {
        if Instant::now() > until {
            return Err("the session server did not start".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Start the reader and writer threads. With `adopt_first`, the first server pane is the
/// window's first pane (returned); the others arrive as [`Event::Added`]. For a mux domain
/// the returned first pane is bound once the link is up ([`Event::FirstBound`]).
pub(crate) fn start(
    a: Attach,
    cfg: TermConfig,
    to_main: calloop::channel::Sender<MainMsg>,
    adopt_first: bool,
) -> io::Result<(Link, Option<NewPane>)> {
    let (tx, rx) = mpsc::channel::<Out>();
    let wire: Arc<Wire> = Arc::default();
    let w = Arc::clone(&wire);
    std::thread::Builder::new()
        .name("apoth-attach-w".into())
        .spawn(move || writer(&w, &rx))?;
    let tokens: Arc<Mutex<HashMap<PaneKey, Slot>>> = Arc::default();
    let (session, host, stream, dec, panes, ssh) = match a {
        Attach::Local(h) => {
            wire.set(Some(h.stream.try_clone()?));
            (h.session, None, Some(h.stream), h.dec, h.panes, None)
        }
        Attach::Ssh(t) => (
            t.session.clone(),
            Some(t.dest.clone()),
            None,
            RecordDecoder::new(),
            Vec::new(),
            Some(t),
        ),
    };
    let mut demux = Demux {
        stream,
        dec,
        cfg,
        to_main,
        tokens: Arc::clone(&tokens),
        panes: HashMap::new(),
        building: HashMap::new(),
        wire: Arc::clone(&wire),
        tx: tx.clone(),
        ssh,
        child: None,
        first_slot: None,
        connected_once: false,
    };
    let mut first = None;
    if adopt_first {
        if demux.ssh.is_some() {
            let (slot, np) = pair(None)?;
            demux.first_slot = Some(slot);
            first = Some(np);
        } else if let Some(p) = panes.first() {
            let (slot, np) = pair(Some(p.pane))?;
            demux.panes.insert(p.pane, DPane::new(slot));
            first = Some(np);
        }
    }
    std::thread::Builder::new()
        .name("apoth-attach-r".into())
        .spawn(move || demux.run())?;
    Ok((
        Link {
            tx,
            wire,
            host,
            tokens,
            session,
            detaching: false,
            // Filled as panes are made (the first pane's local id is the window's).
            sids: HashMap::new(),
        },
        first,
    ))
}

/// The writer thread: records in order to the current stream. While a link is down, input
/// is dropped and other records wait for the next stream.
fn writer(wire: &Wire, rx: &mpsc::Receiver<Out>) {
    'next: while let Ok(o) = rx.recv() {
        let (rec, input) = match o {
            Out::Rec(r) => (r, false),
            Out::Input(r) => (r, true),
        };
        let mut g = wire
            .stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while g.is_none() {
            if input || wire.closed.load(Ordering::Acquire) {
                continue 'next;
            }
            g = wire
                .cv
                .wait(g)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if let Some(s) = g.as_mut()
            && s.write_all(&rec).is_err()
        {
            *g = None;
        }
    }
}

struct DPane {
    slot: Slot,
    /// Holds a snapshot whose stream it follows.
    synced: bool,
    next: u64,
}

impl DPane {
    fn new(slot: Slot) -> DPane {
        DPane {
            slot,
            synced: false,
            next: 0,
        }
    }

    /// One packet to the io thread; blocks while it is behind (the server then sees a slow
    /// view and resyncs it). False once the io thread is gone.
    fn packet(&self, tag: u8, body: &[u8], scratch: &mut Vec<u8>) -> bool {
        scratch.clear();
        scratch.push(tag);
        scratch.extend_from_slice(body);
        loop {
            match rustix::net::send(&self.slot.tx, scratch, SendFlags::NOSIGNAL) {
                Ok(_) => return true,
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return false,
            }
        }
    }
}

/// A snapshot being received: (sequence, state, pages per screen).
type Building = (u64, Vec<u8>, [Vec<Vec<u8>>; 2]);

struct Demux {
    /// `None` until a mux domain's first connection.
    stream: Option<UnixStream>,
    dec: RecordDecoder,
    cfg: TermConfig,
    to_main: calloop::channel::Sender<MainMsg>,
    tx: mpsc::Sender<Out>,
    wire: Arc<Wire>,
    tokens: Arc<Mutex<HashMap<PaneKey, Slot>>>,
    panes: HashMap<u64, DPane>,
    /// Snapshots being received: (sequence, state, pages per screen).
    building: HashMap<u64, Building>,
    /// A mux domain: reconnect on loss.
    ssh: Option<SshTarget>,
    child: Option<Child>,
    /// The window's first pane, bound to the first server pane once connected.
    first_slot: Option<Slot>,
    /// A link was up once: later connections never start a daemon on the host.
    connected_once: bool,
}

impl Demux {
    fn event(&self, e: Event) {
        let _ = self.to_main.send(MainMsg::Remote(e));
    }

    fn run(mut self) {
        let why = loop {
            if self.stream.is_none() {
                match self.connect_ssh() {
                    Ok(()) => {}
                    Err(why) => break why,
                }
            }
            let (why, last) = self.pump();
            self.stream = None;
            self.wire.set(None);
            let Some(t) = self.ssh.clone() else { break why };
            let (why, last) = match self.child.as_mut().map(|c| reap(c, Duration::from_secs(2))) {
                Some(code) if !last => {
                    let (w, l) = ssh_exit_reason(&t.dest, code);
                    (if code.is_some() { w } else { why }, l)
                }
                _ => (why, last),
            };
            self.child = None;
            if last || self.wire.closed.load(Ordering::Acquire) {
                break why;
            }
            tracing::info!("mux link lost; reconnecting");
            self.event(Event::Reconnecting(true));
            for p in self.panes.values_mut() {
                p.synced = false;
            }
            self.building.clear();
        };
        if let Some(c) = self.child.as_mut() {
            let _ = reap(c, Duration::from_millis(500));
        }
        // Every pane's stream ends with ours.
        self.panes.clear();
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.event(Event::Lost(why));
    }

    /// Read records until the stream ends: the reason and whether it is final (a refusal or
    /// a malformed record; a plain end may be a dropped link).
    fn pump(&mut self) -> (String, bool) {
        let mut buf = vec![0u8; 256 * 1024];
        let mut scratch = Vec::with_capacity(MAX_CHUNK + 16);
        let Some(mut stream) = self.stream.take() else {
            return ("not connected".into(), true);
        };
        let r = 'outer: loop {
            while let Some(r) = self.dec.next_record() {
                let m = match r.map(ServerMsg::decode) {
                    Ok(Ok(Decoded::Msg(m))) => m,
                    Ok(Ok(Decoded::Skipped(_))) => continue,
                    _ => {
                        break 'outer (
                            "the session server sent a malformed record".to_owned(),
                            true,
                        );
                    }
                };
                if let Some(why) = self.on_msg(m, &mut scratch) {
                    break 'outer (why, true);
                }
            }
            match stream.read(&mut buf) {
                Ok(0) => break ("the session server went away".to_owned(), false),
                Ok(n) => self.dec.feed(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break ("the session server went away".to_owned(), false),
            }
        };
        self.dec = RecordDecoder::new();
        r
    }

    /// Open (or reopen) a mux domain's link, with back-off; `Err` is final.
    fn connect_ssh(&mut self) -> Result<(), String> {
        let Some(t) = self.ssh.clone() else {
            return Err("not a mux domain".into());
        };
        let first = !self.connected_once;
        let mut wait = BACKOFF.0;
        loop {
            if self.wire.closed.load(Ordering::Acquire) {
                return Err("detached".into());
            }
            let to_main = self.to_main.clone();
            let mk = move || -> crate::askpass::Sink {
                let tx = to_main.clone();
                Box::new(move |e| {
                    let _ = tx.send(MainMsg::Askpass(e));
                })
            };
            let attempt = spawn_ssh(&t, first, Some(&mk))
                .map_err(|e| (format!("cannot run ssh: {e}"), true))
                .and_then(|mut c| {
                    let stream = c.stream.try_clone().map_err(|e| (e.to_string(), true))?;
                    let child = &mut c.child;
                    let mut gone = || match child.try_wait() {
                        Ok(Some(s)) => Some(ssh_exit_reason(&t.dest, s.code()).0),
                        _ => None,
                    };
                    match greet(stream, &t.session, SSH_HANDSHAKE, &mut gone) {
                        Ok(h) => Ok((h, c)),
                        Err(why) => {
                            let code = reap(&mut c.child, Duration::from_secs(2));
                            let (w, last) = ssh_exit_reason(&t.dest, code);
                            // A refusal or version skew is the server's word: final.
                            let refused = code.is_none() || code == Some(0);
                            Err((if refused { why } else { w }, last || refused))
                        }
                    }
                });
            match attempt {
                Ok((h, c)) => {
                    drop(c.stream);
                    self.child = Some(c.child);
                    self.adopt(h)?;
                    return Ok(());
                }
                Err((why, true)) => return Err(why),
                Err((why, false)) if first => return Err(why),
                Err((_, false)) => {
                    tracing::debug!(wait_ms = wait.as_millis() as u64, "mux link still down");
                    std::thread::sleep(wait);
                    wait = (wait * 2).min(BACKOFF.1);
                }
            }
        }
    }

    /// Take a fresh handshake: panes gone meanwhile have exited, the first pane binds, the
    /// writer gets the stream. The server sends every pane's snapshot after this.
    fn adopt(&mut self, h: Handshake) -> Result<(), String> {
        let Handshake {
            stream, dec, panes, ..
        } = h;
        let alive: Vec<u64> = panes.iter().filter(|p| p.alive).map(|p| p.pane).collect();
        let gone: Vec<u64> = self
            .panes
            .keys()
            .copied()
            .filter(|s| !alive.contains(s))
            .collect();
        for sid in gone {
            self.panes.remove(&sid);
            self.event(Event::Exited { sid, status: None });
        }
        if let Some(slot) = self.first_slot.take() {
            let Some(&sid) = alive.first() else {
                return Err("the remote session has no panes".into());
            };
            self.panes.insert(sid, DPane::new(slot));
            self.event(Event::FirstBound { sid });
        } else {
            self.event(Event::Reconnecting(false));
        }
        self.wire
            .set(Some(stream.try_clone().map_err(|e| e.to_string())?));
        self.stream = Some(stream);
        self.dec = dec;
        self.connected_once = true;
        Ok(())
    }

    fn resync(&mut self, sid: u64) {
        if let Some(p) = self.panes.get_mut(&sid) {
            p.synced = false;
        }
        let _ = self
            .tx
            .send(Out::Rec(ViewMsg::Resync { pane: sid }.encode()));
    }

    fn on_msg(&mut self, m: ServerMsg, scratch: &mut Vec<u8>) -> Option<String> {
        match m {
            ServerMsg::Refused { reason } => return Some(reason),
            ServerMsg::Welcome { .. } | ServerMsg::Panes(_) => {}
            ServerMsg::SnapBegin {
                pane, seq, state, ..
            } => {
                self.building
                    .insert(pane, (seq, state, [Vec::new(), Vec::new()]));
            }
            ServerMsg::SnapPage {
                pane,
                screen,
                bytes,
            } => {
                if let Some(b) = self.building.get_mut(&pane) {
                    if b.2[0].len() + b.2[1].len() >= MAX_SNAP_PAGES || screen > 1 {
                        self.building.remove(&pane);
                    } else {
                        b.2[usize::from(screen)].push(bytes);
                    }
                }
            }
            ServerMsg::SnapEnd { pane } => {
                let (seq, state, pages) = self.building.remove(&pane)?;
                let term = match Term::restore(
                    self.cfg.clone(),
                    TermImage { state, pages },
                    Instant::now(),
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(pane, "a pane snapshot was refused: {e}");
                        return None;
                    }
                };
                let mut added = None;
                let p = match self.panes.entry(pane) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => match pair(Some(pane)) {
                        Ok((slot, np)) => {
                            added = Some(np);
                            e.insert(DPane::new(slot))
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "cannot open a pane stream");
                            return None;
                        }
                    },
                };
                *p.slot
                    .mailbox
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(term);
                p.synced = true;
                p.next = seq;
                if !p.packet(PKT_SNAPSHOT, &[], scratch) {
                    self.panes.remove(&pane);
                    return None;
                }
                if let Some(np) = added {
                    self.event(Event::Added(np));
                }
            }
            ServerMsg::Tee { pane, seq, bytes } => {
                self.stream_record(pane, seq, PKT_OUTPUT, &bytes, scratch)
            }
            ServerMsg::Resized {
                pane,
                seq,
                rows,
                cols,
            } => {
                let mut b = [0u8; 4];
                b[..2].copy_from_slice(&rows.to_le_bytes());
                b[2..].copy_from_slice(&cols.to_le_bytes());
                self.stream_record(pane, seq, PKT_RESIZE, &b, scratch);
            }
            ServerMsg::Spawned { token, pane } => {
                let slot = self
                    .tokens
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&token);
                match (slot, pane) {
                    (Some(slot), Some(sid)) => {
                        self.panes.insert(sid, DPane::new(slot));
                        self.event(Event::Bound { token, sid });
                    }
                    // The local pane closed before the answer: so does the server's.
                    (None, Some(sid)) => {
                        let _ = self
                            .tx
                            .send(Out::Rec(ViewMsg::Close { pane: sid }.encode()));
                    }
                    (_, None) => self.event(Event::SpawnFailed { token }),
                }
            }
            ServerMsg::Exited { pane, status } => {
                self.building.remove(&pane);
                self.panes.remove(&pane);
                self.event(Event::Exited { sid: pane, status });
            }
        }
        None
    }

    fn stream_record(&mut self, sid: u64, seq: u64, tag: u8, body: &[u8], scratch: &mut Vec<u8>) {
        let Some(p) = self.panes.get_mut(&sid) else {
            return;
        };
        if !p.synced {
            return;
        }
        if seq != p.next {
            tracing::debug!(pane = sid, "stream gap; asking for a resync");
            self.resync(sid);
            return;
        }
        p.next += 1;
        if !p.packet(tag, body, scratch) {
            self.panes.remove(&sid);
        }
    }
}
