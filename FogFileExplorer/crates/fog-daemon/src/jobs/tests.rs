// SPDX-License-Identifier: AGPL-3.0-only

//! Job queue, conflict, trash and undo tests on temp directories. The trash
//! and journal live under the test's own XDG_DATA_HOME/XDG_STATE_HOME.

use std::fs::{self, File};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

use fog_proto::{ConflictPolicy as P, JobAction, JobId, JobSpec, JobStatus, Reply, Resolution};
use tokio::sync::broadcast::error::TryRecvError;

use super::{Dirs, Jobs};
use crate::ops::raw;

struct H {
    d: tempfile::TempDir,
    dirs: Dirs,
    jobs: Jobs,
    rx: tokio::sync::broadcast::Receiver<Reply>,
}

fn h() -> H {
    let d = tempfile::tempdir().unwrap();
    let dirs = Dirs {
        data_home: d.path().join("data"),
        state_home: d.path().join("state"),
    };
    fs::create_dir(d.path().join("w")).unwrap();
    let jobs = Jobs::new(&dirs);
    let rx = jobs.subscribe();
    H { d, dirs, jobs, rx }
}

fn b(p: &Path) -> Vec<u8> {
    raw(p)
}

impl H {
    fn w(&self, rel: &str) -> PathBuf {
        self.d.path().join("w").join(rel)
    }

    fn submit(&mut self, spec: JobSpec) -> JobId {
        let mut id = None;
        self.jobs.submit(spec, &mut |r| {
            if let Reply::JobAccepted { id: i } = r {
                id = Some(i);
            }
        });
        id.expect("JobAccepted")
    }

    /// Wait for a state of job `id` matching `f`.
    fn wait(&mut self, id: JobId, f: impl Fn(&JobStatus) -> bool) -> JobStatus {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match self.rx.try_recv() {
                Ok(Reply::JobState { id: i, state }) if i == id && f(&state) => return state,
                Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < end, "timed out waiting on job {id}");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(TryRecvError::Closed) => panic!("hub closed"),
            }
        }
    }

    fn end(&mut self, id: JobId) -> JobStatus {
        self.wait(id, |s| {
            matches!(
                s,
                JobStatus::Done | JobStatus::Failed { .. } | JobStatus::Cancelled
            )
        })
    }

    fn run(&mut self, spec: JobSpec) -> JobStatus {
        let id = self.submit(spec);
        self.end(id)
    }

    fn ctl(&self, id: JobId, action: JobAction) {
        self.jobs
            .control(id, action, &mut |r| panic!("control: {r:?}"));
    }

    fn undo(&self) -> Result<(), String> {
        let mut res = None;
        self.jobs.undo(&mut |r| {
            if let Reply::UndoResult { ok, reason } = r {
                res = Some(if ok { Ok(()) } else { Err(reason.unwrap()) });
            }
        });
        res.expect("UndoResult")
    }

    fn trash_list(&self) -> Vec<fog_proto::TrashItem> {
        let mut out = Vec::new();
        self.jobs.list_trash(&mut |r| {
            if let Reply::TrashList(l) = r {
                out = l;
            }
        });
        let root = raw(self.d.path());
        out.retain(|i| i.original_path.starts_with(&root));
        out
    }
}

fn copy(srcs: &[&Path], dest: &Path, on_conflict: P) -> JobSpec {
    JobSpec::Copy {
        srcs: srcs.iter().map(|p| b(p)).collect(),
        dest: b(dest),
        on_conflict,
    }
}

fn mv(srcs: &[&Path], dest: &Path, on_conflict: P) -> JobSpec {
    JobSpec::Move {
        srcs: srcs.iter().map(|p| b(p)).collect(),
        dest: b(dest),
        on_conflict,
    }
}

fn eexist(s: &JobStatus) -> bool {
    matches!(s, JobStatus::Failed { errno, .. } if *errno == libc::EEXIST)
}

fn write_big(p: &Path, len: usize) -> Vec<u8> {
    let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
    fs::write(p, &data).unwrap();
    data
}

