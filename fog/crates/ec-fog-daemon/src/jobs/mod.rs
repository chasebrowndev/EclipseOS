// SPDX-License-Identifier: AGPL-3.0-only

//! The job queue (FOG §File operations/Queue). Every mutation is a job; each
//! finished job appends its effects to the undo journal.
//!
//! Jobs run on one worker thread per destination device (`st_dev`): a
//! device's jobs run one at a time in submission order, independent devices
//! in parallel. State and progress are broadcast to every connected client.

mod run;
mod undo;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Condvar, Mutex, MutexGuard, PoisonError};

use ec_fog_proto::{JobAction, JobId, JobSpec, JobStatus, Reply, Resolution};
use rustix::io::Errno;
use tokio::sync::broadcast;

use crate::journal::Journal;
use crate::ops;
use crate::trash::Trash;

/// Broadcast backlog per client before old progress frames are dropped.
const HUB: usize = 1024;

/// Where the trash and the journal live.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// `$XDG_DATA_HOME`: the home trash is `Trash/` below it.
    pub data_home: PathBuf,
    /// `$XDG_STATE_HOME`: the journal is `fog/journal` below it.
    pub state_home: PathBuf,
}

impl Dirs {
    /// From `XDG_DATA_HOME`/`XDG_STATE_HOME`, else their `$HOME` defaults.
    pub fn from_env() -> io::Result<Self> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home = var("HOME");
        let pick = |k: &str, def: &str| {
            var(k)
                .filter(|p| p.is_absolute())
                .or_else(|| home.as_ref().map(|h| h.join(def)))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))
        };
        Ok(Self {
            data_home: pick("XDG_DATA_HOME", ".local/share")?,
            state_home: pick("XDG_STATE_HOME", ".local/state")?,
        })
    }
}

type Task = Box<dyn FnOnce() + Send>;

/// Pause/cancel/resolve signals for one job.
#[derive(Default)]
pub(crate) struct Ctl {
    st: Mutex<CtlState>,
    cv: Condvar,
}

#[derive(Default)]
struct CtlState {
    paused: bool,
    cancel: bool,
    asking: bool,
    answer: Option<(Resolution, bool)>,
}

impl Ctl {
    fn lock(&self) -> MutexGuard<'_, CtlState> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct Inner {
    hub: broadcast::Sender<Reply>,
    journal: Result<Mutex<Journal>, String>,
    trash: Trash,
    next_id: AtomicU64,
    ctls: Mutex<HashMap<JobId, Arc<Ctl>>>,
    lanes: Mutex<HashMap<u64, VecDeque<Task>>>,
    /// Test hook: a job pauses itself after this many chunks (0: never).
    #[cfg(test)]
    pause_after_chunks: AtomicU64,
}

impl Inner {
    fn emit(&self, r: Reply) {
        let _ = self.hub.send(r);
    }

    fn journal(&self) -> Result<MutexGuard<'_, Journal>, String> {
        match &self.journal {
            Ok(j) => Ok(j.lock().unwrap_or_else(PoisonError::into_inner)),
            Err(e) => Err(e.clone()),
        }
    }
}

/// The queue, journal and trash, shared by every connection.
#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Inner>,
}

impl Jobs {
    /// Open (and compact) the journal under `dirs`. A journal that cannot be
    /// opened makes every job fail: no mutation runs unjournaled.
    pub fn new(dirs: &Dirs) -> Self {
        let journal = Journal::open(&Journal::path_in(&dirs.state_home), crate::trash::now_s())
            .map(Mutex::new)
            .map_err(|e| format!("undo journal unavailable: {e}"));
        if let Err(e) = &journal {
            tracing::error!("{e}");
        }
        Self::with_journal(journal, Trash::new(&dirs.data_home))
    }

    fn with_journal(journal: Result<Mutex<Journal>, String>, trash: Trash) -> Self {
        Self {
            inner: Arc::new(Inner {
                hub: broadcast::channel(HUB).0,
                journal,
                trash,
                next_id: AtomicU64::new(1),
                ctls: Mutex::default(),
                lanes: Mutex::default(),
                #[cfg(test)]
                pause_after_chunks: AtomicU64::new(0),
            }),
        }
    }

    /// From the XDG environment.
    pub fn from_env() -> Self {
        match Dirs::from_env() {
            Ok(d) => Self::new(&d),
            Err(e) => Self::with_journal(
                Err(format!("undo journal unavailable: {e}")),
                Trash::new(Path::new("/nonexistent")),
            ),
        }
    }

    /// No journal: every job fails. For daemons that only list.
    pub fn unconfigured() -> Self {
        Self::with_journal(
            Err("no undo journal configured".into()),
            Trash::new(Path::new("/nonexistent")),
        )
    }

    /// Send `r` to every subscriber alongside job events.
    pub fn broadcast(&self, r: Reply) {
        self.inner.emit(r);
    }

