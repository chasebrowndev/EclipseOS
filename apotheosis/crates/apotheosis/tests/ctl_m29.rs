// SPDX-License-Identifier: AGPL-3.0-only
//! M29 headless gates: the terminal's control socket (T-12) and the `apoth` CLI, on a hidden
//! headless `ec-abyss`. The test process is a client outside every pane (observe and layout
//! only); `apoth` run inside a pane gets that pane's rights. State is read back from the
//! debug-build hooks (`APOTH_TEST_MUX`, `APOTH_TEST_DUMP`). Skips (or fails under
//! `APOTH_REQUIRE_SESSION` and `APOTH_REQUIRE_GPU`) without a compositor or GPU.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running compositor and terminal. Dropped in order: terminal first, then compositor.
struct Session {
    term: Guard,
    _abyss: Guard,
    dir: PathBuf,
    sock: PathBuf,
    dump: PathBuf,
    mux: PathBuf,
    id: u32,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl Session {
    /// `None` when the environment cannot run the test (reason printed).
    fn start(name: &str) -> Option<Session> {
        let Some(abyss) = find_abyss() else {
            assert!(
                std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
                "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
            );
            eprintln!("skipping {name}: ec-abyss not found");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("apm29{name}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sock")).unwrap();
        std::fs::create_dir_all(dir.join("cfg/eclipse")).unwrap();
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            dir.join("cfg/eclipse/policy.kdl"),
            "misc { scripted-input #true }\n",
        )
        .unwrap();
        let abyss = Guard(
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
        let wl = dir.join("wayland-1");
        let t0 = Instant::now();
        while !wl.exists() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "compositor never came up"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let dump = dir.join("dump.txt");
        let mux = dir.join("mux.txt");
        let term = Guard(
            Command::new(env!("CARGO_BIN_EXE_apotheosis"))
                .env_remove("APOTHEOSIS_SOCKET")
                .env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    "unix:path=/nonexistent/apoth-test-bus",
                )
                .env("XDG_RUNTIME_DIR", &dir)
                .env("WAYLAND_DISPLAY", "wayland-1")
                .env("SHELL", "/bin/sh")
                .env("PATH", bin_path())
                .env("HOME", dir.join("home"))
                .env("XDG_CACHE_HOME", dir.join("home/.cache"))
                .env("XDG_CONFIG_HOME", dir.join("home/.config"))
                .env("APOTH_TEST_DUMP", &dump)
                .env("APOTH_TEST_MUX", &mux)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn apotheosis"),
        );
        let mut s = Session {
            term,
            _abyss: abyss,
            sock: dir.join("sock/abyss.sock"),
            dir,
            dump,
            mux,
            id: 0,
        };
        // Wait for the first screen, or learn that there is no GPU.
        let t0 = Instant::now();
        while !s.dump.exists() {
            if let Ok(Some(st)) = s.term.0.try_wait() {
                if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                    eprintln!("skipping {name}: no usable GPU");
                    return None;
                }
                panic!("apotheosis exited early: {st}");
            }
            assert!(
                t0.elapsed() < Duration::from_secs(40),
                "the terminal never drew"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        Some(s)
    }

    fn rpc(&mut self, method: &str, params: &str) {
        self.id += 1;
        let Ok(mut s) = UnixStream::connect(&self.sock) else {
            return;
        };
        let line = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"{method}\",\"params\":{params}}}\n",
            self.id
        );
        let _ = s.write_all(line.as_bytes());
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = s.read(&mut [0u8; 512]);
    }

    fn type_text(&mut self, text: &str) {
        self.rpc("type_text", &format!("{{\"text\":{}}}", json_str(text)));
    }

    fn chord(&mut self, key: &str, mods: &[&str]) {
        let m: Vec<String> = mods.iter().map(|m| json_str(m)).collect();
        self.rpc(
            "press_key",
            &format!(
                "{{\"key\":{},\"modifiers\":[{}]}}",
                json_str(key),
                m.join(",")
            ),
        );
    }

    /// The live screen of pane `id` (the first pane's dump has no suffix).
    fn pane_dump(&self, id: u64, first: u64) -> PathBuf {
        if id == first {
            self.dump.clone()
        } else {
            let mut p = self.dump.clone().into_os_string();
            p.push(format!(".p{id}"));
            PathBuf::from(p)
        }
    }