/// Names in `dir`, sorted, hiding nothing (a leftover `.fog-replaced-*`
/// would show up).
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn copy_preserves_tree_mode_mtime_symlinks_and_holes() {
    let mut h = h();
    let src = h.w("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), b"hello").unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    File::options()
        .write(true)
        .open(src.join("a.txt"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::os::unix::fs::symlink("a.txt", src.join("sub/link")).unwrap();
    let sparse = File::create(src.join("sparse")).unwrap();
    sparse.set_len(32 << 20).unwrap();
    sparse.write_all_at(b"x", 16 << 20).unwrap();
    drop(sparse);
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();

    assert_eq!(h.run(copy(&[&src], &dest, P::Fail)), JobStatus::Done);
    let c = dest.join("src");
    assert_eq!(fs::read(c.join("a.txt")).unwrap(), b"hello");
    let m = fs::metadata(c.join("a.txt")).unwrap();
    assert_eq!(m.mode() & 0o777, 0o640);
    assert_eq!(m.modified().unwrap(), old);
    assert_eq!(
        fs::read_link(c.join("sub/link")).unwrap(),
        Path::new("a.txt")
    );
    let sm = fs::metadata(c.join("sparse")).unwrap();
    assert_eq!(sm.len(), 32 << 20);
    assert!(sm.blocks() * 512 < 32 << 20, "holes were filled in");
    assert_eq!(
        fs::read(c.join("sparse")).unwrap(),
        fs::read(src.join("sparse")).unwrap()
    );
    assert!(src.join("a.txt").exists(), "copy keeps the source");
}

#[test]
fn copy_refuses_into_itself() {
    let mut h = h();
    let src = h.w("src");
    fs::create_dir_all(src.join("in")).unwrap();
    let s = h.run(copy(&[&src], &src.join("in"), P::Fail));
    assert!(matches!(s, JobStatus::Failed { errno, .. } if errno == libc::EINVAL));
    assert_eq!(names(&src.join("in")), Vec::<String>::new());
}

#[test]
fn move_same_device_and_undo() {
    let mut h = h();
    fs::write(h.w("f"), b"F").unwrap();
    fs::create_dir(h.w("d")).unwrap();
    fs::write(h.w("d/x"), b"X").unwrap();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    let ino = fs::metadata(h.w("f")).unwrap().ino();
    assert_eq!(
        h.run(mv(&[&h.w("f"), &h.w("d")], &dest, P::Fail)),
        JobStatus::Done
    );
    assert!(!h.w("f").exists() && !h.w("d").exists());
    assert_eq!(fs::metadata(dest.join("f")).unwrap().ino(), ino, "a rename");
    assert_eq!(fs::read(dest.join("d/x")).unwrap(), b"X");

    h.undo().unwrap();
    assert_eq!(fs::read(h.w("f")).unwrap(), b"F");
    assert_eq!(fs::read(h.w("d/x")).unwrap(), b"X");
    assert_eq!(names(&dest), Vec::<String>::new());
    assert!(h.undo().unwrap_err().contains("nothing to undo"));
}

#[test]
fn rename_conflicts_never_overwrite() {
    let mut h = h();
    fs::write(h.w("a"), b"A").unwrap();
    fs::write(h.w("b"), b"B").unwrap();
    let a = b(&h.w("a"));
    let ren = |on_conflict| JobSpec::Rename {
        path: a.clone(),
        new_name: b"b".to_vec(),
        on_conflict,
    };
    // The collision is only discovered by renameat2(RENAME_NOREPLACE)
    // returning EEXIST: there is no pre-check to race past.
    assert!(eexist(&h.run(ren(P::Fail))));
    assert_eq!(fs::read(h.w("a")).unwrap(), b"A");
    assert_eq!(fs::read(h.w("b")).unwrap(), b"B");
    assert_eq!(h.run(ren(P::Skip)), JobStatus::Done);
    assert_eq!(names(&h.w("")), ["a", "b"]);
    assert_eq!(h.run(ren(P::Rename)), JobStatus::Done);
    assert_eq!(names(&h.w("")), ["b", "b (2)"]);
    assert_eq!(fs::read(h.w("b (2)")).unwrap(), b"A");

    let bad = h.run(JobSpec::Rename {
        path: b(&h.w("b")),
        new_name: b"../x".to_vec(),
        on_conflict: P::Fail,
    });
    assert!(matches!(bad, JobStatus::Failed { errno, .. } if errno == libc::EINVAL));

    h.undo().unwrap();
    assert_eq!(names(&h.w("")), ["a", "b"]);
    assert_eq!(fs::read(h.w("a")).unwrap(), b"A");
}

#[test]
fn copy_conflict_policies() {
    let mut h = h();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(h.w("a.txt"), b"new").unwrap();
    fs::write(h.w("z"), b"z").unwrap();
    fs::write(dest.join("a.txt"), b"old").unwrap();

    // Fail: nothing of the job remains, not even the non-conflicting item.
    assert!(eexist(&h.run(copy(
        &[&h.w("z"), &h.w("a.txt")],
        &dest,
        P::Fail
    ))));
    assert_eq!(names(&dest), ["a.txt"]);
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"old");

    assert_eq!(
        h.run(copy(&[&h.w("a.txt")], &dest, P::Skip)),
        JobStatus::Done
    );
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"old");

    assert_eq!(
        h.run(copy(&[&h.w("a.txt")], &dest, P::Rename)),
        JobStatus::Done
    );
    assert_eq!(
        h.run(copy(&[&h.w("a.txt")], &dest, P::Rename)),
        JobStatus::Done
    );
    assert_eq!(names(&dest), ["a (2).txt", "a (3).txt", "a.txt"]);
    assert_eq!(fs::read(dest.join("a (3).txt")).unwrap(), b"new");

    assert_eq!(
        h.run(copy(&[&h.w("a.txt")], &dest, P::Replace)),
        JobStatus::Done
    );
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"new");
    assert_eq!(
        names(&dest),
        ["a (2).txt", "a (3).txt", "a.txt"],
        "no aside left"
    );
    // Replace is not undoable: undo goes to the keep-both copy before it.
    h.undo().unwrap();
    assert_eq!(names(&dest), ["a (2).txt", "a.txt"]);
    assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"new");
}

