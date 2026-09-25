// SPDX-License-Identifier: AGPL-3.0-only

//! fogd library: backends, cache and the socket server (FOG §Architecture,
//! §Filesystem backend, §Performance model).
//!
//! All filesystem I/O and sorting runs on tokio's blocking pool; the reactor
//! only moves frames.

pub mod backend;
pub mod cache;
pub mod sort;

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use fog_proto::{apply_diff, Entry, Reply, Request};
use rustix::io::Errno;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

pub use backend::{Backend, LocalBackend, BATCH};
pub use cache::{diff, Cache, Listing};

/// Frames queued per client before request handlers wait on the writer.
const CLIENT_QUEUE: usize = 64;

/// Daemon state shared by every connection.
pub struct Daemon {
    backend: Box<dyn Backend>,
    cache: Mutex<Cache>,
    next_dir: AtomicU64,
}

impl Daemon {
    pub fn new(backend: Box<dyn Backend>, cache: Cache) -> Self {
        Self {
            backend,
            cache: Mutex::new(cache),
            next_dir: AtomicU64::new(1),
        }
    }

    /// [`LocalBackend`] with the default cache bounds.
    pub fn local() -> Self {
        Self::new(Box::new(LocalBackend), Cache::default())
    }

    /// The listing cache. Never hold the guard across an `.await`.
    pub fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Serve one request synchronously, emitting replies in order through
    /// `out`. Blocks on I/O: call from a blocking thread.
    pub fn handle(&self, req: Request, out: &mut dyn FnMut(Reply)) {
        match req {
            Request::ListDir { path } => self.list_dir(path, out),
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
        }
    }

    fn list_dir(&self, raw: Vec<u8>, out: &mut dyn FnMut(Reply)) {
        let path = match abs(&raw) {
            Ok(p) => p,
            Err(e) => return out(error(&raw, &e)),
        };
        let cached = self.cache().get(&raw);
        match cached {
            Some(old) => self.revalidate(&raw, path, old, out),
            None => self.cold(&raw, path, out),
        }
    }

    /// Cached: paint the cached listing, rescan, and send what changed.
    fn revalidate(&self, raw: &[u8], path: &Path, old: Arc<Listing>, out: &mut dyn FnMut(Reply)) {
        out(snapshot(raw, &old, true));
        let new = match self.list_all(path) {
            Ok(v) => v,
            Err(e) => {
                let mut c = self.cache();
                if c.peek(raw).is_some_and(|l| Arc::ptr_eq(l, &old)) {
                    c.remove(raw);
                }
                drop(c);
                return out(error(raw, &e));
            }
        };
        let (removed, added) = diff(&old.entries, &new);
        if removed.is_empty() && added.is_empty() {
            return;
        }
        let mut entries = old.entries.clone();
        apply_diff(&mut entries, &removed, &added);
        let order = sort::order(&entries);
        let next = Arc::new(Listing {
            dir: old.dir,
            generation: old.generation + 1,
            entries,
            order,
        });

        // Another client may have bumped the generation meanwhile; keep theirs.
        let current = {
            let mut c = self.cache();
            match c.peek(raw) {
                Some(cur) if !Arc::ptr_eq(cur, &old) => Some(cur.clone()),
                _ => {
                    c.insert(raw.to_vec(), next.clone());
                    None
                }
            }
        };
        match current {
            Some(cur)
                if !(cur.dir == next.dir
                    && cur.generation == next.generation
                    && cur.entries == next.entries) =>
            {
                out(snapshot(raw, &cur, true));
            }
            _ => out(Reply::DirDiff {
                dir: next.dir,
                generation: next.generation,
                removed,
                added,
                order: next.order.clone(),
                complete: true,
            }),
        }
    }

    /// Uncached: stream the first batch as a partial snapshot, complete with
    /// one diff, then cache.
    fn cold(&self, raw: &[u8], path: &Path, out: &mut dyn FnMut(Reply)) {
        let dir = self.next_dir.fetch_add(1, Ordering::Relaxed);
        let mut first: Option<Vec<Entry>> = None;
        let mut rest = Vec::new();
        let res = self.backend.list(path, &mut |b| {
            if first.is_none() {
                out(Reply::DirSnapshot {
                    path: raw.to_vec(),
                    dir,
                    generation: 0,
                    entries: b.clone(),
                    order: sort::order(&b),
                    complete: false,
                });
                first = Some(b);
            } else {
                rest.extend(b);
            }
        });
        let tail = match res {
            Ok(t) => t,
            Err(e) => return out(error(raw, &e)),
        };
        let listing = match first {
            None => {
                let l = Listing {
                    dir,
                    generation: 0,
                    order: sort::order(&tail),
                    entries: tail,
                };
                out(snapshot(raw, &l, true));
                l
            }
            Some(mut entries) => {
                rest.extend(tail);
                entries.extend_from_slice(&rest);
                let order = sort::order(&entries);
                out(Reply::DirDiff {
                    dir,
                    generation: 1,
                    removed: Vec::new(),
                    added: rest,
                    order: order.clone(),
                    complete: true,
                });
                Listing {
                    dir,
                    generation: 1,
                    entries,
                    order,
                }
            }
        };
        let mut c = self.cache();
        if c.peek(raw).is_none() {
            c.insert(raw.to_vec(), Arc::new(listing));
        }
    }

    fn list_all(&self, path: &Path) -> io::Result<Vec<Entry>> {
        let mut all = Vec::new();
        let tail = self.backend.list(path, &mut |b| all.extend(b))?;
        all.extend(tail);
        Ok(all)
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
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if wr.write_all(&frame).await.is_err() {
                break;
            }
        }
    });
    loop {
        match fog_proto::read_frame::<_, Request>(&mut rd).await {
            Ok(Some(req)) => {
                let d = daemon.clone();
                let tx = tx.clone();
                tokio::task::spawn_blocking(move || d.handle(req, &mut |r| send(&tx, &r)));
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(error = %e, "client dropped");
                break;
            }
        }
    }
    drop(tx);
    let _ = writer.await;
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
