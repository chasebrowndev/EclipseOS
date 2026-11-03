// SPDX-License-Identifier: AGPL-3.0-only
//! Progress for the taskbar's dynamic progress widget: any compile, download
//! or copy in flight, from two sources feeding one list.
//!
//! * **Feed.** Tools report through `ec-ctl progress start|set|done`, which
//!   writes JSON lines to `$XDG_RUNTIME_DIR/eclipse/progress.sock`. That
//!   socket is per-user (0600, peer uid checked) and is served here; the
//!   wire contract is `ec_ipc::progress`. A feed job with no update for
//!   [`model::FEED_TTL`] expires.
//! * **Scan.** While scanning is on, `/proc` is read for the tools in
//!   [`scan::TOOLS`] owned by this user: every second while something is
//!   running, every five seconds while nothing is.
//!
//! The thread owns the model and the socket and sends [`Update::Progress`]
//! over an `mpsc` channel when the merged list changes, like every other
//! watcher here. The GUI drains it with [`Handle::try_recv`]. Scanning is
//! only wanted while the widget is looking: [`Handle::set_scanning`] turns it
//! off when the widget is hidden.
//!
//! The socket is served only while a [`Handle`] is alive. Dropping the handle
//! stops the thread and removes the socket file.

pub mod model;
pub mod scan;

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ec_ipc::progress::{err, Request, MAX_LINE};

use model::Model;
use scan::{ProcSource, RealProc};

/// Scan cadence while anything is running.
pub const ACTIVE_INTERVAL: Duration = Duration::from_secs(1);
/// Scan cadence while nothing is.
pub const IDLE_INTERVAL: Duration = Duration::from_secs(5);
/// How often feed jobs are checked for expiry while any exist.
const SWEEP: Duration = Duration::from_secs(1);

/// Where a job came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Reported by a tool through `ec-ctl progress`.
    Feed,
    /// Found by the `/proc` scan.
    Scan,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Scan => "scan",
        }
    }
}

/// One thing in progress. `fraction` is `None` when the amount of work is
/// unknown; the widget then shows it as indeterminate.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: String,
    pub label: String,
    pub source: Source,
    pub fraction: Option<f32>,
    pub started: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// The whole list, oldest first. Sent only when it differs from the last
    /// list sent, so an empty list is sent once when the last job ends.
    Progress(Vec<Job>),
}

/// The GUI's end of the progress service.
pub struct Handle {
    updates: Receiver<Update>,
    /// Write end of the wake pipe. Dropping it stops the thread.
    wake: Option<UnixStream>,
}

impl Handle {
    /// The next list, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Turn the `/proc` scan on or off. Off clears scan jobs and stops
    /// reading `/proc`; feed jobs keep working. Takes effect at once.
    pub fn set_scanning(&self, on: bool) {
        if let Some(w) = &self.wake {
            let _ = (&*w).write_all(if on { b"1" } else { b"0" });
        }
    }
}

/// What the service runs with. Tests point `socket` at a temp dir and inject
/// a fake process source.
pub struct Options {
    /// The feed socket, or `None` for scan only.
    pub socket: Option<PathBuf>,
    pub source: Box<dyn ProcSource>,
}

/// Start the service with the session's feed socket and the real `/proc`.
pub fn spawn() -> Handle {
    spawn_with(Options {
        socket: ec_ipc::progress::socket_path(),
        source: Box::new(RealProc::new()),
    })
}

pub fn spawn_with(opts: Options) -> Handle {
    let (updates, rx) = mpsc::channel();
    let (wake_w, wake_r) = match UnixStream::pair() {
        Ok((w, r)) => (Some(w), Some(r)),
        Err(e) => {
            eprintln!("eclipse-progress: no wake pipe, set_scanning is inert: {e}");
            (None, None)
        }
    };
    if let Some(w) = &wake_w {
        let _ = w.set_nonblocking(true);
    }
    let Options { socket, source } = opts;
    // Bind here, not on the thread: once this returns, the feed is reachable.
    let (listener, bound) = match socket {
        Some(path) => match bind(&path) {
            Ok(l) => (Some(l), Some(path)),
            Err(e) => {
                eprintln!("eclipse-progress: feed disabled, {}: {e}", path.display());
                (None, None)
            }
        },
        None => (None, None),
    };
    let _ = std::thread::Builder::new()
        .name("eclipse-progress".into())
        .spawn(move || {
            let mut server = Server::new(updates, wake_r, listener, bound, source);
            server.run();
        });
    Handle {
        updates: rx,
        wake: wake_w,
    }
}

struct Client {
    stream: UnixStream,
    buf: Vec<u8>,
}

struct Server {
    updates: Sender<Update>,
    wake: Option<UnixStream>,
    listener: Option<UnixListener>,
    bound: Option<PathBuf>,
    clients: Vec<Client>,
    source: Box<dyn ProcSource>,
    scanning: bool,
    next_scan: Instant,
    model: Model,
    started: HashMap<(u32, String), Instant>,
    last_sent: Vec<Job>,
    uid: u32,
}

/// Why the loop stopped.
enum Flow {
    Continue,
    Stop,
}

impl Server {
    fn new(
        updates: Sender<Update>,
        wake: Option<UnixStream>,
        listener: Option<UnixListener>,
        bound: Option<PathBuf>,
        source: Box<dyn ProcSource>,
    ) -> Self {
        // SAFETY: getuid(2) cannot fail and takes no pointers.
        let uid = unsafe { libc::getuid() };
        Self {
            updates,
            wake,
            listener,
            bound,
            clients: Vec::new(),
            source,
            scanning: true,
            next_scan: Instant::now(),
            model: Model::default(),
            started: HashMap::new(),
            last_sent: Vec::new(),
            uid,
        }
    }

