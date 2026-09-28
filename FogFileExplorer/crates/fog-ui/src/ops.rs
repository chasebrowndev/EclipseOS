// SPDX-License-Identifier: AGPL-3.0-only

//! File operations as the UI sees them (FOG §File operations): the job
//! tray, the conflict dialog, the permanent-delete confirmation and inline
//! name entry. Pure state like [`crate::state`]: every mutation leaves as a
//! [`Request`] for `fogd`'s job queue, never as filesystem I/O here.
//!
//! `fogd` answers `Job` with `JobAccepted` on our connection but broadcasts
//! `JobState` and `JobProgress` to everyone, and it serves requests on a
//! pool, so two jobs sent back to back could be accepted out of order. The
//! tray therefore keeps one submission in flight and holds the rest until
//! the previous `JobAccepted` arrives: that is how it knows which id is
//! which kind of job.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use fog_proto::{
    ConflictPolicy, JobAction, JobId, JobSpec, JobStatus, Kind, Reply, Request, Resolution,
    StatReply,
};

use crate::edit::{Edit, LineEdit};
use crate::state::{join, split_parent};

/// How long a finished or cancelled job stays in the tray.
pub const LINGER: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Copy,
    Move,
    Rename,
    Trash,
    Delete,
    Mkdir,
    CreateFile,
    Restore,
    Undo,
    /// Submitted by another client (the CLI, another window).
    Other,
}

impl JobKind {
    pub fn of(spec: &JobSpec) -> Self {
        match spec {
            JobSpec::Copy { .. } => Self::Copy,
            JobSpec::Move { .. } => Self::Move,
            JobSpec::Rename { .. } => Self::Rename,
            JobSpec::Trash { .. } => Self::Trash,
            JobSpec::Delete { .. } => Self::Delete,
            JobSpec::Mkdir { .. } => Self::Mkdir,
            JobSpec::CreateFile { .. } => Self::CreateFile,
            JobSpec::Restore { .. } => Self::Restore,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Rename => "rename",
            Self::Trash => "trash",
            Self::Delete => "delete",
            Self::Mkdir => "mkdir",
            Self::CreateFile => "new file",
            Self::Restore => "restore",
            Self::Undo => "undo",
            Self::Other => "job",
        }
    }
}

/// How many items a spec names, and the first one: for the tray and the
/// status line.
fn subject(spec: &JobSpec) -> (usize, Vec<u8>) {
    match spec {
        JobSpec::Copy { srcs, .. } | JobSpec::Move { srcs, .. } => {
            (srcs.len(), srcs.first().cloned().unwrap_or_default())
        }
        JobSpec::Trash { paths, .. } | JobSpec::Delete { paths, .. } => {
            (paths.len(), paths.first().cloned().unwrap_or_default())
        }
        JobSpec::Restore { trash_ids, .. } => (
            trash_ids.len(),
            trash_ids.first().cloned().unwrap_or_default(),
        ),
        JobSpec::Rename { path, .. }
        | JobSpec::Mkdir { path, .. }
        | JobSpec::CreateFile { path, .. } => (1, path.clone()),
    }
}

/// One job in the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: JobId,
    pub kind: JobKind,
    /// Items the spec named, and the first of them.
    pub count: usize,
    pub subject: Vec<u8>,
    pub status: JobStatus,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    /// The path being worked on, from the last `JobProgress`.
    pub current: Vec<u8>,
    started: Option<Instant>,
    ended: Option<Instant>,
}

impl Job {
    fn new(id: JobId, kind: JobKind) -> Self {
        Self {
            id,
            kind,
            count: 0,
            subject: Vec::new(),
            status: JobStatus::Queued,
            bytes_done: 0,
            bytes_total: 0,
            files_done: 0,
            files_total: 0,
            current: Vec::new(),
            started: None,
            ended: None,
        }
    }