#[test]
fn ask_resolutions_and_apply_all() {
    let mut h = h();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    for n in ["a", "b"] {
        fs::write(h.w(n), b"new").unwrap();
        fs::write(dest.join(n), b"old").unwrap();
    }
    // KeepBoth for all: asked once.
    let id = h.submit(copy(&[&h.w("a"), &h.w("b")], &dest, P::Ask));
    let c = h.wait(id, |s| matches!(s, JobStatus::Conflict { .. }));
    assert_eq!(
        c,
        JobStatus::Conflict {
            src: b(&h.w("a")),
            dest: b(&dest.join("a"))
        }
    );
    h.ctl(
        id,
        JobAction::Resolve {
            choice: Resolution::KeepBoth,
            apply_all: true,
        },
    );
    assert_eq!(h.end(id), JobStatus::Done);
    assert_eq!(names(&dest), ["a", "a (2)", "b", "b (2)"]);

    // Replace for one, Skip for the next: asked twice.
    let id = h.submit(copy(&[&h.w("a"), &h.w("b")], &dest, P::Ask));
    h.wait(id, |s| matches!(s, JobStatus::Conflict { .. }));
    h.ctl(
        id,
        JobAction::Resolve {
            choice: Resolution::Replace,
            apply_all: false,
        },
    );
    let c = h.wait(id, |s| matches!(s, JobStatus::Conflict { .. }));
    assert!(matches!(c, JobStatus::Conflict { dest: d, .. } if d == b(&dest.join("b"))));
    h.ctl(
        id,
        JobAction::Resolve {
            choice: Resolution::Skip,
            apply_all: false,
        },
    );
    assert_eq!(h.end(id), JobStatus::Done);
    assert_eq!(fs::read(dest.join("a")).unwrap(), b"new");
    assert_eq!(fs::read(dest.join("b")).unwrap(), b"old");
    assert_eq!(names(&dest), ["a", "a (2)", "b", "b (2)"]);

    // A resolve with no conflict pending is refused.
    let mut refused = false;
    h.jobs.control(
        id,
        JobAction::Resolve {
            choice: Resolution::Skip,
            apply_all: false,
        },
        &mut |_| refused = true,
    );
    assert!(refused);
}