    /// Job state and progress for every job, as they happen.
    pub fn subscribe(&self) -> broadcast::Receiver<Reply> {
        self.inner.hub.subscribe()
    }

    /// Queue `spec`. `out` gets `JobAccepted` before any broadcast about it.
    pub fn submit(&self, spec: JobSpec, out: &mut dyn FnMut(Reply)) {
        let inner = &self.inner;
        let id = inner.next_id.fetch_add(1, Ordering::Relaxed);
        let ctl = Arc::new(Ctl::default());
        lock(&inner.ctls).insert(id, ctl.clone());
        out(Reply::JobAccepted { id });
        inner.emit(Reply::JobState {
            id,
            state: JobStatus::Queued,
        });
        let dev = device_of(&spec);
        let i = inner.clone();
        self.enqueue(
            dev,
            Box::new(move || {
                run::run_job(&i, id, &ctl, spec);
                lock(&i.ctls).remove(&id);
            }),
        );
    }

    /// `JobControl`. Unknown ids get `ESRCH`; a resolve with no conflict
    /// pending gets `EINVAL`.
    pub fn control(&self, id: JobId, action: JobAction, out: &mut dyn FnMut(Reply)) {
        let Some(ctl) = lock(&self.inner.ctls).get(&id).cloned() else {
            return out(err(Errno::SRCH));
        };
        let mut s = ctl.lock();
        match action {
            JobAction::Pause => s.paused = true,
            JobAction::Resume => s.paused = false,
            JobAction::Cancel => s.cancel = true,
            JobAction::Resolve { choice, apply_all } => {
                if !s.asking {
                    drop(s);
                    return out(err(Errno::INVAL));
                }
                s.answer = Some((choice, apply_all));
            }
        }
        drop(s);
        ctl.cv.notify_all();
    }

    /// `Undo`: runs on the queue like any job, with `JobState` broadcasts
    /// under its own id; replies `UndoResult` when done.
    pub fn undo(&self, out: &mut dyn FnMut(Reply)) {
        let dev = match self.inner.journal() {
            Err(e) => Err(e),
            Ok(j) => j
                .last_undoable()
                .map(undo::device_of)
                .ok_or_else(|| "nothing to undo".to_owned()),
        };
        let dev = match dev {
            Ok(d) => d,
            Err(reason) => {
                return out(Reply::UndoResult {
                    ok: false,
                    reason: Some(reason),
                })
            }
        };
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.emit(Reply::JobState {
            id,
            state: JobStatus::Queued,
        });
        let (tx, rx) = std_mpsc::channel();
        let i = self.inner.clone();
        self.enqueue(
            dev,
            Box::new(move || {
                let _ = tx.send(undo::undo(&i, id));
            }),
        );
        let res = rx
            .recv()
            .unwrap_or_else(|_| Err("undo worker failed".into()));
        out(Reply::UndoResult {
            ok: res.is_ok(),
            reason: res.err(),
        });
    }

    /// `ListTrash`.
    pub fn list_trash(&self, out: &mut dyn FnMut(Reply)) {
        out(Reply::TrashList(self.inner.trash.list()));
    }

    fn enqueue(&self, dev: u64, task: Task) {
        let mut lanes = lock(&self.inner.lanes);
        if let Some(q) = lanes.get_mut(&dev) {
            q.push_back(task);
            return;
        }
        lanes.insert(dev, VecDeque::new());
        drop(lanes);
        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name(format!("fog-jobs-{dev:x}"))
            .spawn(move || {
                let mut next = Some(task);
                while let Some(t) = next.take() {
                    t();
                    let mut lanes = lock(&inner.lanes);
                    next = lanes.get_mut(&dev).and_then(VecDeque::pop_front);
                    if next.is_none() {
                        lanes.remove(&dev);
                    }
                }
            })
            .expect("spawn job worker");
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Inner {
        &self.inner
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn err(e: Errno) -> Reply {
    Reply::Error {
        path: Vec::new(),
        errno: e.raw_os_error(),
    }
}

/// The device a job writes to: its lane key.
fn device_of(spec: &JobSpec) -> u64 {
    let p = match spec {
        JobSpec::Copy { dest, .. } | JobSpec::Move { dest, .. } => ops::path_of(dest),
        JobSpec::Rename { path, .. } => ops::path_of(path),
        JobSpec::Trash { paths, .. } | JobSpec::Delete { paths, .. } => {
            paths.first().map(|p| ops::path_of(p)).unwrap_or_default()
        }
        JobSpec::Mkdir { path, .. } | JobSpec::CreateFile { path, .. } => {
            let p = ops::path_of(path);
            p.parent().map(Path::to_path_buf).unwrap_or(p)
        }
        JobSpec::Restore { trash_ids, .. } => trash_ids
            .first()
            .map(|p| ops::path_of(p))
            .unwrap_or_default(),
    };
    ops::lstat(&p).map_or(0, |s| s.st_dev)
}