    fn run(&mut self) {
        loop {
            let now = Instant::now();
            self.model.expire(now);
            if self.scanning && now >= self.next_scan {
                self.scan(now);
            }
            if !self.publish() {
                return;
            }
            let timeout = self.timeout(now);
            match self.wait(timeout, now) {
                Ok(Flow::Continue) => {}
                Ok(Flow::Stop) | Err(_) => return,
            }
        }
    }

    /// The longest the loop may sleep: until the next scan, and at most one
    /// sweep while feed jobs could expire. `None` means block until a client
    /// or a wake arrives, which is the fully idle state.
    fn timeout(&self, now: Instant) -> Option<Duration> {
        let mut t: Option<Duration> = None;
        if self.scanning {
            t = Some(self.next_scan.saturating_duration_since(now));
        }
        if self.model.feed_active() {
            t = Some(t.map_or(SWEEP, |c| c.min(SWEEP)));
        }
        t
    }

    fn scan(&mut self, now: Instant) {
        let procs = self.source.procs();
        let jobs = scan::scan(&procs, now, &mut self.started);
        let idle = jobs.is_empty();
        self.model.set_scan(jobs);
        self.next_scan = now + if idle { IDLE_INTERVAL } else { ACTIVE_INTERVAL };
    }

    /// Send the list if it changed. `false` means the GUI has gone.
    fn publish(&mut self) -> bool {
        let snap = self.model.snapshot();
        if snap == self.last_sent {
            return true;
        }
        self.last_sent = snap.clone();
        self.updates.send(Update::Progress(snap)).is_ok()
    }

    fn wait(&mut self, timeout: Option<Duration>, now: Instant) -> io::Result<Flow> {
        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(2 + self.clients.len());
        let pfd = |fd: i32| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let listen_at = self.listener.as_ref().map(|l| {
            fds.push(pfd(l.as_raw_fd()));
            fds.len() - 1
        });
        let wake_at = self.wake.as_ref().map(|w| {
            fds.push(pfd(w.as_raw_fd()));
            fds.len() - 1
        });
        let base = fds.len();
        for c in &self.clients {
            fds.push(pfd(c.stream.as_raw_fd()));
        }
        let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
        // SAFETY: `fds` is a valid, exclusively borrowed slice of pollfd
        // structs for the duration of the call.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(Flow::Continue)
            } else {
                Err(e)
            };
        }

        if let Some(i) = listen_at {
            if fds[i].revents != 0 {
                self.accept_all();
            }
        }
        if let Some(i) = wake_at {
            if fds[i].revents != 0 && !self.read_wake(now) {
                return Ok(Flow::Stop);
            }
        }
        let mut k = 0;
        let model = &mut self.model;
        self.clients.retain_mut(|c| {
            let ready = fds.get(base + k).is_some_and(|f| f.revents != 0);
            k += 1;
            !ready || serve(c, model, now)
        });
        Ok(Flow::Continue)
    }

    fn accept_all(&mut self) {
        let Some(listener) = &self.listener else {
            return;
        };
        while let Ok((stream, _)) = listener.accept() {
            // Only our own uid may talk to the feed, whatever the socket's mode.
            if peer_uid(&stream) != Some(self.uid) || stream.set_nonblocking(true).is_err() {
                continue;
            }
            self.clients.push(Client {
                stream,
                buf: Vec::new(),
            });
        }
    }

    /// Returns `false` when the handle is gone and the thread should stop.
    fn read_wake(&mut self, now: Instant) -> bool {
        let Some(wake) = self.wake.as_mut() else {
            return true;
        };
        let mut buf = [0u8; 64];
        let mut last = None;
        loop {
            match wake.read(&mut buf) {
                Ok(0) => return false,
                Ok(n) => last = Some(buf[n - 1]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        if let Some(b) = last {
            let on = b == b'1';
            if on && !self.scanning {
                self.next_scan = now;
            }
            if !on {
                self.model.set_scan(Vec::new());
                self.started.clear();
            }
            self.scanning = on;
        }
        true
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(path) = &self.bound {
            let _ = fs::remove_file(path);
        }
    }
}

/// Read what the client has sent, answer each complete line, and keep the
/// client while it stays well-behaved. `false` drops it.
fn serve(c: &mut Client, model: &mut Model, now: Instant) -> bool {
    let mut tmp = [0u8; 1024];
    loop {
        match c.stream.read(&mut tmp) {
            Ok(0) => return false,
            Ok(n) => c.buf.extend_from_slice(&tmp[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    while let Some(pos) = c.buf.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = c.buf.drain(..=pos).collect();
        let text = String::from_utf8_lossy(&line[..line.len() - 1]);
        let reply = match Request::decode(&text) {
            Ok(req) => model.apply(req, now),
            Err(e) => err(e),
        };
        let mut out = reply.to_string();
        out.push('\n');
        if c.stream.write_all(out.as_bytes()).is_err() {
            return false;
        }
    }
    if c.buf.len() > MAX_LINE {
        let _ = c
            .stream
            .write_all(format!("{}\n", err("request line too long")).as_bytes());
        return false;
    }
    true
}

/// The uid of the process at the other end, via `SO_PEERCRED`.
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    // SAFETY: `cred` and `len` are valid for the sizes passed, and the fd is
    // a live socket owned by `stream` for the duration of the call.
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let r = libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        );
        (r == 0).then_some(cred.uid)
    }
}

/// Bind the feed socket. The directory is created 0700 if it is new; the
/// socket itself is 0600. A live server at `path` is an error, a dead one is
/// replaced.
fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "another progress feed is already serving this socket",
        ));
    }
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

#[cfg(test)]
mod tests;
