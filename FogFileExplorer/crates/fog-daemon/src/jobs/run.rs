// SPDX-License-Identifier: AGPL-3.0-only

//! Running one job (FOG §File operations).
//!
//! Safety rules: every creation is `O_EXCL`/`mkdir`/`RENAME_NOREPLACE`, so a
//! name collision is always seen and routed through the conflict policy. A
//! replaced destination is first renamed aside and only deleted once the job
//! commits. On cancel or failure everything the job created is removed and
//! everything set aside is put back; sources of a cross-device move are
//! removed only after the copies are synced and their sizes verified.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use fog_proto::{ConflictPolicy, JobId, JobSpec, JobStatus, Reply, Resolution};
use rustix::fs::{FileType, Mode};
use rustix::io::Errno;

use super::{Ctl, Inner};
use crate::journal::Item;
use crate::ops::{self, is_dir, is_errno, lstat};

const EMIT_EVERY: Duration = Duration::from_millis(100);

pub(crate) enum Fail {
    Cancelled,
    Io(io::Error, PathBuf),
}

pub(crate) type R<T> = Result<T, Fail>;

pub(crate) fn at<T>(r: io::Result<T>, p: &Path) -> R<T> {
    r.map_err(|e| Fail::Io(e, p.to_path_buf()))
}

fn errno(e: Errno, p: &Path) -> Fail {
    Fail::Io(e.into(), p.to_path_buf())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Copy,
    Move,
    /// A move out of the trash: journaled as `Restored`.
    Restore,
}

pub(crate) struct Runner<'a> {
    id: JobId,
    pub(crate) inner: &'a Inner,
    ctl: &'a Ctl,
    /// This run's journal id (`Journal::begin`); 0 before it began.
    pub(crate) run: u64,
    policy: ConflictPolicy,
    apply_all: Option<Resolution>,

    bytes_done: u64,
    bytes_total: u64,
    files_done: u64,
    files_total: u64,
    current: PathBuf,
    last_emit: Option<Instant>,
    chunks: u64,

    /// Outermost nodes this job created: removed on rollback.
    created: Vec<PathBuf>,
    /// (target, aside): replaced destinations, deleted on commit.
    asides: Vec<(PathBuf, PathBuf)>,
    /// Cross-device move: (source, destination, op), removed at commit.
    pending: Vec<(PathBuf, PathBuf, Op)>,
    /// Files copied for a cross-device move: sizes checked at commit.
    verify: Vec<(PathBuf, PathBuf)>,
    verifying: bool,
    sync_dirs: BTreeSet<PathBuf>,
    /// Source directories emptied by a merge-move, removed at commit.
    rmdirs: Vec<PathBuf>,
    depth: u32,

    pub(crate) items: Vec<Item>,
    pub(crate) replaced: bool,
}

impl<'a> Runner<'a> {
    pub(crate) fn new(inner: &'a Inner, id: JobId, ctl: &'a Ctl) -> Self {
        Self {
            id,
            inner,
            ctl,
            run: 0,
            policy: ConflictPolicy::Fail,
            apply_all: None,
            bytes_done: 0,
            bytes_total: 0,
            files_done: 0,
            files_total: 0,
            current: PathBuf::new(),
            last_emit: None,
            chunks: 0,
            created: Vec::new(),
            asides: Vec::new(),
            pending: Vec::new(),
            verify: Vec::new(),
            verifying: false,
            sync_dirs: BTreeSet::new(),
            rmdirs: Vec::new(),
            depth: 0,
            items: Vec::new(),
            replaced: false,
        }
    }

    pub(crate) fn state(&self, state: JobStatus) {
        self.inner.emit(Reply::JobState { id: self.id, state });
    }

    /// Write a crash-recovery record for this run, durably.
    fn note(&self, f: impl FnOnce(&mut crate::journal::Journal, u64) -> io::Result<()>) -> R<()> {
        let r = self
            .inner
            .journal()
            .map_err(io::Error::other)
            .and_then(|mut j| f(&mut j, self.run));
        r.map_err(|e| Fail::Io(e, PathBuf::from("undo journal")))
    }

