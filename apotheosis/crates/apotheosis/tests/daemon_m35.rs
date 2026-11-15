// SPDX-License-Identifier: AGPL-3.0-only
//! M35 gates: the session daemon (T-21 §3-§4, §6). `apotheosis --server` runs with no
//! compositor; the test is a protocol client that keeps its own restored `Term` per pane
//! exactly as a GUI view does (snapshot, then the tee), so "what a view shows" is checked on
//! real terms fed the real stream.
//!
//! The test binary is not the GUI, so it is pre-granted through the debug-build hook
//! `APOTH_TEST_ATTACH_EXE`; one test leaves the hook out and must be refused.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use apoth_proto::mux::{Decoded, MUX_MAJOR, MUX_MINOR, RecordDecoder, ServerMsg, ViewMsg};
use apoth_term::{Term, TermConfig, TermImage};

/// Kills the server by the pid captured at spawn.
struct Server {
    child: Child,
    dir: PathBuf,
    sock: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn server_cmd(dir: &Path, grant: bool) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_apotheosis"));
    c.env("XDG_RUNTIME_DIR", dir)
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("home/.config"))
        .env("XDG_CACHE_HOME", dir.join("home/.cache"))
        .env("SHELL", "/bin/sh")
        .env("ENV", "/dev/null")
        .env_remove("APOTHEOSIS_SOCKET")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if grant {
        c.env("APOTH_TEST_ATTACH_EXE", std::env::current_exe().unwrap());
    } else {
        c.env_remove("APOTH_TEST_ATTACH_EXE");
    }
    c
}

fn start(name: &str, grant: bool, extra: &[&str]) -> Server {
    let dir = std::env::temp_dir().join(format!("apm35{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home/.config")).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let child = server_cmd(&dir, grant)
        .args(["--server", "--session", "t"])
        .args(extra)
        .spawn()
        .expect("spawn the server");
    let sock = dir.join("apotheosis/server-t.sock");
    let t0 = Instant::now();
    while UnixStream::connect(&sock).is_err() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "the server never listened"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Server { child, dir, sock }
}

/// A snapshot being received: (sequence, state, pages per screen).
type Building = (u64, Vec<u8>, [Vec<Vec<u8>>; 2]);

/// One pane as a view holds it.
#[derive(Default)]
struct PaneView {
    term: Option<Term>,
    next: u64,
    building: Option<Building>,
    snapshots: u32,
    exited: Option<Option<i32>>,
}

struct Client {
    s: UnixStream,
    dec: RecordDecoder,
    panes: BTreeMap<u64, PaneView>,
    refused: Option<String>,
    spawned: Vec<(u64, Option<u64>)>,
    lists: Vec<Vec<apoth_proto::mux::PaneEntry>>,
    eof: bool,
}

impl Client {
    fn connect(sock: &Path) -> Client {
        let s = UnixStream::connect(sock).expect("connect");
        s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        Client {
            s,
            dec: RecordDecoder::new(),
            panes: BTreeMap::new(),
            refused: None,
            spawned: Vec::new(),
            lists: Vec::new(),
            eof: false,
        }
    }

    fn send(&mut self, m: &ViewMsg) {
        let _ = self.s.write_all(&m.encode());
    }

    fn hello(&mut self, major: u16) -> Option<ServerMsg> {
        self.send(&ViewMsg::Hello {
            major,
            minor: MUX_MINOR,
            caps: Vec::new(),
            client: "test".into(),
        });
        let t0 = Instant::now();
        loop {
            if let Some(m) = self.next_msg() {
                return Some(m);
            }
            if self.eof || t0.elapsed() > Duration::from_secs(5) {
                return None;
            }
        }
    }

    fn attach(sock: &Path) -> Client {
        let mut c = Client::connect(sock);
        let w = c.hello(MUX_MAJOR);
        assert!(
            matches!(
                w,
                Some(ServerMsg::Welcome {
                    major: MUX_MAJOR,
                    ..
                })
            ),
            "{w:?}"
        );
        c.send(&ViewMsg::Attach);
        c
    }