    /// Poll `path` until `pred` holds; the last content on failure.
    fn wait_file(&mut self, path: &Path, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let t0 = Instant::now();
        let mut last = String::new();
        while t0.elapsed() < Duration::from_secs(25) {
            if let Ok(Some(st)) = self.term.0.try_wait() {
                panic!("apotheosis exited: {st}");
            }
            if let Ok(t) = std::fs::read_to_string(path) {
                if pred(&t) {
                    return t;
                }
                last = t;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        panic!("timed out waiting for {what}; last content:\n{last}");
    }

    fn exit_shell(&mut self) {
        let t1 = Instant::now();
        loop {
            self.chord("d", &["ctrl"]);
            self.type_text("exit\n");
            if let Ok(Some(_)) = self.term.0.try_wait() {
                break;
            }
            if t1.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

/// One placed pane from the mux dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaneLine {
    id: u64,
    rows: u32,
    cols: u32,
    x: i32,
}

struct Mux {
    tabs: usize,
    active: usize,
    panes: Vec<PaneLine>,
    focus: u64,
    palette: bool,
}

fn parse(t: &str) -> Mux {
    let mut m = Mux {
        tabs: 0,
        active: 0,
        panes: Vec::new(),
        focus: 0,
        palette: false,
    };
    for l in t.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["tabs", n, "active", a] => {
                m.tabs = n.parse().unwrap_or(0);
                m.active = a.parse().unwrap_or(0);
            }
            ["pane", id, "rows", r, "cols", c, "at", xy] => m.panes.push(PaneLine {
                id: id.parse().unwrap_or(0),
                rows: r.parse().unwrap_or(0),
                cols: c.parse().unwrap_or(0),
                x: xy
                    .split(',')
                    .next()
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0),
            }),
            ["focus", f] => m.focus = f.parse().unwrap_or(0),
            ["palette", "open", ..] => m.palette = true,
            _ => {}
        }
    }
    m
}

impl Session {
    fn mux_until(&mut self, what: &str, pred: impl Fn(&Mux) -> bool) -> Mux {
        let path = self.mux.clone();
        parse(&self.wait_file(&path, what, |t| pred(&parse(t))))
    }

    /// Type a command into the focused pane and wait for a line that is exactly `marker` on
    /// pane `id`'s screen (the echoed command line never is).
    fn run_in(&mut self, id: u64, first: u64, cmd: &str, marker: &str) {
        self.type_text(&format!("{cmd}\n"));
        let path = self.pane_dump(id, first);
        self.wait_file(&path, "the command's output", |t| {
            t.lines().any(|l| l.trim() == marker)
        });
    }
}

/// The build's binary directory (`apoth` and `apotheosis`) first on the panes' PATH.
fn bin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_apotheosis"))
        .parent()
        .unwrap()
        .to_owned()
}

fn bin_path() -> std::ffi::OsString {
    let mut v = vec![bin_dir()];
    if let Some(p) = std::env::var_os("PATH") {
        v.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(v).unwrap()
}

/// `apoth` is built by `cargo build/test --workspace`; a lone `-p apotheosis` run lacks it.
fn have_apoth(name: &str) -> bool {
    if bin_dir().join("apoth").is_file() {
        return true;
    }
    assert!(
        std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
        "{name}: the apoth binary is not built (run the workspace tests)"
    );
    eprintln!("skipping {name}: apoth is not built");
    false
}

/// A protocol client in the test process (outside every pane).
struct Client {
    s: UnixStream,
    buf: Vec<u8>,
    id: u64,
}

impl Client {
    fn new(path: &Path) -> Client {
        let s = UnixStream::connect(path).expect("connect to the control socket");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Client {
            s,
            buf: Vec::new(),
            id: 0,
        }
    }

    /// One request; the reply object (`ok` or `error`), events skipped.
    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let line = serde_json::json!({"id": self.id, "method": method, "params": params});
        self.s.write_all(format!("{line}\n").as_bytes()).unwrap();
        loop {
            while let Some(n) = self.buf.iter().position(|b| *b == b'\n') {
                let l: Vec<u8> = self.buf.drain(..=n).collect();
                let v: serde_json::Value = serde_json::from_slice(&l).expect("reply is JSON");
                if v.get("id") == Some(&serde_json::json!(self.id)) {
                    return v;
                }
            }
            let mut b = [0u8; 8192];
            let n = self.s.read(&mut b).expect("reply in time");
            assert!(n > 0, "the instance closed the connection");
            self.buf.extend_from_slice(&b[..n]);
        }
    }
}

fn code(v: &serde_json::Value) -> Option<&str> {
    v.pointer("/error/code").and_then(serde_json::Value::as_str)
}

impl Session {
    /// `$XDG_RUNTIME_DIR/apotheosis/<pid>.sock`, once it exists.
    fn ctl_socket(&mut self) -> PathBuf {
        let p = self
            .dir
            .join("apotheosis")
            .join(format!("{}.sock", self.term.0.id()));
        let t0 = Instant::now();
        while !p.exists() {
            assert!(t0.elapsed() < Duration::from_secs(10), "no control socket");
            std::thread::sleep(Duration::from_millis(100));
        }
        p
    }
}

