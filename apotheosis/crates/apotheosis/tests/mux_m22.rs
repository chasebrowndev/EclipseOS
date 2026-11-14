// SPDX-License-Identifier: AGPL-3.0-only
//! M21/M22 headless gates: tabs, splits, pane focus and close, and the command palette, driven
//! through the compositor's control socket on a hidden headless `ec-abyss`. State is read back
//! from the debug-build hooks: `APOTH_TEST_MUX` (tabs, panes, focus, palette) and the per-pane
//! screen dumps (`APOTH_TEST_DUMP`, `<path>.p<id>` for every pane but the first). Skips (or
//! fails under `APOTH_REQUIRE_SESSION` and `APOTH_REQUIRE_GPU`) without a compositor or GPU.
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
        let dir = std::env::temp_dir().join(format!("apm22{name}{}", std::process::id()));
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
                // The nested compositor's socket, never the host's (T-17 §1 detection).
                .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
                .env("XDG_RUNTIME_DIR", &dir)
                .env("WAYLAND_DISPLAY", "wayland-1")
                .env("SHELL", "/bin/sh")
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

    fn click(&mut self, x: f64, y: f64) {
        self.rpc("click_at", &format!("{{\"x\":{x},\"y\":{y}}}"));
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

/// `ctrl+shift+t` opens a tab with its own shell; the strip appears and takes rows; tab
/// switching and closing work from the default binds.
#[test]
fn new_tab_switch_and_close() {
    let Some(mut s) = Session::start("tabs") else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.tabs == 1 && m.panes.len() == 1);
    let first = m0.panes[0].id;
    s.chord("t", &["ctrl", "shift"]);
    let m1 = s.mux_until("a second tab", |m| m.tabs == 2 && m.active == 1);
    let second = m1.focus;
    assert_ne!(second, first);
    // The strip shows with two tabs: the new tab's grid is shorter than the first one was.
    let rows1 = s
        .mux_until("the new pane's size", |m| {
            m.panes.first().is_some_and(|p| p.rows > 0)
        })
        .panes[0]
        .rows;
    assert!(rows1 < m0.panes[0].rows, "{rows1} vs {}", m0.panes[0].rows);
    s.run_in(second, first, "echo TAB2$((1+1))", "TAB22");
    // Back to the first tab, then forward by number.
    s.chord("Prior", &["ctrl"]);
    s.mux_until("tab 1 shown", |m| m.active == 0 && m.focus == first);
    s.chord("2", &["alt"]);
    s.mux_until("tab 2 shown", |m| m.active == 1 && m.focus == second);
    s.chord("w", &["ctrl", "shift"]);
    let m = s.mux_until("the tab to close", |m| m.tabs == 1 && m.focus == first);
    assert_eq!(m.panes.len(), 1);
    // Typing still reaches the first shell.
    s.run_in(first, first, "echo BACK$((2+2))", "BACK4");
    s.exit_shell();
}

/// Split right and down, move focus, type into each pane, click a pane to focus it, and close
/// a pane by exiting its shell: the survivor gets the room back.
#[test]
fn split_focus_type_and_close() {
    let Some(mut s) = Session::start("split") else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.panes.len() == 1);
    let first = m0.panes[0];
    s.chord("d", &["ctrl", "shift"]);
    let m1 = s.mux_until("a right split", |m| m.panes.len() == 2);
    let right = m1.focus;
    assert_ne!(right, first.id);
    let (l, r) = {
        let m = s.mux_until("both panes resized", |m| {
            m.panes.len() == 2 && m.panes.iter().all(|p| p.cols > 0 && p.cols < first.cols)
        });
        let l = *m.panes.iter().find(|p| p.id == first.id).unwrap();
        let r = *m.panes.iter().find(|p| p.id == right).unwrap();
        (l, r)
    };
    assert!(r.x > l.x, "the new pane is on the right");
    assert!(l.cols + r.cols < first.cols + 1);
    s.run_in(right, first.id, "echo RIGHT$((1+1))", "RIGHT2");
    // The left pane did not get it.
    let left_text = std::fs::read_to_string(s.pane_dump(first.id, first.id)).unwrap_or_default();
    assert!(!left_text.lines().any(|t| t.trim() == "RIGHT2"));
    s.chord("Left", &["ctrl", "alt"]);
    s.mux_until("focus back on the left", |m| m.focus == first.id);
    s.run_in(first.id, first.id, "echo LEFT$((1+1))", "LEFT2");
    // A click on the right half focuses the right pane (the window fills the output).
    s.click(600.0, 300.0);
    s.mux_until("a click to focus the right pane", |m| m.focus == right);
    // Split the right pane down, then close the new one with its shell's exit.
    s.chord("e", &["ctrl", "shift"]);
    let m3 = s.mux_until("a down split", |m| m.panes.len() == 3);
    let below = m3.focus;
    s.type_text("exit\n");
    s.mux_until("the exited pane to go", |m| {
        m.panes.len() == 2 && !m.panes.iter().any(|p| p.id == below)
    });
    // Close the right pane by bind; the left one gets the whole width again.
    s.chord("Right", &["ctrl", "alt"]);
    s.mux_until("focus on the right", |m| m.focus == right);
    s.chord("w", &["ctrl", "shift"]);
    s.mux_until("the left pane to fill the window", |m| {
        m.panes.len() == 1 && m.panes[0].cols == first.cols
    });
    s.exit_shell();
}

/// The palette opens on its bind, filters as it is typed into, runs the picked action on
/// Return, and Escape dismisses it without typing anything into the shell.
#[test]
fn palette_runs_an_action() {
    let Some(mut s) = Session::start("palette") else {
        return;
    };
    let m0 = s.mux_until("the first pane", |m| m.panes.len() == 1);
    let first = m0.panes[0].id;
    s.chord("p", &["ctrl", "shift"]);
    s.mux_until("the palette to open", |m| m.palette);
    s.type_text("new tab");
    let path = s.mux.clone();
    s.wait_file(&path, "the query to filter", |t| {
        t.contains("query new tab")
    });
    s.chord("Return", &[]);
    s.mux_until("the palette to run tab.new", |m| m.tabs == 2 && !m.palette);
    // Escape closes it; nothing typed while it was open reached the shell.
    s.chord("p", &["ctrl", "shift"]);
    s.mux_until("the palette to open again", |m| m.palette);
    s.type_text("zzqq");
    s.chord("Escape", &[]);
    let m = s.mux_until("the palette to close", |m| !m.palette);
    let shown = m.focus;
    s.run_in(shown, first, "echo PAL$((3+3))", "PAL6");
    let text = std::fs::read_to_string(s.pane_dump(shown, first)).unwrap_or_default();
    assert!(
        !text.contains("zzqq"),
        "palette input leaked to the shell:\n{text}"
    );
    // Close the extra tab, then leave.
    s.chord("w", &["ctrl", "shift"]);
    s.mux_until("one tab", |m| m.tabs == 1);
    s.exit_shell();
}