    /// One message, or `None` after a short read timeout.
    fn next_msg(&mut self) -> Option<ServerMsg> {
        loop {
            if let Some(r) = self.dec.next_record() {
                let rec = r.expect("well-formed record").to_vec();
                match ServerMsg::decode(&rec).expect("decodes") {
                    Decoded::Msg(m) => return Some(m),
                    Decoded::Skipped(_) => continue,
                }
            }
            let mut b = [0u8; 65536];
            match self.s.read(&mut b) {
                Ok(0) => {
                    self.eof = true;
                    return None;
                }
                Ok(n) => self.dec.feed(&b[..n]),
                Err(_) => return None,
            }
        }
    }

    fn apply(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::Welcome { .. } => {}
            ServerMsg::Refused { reason } => self.refused = Some(reason),
            ServerMsg::Panes(l) => self.lists.push(l),
            ServerMsg::SnapBegin {
                pane, seq, state, ..
            } => {
                self.panes.entry(pane).or_default().building =
                    Some((seq, state, [Vec::new(), Vec::new()]));
            }
            ServerMsg::SnapPage {
                pane,
                screen,
                bytes,
            } => {
                let p = self.panes.get_mut(&pane).expect("page of a known pane");
                p.building.as_mut().expect("page inside a snapshot").2[usize::from(screen)]
                    .push(bytes);
            }
            ServerMsg::SnapEnd { pane } => {
                let p = self.panes.get_mut(&pane).unwrap();
                let (seq, state, pages) = p.building.take().expect("end of a snapshot");
                let t = Term::restore(
                    TermConfig::default(),
                    TermImage { state, pages },
                    Instant::now(),
                )
                .expect("snapshot restores");
                p.term = Some(t);
                p.next = seq;
                p.snapshots += 1;
            }
            ServerMsg::Tee { pane, seq, bytes } => {
                let p = self.panes.get_mut(&pane).expect("tee of a known pane");
                assert_eq!(seq, p.next, "tee sequence gap on pane {pane}");
                p.next += 1;
                let t = p.term.as_mut().expect("tee after the snapshot");
                t.feed(&bytes, Instant::now());
                while t.replies().pop().is_some() {}
                t.events().clear();
            }
            ServerMsg::Resized {
                pane,
                seq,
                rows,
                cols,
            } => {
                let p = self.panes.get_mut(&pane).unwrap();
                assert_eq!(seq, p.next, "resize sequence gap on pane {pane}");
                p.next += 1;
                p.term
                    .as_mut()
                    .unwrap()
                    .resize(usize::from(rows), usize::from(cols));
            }
            ServerMsg::Spawned { token, pane } => self.spawned.push((token, pane)),
            ServerMsg::Exited { pane, status } => {
                self.panes.entry(pane).or_default().exited = Some(status);
            }
        }
    }

    /// Pump until `pred` holds (or panic after `secs`).
    fn until(&mut self, secs: u64, what: &str, mut pred: impl FnMut(&Client) -> bool) {
        let t0 = Instant::now();
        while !pred(self) {
            assert!(
                t0.elapsed() < Duration::from_secs(secs),
                "timed out waiting for {what}; screen:\n{}",
                self.screen(1)
            );
            if let Some(m) = self.next_msg() {
                self.apply(m);
            } else if self.eof {
                assert!(pred(self), "stream ended waiting for {what}");
                return;
            }
        }
    }

    fn screen(&self, pane: u64) -> String {
        let Some(t) = self.panes.get(&pane).and_then(|p| p.term.as_ref()) else {
            return String::new();
        };
        (0..t.rows())
            .map(|y| t.row_text(y).trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn shows(&self, pane: u64, text: &str) -> bool {
        self.screen(pane).contains(text)
    }

    fn type_line(&mut self, pane: u64, line: &str) {
        self.send(&ViewMsg::Input {
            pane,
            bytes: format!("{line}\r").into_bytes(),
        });
    }
}

#[test]
fn panes_survive_detach_and_a_killed_view() {
    let srv = start("life", true, &[]);
    let mut a = Client::attach(&srv.sock);
    a.until(10, "the first snapshot", |c| {
        c.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    assert_eq!(a.lists.len(), 1);
    a.type_line(1, "echo hi-$((6*7))");
    a.until(10, "the echo", |c| c.shows(1, "hi-42"));
    // Output keeps coming while nobody is attached.
    a.type_line(1, "(sleep 1; echo later-$((2*3))) &");
    a.send(&ViewMsg::Detach);
    drop(a);

    std::thread::sleep(Duration::from_millis(1500));
    let mut b = Client::attach(&srv.sock);
    b.until(10, "the reattached screen", |c| {
        c.shows(1, "hi-42") && c.shows(1, "later-6")
    });
    // The same shell still runs.
    b.type_line(1, "echo still-$((1+1))");
    b.until(10, "the live shell", |c| c.shows(1, "still-2"));
    // A view that dies without detaching changes nothing.
    drop(b);
    let mut c = Client::attach(&srv.sock);
    c.until(10, "the screen after a killed view", |c| {
        c.shows(1, "still-2")
    });
    c.type_line(1, "exit");
    c.until(10, "the exit", |c| {
        c.panes.get(&1).is_some_and(|p| p.exited.is_some())
    });
    assert_eq!(c.panes[&1].exited, Some(Some(0)));
    // The last pane gone, the server leaves and takes its socket.
    let mut srv = srv;
    let t0 = Instant::now();
    let st = loop {
        if let Some(st) = srv.child.try_wait().unwrap() {
            break st;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "server kept running"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(st.success());
    assert!(!srv.sock.exists());
}

#[test]
fn late_views_hold_the_same_state_as_early_ones() {
    let srv = start("tee", true, &[]);
    let mut a = Client::attach(&srv.sock);
    a.until(10, "the first snapshot", |c| {
        c.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    // Styles, colours, a title, wide text, scrolling, cursor modes, the alt screen.
    a.type_line(
        1,
        r"printf '\033[1;31mred\033[0m \033]0;t\007\346\274\242\345\255\227 \033[?1h\033[4 q\n'; seq 1 60; echo mark-$((10+1))",
    );
    a.until(10, "the first burst", |c| c.shows(1, "mark-11"));
    let mut b = Client::attach(&srv.sock);
    b.until(10, "the late snapshot", |c| c.shows(1, "mark-11"));
    a.type_line(
        1,
        r"printf '\033[?1049h\033[2;5Halt-screen\n'; echo mark-$((10+2))",
    );
    a.until(10, "the second burst", |c| c.shows(1, "mark-12\nsh-"));
    b.until(10, "the second burst on b", |c| c.shows(1, "mark-12\nsh-"));
    let now = Instant::now();
    let ia = a.panes[&1].term.as_ref().unwrap().snapshot(now).unwrap();
    let ib = b.panes[&1].term.as_ref().unwrap().snapshot(now).unwrap();
    // Pages are rebuilt on restore (their hash indexes are local), so the grid is compared
    // by what it shows; the machine state byte for byte.
    assert_eq!(a.screen(1), b.screen(1), "views disagree on the grid");
    assert_eq!(
        ia.pages.iter().map(Vec::len).collect::<Vec<_>>(),
        ib.pages.iter().map(Vec::len).collect::<Vec<_>>()
    );
    assert_eq!(ia.state, ib.state, "views disagree on the terminal state");
    // And a third, fresh view agrees with both.
    let mut c = Client::attach(&srv.sock);
    c.until(10, "a third snapshot", |c| c.shows(1, "mark-12\nsh-"));
    let ic = c.panes[&1].term.as_ref().unwrap().snapshot(now).unwrap();
    assert_eq!(a.screen(1), c.screen(1));
    assert_eq!(ia.state, ic.state);
}

#[test]
fn a_slow_view_is_resynced_from_a_fresh_snapshot() {
    let srv = start("slow", true, &[]);
    let mut slow = Client::attach(&srv.sock);
    slow.until(10, "the first snapshot", |c| {
        c.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    let mut fast = Client::attach(&srv.sock);
    fast.until(10, "the first snapshot", |c| {
        c.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    // ~40 MiB of output the slow view does not read.
    fast.type_line(
        1,
        "head -c 40000000 /dev/zero | tr '\\0' x; echo; echo flood-$((5*5))",
    );
    fast.until(120, "the flood on the fast view", |c| {
        c.shows(1, "flood-25\nsh-")
    });
    slow.until(120, "the flood on the slow view", |c| {
        c.shows(1, "flood-25\nsh-")
    });
    assert!(
        slow.panes[&1].snapshots >= 2,
        "the slow view was not resynced ({} snapshots)",
        slow.panes[&1].snapshots
    );
    assert_eq!(fast.screen(1), slow.screen(1));
}

#[test]
fn version_skew_and_strangers_are_refused() {
    let srv = start("auth", false, &[]);
    // Socket and directory modes (T-12 §1).
    let md = std::fs::metadata(&srv.sock).unwrap();
    assert_eq!(md.mode() & 0o777, 0o600);
    assert_eq!(md.uid(), rustix::process::getuid().as_raw());
    let dmd = std::fs::metadata(srv.sock.parent().unwrap()).unwrap();
    assert_eq!(dmd.mode() & 0o777, 0o700);

    // Another major version is refused with a message (T-21 §4).
    let mut v = Client::connect(&srv.sock);
    match v.hello(MUX_MAJOR + 1) {
        Some(ServerMsg::Refused { reason }) => {
            assert!(reason.contains("not compatible"), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // A program that is not the interface may list but not attach.
    let mut s = Client::attach(&srv.sock);
    s.until(5, "the refusal", |c| c.refused.is_some());
    assert!(s.panes.is_empty(), "a refused view got content");
    assert!(s.refused.as_deref().unwrap().contains("consent"));
    let mut l = Client::connect(&srv.sock);
    assert!(matches!(
        l.hello(MUX_MAJOR),
        Some(ServerMsg::Welcome { .. })
    ));
    l.send(&ViewMsg::List);
    l.until(5, "the pane list", |c| !c.lists.is_empty());
    assert_eq!(l.lists[0].len(), 1);
    assert!(l.lists[0][0].alive);
    // Input from a lister goes nowhere (no pane content to check: it has none).
    l.send(&ViewMsg::Input {
        pane: 1,
        bytes: b"exit\r".to_vec(),
    });

    // A second server for the same session refuses to start.
    let out = server_cmd(&srv.dir, false)
        .args(["--server", "--session", "t"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("already running"));
    let mut srv = srv;
    assert!(
        srv.child.try_wait().unwrap().is_none(),
        "lister input reached the shell"
    );
}

#[test]
fn spawn_resize_close_and_detach_all() {
    let srv = start("ops", true, &[]);
    let mut a = Client::attach(&srv.sock);
    a.until(10, "the first snapshot", |c| {
        c.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    // Resizes are records in the stream: the view and the PTY agree.
    a.send(&ViewMsg::Resize {
        pane: 1,
        rows: 30,
        cols: 100,
        cell_w: 9,
        cell_h: 18,
    });
    a.type_line(1, "stty size");
    a.until(10, "the new size", |c| c.shows(1, "30 100"));
    let t = a.panes[&1].term.as_ref().unwrap();
    assert_eq!((t.rows(), t.cols()), (30, 100));

    a.send(&ViewMsg::Spawn {
        token: 7,
        argv: vec!["/bin/sh".into()],
        cwd: Some("/".into()),
        rows: 10,
        cols: 40,
    });
    a.until(10, "the spawned pane", |c| {
        c.spawned == [(7, Some(2))] && c.panes.get(&2).is_some_and(|p| p.term.is_some())
    });
    a.type_line(2, "pwd; echo two-$((1+1))");
    a.until(10, "the second pane", |c| c.shows(2, "two-2"));
    let t = a.panes[&2].term.as_ref().unwrap();
    assert_eq!((t.rows(), t.cols()), (10, 40));

    // Close hangs the pane up.
    a.send(&ViewMsg::Close { pane: 2 });
    a.until(10, "the closed pane", |c| {
        c.panes.get(&2).is_some_and(|p| p.exited.is_some())
    });

    // `apoth server detach`: every view is told and dropped; the panes stay.
    let mut d = Client::connect(&srv.sock);
    assert!(matches!(
        d.hello(MUX_MAJOR),
        Some(ServerMsg::Welcome { .. })
    ));
    d.send(&ViewMsg::DetachAll);
    a.until(10, "the detach", |c| c.refused.is_some());
    assert_eq!(a.refused.as_deref(), Some("detached"));
    let mut b = Client::attach(&srv.sock);
    b.until(10, "the pane after detach-all", |c| c.shows(1, "30 100"));
    assert!(!b.panes.contains_key(&2));
}

// ---- the interface as a view, on a hidden headless compositor ---------------------------

fn find_abyss() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("APOTH_EC_ABYSS") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("ec-abyss"))
            .find(|p| p.is_file())
    })
}

/// Kills a child by the pid captured at spawn.
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn gui(dir: &Path, dump: &Path) -> Guard {
    Guard(
        Command::new(env!("CARGO_BIN_EXE_apotheosis"))
            .args(["--attach", "--session", "t"])
            .env_remove("APOTHEOSIS_SOCKET")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/apoth-test-bus",
            )
            .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
            .env("XDG_RUNTIME_DIR", dir)
            .env("WAYLAND_DISPLAY", "wayland-1")
            .env("SHELL", "/bin/sh")
            .env("HOME", dir.join("home"))
            .env("XDG_CACHE_HOME", dir.join("home/.cache"))
            .env("XDG_CONFIG_HOME", dir.join("home/.config"))
            .env("APOTH_TEST_DUMP", dump)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn apotheosis --attach"),
    )
}

fn rpc(sock: &Path, method: &str, params: &str) {
    let Ok(mut s) = UnixStream::connect(sock) else {
        return;
    };
    let line =
        format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{method}\",\"params\":{params}}}\n");
    let _ = s.write_all(line.as_bytes());
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = s.read(&mut [0u8; 512]);
}

/// Wait until the GUI's dump shows `text`; `None` if the GUI found no GPU (skip).
fn dump_shows(g: &mut Guard, dump: &Path, text: &str) -> Option<()> {
    let t0 = Instant::now();
    loop {
        if std::fs::read_to_string(dump).is_ok_and(|t| t.contains(text)) {
            return Some(());
        }
        if let Ok(Some(st)) = g.0.try_wait() {
            if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                eprintln!("skipping: no usable GPU");
                return None;
            }
            panic!("apotheosis --attach exited: {st}");
        }
        assert!(
            t0.elapsed() < Duration::from_secs(40),
            "the window never showed {text:?}; dump:\n{}",
            std::fs::read_to_string(dump).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_window_is_a_view_that_can_crash_and_come_back() {
    let Some(abyss) = find_abyss() else {
        assert!(
            std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
            "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
        );
        eprintln!("skipping: ec-abyss not found");
        return;
    };
    let srv = start("gui", true, &[]);
    let dir = srv.dir.clone();
    std::fs::create_dir_all(dir.join("sock")).unwrap();
    std::fs::create_dir_all(dir.join("cfg/eclipse")).unwrap();
    std::fs::write(
        dir.join("cfg/eclipse/policy.kdl"),
        "misc { scripted-input #true }\n",
    )
    .unwrap();
    // Output made before any window exists.
    let mut a = Client::attach(&srv.sock);
    a.until(10, "the first snapshot", |c| {
        c.panes.get(&1).is_some_and(|p| p.term.is_some())
    });
    a.type_line(1, "echo before-$((4*4))");
    a.until(10, "the echo", |c| c.shows(1, "before-16"));

    let _abyss = Guard(
        Command::new(abyss)
            .args(["--backend", "headless", "--size", "800x600"])
            .env("XDG_RUNTIME_DIR", &dir)
            .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
            .env("XDG_CONFIG_HOME", dir.join("cfg"))
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ec-abyss"),
    );
    let t0 = Instant::now();
    while !dir.join("wayland-1").exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "compositor never came up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let dump = dir.join("dump.txt");
    let mut g = gui(&dir, &dump);
    if dump_shows(&mut g, &dump, "before-16").is_none() {
        return;
    }
    // Keys typed in the window reach the server's shell; every view sees the result.
    rpc(
        &dir.join("sock/abyss.sock"),
        "type_text",
        "{\"text\":\"echo gui-$((7*7))\\n\"}",
    );
    dump_shows(&mut g, &dump, "gui-49").unwrap();
    a.until(10, "the window's input on another view", |c| {
        c.shows(1, "gui-49")
    });

    // The window crashes: the shell does not notice.
    drop(g);
    let _ = std::fs::remove_file(&dump);
    a.type_line(1, "echo after-$((8*8))");
    a.until(10, "the shell after the crash", |c| c.shows(1, "after-64"));
    let mut g = gui(&dir, &dump);
    dump_shows(&mut g, &dump, "after-64").unwrap();
    drop(g);
    drop(a);
}