    /// Not yet finished, failed or cancelled.
    pub fn live(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Queued | JobStatus::Running | JobStatus::Paused | JobStatus::Conflict { .. }
        )
    }

    /// Done so far, 0 to 1: by bytes when there are bytes to move, else by
    /// files. `None` before the totals are known.
    pub fn fraction(&self) -> Option<f32> {
        if self.status == JobStatus::Done {
            return Some(1.0);
        }
        let (d, t) = if self.bytes_total > 0 {
            (self.bytes_done, self.bytes_total)
        } else {
            (self.files_done, self.files_total)
        };
        (t > 0).then(|| (d.min(t) as f64 / t as f64) as f32)
    }

    /// Time left at the rate so far. Only while running with bytes moved.
    pub fn eta(&self, now: Instant) -> Option<Duration> {
        if self.status != JobStatus::Running || self.bytes_done == 0 {
            return None;
        }
        let left = self.bytes_total.checked_sub(self.bytes_done)?;
        let spent = now.checked_duration_since(self.started?)?;
        Some(spent.mul_f64(left as f64 / self.bytes_done as f64))
    }
}

/// What the app must do after the tray took a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    None,
    /// The held-back next submission: send it.
    Send(Request),
    /// Job `id` paused on a name collision.
    Conflict {
        id: JobId,
        src: Vec<u8>,
        dest: Vec<u8>,
    },
    /// A job of ours ended (done, failed or cancelled).
    Ended(Job),
    /// `fogd` answered `Undo`.
    Undone {
        ok: bool,
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tray {
    /// Oldest first.
    pub jobs: Vec<Job>,
    /// Index into `jobs`, while the tray has the keyboard.
    pub cursor: usize,
    /// Sent, not yet accepted.
    inflight: Option<JobSpec>,
    outbox: VecDeque<JobSpec>,
    /// An `Undo` was sent and not answered: the next unknown job id is it.
    undoing: bool,
}

impl Tray {
    /// Queue `spec`. Returns the request to send now, or `None` while an
    /// earlier submission awaits its `JobAccepted`.
    pub fn submit(&mut self, spec: JobSpec) -> Option<Request> {
        if self.inflight.is_some() {
            self.outbox.push_back(spec);
            return None;
        }
        self.inflight = Some(spec.clone());
        Some(Request::Job(spec))
    }

    pub fn undo(&mut self) -> Request {
        self.undoing = true;
        Request::Undo
    }

    /// Jobs still running, queued, paused or asking.
    pub fn live(&self) -> usize {
        self.jobs.iter().filter(|j| j.live()).count()
    }

    pub fn get(&self, id: JobId) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn entry(&mut self, id: JobId) -> &mut Job {
        if let Some(i) = self.jobs.iter().position(|j| j.id == id) {
            return &mut self.jobs[i];
        }
        let kind = if std::mem::take(&mut self.undoing) {
            JobKind::Undo
        } else {
            JobKind::Other
        };
        self.jobs.push(Job::new(id, kind));
        let n = self.jobs.len() - 1;
        &mut self.jobs[n]
    }

    pub fn on_reply(&mut self, r: &Reply, now: Instant) -> TrayEvent {
        match r {
            Reply::JobAccepted { id } => {
                let Some(spec) = self.inflight.take() else {
                    return TrayEvent::None;
                };
                let (count, subject) = subject(&spec);
                let j = self.entry(*id);
                j.kind = JobKind::of(&spec);
                j.count = count;
                j.subject = subject;
                match self.outbox.pop_front() {
                    Some(next) => {
                        self.inflight = Some(next.clone());
                        TrayEvent::Send(Request::Job(next))
                    }
                    None => TrayEvent::None,
                }
            }
            Reply::JobProgress {
                id,
                bytes_done,
                bytes_total,
                files_done,
                files_total,
                current,
            } => {
                let j = self.entry(*id);
                j.bytes_done = *bytes_done;
                j.bytes_total = *bytes_total;
                j.files_done = *files_done;
                j.files_total = *files_total;
                j.current.clone_from(current);
                j.started.get_or_insert(now);
                TrayEvent::None
            }
            Reply::JobState { id, state } => {
                let j = self.entry(*id);
                j.status = state.clone();
                match state {
                    JobStatus::Running => {
                        j.started.get_or_insert(now);
                        TrayEvent::None
                    }
                    JobStatus::Conflict { src, dest } => TrayEvent::Conflict {
                        id: *id,
                        src: src.clone(),
                        dest: dest.clone(),
                    },
                    JobStatus::Done | JobStatus::Cancelled | JobStatus::Failed { .. } => {
                        j.ended = Some(now);
                        TrayEvent::Ended(j.clone())
                    }
                    JobStatus::Queued | JobStatus::Paused => TrayEvent::None,
                }
            }
            Reply::UndoResult { ok, reason } => {
                self.undoing = false;
                TrayEvent::Undone {
                    ok: *ok,
                    reason: reason.clone(),
                }
            }
            _ => TrayEvent::None,
        }
    }

    /// Clear jobs that finished or were cancelled more than [`LINGER`] ago.
    /// Failures stay until dismissed. Returns whether anything went.
    pub fn tick(&mut self, now: Instant) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|j| {
            matches!(j.status, JobStatus::Failed { .. })
                || j.ended.is_none_or(|t| now.duration_since(t) < LINGER)
        });
        self.clamp();
        self.jobs.len() != before
    }

    /// Drop a job that is over (a failure, usually). Live jobs stay: they
    /// are cancelled, not dismissed.
    pub fn dismiss(&mut self, id: JobId) {
        self.jobs.retain(|j| j.id != id || j.live());
        self.clamp();
    }

    /// Pause a running job, resume a paused one.
    pub fn toggle_pause(&self, id: JobId) -> Option<Request> {
        let action = match self.get(id)?.status {
            JobStatus::Running | JobStatus::Queued => JobAction::Pause,
            JobStatus::Paused => JobAction::Resume,
            _ => return None,
        };
        Some(Request::JobControl { id, action })
    }

    pub fn cancel(&self, id: JobId) -> Option<Request> {
        self.get(id)
            .filter(|j| j.live())
            .map(|_| Request::JobControl {
                id,
                action: JobAction::Cancel,
            })
    }

    pub fn step(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta);
        self.clamp();
    }

    pub fn at_cursor(&self) -> Option<JobId> {
        self.jobs.get(self.cursor).map(|j| j.id)
    }

    fn clamp(&mut self) {
        self.cursor = self.cursor.min(self.jobs.len().saturating_sub(1));
    }

    /// The connection to `fogd` went away: its job ids mean nothing to the
    /// next one, and a submission in flight may or may not have landed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The conflict dialog: job `id` found `dest` in the way of `src`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub id: JobId,
    pub src: Vec<u8>,
    pub dest: Vec<u8>,
    pub apply_all: bool,
    /// The highlighted choice; Enter takes it. Starts on Skip, the choice
    /// that changes nothing.
    pub pick: Resolution,
    pub src_stat: Option<StatReply>,
    pub dest_stat: Option<StatReply>,
}