#[test]
fn ask_merge_directories_only() {
    let mut h = h();
    let dest = h.w("dest");
    fs::create_dir_all(dest.join("d")).unwrap();
    fs::write(dest.join("d/x"), b"old").unwrap();
    fs::create_dir(h.w("d")).unwrap();
    fs::write(h.w("d/x"), b"new").unwrap();
    fs::write(h.w("d/y"), b"y").unwrap();

    let id = h.submit(mv(&[&h.w("d")], &dest, P::Ask));
    h.wait(
        id,
        |s| matches!(s, JobStatus::Conflict { dest: d, .. } if d.ends_with(b"dest/d")),
    );
    h.ctl(
        id,
        JobAction::Resolve {
            choice: Resolution::Merge,
            apply_all: true,
        },
    );
    // x collides inside: merge does not apply to files, so it asks again,
    // and a Merge answer for a file is asked once more.
    h.wait(
        id,
        |s| matches!(s, JobStatus::Conflict { dest: d, .. } if d.ends_with(b"d/x")),
    );
    h.ctl(
        id,
        JobAction::Resolve {
            choice: Resolution::Merge,
            apply_all: false,
        },
    );
    h.wait(
        id,
        |s| matches!(s, JobStatus::Conflict { dest: d, .. } if d.ends_with(b"d/x")),
    );
    h.ctl(
        id,
        JobAction::Resolve {
            choice: Resolution::Skip,
            apply_all: false,
        },
    );
    assert_eq!(h.end(id), JobStatus::Done);
    assert_eq!(names(&dest.join("d")), ["x", "y"]);
    assert_eq!(fs::read(dest.join("d/x")).unwrap(), b"old");
    // The skipped file stays at the source, so its directory does too.
    assert_eq!(names(&h.w("d")), ["x"]);
    assert_eq!(fs::read(h.w("d/x")).unwrap(), b"new");
}

#[test]
fn cancel_while_asking_rolls_back() {
    let mut h = h();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(h.w("a"), b"a").unwrap();
    fs::write(h.w("b"), b"new").unwrap();
    fs::write(dest.join("b"), b"old").unwrap();
    let id = h.submit(copy(&[&h.w("a"), &h.w("b")], &dest, P::Ask));
    h.wait(id, |s| matches!(s, JobStatus::Conflict { .. }));
    h.ctl(id, JobAction::Cancel);
    assert_eq!(h.end(id), JobStatus::Cancelled);
    assert_eq!(names(&dest), ["b"]);
    assert_eq!(fs::read(dest.join("b")).unwrap(), b"old");
}

#[test]
fn cancel_mid_copy_leaves_no_partial_destination() {
    let mut h = h();
    h.jobs
        .inner()
        .pause_after_chunks
        .store(1, Ordering::Relaxed);
    let src = h.w("big");
    let data = write_big(&src, 24 << 20);
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    let id = h.submit(copy(&[&src], &dest, P::Fail));
    h.wait(id, |s| *s == JobStatus::Paused);
    // Paused between chunks: a partial destination exists right now.
    let part = fs::metadata(dest.join("big")).unwrap().len();
    assert!(part > 0 && part < data.len() as u64, "partial {part}");
    h.ctl(id, JobAction::Cancel);
    assert_eq!(h.end(id), JobStatus::Cancelled);
    assert_eq!(names(&dest), Vec::<String>::new());
    assert_eq!(fs::read(&src).unwrap(), data);
    assert!(h.undo().is_err(), "a cancelled copy journals nothing");
}

#[test]
fn pause_and_resume_between_chunks() {
    let mut h = h();
    h.jobs
        .inner()
        .pause_after_chunks
        .store(1, Ordering::Relaxed);
    let src = h.w("big");
    let data = write_big(&src, 20 << 20);
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    let id = h.submit(copy(&[&src], &dest, P::Fail));
    h.wait(id, |s| *s == JobStatus::Paused);
    h.ctl(id, JobAction::Resume);
    h.wait(id, |s| *s == JobStatus::Running);
    assert_eq!(h.end(id), JobStatus::Done);
    assert_eq!(fs::read(dest.join("big")).unwrap(), data);
}

