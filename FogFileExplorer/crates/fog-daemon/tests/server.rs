// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end over a real Unix socket.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fog_daemon::{bind, serve, sort, Cache, Daemon, Dirs, Jobs, LocalBackend, BATCH};
use fog_proto::{
    apply_diff, read_frame, write_frame, ConflictPolicy, Entry, JobSpec, JobStatus, Kind, Reply,
    Request,
};
use tokio::net::UnixStream;

struct Harness {
    sock: std::path::PathBuf,
    _dir: tempfile::TempDir,
    stream: UnixStream,
}

async fn start() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("run/fog/fogd.sock");
    let listener = bind(&sock).unwrap();
    assert_eq!(
        fs::metadata(&sock).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(sock.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    // Trash and journal under the test's own XDG dirs, never the real ones.
    let jobs = Jobs::new(&Dirs {
        data_home: dir.path().join("data"),
        state_home: dir.path().join("state"),
    });
    tokio::spawn(serve(
        listener,
        Arc::new(Daemon::new(Box::new(LocalBackend), Cache::default()).with_jobs(jobs)),
    ));
    let stream = UnixStream::connect(&sock).await.unwrap();
    Harness {
        sock,
        _dir: dir,
        stream,
    }
}

impl Harness {
    async fn send(&mut self, req: Request) {
        write_frame(&mut self.stream, &req).await.unwrap();
    }

    async fn recv(&mut self) -> Reply {
        read_frame(&mut self.stream).await.unwrap().unwrap()
    }

    async fn list(&mut self, path: &Path) {
        self.send(Request::ListDir {
            path: path.as_os_str().as_bytes().to_vec(),
        })
        .await;
    }

    /// Apply replies to `v` until `done` holds; fails after 5 s.
    async fn until(&mut self, v: &mut View, done: impl Fn(&View) -> bool) {
        let t = tokio::time::timeout(Duration::from_secs(5), async {
            while !done(v) {
                v.apply(self.recv().await);
            }
        })
        .await;
        assert!(t.is_ok(), "timed out; have {:?}", v.names());
    }

    /// Assert nothing arrives for a while.
    async fn quiet(&mut self) {
        if let Ok(r) = tokio::time::timeout(Duration::from_millis(300), self.recv()).await {
            panic!("unexpected {r:?}");
        }
    }
}

fn raw(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

/// Client-side view: entries + order, as the UI would hold them.
#[derive(Default)]
struct View {
    dir: u64,
    generation: u64,
    entries: Vec<Entry>,
    order: Vec<u32>,
    complete: bool,
}

impl View {
    fn apply(&mut self, r: Reply) {
        match r {
            Reply::DirSnapshot {
                dir,
                generation,
                entries,
                order,
                complete,
                ..
            } => {
                *self = View {
                    dir,
                    generation,
                    entries,
                    order,
                    complete,
                }
            }
            Reply::DirDiff {
                dir,
                generation,
                removed,
                added,
                changed,
                order,
                complete,
            } => {
                assert_eq!(dir, self.dir);
                assert!(generation > self.generation);
                apply_diff(&mut self.entries, &removed, &added, &changed);
                self.generation = generation;
                self.order = order;
                self.complete = complete;
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn meta_done(&self) -> bool {
        self.complete && self.entries.iter().all(|e| e.mode.is_some())
    }

    fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name.as_bytes())
    }

    fn names(&self) -> Vec<String> {
        assert_eq!(self.order.len(), self.entries.len());
        self.order
            .iter()
            .map(|&i| self.entries[i as usize].display().into_owned())
            .collect()
    }
}

fn expected(dir: &Path) -> Vec<String> {
    let entries: Vec<Entry> = fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            Entry::new(
                e.file_name().as_bytes().to_vec(),
                if e.file_type().unwrap().is_dir() {
                    Kind::Dir
                } else {
                    Kind::File
                },
            )
        })
        .collect();
    sort::order(&entries)
        .into_iter()
        .map(|i| entries[i as usize].display().into_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn uncached_then_cached_with_diff() {
    let mut h = start().await;
    let d = tempfile::tempdir().unwrap();
    for n in ["file10", "file2", "gone"] {
        fs::write(d.path().join(n), b"").unwrap();
    }
    fs::create_dir(d.path().join("zdir")).unwrap();

    h.list(d.path()).await;
    let mut v = View::default();
    v.apply(h.recv().await);
    assert!(v.complete);
    assert_eq!(v.generation, 0);
    assert_eq!(v.names(), ["zdir", "file2", "file10", "gone"]);
    // Phase 2 follows as metadata-only diffs.
    h.until(&mut v, View::meta_done).await;
    assert!(v.generation >= 1);
    let dir = v.dir;

    fs::write(d.path().join("file1"), b"").unwrap();
    fs::remove_file(d.path().join("gone")).unwrap();
    // A kind change: zdir becomes a file.
    fs::remove_dir(d.path().join("zdir")).unwrap();
    fs::write(d.path().join("zdir"), b"").unwrap();

    // Cached snapshot first (the watcher may already have refreshed it),
    // then whatever diff the mtime check finds.
    h.list(d.path()).await;
    let mut v2 = View::default();
    v2.apply(h.recv().await);
    assert!(v2.complete);
    assert_eq!(v2.dir, dir);
    let want = expected(d.path());
    h.until(&mut v2, |v| v.names() == want && v.meta_done())
        .await;
    assert_eq!(v2.names(), ["file1", "file2", "file10", "zdir"]);

    // Unchanged: the cached snapshot, metadata kept, and nothing else.
    h.list(d.path()).await;
    let mut v3 = View::default();
    v3.apply(h.recv().await);
    assert_eq!(v3.generation, v2.generation);
    assert_eq!(v3.names(), v2.names());
    assert!(v3.meta_done());
    h.quiet().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn phase2_metadata() {
    let mut h = start().await;
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("f"), b"hello").unwrap();
    fs::create_dir(d.path().join("sub")).unwrap();
    std::os::unix::fs::symlink("f", d.path().join("l")).unwrap();

    h.list(d.path()).await;
    let mut v = View::default();
    v.apply(h.recv().await);
    assert!(
        v.entries.iter().all(|e| e.mode.is_none()),
        "phase 1 has no stat"
    );
    h.until(&mut v, View::meta_done).await;
    for name in ["f", "sub", "l"] {
        let e = v.get(name).unwrap();
        let m = fs::symlink_metadata(d.path().join(name)).unwrap();
        assert_eq!(e.mode, Some(m.mode()), "{name}");
        assert_eq!(e.size, Some(m.size()), "{name}");
        assert_eq!(
            e.mtime_ns,
            Some(i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec())),
            "{name}"
        );
    }
    assert_eq!(v.get("f").unwrap().size, Some(5));
    assert_eq!(v.get("l").unwrap().mode.unwrap() & 0o170000, 0o120000);
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_pushes_diffs() {
    let mut h = start().await;
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a"), b"").unwrap();

    h.send(Request::Subscribe {
        path: raw(d.path()),
    })
    .await;
    let mut v = View::default();
    h.until(&mut v, View::meta_done).await;
    assert_eq!(v.names(), ["a"]);

    // Create: an added entry, stat'ed before it is pushed.
    fs::write(d.path().join("b"), b"xy").unwrap();
    h.until(&mut v, |v| v.get("b").is_some_and(|e| e.size == Some(2)))
        .await;
    assert_eq!(v.names(), ["a", "b"]);

    // Rename: removed + added.
    fs::rename(d.path().join("b"), d.path().join("c")).unwrap();
    h.until(&mut v, |v| v.names() == ["a", "c"]).await;
    assert_eq!(v.get("c").unwrap().size, Some(2));

    // Content change: metadata only.
    fs::write(d.path().join("c"), b"four").unwrap();
    h.until(&mut v, |v| v.get("c").unwrap().size == Some(4))
        .await;

    // Delete.
    fs::remove_file(d.path().join("a")).unwrap();
    h.until(&mut v, |v| v.names() == ["c"]).await;
    assert_eq!(v.names(), expected(d.path()));

    // Unsubscribed: changes are no longer pushed.
    h.send(Request::Unsubscribe { dir: v.dir }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    fs::write(d.path().join("z"), b"").unwrap();
    h.quiet().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribed_dir_removed_reports_error() {
    let mut h = start().await;
    let d = tempfile::tempdir().unwrap();
    let sub = d.path().join("sub");
    fs::create_dir(&sub).unwrap();
    h.send(Request::Subscribe { path: raw(&sub) }).await;
    let mut v = View::default();
    v.apply(h.recv().await);
    assert!(v.complete && v.entries.is_empty());
    fs::remove_dir(&sub).unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), h.recv())
        .await
        .unwrap();
    assert_eq!(
        r,
        Reply::Error {
            path: raw(&sub),
            errno: 2
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn large_dir_streams() {
    let mut h = start().await;
    let d = tempfile::tempdir().unwrap();
    let n = 2 * BATCH + 123;
    for i in 0..n {
        fs::write(d.path().join(format!("f{i}")), b"").unwrap();
    }
    h.list(d.path()).await;
    let first = h.recv().await;
    let mut v = View::default();
    match &first {
        Reply::DirSnapshot {
            complete, entries, ..
        } => {
            assert!(!complete);
            assert_eq!(entries.len(), BATCH);
        }
        other => panic!("unexpected {other:?}"),
    }
    v.apply(first);
    let partial = v.names();
    assert!(partial
        .windows(2)
        .all(|w| sort::natural(w[0].as_bytes(), w[1].as_bytes()).is_lt()));

    let r = h.recv().await;
    match &r {
        Reply::DirDiff {
            complete,
            removed,
            added,
            ..
        } => {
            assert!(complete);
            assert!(removed.is_empty());
            assert_eq!(added.len(), n - BATCH);
        }
        other => panic!("unexpected {other:?}"),
    }
    v.apply(r);
    let want: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
    assert_eq!(v.names(), want);
    // Phase 2 fills every entry, in batches.
    h.until(&mut v, View::meta_done).await;
    assert!(v.generation >= 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn errors() {
    let mut h = start().await;
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope").as_os_str().as_bytes().to_vec();
    h.send(Request::ListDir {
        path: missing.clone(),
    })
    .await;
    assert_eq!(
        h.recv().await,
        Reply::Error {
            path: missing.clone(),
            errno: 2
        }
    );
    h.send(Request::Stat {
        path: missing.clone(),
    })
    .await;
    assert_eq!(
        h.recv().await,
        Reply::Error {
            path: missing,
            errno: 2
        }
    );
    h.send(Request::ListDir {
        path: b"relative/dir".to_vec(),
    })
    .await;
    assert_eq!(
        h.recv().await,
        Reply::Error {
            path: b"relative/dir".to_vec(),
            errno: 22
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_socket_is_not_stolen() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("fog/fogd.sock");
    let l = bind(&sock).unwrap();
    assert_eq!(
        bind(&sock).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );
    drop(l);
    // Now stale: replaced.
    bind(&sock).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn job_states_reach_every_client() {
    let mut h = start().await;
    let other = UnixStream::connect(&h.sock).await.unwrap();
    let mut o = Harness {
        sock: h.sock.clone(),
        _dir: tempfile::tempdir().unwrap(),
        stream: other,
    };
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("new");
    // Round trips so both clients are subscribed before the job runs.
    h.send(Request::Places).await;
    h.recv().await;
    o.send(Request::Places).await;
    o.recv().await;
    h.send(Request::Job(JobSpec::Mkdir {
        path: path.as_os_str().as_bytes().to_vec(),
        on_conflict: ConflictPolicy::Fail,
    }))
    .await;
    let Reply::JobAccepted { id } = h.recv().await else {
        panic!("JobAccepted first");
    };
    for c in [&mut h, &mut o] {
        loop {
            match c.recv().await {
                Reply::JobState {
                    id: i,
                    state: JobStatus::Done,
                } if i == id => break,
                Reply::JobState { id: i, state } => {
                    assert_eq!(i, id);
                    assert!(matches!(state, JobStatus::Queued | JobStatus::Running));
                }
                Reply::JobProgress { id: i, .. } => assert_eq!(i, id),
                r => panic!("unexpected {r:?}"),
            }
        }
    }
    assert!(path.is_dir());
    // Undo is broadcast like a job; its result goes to the asker.
    h.send(Request::Undo).await;
    loop {
        match h.recv().await {
            Reply::UndoResult { ok, reason } => {
                assert!(ok, "{reason:?}");
                break;
            }
            Reply::JobState { .. } | Reply::JobProgress { .. } => {}
            r => panic!("unexpected {r:?}"),
        }
    }
    assert!(!path.exists());
    let mut states = Vec::new();
    while states.last() != Some(&JobStatus::Done) {
        match o.recv().await {
            Reply::JobState { state, .. } => states.push(state),
            Reply::JobProgress { .. } => {}
            r => panic!("unexpected {r:?}"),
        }
    }
    assert_eq!(
        states,
        vec![JobStatus::Queued, JobStatus::Running, JobStatus::Done]
    );
}