impl Conflict {
    pub fn new(id: JobId, src: Vec<u8>, dest: Vec<u8>) -> Self {
        Self {
            id,
            src,
            dest,
            apply_all: false,
            pick: Resolution::Skip,
            src_stat: None,
            dest_stat: None,
        }
    }

    /// The `Stat` requests that fill in both sides.
    pub fn stats(&self) -> [Request; 2] {
        [
            Request::Stat {
                path: self.src.clone(),
            },
            Request::Stat {
                path: self.dest.clone(),
            },
        ]
    }

    pub fn on_stat(&mut self, st: &StatReply) {
        if st.path == self.src {
            self.src_stat = Some(st.clone());
        }
        if st.path == self.dest {
            self.dest_stat = Some(st.clone());
        }
    }

    /// Both sides are folders, so Merge is on offer.
    pub fn dirs(&self) -> bool {
        let dir = |s: &Option<StatReply>| s.as_ref().is_some_and(|s| s.kind == Kind::Dir);
        dir(&self.src_stat) && dir(&self.dest_stat)
    }

    pub fn choices(&self) -> Vec<Resolution> {
        let mut v = vec![Resolution::Replace, Resolution::Skip, Resolution::KeepBoth];
        if self.dirs() {
            v.push(Resolution::Merge);
        }
        v
    }

