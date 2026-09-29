// SPDX-License-Identifier: AGPL-3.0-only

//! The undo journal (FOG §File operations/Undo journal).
//!
//! `$XDG_STATE_HOME/fog/journal`, append-only, one `fsync` per record. A
//! record is `u32 len | u32 fnv1a(payload) | postcard(Record)`; loading stops
//! at the first torn or corrupt record. On open the file is compacted to the
//! last [`KEEP_OPS`] live entries no older than [`KEEP_SECS`].
//!
//! Crash safety: a job writes `Begin` before it touches anything, `Created`
//! for each outermost node right after its `O_EXCL` creation (before any
//! data), `Aside` for each replaced destination, `Commit` before its first
//! irreversible step, and `End` when finished. On open every run without an
//! `End` is reconciled: nodes created since its last `Commit` are removed if
//! their inode still matches, and set-aside originals are renamed back.
//! Sources are never touched, and nothing is removed that this journal does
//! not name as created by that run.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use fog_proto::JobSpec;
use serde::{Deserialize, Serialize};

use crate::ops;

pub const KEEP_OPS: usize = 200;
pub const KEEP_SECS: i64 = 30 * 24 * 3600;

/// One effect of a job, with the identity of what it produced so undo can
/// tell whether it changed since (mtime/inode check).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
    /// Rename or move; undo moves `to` back to `from`.
    Moved {
        from: Vec<u8>,
        to: Vec<u8>,
        ino: u64,
        mtime_ns: i128,
    },
    /// A copy; undo trashes it.
    Copied {
        path: Vec<u8>,
        ino: u64,
        mtime_ns: i128,
    },
    /// Mkdir (`dir`) or create file; undo removes it if still unchanged.
    Made {
        path: Vec<u8>,
        ino: u64,
        mtime_ns: i128,
        dir: bool,
    },
    /// Undo restores `files` (with `info`) to `orig`.
    Trashed {
        orig: Vec<u8>,
        files: Vec<u8>,
        info: Vec<u8>,
        ino: u64,
        mtime_ns: i128,
    },
    /// Restored from trash; undo trashes it again.
    Restored {
        path: Vec<u8>,
        ino: u64,
        mtime_ns: i128,
    },
    /// Permanent delete: never undoable.
    Deleted { path: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub time_s: i64,
    /// False for permanent delete and for any job that replaced a file.
    pub undoable: bool,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Record {
    Op(Entry),
    Undone(u64),
    /// A run started; `None` is an undo.
    Begin {
        run: u64,
        spec: Option<JobSpec>,
    },
    /// The run created `path` (outermost node) with inode `ino`.
    Created {
        run: u64,
        path: Vec<u8>,
        ino: u64,
    },
    /// The run renamed the existing `target` aside to `aside`.
    Aside {
        run: u64,
        target: Vec<u8>,
        aside: Vec<u8>,
    },
    /// Everything recorded so far for the run is final.
    Commit(u64),
    End(u64),
}

/// A run without an `End`, as far as the journal knows.
#[derive(Default)]
struct Intent {
    spec: Option<JobSpec>,
    created: Vec<(Vec<u8>, u64)>,
    asides: Vec<(Vec<u8>, Vec<u8>)>,
    /// Asides of committed parts: replaced originals not yet deleted.
    stale: Vec<Vec<u8>>,
}

pub struct Journal {
    path: PathBuf,
    file: File,
    /// Live (not undone) entries, oldest first.
    entries: Vec<Entry>,
    next_seq: u64,
    /// What reconciling interrupted runs did at open, one line each.
    recovered: Vec<String>,
}

impl Journal {
    /// `$XDG_STATE_HOME/fog/journal` under `state_home`.
    pub fn path_in(state_home: &Path) -> PathBuf {
        state_home.join("fog").join("journal")
    }

    /// Load, compact and reopen for appending.
    pub fn open(path: &Path, now_s: i64) -> io::Result<Self> {
        if let Some(p) = path.parent() {
            DirBuilder::new().recursive(true).mode(0o700).create(p)?;
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut max_seq = 0;
        let mut open: BTreeMap<u64, Intent> = BTreeMap::new();
        match fs::read(path) {
            Ok(buf) => {
                for r in records(&buf) {
                    match r {
                        Record::Op(e) => {
                            max_seq = max_seq.max(e.seq);
                            entries.push(e);
                        }
                        Record::Undone(s) => entries.retain(|e| e.seq != s),
                        Record::Begin { run, spec } => {
                            max_seq = max_seq.max(run);
                            open.insert(
                                run,
                                Intent {
                                    spec,
                                    ..Intent::default()
                                },
                            );
                        }
                        Record::Created { run, path, ino } => {
                            if let Some(i) = open.get_mut(&run) {
                                i.created.push((path, ino));
                            }
                        }
                        Record::Aside { run, target, aside } => {
                            if let Some(i) = open.get_mut(&run) {
                                i.asides.push((target, aside));
                            }
                        }
                        Record::Commit(run) => {
                            if let Some(i) = open.get_mut(&run) {
                                i.created.clear();
                                let a = std::mem::take(&mut i.asides);
                                i.stale.extend(a.into_iter().map(|(_, a)| a));
                            }
                        }
                        Record::End(run) => {
                            open.remove(&run);
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut recovered = Vec::new();
        for (run, i) in open {
            recover(run, i, &mut recovered);
        }
        for line in &recovered {
            tracing::warn!("journal recovery: {line}");
        }
        entries.retain(|e| now_s - e.time_s <= KEEP_SECS);
        if entries.len() > KEEP_OPS {
            entries.drain(..entries.len() - KEEP_OPS);
        }
        // Rewrite compacted: write aside, sync, rename into place, sync dir.
        let tmp = path.with_extension("compact");
        {
            let mut f = File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            for e in &entries {
                f.write_all(&frame(&Record::Op(e.clone()))?)?;
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        if let Some(p) = path.parent() {
            File::open(p)?.sync_all()?;
        }
        let file = File::options().append(true).mode(0o600).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            entries,
            next_seq: max_seq + 1,
            recovered,
        })
    }

    /// What reconciling interrupted runs did when this journal was opened.
    pub fn recovered(&self) -> &[String] {
        &self.recovered
    }

    /// Start a run, durably, before it touches the filesystem.
    pub fn begin(&mut self, spec: Option<&JobSpec>) -> io::Result<u64> {
        let run = self.next_seq;
        self.write(&Record::Begin {
            run,
            spec: spec.cloned(),
        })?;
        self.next_seq += 1;
        Ok(run)
    }

    /// `path` was just created by `run` (before any data is written to it).
    pub fn created(&mut self, run: u64, path: &Path, ino: u64) -> io::Result<()> {
        self.write(&Record::Created {
            run,
            path: ops::raw(path),
            ino,
        })
    }

    /// `run` renamed the existing `target` to `aside`.
    pub fn aside(&mut self, run: u64, target: &Path, aside: &Path) -> io::Result<()> {
        self.write(&Record::Aside {
            run,
            target: ops::raw(target),
            aside: ops::raw(aside),
        })
    }

    /// Everything `run` recorded so far is final.
    pub fn commit(&mut self, run: u64) -> io::Result<()> {
        self.write(&Record::Commit(run))
    }

    /// `run` finished (done, failed and rolled back, or cancelled).
    pub fn end(&mut self, run: u64) -> io::Result<()> {
        self.write(&Record::End(run))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, r: &Record) -> io::Result<()> {
        self.file.write_all(&frame(r)?)?;
        self.file.sync_data()
    }

    /// Append an entry; durable when this returns.
    pub fn append(&mut self, undoable: bool, items: Vec<Item>, now_s: i64) -> io::Result<u64> {
        let e = Entry {
            seq: self.next_seq,
            time_s: now_s,
            undoable,
            items,
        };
        self.write(&Record::Op(e.clone()))?;
        self.next_seq += 1;
        self.entries.push(e);
        if self.entries.len() > KEEP_OPS {
            self.entries.remove(0);
        }
        Ok(self.next_seq - 1)
    }

    /// The newest entry that can still be undone.
    pub fn last_undoable(&self) -> Option<&Entry> {
        self.entries.iter().rev().find(|e| e.undoable)
    }

    pub fn mark_undone(&mut self, seq: u64) -> io::Result<()> {
        self.write(&Record::Undone(seq))?;
        self.entries.retain(|e| e.seq != seq);
        Ok(())
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

/// Reconcile one interrupted run: remove what it created since its last
/// commit (only if the inode still matches) and rename its set-aside
/// originals back. Sources are never touched.
fn recover(run: u64, i: Intent, log: &mut Vec<String>) {
    let what = i
        .spec
        .as_ref()
        .map_or_else(|| "undo".to_owned(), |s| format!("{s:?}"));
    log.push(format!("run {run} was interrupted: {what}"));
    for (p, ino) in i.created.iter().rev() {
        let p = ops::path_of(p);
        match ops::lstat(&p) {
            Ok(st) if st.st_ino == *ino => match ops::remove_tree(&p) {
                Ok(()) => log.push(format!("removed partial {}", p.display())),
                Err(e) => log.push(format!("could not remove partial {}: {e}", p.display())),
            },
            Ok(_) => log.push(format!("kept {} (not created by this run)", p.display())),
            Err(_) => {}
        }
    }
    for (t, a) in i.asides.iter().rev() {
        let (t, a) = (ops::path_of(t), ops::path_of(a));
        if ops::lstat(&a).is_err() {
            continue;
        }
        match ops::rename_noreplace(&a, &t) {
            Ok(()) => log.push(format!("put back {}", t.display())),
            Err(e) => log.push(format!(
                "kept replaced original at {} ({} not free: {e})",
                a.display(),
                t.display()
            )),
        }
    }
    for a in &i.stale {
        let a = ops::path_of(a);
        if ops::lstat(&a).is_ok() {
            log.push(format!("replaced original left at {}", a.display()));
        }
    }
}

fn fnv1a(b: &[u8]) -> u32 {
    b.iter().fold(0x811c_9dc5u32, |h, &c| {
        (h ^ u32::from(c)).wrapping_mul(0x0100_0193)
    })
}

fn frame(r: &Record) -> io::Result<Vec<u8>> {
    let payload = postcard::to_allocvec(r).map_err(io::Error::other)?;
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&fnv1a(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

fn records(mut buf: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    while let Some((hdr, rest)) = buf.split_first_chunk::<8>() {
        let len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        let sum = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
        let Some(payload) = rest.get(..len) else {
            break;
        };
        if fnv1a(payload) != sum {
            break;
        }
        let Ok(r) = postcard::from_bytes(payload) else {
            break;
        };
        out.push(r);
        buf = &rest[len..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn made(p: &str) -> Vec<Item> {
        vec![Item::Made {
            path: p.as_bytes().to_vec(),
            ino: 1,
            mtime_ns: -1,
            dir: true,
        }]
    }

    #[test]
    fn survives_restart_and_undone_marks() {
        let d = tempfile::tempdir().unwrap();
        let p = Journal::path_in(d.path());
        let mut j = Journal::open(&p, 1000).unwrap();
        let a = j.append(true, made("/a"), 1000).unwrap();
        let b = j.append(true, made("/b"), 1001).unwrap();
        j.append(
            false,
            vec![Item::Deleted {
                path: b"/c".to_vec(),
            }],
            1002,
        )
        .unwrap();
        assert_eq!(j.last_undoable().unwrap().seq, b);
        j.mark_undone(b).unwrap();
        drop(j);
        let j = Journal::open(&p, 1003).unwrap();
        assert_eq!(j.entries().len(), 2);
        assert_eq!(j.last_undoable().unwrap().seq, a);
        assert!(j.next_seq > b);
    }

    #[test]
    fn compacts_by_count_and_age() {
        let d = tempfile::tempdir().unwrap();
        let p = Journal::path_in(d.path());
        let mut j = Journal::open(&p, 0).unwrap();
        for i in 0..250 {
            j.append(true, made("/x"), i).unwrap();
        }
        drop(j);
        let j = Journal::open(&p, 250).unwrap();
        assert_eq!(j.entries().len(), KEEP_OPS);
        assert_eq!(j.entries()[0].time_s, 50);
        drop(j);
        let j = Journal::open(&p, 249 + KEEP_SECS).unwrap();
        assert_eq!(j.entries().len(), 1);
    }

    fn ino(p: &Path) -> u64 {
        ops::lstat(p).unwrap().st_ino
    }

    /// Crash simulation: intent plus partial-destination records and a
    /// partial file, then a restart.
    #[test]
    fn restart_removes_only_what_the_run_created() {
        let d = tempfile::tempdir().unwrap();
        let p = Journal::path_in(&d.path().join("state"));
        let (src, dest) = (d.path().join("src"), d.path().join("dest"));
        fs::create_dir(&dest).unwrap();
        fs::write(&src, b"all the data").unwrap();
        fs::write(dest.join("before"), b"was here first").unwrap();
        let spec = JobSpec::Copy {
            srcs: vec![ops::raw(&src)],
            dest: ops::raw(&dest),
            on_conflict: fog_proto::ConflictPolicy::Fail,
        };
        let mut j = Journal::open(&p, 0).unwrap();
        let run = j.begin(Some(&spec)).unwrap();
        let part = dest.join("src");
        fs::write(&part, b"all t").unwrap();
        j.created(run, &part, ino(&part)).unwrap();
        // A record naming a path whose inode is not the one this run made
        // (the file that was there before) must never be acted on.
        j.created(run, &dest.join("before"), ino(&part)).unwrap();
        drop(j);

        let j = Journal::open(&p, 0).unwrap();
        assert!(!part.exists());
        assert_eq!(fs::read(dest.join("before")).unwrap(), b"was here first");
        assert_eq!(fs::read(&src).unwrap(), b"all the data");
        assert_eq!(j.recovered().len(), 3, "{:?}", j.recovered());
        drop(j);
        assert!(Journal::open(&p, 0).unwrap().recovered().is_empty());
    }

    #[test]
    fn restart_puts_asides_back_and_keeps_committed_work() {
        let d = tempfile::tempdir().unwrap();
        let p = Journal::path_in(&d.path().join("state"));
        let (t, a) = (d.path().join("t"), d.path().join(".fog-replaced-1-0"));
        fs::write(&t, b"original").unwrap();
        let mut j = Journal::open(&p, 0).unwrap();
        let run = j.begin(None).unwrap();
        fs::rename(&t, &a).unwrap();
        j.aside(run, &t, &a).unwrap();
        fs::write(&t, b"partial").unwrap();
        j.created(run, &t, ino(&t)).unwrap();
        // A second, committed run: its output is final.
        let done = d.path().join("done");
        let run2 = j.begin(None).unwrap();
        fs::write(&done, b"final").unwrap();
        j.created(run2, &done, ino(&done)).unwrap();
        j.commit(run2).unwrap();
        // A finished run is never revisited.
        let ended = d.path().join("ended");
        let run3 = j.begin(None).unwrap();
        fs::write(&ended, b"x").unwrap();
        j.created(run3, &ended, ino(&ended)).unwrap();
        j.end(run3).unwrap();
        drop(j);

        let j = Journal::open(&p, 0).unwrap();
        assert_eq!(fs::read(&t).unwrap(), b"original");
        assert!(!a.exists());
        assert_eq!(fs::read(&done).unwrap(), b"final");
        assert!(ended.exists());
        assert!(j.next_seq > run3);
    }

    #[test]
    fn torn_tail_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let p = Journal::path_in(d.path());
        let mut j = Journal::open(&p, 0).unwrap();
        j.append(true, made("/a"), 0).unwrap();
        drop(j);
        let mut f = File::options().append(true).open(&p).unwrap();
        f.write_all(&[40, 0, 0, 0, 1, 2, 3, 4, 9]).unwrap();
        drop(f);
        let mut j = Journal::open(&p, 0).unwrap();
        assert_eq!(j.entries().len(), 1);
        j.append(true, made("/b"), 0).unwrap();
        drop(j);
        assert_eq!(Journal::open(&p, 0).unwrap().entries().len(), 2);
    }
}
