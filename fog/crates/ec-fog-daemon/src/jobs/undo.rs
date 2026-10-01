// SPDX-License-Identifier: AGPL-3.0-only

//! Undo: apply the inverse of the newest undoable journal entry (FOG §File
//! operations/Undo journal). Every target is checked against the inode and
//! mtime recorded when the operation ran; any difference refuses the whole
//! undo with a reason, before anything is touched.

use std::fs;
use std::path::Path;

use fog_proto::{JobId, JobStatus};
use rustix::io::Errno;

use super::run::{at, describe, Op, Runner};
use super::{Ctl, Inner};
use crate::journal::{Entry, Item};
use crate::ops::{self, lstat};

/// The lane an undo runs on: the device of its first target.
pub(crate) fn device_of(e: &Entry) -> u64 {
    e.items
        .first()
        .and_then(|i| target(i).map(|(p, _, _)| ops::path_of(p)))
        .and_then(|p| lstat(&p).ok())
        .map_or(0, |s| s.st_dev)
}

/// The path an item's inverse acts on, and its recorded identity.
fn target(i: &Item) -> Option<(&[u8], u64, i128)> {
    match i {
        Item::Moved {
            to: p,
            ino,
            mtime_ns,
            ..
        }
        | Item::Copied {
            path: p,
            ino,
            mtime_ns,
        }
        | Item::Made {
            path: p,
            ino,
            mtime_ns,
            ..
        }
        | Item::Restored {
            path: p,
            ino,
            mtime_ns,
        }
        | Item::Trashed {
            files: p,
            ino,
            mtime_ns,
            ..
        } => Some((p, *ino, *mtime_ns)),
        Item::Deleted { .. } => None,
    }
}

/// Where the inverse puts things back, which must be free.
fn back(i: &Item) -> Option<&[u8]> {
    match i {
        Item::Moved { from, .. } => Some(from),
        Item::Trashed { orig, .. } => Some(orig),
        _ => None,
    }
}

/// Undo as job `id`: its states and progress are broadcast like any job's.
pub(crate) fn undo(inner: &Inner, id: JobId) -> Result<(), String> {
    let ctl = Ctl::default();
    let mut r = Runner::new(inner, id, &ctl);
    r.state(JobStatus::Running);
    let res = check_and_apply(&mut r);
    r.state(match &res {
        Ok(()) => JobStatus::Done,
        Err(msg) => JobStatus::Failed {
            errno: Errno::CANCELED.raw_os_error(),
            msg: msg.clone(),
        },
    });
    res
}

fn check_and_apply(r: &mut Runner<'_>) -> Result<(), String> {
    let inner = r.inner;
    let entry = {
        let j = inner.journal()?;
        j.last_undoable().cloned().ok_or("nothing to undo")?
    };
    for i in &entry.items {
        let Some((p, ino, mtime)) = target(i) else {
            return Err("a permanent delete cannot be undone".into());
        };
        let p = ops::path_of(p);
        let st = lstat(&p).map_err(|e| format!("{} is gone: {e}", p.display()))?;
        if st.st_ino != ino {
            return Err(format!(
                "{} was replaced since the operation (inode changed)",
                p.display()
            ));
        }
        if ops::mtime_ns(&st) != mtime {
            return Err(format!(
                "{} was modified since the operation (mtime changed)",
                p.display()
            ));
        }
        if let Some(b) = back(i) {
            let b = ops::path_of(b);
            if lstat(&b).is_ok() {
                return Err(format!("{} is occupied", b.display()));
            }
        }
    }

    r.run = inner
        .journal()?
        .begin(None)
        .map_err(|e| format!("journal write failed: {e}"))?;
    let res = (|| {
        for i in entry.items.iter().rev() {
            apply(r, i)?;
        }
        r.commit()
    })();
    if let Err(f) = res {
        r.rollback();
        r.end();
        return Err(format!("undo failed: {}", describe(&f).1));
    }
    let marked = inner
        .journal()?
        .mark_undone(entry.seq)
        .map_err(|e| format!("journal write failed: {e}"));
    r.end();
    marked
}

fn apply(r: &mut Runner<'_>, i: &Item) -> super::run::R<()> {
    match i {
        Item::Moved { from, to, .. } => move_back(r, &ops::path_of(to), &ops::path_of(from)),
        Item::Trashed {
            orig, files, info, ..
        } => {
            move_back(r, &ops::path_of(files), &ops::path_of(orig))?;
            let _ = fs::remove_file(ops::path_of(info));
            Ok(())
        }
        Item::Copied { path, .. } | Item::Restored { path, .. } => {
            let p = ops::path_of(path);
            at(r.inner.trash.trash(&p).map(drop), &p)
        }
        Item::Made { path, dir, .. } => {
            let p = ops::path_of(path);
            if *dir {
                at(fs::remove_dir(&p), &p) // refuses a non-empty directory
            } else {
                at(r.inner.trash.trash(&p).map(drop), &p)
            }
        }
        Item::Deleted { .. } => Ok(()),
    }
}

fn move_back(r: &mut Runner<'_>, from: &Path, to: &Path) -> super::run::R<()> {
    let parent = at(ops::parent_of(to), to)?;
    let name = at(ops::name_of(to), to)?;
    at(fs::create_dir_all(parent), parent)?;
    r.put(Op::Move, from, parent, name)
}