    fn progress(&mut self, force: bool) {
        let now = Instant::now();
        if !force && self.last_emit.is_some_and(|t| now - t < EMIT_EVERY) {
            return;
        }
        self.last_emit = Some(now);
        self.inner.emit(Reply::JobProgress {
            id: self.id,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            files_done: self.files_done,
            files_total: self.files_total,
            current: ops::raw(&self.current),
        });
    }

    fn file_done(&mut self, bytes: u64) {
        self.files_done += 1;
        self.bytes_done += bytes;
        self.progress(false);
    }

    /// Between chunks and files: honour pause and cancel.
    pub(crate) fn checkpoint(&mut self) -> R<()> {
        let mut s = self.ctl.lock();
        if s.cancel {
            return Err(Fail::Cancelled);
        }
        if s.paused {
            self.state(JobStatus::Paused);
            while s.paused && !s.cancel {
                s = self.ctl.cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if s.cancel {
                return Err(Fail::Cancelled);
            }
            self.state(JobStatus::Running);
        }
        Ok(())
    }

    fn chunk_done(&mut self, n: u64) -> R<()> {
        self.bytes_done += n;
        self.chunks += 1;
        #[cfg(test)]
        if self.chunks == self.inner.pause_after_chunks.load(Ordering::Relaxed) {
            self.ctl.lock().paused = true;
        }
        self.progress(false);
        self.checkpoint()
    }

    /// Wait for the user in `Conflict`.
    fn ask(&mut self, src: &Path, dest: &Path) -> R<(Resolution, bool)> {
        let mut s = self.ctl.lock();
        s.asking = true;
        s.answer = None;
        self.state(JobStatus::Conflict {
            src: ops::raw(src),
            dest: ops::raw(dest),
        });
        while s.answer.is_none() && !s.cancel {
            s = self.ctl.cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.asking = false;
        if s.cancel {
            return Err(Fail::Cancelled);
        }
        let a = s.answer.take().expect("answered");
        drop(s);
        self.state(JobStatus::Running);
        Ok(a)
    }

    /// `dest` exists: decide per the job's policy.
    fn resolve(&mut self, src: &Path, dest: &Path, both_dirs: bool) -> R<Resolution> {
        match self.policy {
            ConflictPolicy::Skip => Ok(Resolution::Skip),
            ConflictPolicy::Rename => Ok(Resolution::KeepBoth),
            ConflictPolicy::Replace => Ok(Resolution::Replace),
            ConflictPolicy::Fail => Err(errno(Errno::EXIST, dest)),
            ConflictPolicy::Ask => loop {
                if let Some(a) = self.apply_all {
                    if a != Resolution::Merge || both_dirs {
                        return Ok(a);
                    }
                }
                let (c, all) = self.ask(src, dest)?;
                if c == Resolution::Merge && !both_dirs {
                    continue; // merge is for directories only: ask again
                }
                if all {
                    self.apply_all = Some(c);
                }
                return Ok(c);
            },
        }
    }

    /// Put `src` into `dest_dir` as `name`, by copy or move, resolving
    /// collisions. Records its journal item.
    pub(crate) fn put(&mut self, op: Op, src: &Path, dest_dir: &Path, name: &OsStr) -> R<()> {
        self.checkpoint()?;
        let sst = at(lstat(src), src)?;
        self.current = src.to_path_buf();
        let sdir = is_dir(&sst);
        let mut target = dest_dir.join(name);
        let mut keep: Option<u32> = None;
        loop {
            match self.produce(op, src, &sst, &target) {
                Ok(_) => return Ok(()),
                Err(Fail::Io(e, p)) if is_errno(&e, Errno::EXIST) && p == target => {
                    if let Some(n) = keep.as_mut() {
                        *n += 1;
                        target = dest_dir.join(ops::path_of(&ops::keep_both_name(
                            name.as_bytes(),
                            *n,
                            sdir,
                        )));
                        continue;
                    }
                    let ddir = lstat(&target).is_ok_and(|s| is_dir(&s));
                    match self.resolve(src, &target, sdir && ddir)? {
                        Resolution::Skip => {
                            let (b, f) = ops::tree_size(src);
                            self.files_done += f;
                            self.bytes_done += b;
                            self.progress(false);
                            return Ok(());
                        }
                        Resolution::KeepBoth => {
                            keep = Some(2);
                            target = dest_dir.join(ops::path_of(&ops::keep_both_name(
                                name.as_bytes(),
                                2,
                                sdir,
                            )));
                        }
                        Resolution::Merge => return self.merge(op, src, &target),
                        Resolution::Replace => return self.replace(op, src, &sst, &target),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn merge(&mut self, op: Op, src: &Path, target: &Path) -> R<()> {
        for e in at(fs::read_dir(src), src)? {
            let e = at(e, src)?;
            self.put(op, &e.path(), target, &e.file_name())?;
        }
        if op != Op::Copy {
            self.rmdirs.push(src.to_path_buf());
        }
        Ok(())
    }

    /// Explicit replace: set the destination aside, then put `src` there.
    fn replace(&mut self, op: Op, src: &Path, sst: &rustix::fs::Stat, target: &Path) -> R<()> {
        let aside = self.set_aside(target)?;
        self.replaced = true;
        match self.produce(op, src, sst, target) {
            Ok(true) => {
                // A same-device rename: this item is final, so is the replace.
                at(ops::remove_tree(&aside), &aside)?;
            }
            Ok(false) => self.asides.push((target.to_path_buf(), aside)),
            Err(e) => {
                let _ = ops::rename_noreplace(&aside, target);
                return Err(e);
            }
        }
        Ok(())
    }

    fn set_aside(&mut self, target: &Path) -> R<PathBuf> {
        let dir = at(ops::parent_of(target), target)?;
        for k in 0u32.. {
            let aside = dir.join(format!(".fog-replaced-{}-{k}", self.id));
            match ops::rename_noreplace(target, &aside) {
                Ok(()) => {
                    self.note(|j, run| j.aside(run, target, &aside))?;
                    return Ok(aside);
                }
                Err(e) if is_errno(&e, Errno::EXIST) => continue,
                Err(e) => return Err(Fail::Io(e, target.to_path_buf())),
            }
        }
        unreachable!("u32 aside names exhausted")
    }

    /// Create `target` from `src`. `Ok(true)` if it was a final rename.
    /// A collision on `target` itself is `Fail::Io(EEXIST, target)`.
    fn produce(&mut self, op: Op, src: &Path, sst: &rustix::fs::Stat, target: &Path) -> R<bool> {
        match op {
            Op::Copy => {
                self.copy_node(src, sst, target)?;
                let st = at(lstat(target), target)?;
                self.items.push(Item::Copied {
                    path: ops::raw(target),
                    ino: st.st_ino,
                    mtime_ns: ops::mtime_ns(&st),
                });
                Ok(false)
            }
            Op::Move | Op::Restore => match ops::rename_noreplace(src, target) {
                Ok(()) => {
                    let (b, f) = ops::tree_size(target);
                    self.files_done += f;
                    self.bytes_done += b;
                    self.progress(false);
                    let st = at(lstat(target), target)?;
                    self.items.push(moved(op, src, target, &st));
                    Ok(true)
                }
                Err(e) if is_errno(&e, Errno::XDEV) => {
                    self.verifying = true;
                    let r = self.copy_node(src, sst, target);
                    self.verifying = false;
                    r?;
                    self.sync_dirs
                        .insert(target.parent().unwrap_or(target).to_path_buf());
                    self.pending
                        .push((src.to_path_buf(), target.to_path_buf(), op));
                    Ok(false)
                }
                Err(e) => Err(Fail::Io(e, target.to_path_buf())),
            },
        }
    }

    /// `p` was just created. Outermost nodes are recorded for rollback and,
    /// durably and before any data goes in, for crash recovery.
    fn created(&mut self, p: &Path) -> R<()> {
        if self.depth == 0 {
            self.created.push(p.to_path_buf());
            let ino = at(lstat(p), p)?.st_ino;
            self.note(|j, run| j.created(run, p, ino))?;
        }
        Ok(())
    }

    /// Copy one node (recursively for directories) to a new `target`.
    fn copy_node(&mut self, src: &Path, sst: &rustix::fs::Stat, target: &Path) -> R<()> {
        self.checkpoint()?;
        self.current = src.to_path_buf();
        let perm = fs::Permissions::from_mode(sst.st_mode & 0o1777);
        match ops::file_type(sst) {
            FileType::RegularFile => {
                let s = at(
                    File::options()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(src),
                    src,
                )?;
                let d = at(
                    File::options()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(target),
                    target,
                )?;
                self.created(target)?;
                let len = at(s.metadata(), src)?.len();
                let mut fail = None;
                let r = ops::copy_data(&s, &d, len, &mut |n| {
                    self.chunk_done(n).map_err(|f| {
                        fail = Some(f);
                        io::Error::from(Errno::CANCELED)
                    })
                });
                if let Some(f) = fail {
                    return Err(f);
                }
                at(r, target)?;
                at(d.set_permissions(perm), target)?;
                drop(d);
                at(ops::set_times(target, sst), target)?;
                if self.verifying {
                    self.verify.push((src.to_path_buf(), target.to_path_buf()));
                }
                self.file_done(0);
            }
            FileType::Directory => {
                at(DirBuilder::new().mode(0o700).create(target), target)?;
                self.created(target)?;
                self.depth += 1;
                let r = (|| {
                    for e in at(fs::read_dir(src), src)? {
                        let e = at(e, src)?;
                        let c = e.path();
                        let cst = at(lstat(&c), &c)?;
                        self.copy_node(&c, &cst, &target.join(e.file_name()))?;
                    }
                    Ok(())
                })();
                self.depth -= 1;
                r?;
                at(fs::set_permissions(target, perm), target)?;
                at(ops::set_times(target, sst), target)?;
            }
            FileType::Symlink => {
                let l = at(fs::read_link(src), src)?;
                at(std::os::unix::fs::symlink(&l, target), target)?;
                self.created(target)?;
                at(ops::set_times(target, sst), target)?;
                self.file_done(sst.st_size as u64);
            }
            FileType::Fifo => {
                at(
                    rustix::fs::mknodat(
                        rustix::fs::CWD,
                        target,
                        FileType::Fifo,
                        Mode::from_raw_mode(sst.st_mode & 0o777),
                        0,
                    )
                    .map_err(io::Error::from),
                    target,
                )?;
                self.created(target)?;
                self.file_done(0);
            }
            // Sockets and device nodes cannot be recreated unprivileged; fail
            // rather than let a move drop them.
            _ => return Err(errno(Errno::OPNOTSUPP, src)),
        }
        Ok(())
    }

    /// Mkdir / create file at `path`, resolving collisions.
    fn make(&mut self, path: &Path, dir: bool) -> R<()> {
        let parent = at(ops::parent_of(path), path)?.to_path_buf();
        let name = at(ops::name_of(path), path)?.to_os_string();
        let mut target = path.to_path_buf();
        let mut keep: Option<u32> = None;
        let mut aside: Option<PathBuf> = None;
        loop {
            let r = if dir {
                DirBuilder::new().mode(0o777).create(&target)
            } else {
                File::options()
                    .write(true)
                    .create_new(true)
                    .mode(0o666)
                    .open(&target)
                    .map(drop)
            };
            match r {
                Ok(()) => {
                    self.created(&target)?;
                    if let Some(a) = aside {
                        self.asides.push((target.clone(), a));
                    }
                    let st = at(lstat(&target), &target)?;
                    self.items.push(Item::Made {
                        path: ops::raw(&target),
                        ino: st.st_ino,
                        mtime_ns: ops::mtime_ns(&st),
                        dir,
                    });
                    self.file_done(0);
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if let Some(n) = keep.as_mut() {
                        *n += 1;
                        target = parent.join(ops::path_of(&ops::keep_both_name(
                            name.as_bytes(),
                            *n,
                            dir,
                        )));
                        continue;
                    }
                    if aside.is_some() {
                        return Err(Fail::Io(e, target));
                    }
                    let ddir = lstat(&target).is_ok_and(|s| is_dir(&s));
                    match self.resolve(path, &target, dir && ddir)? {
                        Resolution::Skip | Resolution::Merge => return Ok(()),
                        Resolution::KeepBoth => {
                            keep = Some(2);
                            target = parent.join(ops::path_of(&ops::keep_both_name(
                                name.as_bytes(),
                                2,
                                dir,
                            )));
                        }
                        Resolution::Replace => {
                            aside = Some(self.set_aside(&target)?);
                            self.replaced = true;
                        }
                    }
                }
                Err(e) => {
                    if let Some(a) = aside {
                        let _ = ops::rename_noreplace(&a, &target);
                    }
                    return Err(Fail::Io(e, target));
                }
            }
        }
    }

    /// Everything placed: sync and verify cross-device copies, remove their
    /// sources, drop replaced originals and merged-away directories.
    pub(crate) fn commit(&mut self) -> R<()> {
        if !self.pending.is_empty() {
            for d in &self.sync_dirs {
                let f = at(File::open(d), d)?;
                at(rustix::fs::syncfs(&f).map_err(io::Error::from), d)?;
            }
            for (s, d) in &self.verify {
                let (a, b) = (at(lstat(s), s)?, at(lstat(d), d)?);
                if a.st_size != b.st_size {
                    return Err(Fail::Io(
                        io::Error::other("size mismatch after copy"),
                        d.clone(),
                    ));
                }
            }
        }
        // Past this point the copies are final: nothing is rolled back, and
        // crash recovery keeps them too.
        self.note(|j, run| j.commit(run))?;
        self.created.clear();
        for (_, aside) in std::mem::take(&mut self.asides) {
            at(ops::remove_tree(&aside), &aside)?;
        }
        for (src, dst, op) in std::mem::take(&mut self.pending) {
            at(ops::remove_tree(&src), &src)?;
            let st = at(lstat(&dst), &dst)?;
            self.items.push(moved(op, &src, &dst, &st));
        }
        for d in std::mem::take(&mut self.rmdirs) {
            let _ = fs::remove_dir(d); // stays if something in it was skipped
        }
        Ok(())
    }

    /// Undo whatever of this job is not final.
    pub(crate) fn rollback(&mut self) {
        for p in self.created.drain(..).rev() {
            if let Err(e) = ops::remove_tree(&p) {
                tracing::warn!(path = %p.display(), error = %e, "rollback: remove failed");
            }
        }
        for (t, a) in self.asides.drain(..).rev() {
            if let Err(e) = ops::rename_noreplace(&a, &t) {
                tracing::warn!(path = %a.display(), error = %e, "rollback: set-aside kept");
            }
        }
        // Cross-device items never committed: their sources are untouched.
        self.pending.clear();
        self.rmdirs.clear();
    }

    fn totals(&mut self, srcs: &[PathBuf]) {
        for s in srcs {
            let (b, f) = ops::tree_size(s);
            self.bytes_total += b;
            self.files_total += f;
        }
    }

    fn spec(&mut self, spec: JobSpec) -> R<()> {
        match spec {
            JobSpec::Copy {
                srcs,
                dest,
                on_conflict,
            } => self.transfer(Op::Copy, &srcs, &dest, on_conflict),
            JobSpec::Move {
                srcs,
                dest,
                on_conflict,
            } => self.transfer(Op::Move, &srcs, &dest, on_conflict),
            JobSpec::Rename {
                path,
                new_name,
                on_conflict,
            } => {
                self.policy = on_conflict;
                let p = abs(&path)?;
                if !ops::valid_name(&new_name) {
                    return Err(errno(Errno::INVAL, &p));
                }
                let parent = at(ops::parent_of(&p), &p)?.to_path_buf();
                self.files_total = 1;
                if at(ops::name_of(&p), &p)?.as_bytes() == new_name.as_slice() {
                    return Ok(());
                }
                self.put(Op::Move, &p, &parent, OsStr::from_bytes(&new_name))?;
                self.commit()
            }
            JobSpec::Trash { paths, .. } => {
                let paths = paths.iter().map(|p| abs(p)).collect::<R<Vec<_>>>()?;
                self.files_total = paths.len() as u64;
                for p in paths {
                    self.checkpoint()?;
                    self.current.clone_from(&p);
                    let t = at(self.inner.trash.trash(&p), &p)?;
                    let st = at(lstat(&t.files), &t.files)?;
                    self.items.push(Item::Trashed {
                        orig: ops::raw(&p),
                        files: ops::raw(&t.files),
                        info: ops::raw(&t.info),
                        ino: st.st_ino,
                        mtime_ns: ops::mtime_ns(&st),
                    });
                    self.file_done(0);
                }
                Ok(())
            }
            JobSpec::Delete { paths, .. } => {
                let paths = paths.iter().map(|p| abs(p)).collect::<R<Vec<_>>>()?;
                self.totals(&paths);
                for p in paths {
                    self.delete(&p)?;
                    self.items.push(Item::Deleted { path: ops::raw(&p) });
                }
                Ok(())
            }
            JobSpec::Mkdir { path, on_conflict } => {
                self.policy = on_conflict;
                self.files_total = 1;
                self.make(&abs(&path)?, true)?;
                self.commit()
            }
            JobSpec::CreateFile { path, on_conflict } => {
                self.policy = on_conflict;
                self.files_total = 1;
                self.make(&abs(&path)?, false)?;
                self.commit()
            }
            JobSpec::Restore {
                trash_ids,
                on_conflict,
            } => {
                self.policy = on_conflict;
                let mut found = Vec::new();
                for id in &trash_ids {
                    let f = self
                        .inner
                        .trash
                        .find(id)
                        .map_err(|e| Fail::Io(e, ops::path_of(id)))?;
                    found.push(f);
                }
                self.totals(&found.iter().map(|f| f.files.clone()).collect::<Vec<_>>());
                for f in &found {
                    let parent = at(ops::parent_of(&f.original), &f.original)?;
                    let name = at(ops::name_of(&f.original), &f.original)?;
                    at(fs::create_dir_all(parent), parent)?;
                    self.put(Op::Restore, &f.files, parent, name)?;
                }
                self.commit()?;
                for f in &found {
                    if lstat(&f.files).is_err() {
                        let _ = fs::remove_file(&f.info);
                    }
                }
                Ok(())
            }
        }
    }

    fn transfer(&mut self, op: Op, srcs: &[Vec<u8>], dest: &[u8], policy: ConflictPolicy) -> R<()> {
        self.policy = policy;
        let dest = abs(dest)?;
        if !at(lstat(&dest), &dest).map(|s| is_dir(&s))? {
            return Err(errno(Errno::NOTDIR, &dest));
        }
        let srcs = srcs.iter().map(|p| abs(p)).collect::<R<Vec<_>>>()?;
        for s in &srcs {
            if lstat(s).is_ok_and(|st| is_dir(&st)) && ops::is_within(&dest, s) {
                return Err(errno(Errno::INVAL, s));
            }
        }
        self.totals(&srcs);
        for s in &srcs {
            let name = at(ops::name_of(s), s)?;
            self.put(op, s, &dest, name)?;
        }
        self.commit()
    }

    /// Permanent delete with a checkpoint between entries.
    fn delete(&mut self, p: &Path) -> R<()> {
        self.checkpoint()?;
        self.current = p.to_path_buf();
        let st = at(lstat(p), p)?;
        if is_dir(&st) {
            for e in at(fs::read_dir(p), p)? {
                let e = at(e, p)?;
                let c = e.path();
                let cst = at(lstat(&c), &c)?;
                if is_dir(&cst) {
                    let (b, f) = ops::tree_size(&c);
                    at(ops::remove_tree(&c), &c)?;
                    self.files_done += f;
                    self.bytes_done += b;
                } else {
                    at(fs::remove_file(&c), &c)?;
                    self.files_done += 1;
                    self.bytes_done += cst.st_size as u64;
                }
                self.progress(false);
                self.checkpoint()?;
            }
            at(fs::remove_dir(p), p)
        } else {
            at(fs::remove_file(p), p)?;
            self.file_done(st.st_size as u64);
            Ok(())
        }
    }
}

fn moved(op: Op, src: &Path, dst: &Path, st: &rustix::fs::Stat) -> Item {
    match op {
        Op::Restore => Item::Restored {
            path: ops::raw(dst),
            ino: st.st_ino,
            mtime_ns: ops::mtime_ns(st),
        },
        _ => Item::Moved {
            from: ops::raw(src),
            to: ops::raw(dst),
            ino: st.st_ino,
            mtime_ns: ops::mtime_ns(st),
        },
    }
}

fn abs(raw: &[u8]) -> R<PathBuf> {
    let p = ops::path_of(raw);
    if p.is_absolute() {
        Ok(p)
    } else {
        Err(errno(Errno::INVAL, &p))
    }
}

pub(crate) fn describe(f: &Fail) -> (i32, String) {
    match f {
        Fail::Cancelled => (Errno::CANCELED.raw_os_error(), "cancelled".into()),
        Fail::Io(e, p) => (
            e.raw_os_error().unwrap_or(Errno::IO.raw_os_error()),
            format!("{}: {e}", p.display()),
        ),
    }
}

/// Run a queued job to its end state and journal what it did.
pub(crate) fn run_job(inner: &Inner, id: JobId, ctl: &Ctl, spec: JobSpec) {
    let mut r = Runner::new(inner, id, ctl);
    let delete = matches!(spec, JobSpec::Delete { .. });
    // The intent is durable before anything is touched.
    let begun = inner.journal().and_then(|mut j| {
        j.begin(Some(&spec))
            .map_err(|e| format!("journal write failed: {e}"))
    });
    match begun {
        Ok(run) => r.run = run,
        Err(msg) => {
            r.state(JobStatus::Failed {
                errno: Errno::IO.raw_os_error(),
                msg,
            });
            return;
        }
    }
    r.state(JobStatus::Running);
    let res = r.checkpoint().and_then(|()| r.spec(spec));
    if res.is_err() {
        r.rollback();
    }
    let mut end = match &res {
        Ok(()) => JobStatus::Done,
        Err(Fail::Cancelled) => JobStatus::Cancelled,
        Err(f) => {
            let (errno, msg) = describe(f);
            JobStatus::Failed { errno, msg }
        }
    };
    if !r.items.is_empty() {
        let undoable = !delete && !r.replaced;
        let items = std::mem::take(&mut r.items);
        let w = inner.journal().and_then(|mut j| {
            j.append(undoable, items, crate::trash::now_s())
                .map_err(|e| format!("journal write failed: {e}"))
        });
        if let Err(msg) = w {
            tracing::error!("{msg}");
            end = JobStatus::Failed {
                errno: Errno::IO.raw_os_error(),
                msg,
            };
        }
    }
    r.end();
    r.progress(true);
    r.state(end);
}

impl Runner<'_> {
    /// Close this run's intent; a failure only means recovery re-checks it.
    pub(crate) fn end(&self) {
        if let Err(Fail::Io(e, _)) = self.note(crate::journal::Journal::end) {
            tracing::error!(run = self.run, error = %e, "journal end record failed");
        }
    }
}
