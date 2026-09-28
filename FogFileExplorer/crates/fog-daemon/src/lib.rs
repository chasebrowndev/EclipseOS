// SPDX-License-Identifier: AGPL-3.0-only

//! fogd library: backends, cache and the socket server (FOG §Architecture,
//! §Filesystem backend, §Performance model).
//!
//! All filesystem I/O and sorting runs on tokio's blocking pool; the reactor
//! only moves frames.

pub mod activate;
pub mod backend;
pub mod cache;
pub mod config;
pub mod jobs;
pub mod journal;
mod list;
pub mod meta;
pub mod open;
pub mod ops;
pub mod places;
pub mod sort;
pub mod trash;
pub mod watch;

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fog_proto::{Reply, Request};
use rustix::io::Errno;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};

pub use backend::{Backend, LocalBackend, BATCH};
pub use cache::{diff, Cache, Listing};
pub use jobs::{Dirs, Jobs};

/// Frames queued per client before request handlers wait on the writer.
const CLIENT_QUEUE: usize = 64;

/// Daemon state shared by every connection.
pub struct Daemon {
    backend: Box<dyn Backend>,
    cache: Mutex<Cache>,
    next_dir: AtomicU64,
    /// Also carries every daemon-wide broadcast (see [`Daemon::broadcast`]).
    jobs: Jobs,
    hub: watch::Hub,
}

impl Daemon {
    pub fn new(backend: Box<dyn Backend>, cache: Cache) -> Self {
        Self {
            backend,
            cache: Mutex::new(cache),
            next_dir: AtomicU64::new(1),
            jobs: Jobs::unconfigured(),
            hub: watch::Hub::default(),
        }
    }

    /// Replace the job queue, e.g. with one on test directories.
    pub fn with_jobs(mut self, jobs: Jobs) -> Self {
        self.jobs = jobs;
        self
    }

    /// The job queue, undo journal and trash.
    pub fn jobs(&self) -> &Jobs {
        &self.jobs
    }

    /// [`LocalBackend`] with the default cache bounds, and the job queue on
    /// the XDG trash and journal. [`Daemon::new`] alone refuses jobs.
    pub fn local() -> Self {
        Self::new(Box::new(LocalBackend), Cache::default()).with_jobs(Jobs::from_env())
    }

    /// The listing cache. Never hold the guard across an `.await`.
    pub fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Send `r` to every connected client, e.g. [`Reply::ConfigError`].
    /// Shares the job queue's channel, so one relay per client carries both.
    pub fn broadcast(&self, r: Reply) {
        self.jobs.broadcast(r);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Reply> {
        self.jobs.subscribe()
    }

    /// Serve one request from a connected `peer`: subscriptions need its
    /// push channel, everything else goes to [`Self::handle`].
    pub fn dispatch(&self, peer: &watch::Peer, req: Request) {
        match req {
            Request::Subscribe { path } => {
                self.open_dir(path, Some(peer), &mut |r| send(&peer.tx, &r))
            }
            Request::Unsubscribe { dir } => self.unsubscribe(peer.id, dir),
            req => self.handle(req, &mut |r| send(&peer.tx, &r)),
        }
    }

    /// Serve one request synchronously, emitting replies in order through
    /// `out`. Blocks on I/O: call from a blocking thread.
    pub fn handle(&self, req: Request, out: &mut dyn FnMut(Reply)) {
        match req {
            // Without a peer to push to, a subscription is a listing; the
            // connection loop routes real ones to `dispatch`.
            Request::ListDir { path } | Request::Subscribe { path } => {
                self.open_dir(path, None, out)
            }
            Request::Stat { path } => {
                let reply = match abs(&path) {
                    Ok(p) => match self.backend.stat(p) {
                        Ok(st) => Reply::Stat(st),
                        Err(e) => error(&path, &e),
                    },
                    Err(e) => error(&path, &e),
                };
                out(reply);
            }
            Request::Open { path, app } => out(open_file(path, app.as_deref())),
            Request::Places => out(Reply::PlacesList(places::current())),
            Request::Unsubscribe { .. } => {}
            Request::Job(spec) => self.jobs.submit(spec, out),
            Request::JobControl { id, action } => self.jobs.control(id, action, out),
            Request::Undo => self.jobs.undo(out),
            Request::ListTrash => self.jobs.list_trash(out),
        }
    }
}

/// Directories are refused (`EISDIR`): the UI navigates into them.
fn open_file(raw: Vec<u8>, app: Option<&str>) -> Reply {
    let res = abs(&raw).and_then(|p| {
        if fs::metadata(p)?.is_dir() {
            return Err(Errno::ISDIR.into());
        }
        open::open(&open::Xdg::from_env(), p, app)
    });
    match res {
        Ok(()) => Reply::Opened { path: raw },
        Err(e) => error(&raw, &e),
    }
}

fn abs(raw: &[u8]) -> io::Result<&Path> {
    let p = Path::new(OsStr::from_bytes(raw));
    if p.is_absolute() {
        Ok(p)
    } else {
        Err(Errno::INVAL.into())
    }
}

fn error(path: &[u8], e: &io::Error) -> Reply {
    Reply::Error {
        path: path.to_vec(),
        errno: e.raw_os_error().unwrap_or(Errno::IO.raw_os_error()),
    }
}

fn snapshot(raw: &[u8], l: &Listing, complete: bool) -> Reply {
    Reply::DirSnapshot {
        path: raw.to_vec(),
        dir: l.dir,
        generation: l.generation,
        entries: l.entries.clone(),
        order: l.order.clone(),
        complete,
    }
}

/// `$XDG_RUNTIME_DIR/fog/fogd.sock`.
pub fn socket_path() -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(PathBuf::from(dir).join("fog").join("fogd.sock"))
}