#[test]
fn trash_list_restore_and_undo() {
    let mut h = h();
    fs::write(h.w("f.txt"), b"F").unwrap();
    fs::create_dir(h.w("d")).unwrap();
    fs::write(h.w("d/x"), b"X").unwrap();
    let spec = JobSpec::Trash {
        paths: vec![b(&h.w("f.txt")), b(&h.w("d"))],
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(spec), JobStatus::Done);
    assert!(!h.w("f.txt").exists() && !h.w("d").exists());
    // Home filesystem: $XDG_DATA_HOME/Trash with info written.
    let home = h.dirs.data_home.join("Trash");
    assert_eq!(
        names(&home.join("info")),
        ["d.trashinfo", "f.txt.trashinfo"]
    );
    let l = h.trash_list();
    assert_eq!(l.len(), 2);
    let canon = fs::canonicalize(h.w("")).unwrap();
    let f = l
        .iter()
        .find(|i| i.original_path == b(&canon.join("f.txt")))
        .unwrap();
    assert_eq!(f.kind, fog_proto::Kind::File);
    assert!(f.deleted_s.is_some());

    // Restore one; the other stays listed.
    let spec = JobSpec::Restore {
        trash_ids: vec![f.id.clone()],
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(spec), JobStatus::Done);
    assert_eq!(fs::read(h.w("f.txt")).unwrap(), b"F");
    let l = h.trash_list();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].kind, fog_proto::Kind::Dir);

    // Undo the restore (trashes it again, same inode and mtime), then undo
    // the trash (restores both).
    h.undo().unwrap();
    assert!(!h.w("f.txt").exists());
    assert_eq!(h.trash_list().len(), 2);
    h.undo().unwrap();
    assert_eq!(fs::read(h.w("f.txt")).unwrap(), b"F");
    assert_eq!(fs::read(h.w("d/x")).unwrap(), b"X");
    assert!(h.trash_list().is_empty());
}

#[test]
fn undo_trash_restores() {
    let mut h = h();
    fs::write(h.w("f"), b"F").unwrap();
    let spec = JobSpec::Trash {
        paths: vec![b(&h.w("f"))],
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(spec), JobStatus::Done);
    h.undo().unwrap();
    assert_eq!(fs::read(h.w("f")).unwrap(), b"F");
    assert!(h.trash_list().is_empty());
    assert_eq!(
        names(&h.dirs.data_home.join("Trash/info")),
        Vec::<String>::new()
    );
}

#[test]
fn restore_conflict_keep_both() {
    let mut h = h();
    fs::write(h.w("f"), b"1").unwrap();
    let t = JobSpec::Trash {
        paths: vec![b(&h.w("f"))],
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(t), JobStatus::Done);
    fs::write(h.w("f"), b"2").unwrap();
    let id = h.trash_list()[0].id.clone();
    let r = |on_conflict| JobSpec::Restore {
        trash_ids: vec![id.clone()],
        on_conflict,
    };
    assert!(eexist(&h.run(r(P::Fail))));
    assert_eq!(h.trash_list().len(), 1);
    assert_eq!(h.run(r(P::Rename)), JobStatus::Done);
    assert_eq!(fs::read(h.w("f")).unwrap(), b"2");
    assert_eq!(fs::read(h.w("f (2)")).unwrap(), b"1");
    assert!(h.trash_list().is_empty());
}

