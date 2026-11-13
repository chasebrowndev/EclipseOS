// SPDX-License-Identifier: AGPL-3.0-only
//! M16 headless gates: selection and clipboard round trip through the compositor, bracketed
//! paste, and wheel scrolling of the scrollback. Same harness as `first_light`: a hidden
//! headless `ec-abyss`, the real binary, input injected through the control socket, output read
//! back from the debug-build dump hooks (`APOTH_TEST_DUMP` for the live screen,
//! `APOTH_TEST_VIEW` for what the frame draws). Skips (or fails under `APOTH_REQUIRE_SESSION`
//! and `APOTH_REQUIRE_GPU`) when there is no compositor or GPU.
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
    view: PathBuf,
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
        let dir = std::env::temp_dir().join(format!("apm16{name}{}", std::process::id()));
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
        let view = dir.join("view.txt");
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
                .env("HOME", dir.join("home"))
                .env("XDG_CACHE_HOME", dir.join("home/.cache"))
                .env("XDG_CONFIG_HOME", dir.join("home/.config"))
                .env("APOTH_TEST_DUMP", &dump)
                .env("APOTH_TEST_VIEW", &view)
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
            view,
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

    fn scroll(&mut self, dy: f64) {
        self.rpc(
            "scroll_at",
            &format!("{{\"x\":400,\"y\":300,\"dx\":0,\"dy\":{dy}}}"),
        );
    }

    /// Poll `file` until `pred` holds; the last content on failure.
    fn wait(&mut self, view: bool, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let path: &Path = if view { &self.view } else { &self.dump };
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
        panic!(
            "timed out waiting for {what}; last content has {} lines",
            last.lines().count()
        );
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

fn offset_of(view: &str) -> usize {
    view.lines()
        .next()
        .and_then(|l| l.strip_prefix("offset="))
        .and_then(|n| n.parse().ok())
        .unwrap_or(usize::MAX)
}

/// Type a command line once and wait until its output shows: a screen line that is exactly
/// `marker` (the echoed command line never is).
fn run(s: &mut Session, cmd: &str, marker: &str) {
    s.type_text(&format!("{cmd}\n"));
    s.wait(false, "the command's output", |t| {
        t.lines().any(|l| l.trim() == marker)
    });
}

/// Select everything, copy it to the clipboard (a seat selection owned through the compositor),
/// read it back through the compositor and paste it with bracketed paste on.
#[test]
fn select_copy_paste_round_trip_with_bracketed_paste() {
    let Some(mut s) = Session::start("copy") else {
        return;
    };
    // Bracketed paste on, a known single word on the screen, then `cat -v` to show raw bytes.
    run(
        &mut s,
        "printf '\\033[?2004h\\033[2J\\033[H%s\\n' COPYME; exec cat -v",
        "COPYME",
    );
    s.chord("a", &["ctrl", "shift"]);
    std::thread::sleep(Duration::from_millis(500));
    s.chord("c", &["ctrl", "shift"]);
    std::thread::sleep(Duration::from_millis(500));
    s.chord("v", &["ctrl", "shift"]);
    // The paste arrives wrapped in ESC [ 200 ~ .. ESC [ 201 ~; the tty echoes ESC as ^[.
    let t = s.wait(false, "the bracketed paste to be echoed", |t| {
        t.lines()
            .any(|l| l.contains("^[[200~") && l.contains("COPYME"))
    });
    assert!(t.contains("^[[201~"), "no closing bracket in:\n{t}");
    s.exit_shell();
}

/// The wheel scrolls the scrollback (not the shell), a key snaps back to the bottom.
#[test]
fn wheel_scrolls_scrollback_and_typing_returns_to_the_bottom() {
    let Some(mut s) = Session::start("wheel") else {
        return;
    };
    run(&mut s, "seq 1 400", "400");
    let at_bottom = s.wait(true, "the live view", |v| {
        v.lines().any(|l| l.trim() == "400")
    });
    assert_eq!(offset_of(&at_bottom), 0);
    s.scroll(-3.0);
    let up = s.wait(true, "the view to scroll back", |v| {
        offset_of(v) > 0 && offset_of(v) != usize::MAX
    });
    assert!(
        !up.lines().any(|l| l.trim() == "400"),
        "the last line is still on screen after scrolling back:\n{up}"
    );
    // The live screen (the shell's own state) did not move.
    let live = std::fs::read_to_string(&s.dump).unwrap_or_default();
    assert!(live.lines().any(|l| l.trim() == "400"));
    s.scroll(5.0);
    let down = s.wait(true, "the view to return", |v| offset_of(v) == 0);
    assert!(down.lines().any(|l| l.trim() == "400"));
    // Scroll back again, then type: the view returns to the bottom.
    s.scroll(-3.0);
    s.wait(true, "scrolled back again", |v| offset_of(v) > 0);
    s.type_text("x");
    s.wait(true, "typing to snap back", |v| offset_of(v) == 0);
    s.exit_shell();
}