/// Bind the socket: parent directory 0700, a stale socket replaced (a live
/// one is `AddrInUse`), socket 0600. Call inside a tokio runtime.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        fs::set_permissions(parent, Permissions::from_mode(0o700))?;
    }
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(io::ErrorKind::AddrInUse.into());
            }
            fs::remove_file(path)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "socket path exists and is not a socket",
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accept clients forever. Peers whose `SO_PEERCRED` uid differs from ours
/// are dropped.
pub async fn serve(listener: UnixListener, daemon: Arc<Daemon>) -> io::Result<()> {
    let me = rustix::process::geteuid().as_raw();
    watch::spawn(&daemon);
    loop {
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        match stream.peer_cred() {
            Ok(c) if c.uid() == me => {
                tokio::spawn(client(stream, daemon.clone()));
            }
            Ok(c) => tracing::warn!(uid = c.uid(), "refused peer with foreign uid"),
            Err(e) => tracing::warn!(error = %e, "SO_PEERCRED failed; peer refused"),
        }
    }
}

/// One connection: a reader loop dispatching each request to the blocking
/// pool, and a writer task draining encoded frames.
async fn client(stream: UnixStream, daemon: Arc<Daemon>) {
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(CLIENT_QUEUE);
    let peer = daemon.hub.peer(tx.clone());
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wr.write_all(&frame).await.is_err() {
                break;
            }
        }
    });
    let events = tokio::spawn(forward(daemon.subscribe(), tx.clone()));
    loop {
        match fog_proto::read_frame::<_, Request>(&mut rd).await {
            Ok(Some(req)) => {
                let d = daemon.clone();
                let peer = peer.clone();
                tokio::task::spawn_blocking(move || d.dispatch(&peer, req));
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(error = %e, "client dropped");
                break;
            }
        }
    }
    events.abort();
    daemon.drop_peer(peer.id);
    drop(peer);
    drop(tx);
    let _ = writer.await;
}

/// Relay daemon-wide broadcasts into one client's queue.
async fn forward(mut events: broadcast::Receiver<Reply>, tx: mpsc::Sender<Vec<u8>>) {
    loop {
        match events.recv().await {
            Ok(r) => match fog_proto::encode(&r) {
                Ok(f) => {
                    if tx.send(f).await.is_err() {
                        return;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "broadcast not encodable"),
            },
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!(n, "client lagged; broadcasts dropped");
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Encode and queue a reply from a blocking thread. A client that went away
/// is ignored.
fn send(tx: &mpsc::Sender<Vec<u8>>, r: &Reply) {
    let frame = match fog_proto::encode(r) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "reply not encodable");
            let fallback = Reply::Error {
                path: match r {
                    Reply::DirSnapshot { path, .. } => path.clone(),
                    _ => Vec::new(),
                },
                errno: Errno::FBIG.raw_os_error(),
            };
            match fog_proto::encode(&fallback) {
                Ok(f) => f,
                Err(_) => return,
            }
        }
    };
    let _ = tx.blocking_send(frame);
}