#[test]
fn delete_mkdir_create_file() {
    let mut h = h();
    let mk = |p: &Path, on_conflict| JobSpec::Mkdir {
        path: b(p),
        on_conflict,
    };
    let cf = |p: &Path, on_conflict| JobSpec::CreateFile {
        path: b(p),
        on_conflict,
    };
    assert_eq!(h.run(mk(&h.w("n"), P::Fail)), JobStatus::Done);
    assert!(h.w("n").is_dir());
    assert!(eexist(&h.run(mk(&h.w("n"), P::Fail))));
    assert_eq!(h.run(mk(&h.w("n"), P::Rename)), JobStatus::Done);
    assert!(h.w("n (2)").is_dir());

    assert_eq!(h.run(cf(&h.w("n/f.txt"), P::Fail)), JobStatus::Done);
    fs::write(h.w("n/f.txt"), b"keep").unwrap();
    assert!(eexist(&h.run(cf(&h.w("n/f.txt"), P::Fail))));
    assert_eq!(h.run(cf(&h.w("n/f.txt"), P::Skip)), JobStatus::Done);
    assert_eq!(
        fs::read(h.w("n/f.txt")).unwrap(),
        b"keep",
        "O_EXCL: no truncation"
    );
    assert_eq!(h.run(cf(&h.w("n/f.txt"), P::Rename)), JobStatus::Done);
    assert_eq!(names(&h.w("n")), ["f (2).txt", "f.txt"]);

    // Undo the newest create: f (2).txt goes to the trash.
    h.undo().unwrap();
    assert_eq!(names(&h.w("n")), ["f.txt"]);
    // The older create was modified since: refused.
    assert!(h.undo().unwrap_err().contains("modified"));
    let del = JobSpec::Delete {
        paths: vec![b(&h.w("n"))],
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(del), JobStatus::Done);
    assert!(!h.w("n").exists());
    // Delete is never undoable; the create before it has lost its target.
    assert!(h.undo().unwrap_err().contains("gone"));
}

#[test]
fn undo_copy_trashes_the_copies() {
    let mut h = h();
    fs::write(h.w("a"), b"A").unwrap();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    assert_eq!(h.run(copy(&[&h.w("a")], &dest, P::Fail)), JobStatus::Done);
    h.undo().unwrap();
    assert!(!dest.join("a").exists());
    assert!(h.w("a").exists());
    let l = h.trash_list();
    assert_eq!(l.len(), 1);
    assert_eq!(
        l[0].original_path,
        b(&fs::canonicalize(&dest).unwrap().join("a"))
    );
}

#[test]
fn undo_refused_after_external_modification() {
    let mut h = h();
    fs::write(h.w("a"), b"A").unwrap();
    let ren = JobSpec::Rename {
        path: b(&h.w("a")),
        new_name: b"b".to_vec(),
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(ren), JobStatus::Done);
    let f = File::options().write(true).open(h.w("b")).unwrap();
    f.write_all_at(b"changed", 0).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(7))
        .unwrap();
    let e = h.undo().unwrap_err();
    assert!(e.contains("modified"), "{e}");
    assert!(h.w("b").exists() && !h.w("a").exists());

    // Replaced by another inode: refused too.
    fs::remove_file(h.w("b")).unwrap();
    fs::write(h.w("b"), b"other").unwrap();
    let e = h.undo().unwrap_err();
    assert!(e.contains("inode") || e.contains("modified"), "{e}");
    assert_eq!(fs::read(h.w("b")).unwrap(), b"other");
}

#[test]
fn undo_refused_when_original_is_occupied() {
    let mut h = h();
    fs::write(h.w("a"), b"A").unwrap();
    let ren = JobSpec::Rename {
        path: b(&h.w("a")),
        new_name: b"b".to_vec(),
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(ren), JobStatus::Done);
    fs::write(h.w("a"), b"squatter").unwrap();
    assert!(h.undo().unwrap_err().contains("occupied"));
    assert_eq!(fs::read(h.w("a")).unwrap(), b"squatter");
    assert_eq!(fs::read(h.w("b")).unwrap(), b"A");
}

#[test]
fn journal_survives_restart() {
    let mut h = h();
    fs::write(h.w("a"), b"A").unwrap();
    let ren = JobSpec::Rename {
        path: b(&h.w("a")),
        new_name: b"b".to_vec(),
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(ren), JobStatus::Done);
    // A new daemon on the same XDG_STATE_HOME undoes the old rename.
    let again = Jobs::new(&h.dirs);
    h.jobs = again;
    h.undo().unwrap();
    assert_eq!(fs::read(h.w("a")).unwrap(), b"A");
    assert!(h.dirs.state_home.join("fog/journal").is_file());
}

