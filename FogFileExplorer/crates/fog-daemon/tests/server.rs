// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end over a real Unix socket.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use fog_daemon::{bind, serve, sort, Daemon, BATCH};
use fog_proto::{apply_diff, read_frame, write_frame, Entry, Kind, Reply, Request};
use tokio::net::UnixStream;

struct Harness {
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
    tokio::spawn(serve(listener, Arc::new(Daemon::local())));
    let stream = UnixStream::connect(&sock).await.unwrap();
    Harness { _dir: dir, stream }
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
                order,
                complete,
            } => {
                assert_eq!(dir, self.dir);
                assert!(generation > self.generation);
                apply_diff(&mut self.entries, &removed, &added);
                self.generation = generation;
                self.order = order;
                self.complete = complete;
            }
            other => panic!("unexpected {other:?}"),
        }
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
            Entry {
                name: e.file_name().as_bytes().to_vec(),
                kind: if e.file_type().unwrap().is_dir() {
                    Kind::Dir
                } else {
                    Kind::File
                },
            }
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
    let dir = v.dir;

    fs::write(d.path().join("file1"), b"").unwrap();
    fs::remove_file(d.path().join("gone")).unwrap();
    // A kind change: zdir becomes a file.
    fs::remove_dir(d.path().join("zdir")).unwrap();
    fs::write(d.path().join("zdir"), b"").unwrap();

    h.list(d.path()).await;
    // Cached snapshot first, unchanged.
    let snap = h.recv().await;
    let mut v2 = View::default();
    v2.apply(snap);
    assert!(v2.complete);
    assert_eq!(v2.dir, dir);
    assert_eq!(v2.names(), ["zdir", "file2", "file10", "gone"]);
    // Then the diff.
    let r = h.recv().await;
    assert!(matches!(
        r,
        Reply::DirDiff {
            generation: 1,
            complete: true,
            ..
        }
    ));
    v2.apply(r);
    assert_eq!(v2.names(), ["file1", "file2", "file10", "zdir"]);
    assert_eq!(v2.names(), expected(d.path()));

    // Unchanged: snapshot at generation 1 and nothing else. A Stat after it
    // proves no diff was queued in between.
    h.list(d.path()).await;
    let mut v3 = View::default();
    v3.apply(h.recv().await);
    assert_eq!(v3.generation, 1);
    assert_eq!(v3.names(), v2.names());
    h.send(Request::Stat {
        path: d.path().join("file1").as_os_str().as_bytes().to_vec(),
    })
    .await;
    match h.recv().await {
        Reply::Stat(s) => assert_eq!(s.kind, Kind::File),
        other => panic!("unexpected {other:?}"),
    }
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
