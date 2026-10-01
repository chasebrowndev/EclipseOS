// SPDX-License-Identifier: AGPL-3.0-only

//! Subscriptions and directory watching (FOG §Performance model,
//! technique 3; §Architecture: "after the first full listing, `ec-fogd` pushes
//! only diffs").
//!
//! Every cached or subscribed directory gets an inotify watch; eviction from
//! the LRU drops it. One unprivileged thread reads the inotify fd, coalesces
//! events for [`COALESCE`], and hands each dirty directory to
//! [`Daemon::on_change`], which rescans or re-stats and pushes a `DirDiff`
//! with a bumped generation to every subscriber. `IN_Q_OVERFLOW` rescans
//! everything watched. A directory that could not be watched (the
//! `max_user_watches` limit) is retried every [`TICK`] and, meanwhile,
//! rescanned whenever its mtime moves.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use ec_fog_proto::Reply;
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tokio::sync::mpsc;

use crate::Daemon;

/// How long a burst of events is collected before diffs go out.
pub const COALESCE: Duration = Duration::from_millis(20);
/// Retry period for directories without a watch; also how soon the thread
/// notices the daemon is gone.
pub const TICK: Duration = Duration::from_secs(1);

const STRIPES: usize = 64;

const FLAGS: WatchFlags = WatchFlags::CREATE
    .union(WatchFlags::DELETE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MODIFY)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR)
    .union(WatchFlags::EXCL_UNLINK);

/// One client connection, as the hub needs it to push diffs.
#[derive(Clone)]
pub struct Peer {
    pub id: u64,
    pub tx: mpsc::Sender<Vec<u8>>,
}

/// What changed in a directory since its listing was last published.
#[derive(Debug, Default, Clone)]
pub struct Dirty {
    /// Entries came or went: list again.
    pub rescan: bool,
    /// Events may have been lost: re-stat every entry too.
    pub full: bool,
    /// Entries whose metadata must be re-read.
    pub names: HashSet<Vec<u8>>,
}

impl Dirty {
    pub fn rescan() -> Self {
        Self {
            rescan: true,
            ..Self::default()
        }
    }
}

struct Sub {
    peer: u64,
    dir: u64,
    tx: mpsc::Sender<Vec<u8>>,
}

#[derive(Default)]
struct State {
    subs: HashMap<Vec<u8>, Vec<Sub>>,
    wd_paths: HashMap<i32, BTreeSet<Vec<u8>>>,
    path_wd: HashMap<Vec<u8>, i32>,
    /// Wanted but not watched: the watch limit was hit.
    unwatched: HashSet<Vec<u8>>,
}

/// Subscribers, watches, and the per-directory locks that order every
/// published generation.
pub struct Hub {
    fd: Option<Arc<OwnedFd>>,
    stripes: Vec<Mutex<()>>,
    st: Mutex<State>,
    next_peer: AtomicU64,
    started: AtomicBool,
}