    /// The answer to send, if `choice` is on offer.
    pub fn resolve(&self, choice: Resolution) -> Option<Request> {
        self.choices()
            .contains(&choice)
            .then_some(Request::JobControl {
                id: self.id,
                action: JobAction::Resolve {
                    choice,
                    apply_all: self.apply_all,
                },
            })
    }

    /// A typed key: `r` `s` `k` `m` answer, `a` toggles apply-to-all.
    pub fn key(&mut self, c: &str) -> Option<Request> {
        if c.eq_ignore_ascii_case("a") {
            self.apply_all = !self.apply_all;
            return None;
        }
        self.resolve(resolution_key(c)?)
    }

    pub fn step(&mut self, delta: isize) {
        let c = self.choices();
        let at = c.iter().position(|&r| r == self.pick).unwrap_or(1);
        let to = at.saturating_add_signed(delta).min(c.len() - 1);
        self.pick = c[to];
    }
}

/// The dialog's letter for each answer.
pub fn resolution_key(c: &str) -> Option<Resolution> {
    Some(match c.to_ascii_lowercase().as_str() {
        "r" => Resolution::Replace,
        "s" => Resolution::Skip,
        "k" => Resolution::KeepBoth,
        "m" => Resolution::Merge,
        _ => return None,
    })
}

pub fn resolution_label(r: Resolution) -> &'static str {
    match r {
        Resolution::Replace => "replace",
        Resolution::Skip => "skip",
        Resolution::KeepBoth => "keep both",
        Resolution::Merge => "merge",
    }
}

/// The permanent-delete confirmation. Nothing is deleted unless the user
/// moves to "delete" and presses Enter (or clicks it): the pick starts on
/// cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub paths: Vec<Vec<u8>>,
    /// Display names, in the same order.
    pub names: Vec<String>,
    /// Whether "delete" is highlighted rather than "cancel".
    pub delete: bool,
}

impl Confirm {
    /// How many names the prompt spells out.
    const SHOWN: usize = 3;

    pub fn new(paths: Vec<Vec<u8>>, names: Vec<String>) -> Self {
        Self {
            paths,
            names,
            delete: false,
        }
    }

    pub fn headline(&self) -> String {
        match self.paths.len() {
            1 => "Delete 1 item permanently?".to_owned(),
            n => format!("Delete {n} items permanently?"),
        }
    }

    /// The first names, and how many more.
    pub fn listing(&self) -> String {
        let shown: Vec<&str> = self
            .names
            .iter()
            .take(Self::SHOWN)
            .map(String::as_str)
            .collect();
        let rest = self.names.len().saturating_sub(Self::SHOWN);
        if rest == 0 {
            shown.join(", ")
        } else {
            format!("{} and {rest} more", shown.join(", "))
        }
    }

    pub fn toggle(&mut self) {
        self.delete = !self.delete;
    }

    /// Enter: the delete job only if "delete" is highlighted.
    pub fn enter(&self) -> Option<JobSpec> {
        self.delete.then(|| self.job())
    }

    /// The job itself, for a click on "delete".
    pub fn job(&self) -> JobSpec {
        JobSpec::Delete {
            paths: self.paths.clone(),
            on_conflict: ConflictPolicy::Fail,
        }
    }
}

/// What an inline name is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameFor {
    /// Rename the entry at `path`, whose name is `old`.
    Rename {
        path: Vec<u8>,
        old: Vec<u8>,
    },
    Folder,
    File,
}

/// Inline name entry: a row's name while renaming, or the name of a new
/// folder or file in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    pub what: NameFor,
    /// The folder the name goes in.
    pub dir: Vec<u8>,
    pub line: LineEdit,
    /// Why the last Enter was refused.
    pub error: Option<&'static str>,
}

impl NameEntry {
    /// Rename `path`: the line starts with its current name.
    pub fn rename(path: Vec<u8>) -> Option<Self> {
        let (dir, old) = split_parent(&path)?;
        Some(Self {
            line: LineEdit::new(String::from_utf8_lossy(&old).into_owned()),
            what: NameFor::Rename { path, old },
            dir,
            error: None,
        })
    }