/// The socket is private; an outside client may observe and arrange but not read, type or
/// spawn (those ask for consent, which is refused while no consent bar exists).
#[test]
fn outside_client_rights_and_socket_mode() {
    let Some(mut s) = Session::start("outside") else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.tabs == 1 && m.panes.len() == 1);
    let first = m0.panes[0].id;
    s.run_in(first, first, "echo UP$((1+1))", "UP2");
    let path = s.ctl_socket();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket mode {mode:o}");
    let dmode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dmode, 0o700, "socket directory mode {dmode:o}");

    let mut c = Client::new(&path);
    let h = c.call("hello", serde_json::json!({"proto": 1, "client": "test"}));
    assert_eq!(h.pointer("/ok/proto"), Some(&serde_json::json!(1)), "{h}");
    let t = c.call("tree", serde_json::json!({}));
    let pane = t.pointer("/ok/windows/0/tabs/0/panes/0/id").cloned();
    assert_eq!(pane, Some(serde_json::json!(first)), "{t}");
    let target = format!("id:{first}");
    // No target and not inside a pane: there is no `self`.
    let r = c.call("pane.text", serde_json::json!({}));
    assert_eq!(code(&r), Some("no-target"), "{r}");
    let r = c.call("pane.text", serde_json::json!({"target": target}));
    assert_eq!(code(&r), Some("denied"), "{r}");
    let r = c.call(
        "input.send-keys",
        serde_json::json!({"keys": ["a"], "target": target}),
    );
    assert_eq!(code(&r), Some("denied"), "{r}");
    let r = c.call("launch", serde_json::json!({"argv": ["true"]}));
    assert_eq!(code(&r), Some("denied"), "{r}");
    // Layout is granted: a split happens.
    let r = c.call(
        "action.run",
        serde_json::json!({"action": "pane.split right", "target": target}),
    );
    assert_eq!(r.get("ok"), Some(&serde_json::Value::Null), "{r}");
    s.mux_until("the split", |m| m.panes.len() == 2);
    drop(c);
    s.exit_shell();
}

/// `apoth` inside a pane: get-text reads its own pane, send-keys types without firing the
/// bound chord, launch (and `apotheosis` as a second launch) opens a tab and `--wait` relays
/// the exit status.
#[test]
fn in_pane_cli() {
    if !have_apoth("in_pane_cli") {
        return;
    }
    let Some(mut s) = Session::start("inpane") else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.tabs == 1 && m.panes.len() == 1);
    let first = m0.panes[0].id;
    s.run_in(first, first, "echo UP$((1+1))", "UP2");
    let _ = s.ctl_socket();
    let out = s.dir.join("text.txt");

    // get-text: the pane's own screen, without the escape sequences.
    s.run_in(
        first,
        first,
        &format!(
            "echo GT$((3+4)); apoth get-text > {}; echo GOT$?",
            out.display()
        ),
        "GOT0",
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.lines().any(|l| l.trim() == "GT7"), "{text}");

    // send-keys: ctrl+shift+t (new tab by default) reaches the program as a key; no tab opens.
    s.run_in(
        first,
        first,
        "apoth send-keys ctrl+shift+t ctrl+u e c h o space S K 9 Return",
        "SK9",
    );
    std::thread::sleep(Duration::from_millis(500));
    let m = s.mux_until("the mux state", |_| true);
    assert_eq!(m.tabs, 1, "a bound chord sent by send-keys fired its bind");

    // launch --wait relays the exit status; the tab opened in this instance.
    s.run_in(
        first,
        first,
        "apoth launch --wait -- sh -c 'exit 7'; echo RC$?",
        "RC7",
    );
    // A second `apotheosis` hands its launch to this instance and relays the status too.
    s.run_in(
        first,
        first,
        "apotheosis --wait -e sh -c 'exit 5'; echo GL$?",
        "GL5",
    );
    // A plain launch prints the new pane id and leaves its tab open.
    s.run_in(
        first,
        first,
        "apoth launch -- sh -c 'sleep 30'; echo LN$?",
        "LN0",
    );
    s.mux_until("the launched tab", |m| m.tabs == 2 && m.focus != first);
    s.chord("Prior", &["ctrl"]);
    s.mux_until("tab 1 shown", |m| m.active == 0 && m.focus == first);
    // Denied and usage paths map to their exit codes.
    s.run_in(
        first,
        first,
        "apoth get-text --extent page; echo US$?",
        "US2",
    );
    s.run_in(
        first,
        first,
        "apoth --socket /nonexistent ls; echo NI$?",
        "NI4",
    );
    s.exit_shell();
}