#[test]
fn journal_unavailable_refuses_jobs() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("state"), b"not a dir").unwrap();
    let dirs = Dirs {
        data_home: d.path().join("data"),
        state_home: d.path().join("state"),
    };
    let jobs = Jobs::new(&dirs);
    let mut h = H {
        rx: jobs.subscribe(),
        jobs,
        dirs,
        d,
    };
    fs::create_dir(h.w("")).unwrap();
    let s = h.run(JobSpec::Mkdir {
        path: b(&h.w("n")),
        on_conflict: P::Fail,
    });
    assert!(matches!(s, JobStatus::Failed { msg, .. } if msg.contains("journal")));
    assert!(!h.w("n").exists());
}

#[test]
fn unknown_job_control_is_esrch() {
    let h = h();
    let mut got = None;
    h.jobs
        .control(999, JobAction::Cancel, &mut |r| got = Some(r));
    assert!(matches!(got, Some(Reply::Error { errno, .. }) if errno == libc::ESRCH));
}

/// A tmpfs on another device than the test's temp dir, if there is one.
fn other_device(than: &Path) -> Option<tempfile::TempDir> {
    let shm = Path::new("/dev/shm");
    let d = tempfile::tempdir_in(shm).ok()?;
    let dev = |p: &Path| fs::metadata(p).map(|m| m.dev()).ok();
    (dev(d.path())? != dev(than)?).then_some(d)
}

#[test]
fn cross_device_move_verifies_then_removes_sources() {
    let mut h = h();
    let Some(far) = other_device(h.d.path()) else {
        eprintln!("skipped: no second filesystem at /dev/shm");
        return;
    };
    let src = far.path().join("d");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub/x"), b"X").unwrap();
    let data = write_big(&src.join("big"), 3 << 20);
    std::os::unix::fs::symlink("sub/x", src.join("l")).unwrap();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();

    assert_eq!(h.run(mv(&[&src], &dest, P::Fail)), JobStatus::Done);
    assert!(!src.exists(), "source removed after verification");
    assert_eq!(fs::read(dest.join("d/big")).unwrap(), data);
    assert_eq!(fs::read(dest.join("d/sub/x")).unwrap(), b"X");
    assert!(fs::symlink_metadata(dest.join("d/l")).unwrap().is_symlink());

    // Undo moves it back across devices.
    h.undo().unwrap();
    assert_eq!(fs::read(src.join("big")).unwrap(), data);
    assert!(!dest.join("d").exists());
}

#[test]
fn cross_device_move_cancel_keeps_sources() {
    let mut h = h();
    let Some(far) = other_device(h.d.path()) else {
        eprintln!("skipped: no second filesystem at /dev/shm");
        return;
    };
    h.jobs
        .inner()
        .pause_after_chunks
        .store(2, Ordering::Relaxed);
    let src = far.path().join("d");
    fs::create_dir(&src).unwrap();
    let a = write_big(&src.join("a"), 9 << 20);
    let b2 = write_big(&src.join("b"), 9 << 20);
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    let id = h.submit(mv(&[&src], &dest, P::Fail));
    h.wait(id, |s| *s == JobStatus::Paused);
    h.ctl(id, JobAction::Cancel);
    assert_eq!(h.end(id), JobStatus::Cancelled);
    assert_eq!(names(&dest), Vec::<String>::new());
    assert_eq!(fs::read(src.join("a")).unwrap(), a);
    assert_eq!(fs::read(src.join("b")).unwrap(), b2);
}

#[test]
fn one_job_per_device_independent_devices_in_parallel() {
    let mut h = h();
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(h.w("a"), b"new").unwrap();
    fs::write(dest.join("a"), b"old").unwrap();
    // Job 1 blocks its device in Conflict.
    let j1 = h.submit(copy(&[&h.w("a")], &dest, P::Ask));
    h.wait(j1, |s| matches!(s, JobStatus::Conflict { .. }));
    // Job 2 on the same device waits behind it.
    let j2 = h.submit(JobSpec::Mkdir {
        path: b(&h.w("n")),
        on_conflict: P::Fail,
    });
    // Job 3 on another device runs meanwhile.
    if let Some(far) = other_device(h.d.path()) {
        let j3 = h.submit(JobSpec::Mkdir {
            path: b(&far.path().join("n")),
            on_conflict: P::Fail,
        });
        assert_eq!(h.end(j3), JobStatus::Done);
    } else {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!h.w("n").exists(), "job 2 ran beside job 1");
    h.ctl(
        j1,
        JobAction::Resolve {
            choice: Resolution::Skip,
            apply_all: false,
        },
    );
    assert_eq!(h.end(j1), JobStatus::Done);
    assert_eq!(h.end(j2), JobStatus::Done);
    assert!(h.w("n").is_dir());
}

