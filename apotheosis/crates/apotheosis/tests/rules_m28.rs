// SPDX-License-Identifier: AGPL-3.0-only
//! M28 headless gates: rules, user variables and profiles end to end (T-11 §5, §6, §9).
//! A program sets a user variable with OSC 1337; a rule on `user-var-changed` opens a tab
//! with a profile whose environment the new shell sees; an opted-in rule waits for its
//! confirm-once bar, which ignores Return. Skips (or fails under `APOTH_REQUIRE_SESSION`
//! and `APOTH_REQUIRE_GPU`) without a compositor or GPU.
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
    fn start(name: &str, config: &str) -> Option<Session> {
        let Some(abyss) = find_abyss() else {
            assert!(
                std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
                "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
            );
            eprintln!("skipping {name}: ec-abyss not found");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("apm28{name}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sock")).unwrap();
        std::fs::create_dir_all(dir.join("cfg/eclipse")).unwrap();
        std::fs::create_dir_all(dir.join("home/.config/eclipse")).unwrap();
        std::fs::write(dir.join("home/.config/eclipse/apotheosis.kdl"), config).unwrap();
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
                // The nested compositor's socket, never the host's (T-17 §1 detection).
                .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
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

const CONFIG: &str = r#"version 1
profile "m28" {
    env { APOTH_M28 "from-profile"; }
}
rule "open" {
    on "user-var-changed"
    where "name:go and value:yes"
    do { tab.new profile="m28"; }
}
rule "spawn" {
    on "user-var-changed"
    where "name:spawn"
    allow-spawn #true
    do { tab.new { argv "/bin/sh"; }; }
}
"#;

/// OSC 1337 SetUserVar raises `user-var-changed`; the rule's `tab.new profile=` starts a
/// shell with the profile's environment.
#[test]
fn user_var_rule_opens_a_profiled_tab() {
    let Some(mut s) = Session::start("uservar", CONFIG) else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.tabs == 1 && m.panes.len() == 1);
    let first = m0.panes[0].id;
    s.run_in(first, first, "echo UP$((1+1))", "UP2");
    // A different value does not match the rule's `where`.
    s.run_in(
        first,
        first,
        "printf '\\033]1337;SetUserVar=go=bm8=\\007'; echo SET$((1+1))",
        "SET2",
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(parse(&std::fs::read_to_string(&s.mux).unwrap()).tabs, 1);
    // `yes` matches: a second tab opens with the profile's environment.
    s.type_text("printf '\\033]1337;SetUserVar=go=eWVz\\007'\n");
    let m = s.mux_until("the rule's tab", |m| m.tabs == 2);
    let pane = m.focus;
    assert_ne!(pane, first);
    s.run_in(pane, first, "echo \"$APOTH_M28\"-x", "from-profile-x");
    s.exit_shell();
}

/// An opted-in rule asks first: Return does nothing, an explicit choice runs it.
#[test]
fn opted_in_rule_waits_for_confirm_once() {
    let Some(mut s) = Session::start("confirm", CONFIG) else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.tabs == 1 && m.panes.len() == 1);
    let first = m0.panes[0].id;
    s.run_in(first, first, "echo UP$((1+1))", "UP2");
    s.type_text("printf '\\033]1337;SetUserVar=spawn=MQ==\\007'\n");
    // The bar is up: Return (even after the guard) answers nothing.
    std::thread::sleep(Duration::from_millis(900));
    s.chord("Return", &[]);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(parse(&std::fs::read_to_string(&s.mux).unwrap()).tabs, 1);
    // "Allow this time".
    s.chord("o", &[]);
    let m = s.mux_until("the confirmed tab", |m| m.tabs == 2);
    assert_ne!(m.focus, first);
    s.exit_shell();
}