    pub fn create(dir: Vec<u8>, folder: bool) -> Self {
        Self {
            what: if folder {
                NameFor::Folder
            } else {
                NameFor::File
            },
            dir,
            line: LineEdit::default(),
            error: None,
        }
    }

    pub fn edit(&mut self, e: Edit) {
        self.line.apply(e);
        self.error = None;
    }

    /// Enter. `Ok(None)`: nothing to do (a rename to the same name).
    /// `Ok(Some)`: the job, and the new entry's name to select. `Err`: the
    /// name is refused and the entry stays open with the reason.
    pub fn submit(&mut self) -> Result<Option<(JobSpec, Vec<u8>)>, &'static str> {
        let name = validate_name(self.line.text()).inspect_err(|e| self.error = Some(e))?;
        let on_conflict = ConflictPolicy::Ask;
        let spec = match &self.what {
            NameFor::Rename { old, .. } if *old == name => return Ok(None),
            NameFor::Rename { path, .. } => JobSpec::Rename {
                path: path.clone(),
                new_name: name.clone(),
                on_conflict,
            },
            NameFor::Folder => JobSpec::Mkdir {
                path: join(&self.dir, &name),
                on_conflict,
            },
            NameFor::File => JobSpec::CreateFile {
                path: join(&self.dir, &name),
                on_conflict,
            },
        };
        Ok(Some((spec, name)))
    }
}