impl Default for Hub {
    fn default() -> Self {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
            .map_err(|e| tracing::warn!(error = %e, "inotify unavailable; mtime checks only"))
            .ok()
            .map(Arc::new);
        Self {
            fd,
            stripes: (0..STRIPES).map(|_| Mutex::new(())).collect(),
            st: Mutex::new(State::default()),
            next_peer: AtomicU64::new(1),
            started: AtomicBool::new(false),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Hub {
    pub fn peer(&self, tx: mpsc::Sender<Vec<u8>>) -> Peer {
        Peer {
            id: self.next_peer.fetch_add(1, Ordering::Relaxed),
            tx,
        }
    }

    /// Serialises everything that reads and republishes the listing for
    /// `raw`: generations go out in order to every subscriber.
    pub fn lock(&self, raw: &[u8]) -> MutexGuard<'_, ()> {
        let mut h = DefaultHasher::new();
        raw.hash(&mut h);
        lock(&self.stripes[h.finish() as usize % STRIPES])
    }

    fn st(&self) -> MutexGuard<'_, State> {
        lock(&self.st)
    }

    /// Watch `path` (cached as `raw`). Idempotent.
    pub fn watch(&self, raw: &[u8], path: &Path) {
        let Some(fd) = &self.fd else { return };
        let mut st = self.st();
        if st.path_wd.contains_key(raw) {
            return;
        }
        match inotify::add_watch(fd, path, FLAGS) {
            Ok(wd) => {
                st.unwatched.remove(raw);
                st.path_wd.insert(raw.to_vec(), wd);
                st.wd_paths.entry(wd).or_default().insert(raw.to_vec());
            }
            Err(Errno::NOSPC) => {
                if st.unwatched.insert(raw.to_vec()) {
                    tracing::warn!(path = %path.display(), "inotify watch limit reached; falling back to mtime checks");
                }
            }
            Err(e) => tracing::debug!(path = %path.display(), error = %e, "watch failed"),
        }
    }

    /// Stop watching `raw`; the kernel watch goes when no path uses it.
    pub fn unwatch(&self, raw: &[u8]) {
        let mut st = self.st();
        st.unwatched.remove(raw);
        let Some(wd) = st.path_wd.remove(raw) else {
            return;
        };
        let last = st.wd_paths.get_mut(&wd).is_none_or(|s| {
            s.remove(raw);
            s.is_empty()
        });
        if last {
            st.wd_paths.remove(&wd);
            if let Some(fd) = &self.fd {
                let _ = inotify::remove_watch(fd, wd);
            }
        }
    }

    /// The kernel dropped (or is about to drop) `wd`: forget its paths.
    fn forget(&self, wd: i32, remove: bool) -> Vec<Vec<u8>> {
        let mut st = self.st();
        let paths: Vec<Vec<u8>> = st.wd_paths.remove(&wd).into_iter().flatten().collect();
        for p in &paths {
            st.path_wd.remove(p);
        }
        if remove && !paths.is_empty() {
            if let Some(fd) = &self.fd {
                let _ = inotify::remove_watch(fd, wd);
            }
        }
        paths
    }

    fn paths(&self, wd: i32) -> Vec<Vec<u8>> {
        self.st()
            .wd_paths
            .get(&wd)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn all(&self) -> Vec<Vec<u8>> {
        let st = self.st();
        st.path_wd.keys().chain(&st.unwatched).cloned().collect()
    }

    fn unwatched(&self) -> Vec<Vec<u8>> {
        self.st().unwatched.iter().cloned().collect()
    }

    pub fn is_watched(&self, raw: &[u8]) -> bool {
        self.st().path_wd.contains_key(raw)
    }

    /// Add `peer` to `raw`'s subscribers, for listing `dir`. Idempotent.
    pub fn subscribe(&self, peer: &Peer, raw: &[u8], dir: u64) {
        let mut st = self.st();
        let subs = st.subs.entry(raw.to_vec()).or_default();
        match subs.iter_mut().find(|s| s.peer == peer.id) {
            Some(s) => s.dir = dir,
            None => subs.push(Sub {
                peer: peer.id,
                dir,
                tx: peer.tx.clone(),
            }),
        }
    }

    /// Drop `peer`'s subscription to `dir`. Returns the path if it has no
    /// subscribers left.
    pub fn unsubscribe(&self, peer: u64, dir: u64) -> Option<Vec<u8>> {
        let mut st = self.st();
        let path = st
            .subs
            .iter()
            .find(|(_, v)| v.iter().any(|s| s.peer == peer && s.dir == dir))
            .map(|(p, _)| p.clone())?;
        let subs = st.subs.get_mut(&path)?;
        subs.retain(|s| !(s.peer == peer && s.dir == dir));
        if subs.is_empty() {
            st.subs.remove(&path);
            Some(path)
        } else {
            None
        }
    }

    /// Drop every subscription of a departed peer. Returns the paths left
    /// without subscribers.
    pub fn drop_peer(&self, peer: u64) -> Vec<Vec<u8>> {
        let mut st = self.st();
        let mut orphaned = Vec::new();
        st.subs.retain(|path, subs| {
            subs.retain(|s| s.peer != peer);
            if subs.is_empty() {
                orphaned.push(path.clone());
            }
            !subs.is_empty()
        });
        orphaned
    }

    pub fn subscribed(&self, raw: &[u8]) -> bool {
        self.st().subs.contains_key(raw)
    }

    /// Push `r` to `raw`'s subscribers except `skip`. Never blocks: a
    /// subscriber whose queue is full misses this frame, sees a generation
    /// gap on the next one, and relists.
    pub fn broadcast(&self, raw: &[u8], r: &Reply, skip: Option<u64>) {
        let st = self.st();
        let Some(subs) = st.subs.get(raw) else { return };
        let mut frame = None;
        for s in subs.iter().filter(|s| Some(s.peer) != skip) {
            let f = match &frame {
                Some(f) => f,
                None => match ec_fog_proto::encode(r) {
                    Ok(f) => frame.insert(f),
                    Err(e) => return tracing::warn!(error = %e, "push not encodable"),
                },
            };
            if let Err(mpsc::error::TrySendError::Full(_)) = s.tx.try_send(f.clone()) {
                tracing::debug!(peer = s.peer, "subscriber lagging; frame dropped");
            }
        }
    }

    /// Remove and return `raw`'s subscribers' senders (the directory is gone).
    pub fn take_subs(&self, raw: &[u8]) -> Vec<mpsc::Sender<Vec<u8>>> {
        self.st()
            .subs
            .remove(raw)
            .into_iter()
            .flatten()
            .map(|s| s.tx)
            .collect()
    }
}

/// Start the watcher thread for `daemon`, once. It exits within [`TICK`]
/// of the daemon being dropped.
pub fn spawn(daemon: &Arc<Daemon>) {
    let hub = &daemon.hub;
    let Some(fd) = hub.fd.clone() else { return };
    if hub.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let weak = Arc::downgrade(daemon);
    let res = std::thread::Builder::new()
        .name("fogd-watch".into())
        .spawn(move || run(&fd, &weak));
    if let Err(e) = res {
        tracing::warn!(error = %e, "watcher thread not started");
    }
}

fn run(fd: &OwnedFd, weak: &Weak<Daemon>) {
    let mut buf = vec![MaybeUninit::<u8>::uninit(); 64 << 10];
    let mut dirty: HashMap<Vec<u8>, Dirty> = HashMap::new();
    let mut deadline: Option<Instant> = None;
    let mut next_tick = Instant::now() + TICK;
    loop {
        let now = Instant::now();
        let until = deadline.map_or(next_tick, |d| d.min(next_tick));
        let wait = Timespec::try_from(until.saturating_duration_since(now)).ok();
        let mut fds = [PollFd::new(fd, PollFlags::IN)];
        match poll(&mut fds, wait.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => {
                tracing::warn!(error = %e, "inotify poll failed; watcher stops");
                return;
            }
        }
        let Some(daemon) = weak.upgrade() else { return };
        read(fd, &mut buf, &daemon.hub, &mut dirty);
        let now = Instant::now();
        if !dirty.is_empty() && deadline.is_none() {
            deadline = Some(now + COALESCE);
        }
        if deadline.is_some_and(|d| now >= d) {
            deadline = None;
            for (raw, d) in dirty.drain() {
                daemon.on_change(&raw, &d);
            }
        }
        if now >= next_tick {
            next_tick = now + TICK;
            for raw in daemon.hub.unwatched() {
                daemon.recheck(&raw);
            }
        }
    }
}

/// Drain the inotify fd into `dirty`.
fn read(fd: &OwnedFd, buf: &mut [MaybeUninit<u8>], hub: &Hub, dirty: &mut HashMap<Vec<u8>, Dirty>) {
    let mut rd = inotify::Reader::new(fd, buf);
    loop {
        let ev = match rd.next() {
            Ok(ev) => ev,
            Err(Errno::AGAIN | Errno::INTR) => return,
            Err(e) => return tracing::warn!(error = %e, "inotify read failed"),
        };
        let flags = ev.events();
        if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
            tracing::info!("inotify queue overflow; rescanning every watched directory");
            for p in hub.all() {
                let d = dirty.entry(p).or_default();
                d.rescan = true;
                d.full = true;
            }
            continue;
        }
        if flags.intersects(ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF | ReadFlags::UNMOUNT) {
            // The watched inode left this path; the rescan re-watches
            // whatever lives there now, or reports the error.
            for p in hub.forget(ev.wd(), flags.contains(ReadFlags::MOVE_SELF)) {
                dirty.entry(p).or_default().rescan = true;
            }
            continue;
        }
        if flags.contains(ReadFlags::IGNORED) {
            hub.forget(ev.wd(), false);
            continue;
        }
        let Some(name) = ev.file_name().map(|n| n.to_bytes().to_vec()) else {
            continue;
        };
        let structural = flags.intersects(
            ReadFlags::CREATE | ReadFlags::DELETE | ReadFlags::MOVED_FROM | ReadFlags::MOVED_TO,
        );
        for p in hub.paths(ev.wd()) {
            let d = dirty.entry(p).or_default();
            d.rescan |= structural;
            d.names.insert(name.clone());
        }
    }
}
