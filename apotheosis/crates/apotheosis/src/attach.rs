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

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
}

/// Main's handle on the attachment.
pub(crate) struct Link {
    tx: mpsc::Sender<Vec<u8>>,
    tokens: Arc<Mutex<HashMap<PaneKey, Slot>>>,
    pub(crate) session: String,
    /// The window is closing: panes are left running, never closed.
    pub(crate) detaching: bool,
    /// Server pane id to local pane id.
    pub(crate) sids: HashMap<u64, PaneKey>,
}

impl Link {
    fn send(&self, m: &ViewMsg) {
        let _ = self.tx.send(m.encode());
    }

    /// Bytes for a server pane, in records of at most one chunk. Never logged.
    pub(crate) fn input(&self, sid: u64, bytes: &[u8]) {
        for c in bytes.chunks(MAX_CHUNK) {
            self.send(&ViewMsg::Input {
                pane: sid,
                bytes: c.to_vec(),
            });
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
        }
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
    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("no session server {session:?} ({e})"))?;
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
        .map_err(|e| e.to_string())?;
    let mut dec = RecordDecoder::new();
    let until = Instant::now() + HANDSHAKE;
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
            Ok(0) => return Err("the session server closed the connection".into()),
            Ok(n) => dec.feed(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.to_string()),
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
/// window's first pane (returned); the others arrive as [`Event::Added`].
pub(crate) fn start(
    h: Handshake,
    cfg: TermConfig,
    to_main: calloop::channel::Sender<MainMsg>,
    adopt_first: bool,
) -> io::Result<(Link, Option<NewPane>)> {
    let Handshake {
        stream,
        dec,
        session,
        panes,
    } = h;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut writer = stream.try_clone()?;
    std::thread::Builder::new()
        .name("apoth-attach-w".into())
        .spawn(move || {
            while let Ok(rec) = rx.recv() {
                if writer.write_all(&rec).is_err() {
                    break;
                }
            }
        })?;
    let tokens: Arc<Mutex<HashMap<PaneKey, Slot>>> = Arc::default();
    let mut demux = Demux {
        stream,
        dec,
        cfg,
        to_main,
        tx: tx.clone(),
        tokens: Arc::clone(&tokens),
        panes: HashMap::new(),
        building: HashMap::new(),
    };
    let mut first = None;
    if adopt_first && let Some(p) = panes.first() {
        let (slot, np) = pair(Some(p.pane))?;
        demux.panes.insert(p.pane, DPane::new(slot));
        first = Some(np);
    }
    std::thread::Builder::new()
        .name("apoth-attach-r".into())
        .spawn(move || demux.run())?;
    Ok((
        Link {
            tx,
            tokens,
            session,
            detaching: false,
            // Filled as panes are made (the first pane's local id is the window's).
            sids: HashMap::new(),
        },
        first,
    ))
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
    stream: UnixStream,
    dec: RecordDecoder,
    cfg: TermConfig,
    to_main: calloop::channel::Sender<MainMsg>,
    tx: mpsc::Sender<Vec<u8>>,
    tokens: Arc<Mutex<HashMap<PaneKey, Slot>>>,
    panes: HashMap<u64, DPane>,
    /// Snapshots being received: (sequence, state, pages per screen).
    building: HashMap<u64, Building>,
}

impl Demux {
    fn event(&self, e: Event) {
        let _ = self.to_main.send(MainMsg::Remote(e));
    }

    fn run(mut self) {
        let mut buf = vec![0u8; 256 * 1024];
        let mut scratch = Vec::with_capacity(MAX_CHUNK + 16);
        let why = 'outer: loop {
            while let Some(r) = self.dec.next_record() {
                let m = match r.map(ServerMsg::decode) {
                    Ok(Ok(Decoded::Msg(m))) => m,
                    Ok(Ok(Decoded::Skipped(_))) => continue,
                    _ => break 'outer "the session server sent a malformed record".to_owned(),
                };
                if let Some(why) = self.on_msg(m, &mut scratch) {
                    break 'outer why;
                }
            }
            match self.stream.read(&mut buf) {
                Ok(0) => break "the session server went away".to_owned(),
                Ok(n) => self.dec.feed(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break "the session server went away".to_owned(),
            }
        };
        // Every pane's stream ends with ours.
        self.panes.clear();
        self.tokens
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.event(Event::Lost(why));
    }

    fn resync(&mut self, sid: u64) {
        if let Some(p) = self.panes.get_mut(&sid) {
            p.synced = false;
        }
        let _ = self.tx.send(ViewMsg::Resync { pane: sid }.encode());
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
                        let _ = self.tx.send(ViewMsg::Close { pane: sid }.encode());
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