/// A daemon killed mid-copy (the paused job simply never resumes) leaves an
/// intent in the journal; the next daemon removes the partial destination,
/// puts the replaced original back and leaves the source alone.
#[test]
fn crash_mid_copy_is_reconciled_at_next_start() {
    let mut h = h();
    h.jobs
        .inner()
        .pause_after_chunks
        .store(1, Ordering::Relaxed);
    let src = h.w("big");
    let data = write_big(&src, 24 << 20);
    let dest = h.w("dest");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("big"), b"OLD").unwrap();
    fs::write(dest.join("other"), b"mine").unwrap();
    let id = h.submit(copy(&[&src], &dest, P::Replace));
    h.wait(id, |s| *s == JobStatus::Paused);
    let part = fs::metadata(dest.join("big")).unwrap().len();
    assert!(part > 3 && part < data.len() as u64, "partial {part}");
    assert_eq!(names(&dest).len(), 3, "partial, aside and other");

    // "Crash": a new daemon on the same state, the old worker frozen.
    let again = Jobs::new(&h.dirs);
    let log = again.inner().journal().unwrap().recovered().to_vec();
    assert!(
        log.iter().any(|l| l.starts_with("removed partial")),
        "{log:?}"
    );
    assert!(log.iter().any(|l| l.starts_with("put back")), "{log:?}");
    assert_eq!(names(&dest), vec!["big", "other"]);
    assert_eq!(fs::read(dest.join("big")).unwrap(), b"OLD");
    assert_eq!(fs::read(dest.join("other")).unwrap(), b"mine");
    assert_eq!(fs::read(&src).unwrap(), data);
    // Reconciled once: a further restart finds nothing to do.
    drop(again);
    let third = Jobs::new(&h.dirs);
    assert!(third.inner().journal().unwrap().recovered().is_empty());
}

#[test]
fn crash_mid_cross_device_move_keeps_sources() {
    let mut h = h();
    let Some(far) = other_device(h.d.path()) else {
        eprintln!("skipped: no second filesystem at /dev/shm");
        return;
    };
    h.jobs
        .inner()
        .pause_after_chunks
        .store(2, Ordering::Relaxed);
    let src = h.w("d");
    fs::create_dir(&src).unwrap();
    let a = write_big(&src.join("a"), 9 << 20);
    let b2 = write_big(&src.join("b"), 9 << 20);
    fs::write(far.path().join("before"), b"kept").unwrap();
    let id = h.submit(mv(&[&src], far.path(), P::Fail));
    h.wait(id, |s| *s == JobStatus::Paused);
    assert!(far.path().join("d").exists());

    let _again = Jobs::new(&h.dirs);
    assert_eq!(names(far.path()), vec!["before"]);
    assert_eq!(fs::read(far.path().join("before")).unwrap(), b"kept");
    assert_eq!(fs::read(src.join("a")).unwrap(), a);
    assert_eq!(fs::read(src.join("b")).unwrap(), b2);
}

#[test]
fn undo_is_broadcast_as_a_job() {
    let mut h = h();
    fs::write(h.w("a"), b"A").unwrap();
    let ren = JobSpec::Rename {
        path: b(&h.w("a")),
        new_name: b"b".to_vec(),
        on_conflict: P::Fail,
    };
    assert_eq!(h.run(ren), JobStatus::Done);
    let mut rx = h.jobs.subscribe();
    h.undo().unwrap();
    let mut states = Vec::new();
    while let Ok(r) = rx.try_recv() {
        if let Reply::JobState { state, .. } = r {
            states.push(state);
        }
    }
    assert_eq!(
        states,
        vec![JobStatus::Queued, JobStatus::Running, JobStatus::Done]
    );
    assert!(h.w("a").exists());
    // Nothing left: refused at once, no job broadcast.
    assert_eq!(h.undo().unwrap_err(), "nothing to undo");
    assert!(rx.try_recv().is_err());
}