/// A name that can be one path component: not empty, no `/`, no NUL, not
/// `.` or `..`.
pub fn validate_name(s: &str) -> Result<Vec<u8>, &'static str> {
    match s {
        "" => Err("name is empty"),
        "." | ".." => Err("name cannot be . or .."),
        s if s.contains('/') => Err("name cannot contain /"),
        s if s.contains('\0') => Err("name cannot contain NUL"),
        s => Ok(s.as_bytes().to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(src: &str) -> JobSpec {
        JobSpec::Copy {
            srcs: vec![src.as_bytes().to_vec()],
            dest: b"/d".to_vec(),
            on_conflict: ConflictPolicy::Ask,
        }
    }

    fn state(id: JobId, state: JobStatus) -> Reply {
        Reply::JobState { id, state }
    }

    fn progress(id: JobId, done: u64, total: u64) -> Reply {
        Reply::JobProgress {
            id,
            bytes_done: done,
            bytes_total: total,
            files_done: 0,
            files_total: 1,
            current: b"/a/big".to_vec(),
        }
    }

    #[test]
    fn submissions_wait_for_acceptance_so_ids_match_kinds() {
        let mut t = Tray::default();
        let now = Instant::now();
        assert_eq!(t.submit(copy("/a")), Some(Request::Job(copy("/a"))));
        let trash = JobSpec::Trash {
            paths: vec![b"/x".to_vec(), b"/y".to_vec()],
            on_conflict: ConflictPolicy::Fail,
        };
        // Held back until the copy is accepted.
        assert_eq!(t.submit(trash.clone()), None);
        // The broadcast can beat the acceptance.
        t.on_reply(&state(7, JobStatus::Queued), now);
        assert_eq!(t.get(7).unwrap().kind, JobKind::Other);
        assert_eq!(
            t.on_reply(&Reply::JobAccepted { id: 7 }, now),
            TrayEvent::Send(Request::Job(trash))
        );
        assert_eq!(t.get(7).unwrap().kind, JobKind::Copy);
        t.on_reply(&Reply::JobAccepted { id: 8 }, now);
        let j = t.get(8).unwrap();
        assert_eq!(
            (j.kind, j.count, j.subject.as_slice()),
            (JobKind::Trash, 2, &b"/x"[..])
        );
        // Nothing in flight: the next goes straight out.
        assert!(t.submit(copy("/b")).is_some());
    }

    #[test]
    fn tray_lifecycle_lingers_done_and_keeps_failures() {
        let mut t = Tray::default();
        let t0 = Instant::now();
        t.submit(copy("/a"));
        t.on_reply(&Reply::JobAccepted { id: 1 }, t0);
        t.submit(copy("/b"));
        t.on_reply(&Reply::JobAccepted { id: 2 }, t0);
        t.on_reply(&state(1, JobStatus::Running), t0);
        t.on_reply(&progress(1, 25, 100), t0 + Duration::from_secs(1));
        assert_eq!(t.live(), 2);
        let j = t.get(1).unwrap();
        assert_eq!(j.fraction(), Some(0.25));
        // 25 bytes in 2 s: 75 more take 6 s.
        assert_eq!(
            j.eta(t0 + Duration::from_secs(2)),
            Some(Duration::from_secs(6))
        );
        assert_eq!(
            t.toggle_pause(1),
            Some(Request::JobControl {
                id: 1,
                action: JobAction::Pause
            })
        );
        t.on_reply(&state(1, JobStatus::Paused), t0);
        assert_eq!(
            t.toggle_pause(1),
            Some(Request::JobControl {
                id: 1,
                action: JobAction::Resume
            })
        );
        assert_eq!(t.get(1).unwrap().eta(t0), None);

        let end = t0 + Duration::from_secs(3);
        assert!(
            matches!(t.on_reply(&state(1, JobStatus::Done), end), TrayEvent::Ended(j) if j.id == 1)
        );
        let failed = JobStatus::Failed {
            errno: 28,
            msg: "no space".into(),
        };
        assert!(matches!(t.on_reply(&state(2, failed), end), TrayEvent::Ended(j) if j.id == 2));
        assert_eq!(t.live(), 0);
        assert_eq!(t.cancel(2), None);
        // Still lingering.
        assert!(!t.tick(end + LINGER / 2));
        assert_eq!(t.jobs.len(), 2);
        // The done job clears; the failure waits for dismissal.
        assert!(t.tick(end + LINGER));
        assert_eq!(t.jobs.iter().map(|j| j.id).collect::<Vec<_>>(), [2]);
        t.dismiss(2);
        assert!(t.jobs.is_empty());
    }

    #[test]
    fn live_jobs_are_not_dismissed_and_unknown_ids_are_undo_while_undoing() {
        let mut t = Tray::default();
        let now = Instant::now();
        t.on_reply(&state(3, JobStatus::Running), now);
        t.dismiss(3);
        assert_eq!(t.jobs.len(), 1);
        assert_eq!(
            t.cancel(3),
            Some(Request::JobControl {
                id: 3,
                action: JobAction::Cancel
            })
        );
        assert_eq!(t.undo(), Request::Undo);
        t.on_reply(&state(4, JobStatus::Queued), now);
        assert_eq!(t.get(4).unwrap().kind, JobKind::Undo);
        t.on_reply(&state(5, JobStatus::Queued), now);
        assert_eq!(t.get(5).unwrap().kind, JobKind::Other);
        let refused = Reply::UndoResult {
            ok: false,
            reason: Some("target changed since the operation".into()),
        };
        assert_eq!(
            t.on_reply(&refused, now),
            TrayEvent::Undone {
                ok: false,
                reason: Some("target changed since the operation".into())
            }
        );
    }

    fn stat(path: &str, kind: Kind) -> StatReply {
        StatReply {
            path: path.as_bytes().to_vec(),
            kind,
            size: 3,
            mtime_ns: 0,
            inode: 1,
        }
    }

    #[test]
    fn conflict_keys_map_to_resolutions() {
        let mut t = Tray::default();
        let ev = t.on_reply(
            &state(
                9,
                JobStatus::Conflict {
                    src: b"/a/f".to_vec(),
                    dest: b"/b/f".to_vec(),
                },
            ),
            Instant::now(),
        );
        let TrayEvent::Conflict { id, src, dest } = ev else {
            panic!("no conflict event")
        };
        let mut c = Conflict::new(id, src, dest);
        let answer = |choice, apply_all| {
            Some(Request::JobControl {
                id: 9,
                action: JobAction::Resolve { choice, apply_all },
            })
        };
        assert_eq!(c.pick, Resolution::Skip);
        assert_eq!(c.key("r"), answer(Resolution::Replace, false));
        assert_eq!(c.key("s"), answer(Resolution::Skip, false));
        assert_eq!(c.key("k"), answer(Resolution::KeepBoth, false));
        // Files cannot merge.
        c.on_stat(&stat("/a/f", Kind::File));
        c.on_stat(&stat("/b/f", Kind::File));
        assert_eq!(c.key("m"), None);
        assert_eq!(c.key("a"), None);
        assert!(c.apply_all);
        assert_eq!(c.key("R"), answer(Resolution::Replace, true));
        assert_eq!(c.key("x"), None);
        // Two folders can.
        c.on_stat(&stat("/a/f", Kind::Dir));
        c.on_stat(&stat("/b/f", Kind::Dir));
        assert_eq!(c.key("m"), answer(Resolution::Merge, true));
        c.step(10);
        assert_eq!(c.pick, Resolution::Merge);
        c.step(-10);
        assert_eq!(c.pick, Resolution::Replace);
        assert_eq!(
            c.stats()[1],
            Request::Stat {
                path: b"/b/f".to_vec()
            }
        );
    }

    #[test]
    fn delete_confirm_defaults_to_cancel() {
        let names: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        let paths = names
            .iter()
            .map(|n| format!("/w/{n}").into_bytes())
            .collect();
        let mut c = Confirm::new(paths, names);
        assert_eq!(c.headline(), "Delete 5 items permanently?");
        assert_eq!(c.listing(), "a, b, c and 2 more");
        // Enter straight away cancels.
        assert_eq!(c.enter(), None);
        c.toggle();
        assert!(matches!(c.enter(), Some(JobSpec::Delete { paths, .. }) if paths.len() == 5));
        c.toggle();
        assert_eq!(c.enter(), None);
        let one = Confirm::new(vec![b"/w/x".to_vec()], vec!["x".into()]);
        assert_eq!(
            (one.headline().as_str(), one.listing().as_str()),
            ("Delete 1 item permanently?", "x")
        );
    }

    #[test]
    fn names_are_validated() {
        assert_eq!(validate_name("ok.txt"), Ok(b"ok.txt".to_vec()));
        assert_eq!(validate_name(" spaced "), Ok(b" spaced ".to_vec()));
        assert!(validate_name("").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("nul\0").is_err());
    }

    #[test]
    fn rename_and_create_build_their_jobs() {
        let mut r = NameEntry::rename(b"/w/old.txt".to_vec()).unwrap();
        assert_eq!(r.line.text(), "old.txt");
        // Unchanged: nothing to do.
        assert_eq!(r.submit(), Ok(None));
        r.edit(Edit::Home);
        r.edit(Edit::Insert("x/".into()));
        assert_eq!(r.submit(), Err("name cannot contain /"));
        assert_eq!(r.error, Some("name cannot contain /"));
        // The caret sits after "x/": drop the slash.
        r.edit(Edit::Backspace);
        assert_eq!(r.error, None);
        assert_eq!(
            r.submit(),
            Ok(Some((
                JobSpec::Rename {
                    path: b"/w/old.txt".to_vec(),
                    new_name: b"xold.txt".to_vec(),
                    on_conflict: ConflictPolicy::Ask,
                },
                b"xold.txt".to_vec()
            )))
        );
        let mut n = NameEntry::create(b"/w".to_vec(), true);
        assert_eq!(n.submit(), Err("name is empty"));
        n.edit(Edit::Insert("new".into()));
        assert_eq!(
            n.submit(),
            Ok(Some((
                JobSpec::Mkdir {
                    path: b"/w/new".to_vec(),
                    on_conflict: ConflictPolicy::Ask
                },
                b"new".to_vec()
            )))
        );
        let mut f = NameEntry::create(b"/".to_vec(), false);
        f.edit(Edit::Insert("f".into()));
        assert!(
            matches!(f.submit(), Ok(Some((JobSpec::CreateFile { path, .. }, _))) if path == b"/f")
        );
    }
}
