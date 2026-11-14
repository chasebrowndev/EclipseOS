// SPDX-License-Identifier: AGPL-3.0-only
//! "Open with" headless gate (T-14 §11): an image path on screen, opened from the Open menu
//! (`open.menu` with nothing selected labels what can open; the label opens the menu; its
//! first row is the default) into a float running `apoth icat --hold`. Driven through the
//! compositor's control socket on a hidden headless `ec-abyss`; state from the debug-build
//! mux dump (`APOTH_TEST_MUX`). Skips (or fails under `APOTH_REQUIRE_SESSION` and
//! `APOTH_REQUIRE_GPU`) without a compositor, a GPU or the workspace-built `apoth`.
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
        let dir = std::env::temp_dir().join(format!("apm33{name}{}", std::process::id()));
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

fn bin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_apotheosis"))
        .parent()
        .unwrap()
        .to_owned()
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

fn floats(t: &str) -> Vec<u64> {
    t.lines()
        .filter(|l| l.starts_with("pane ") && l.ends_with(" float"))
        .filter_map(|l| l.split_whitespace().nth(1)?.parse().ok())
        .collect()
}

/// A 2x2 PNG (stored deflate): enough for the magic sniff and for `apoth icat` to decode.
fn png() -> Vec<u8> {
    fn crc(bytes: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in bytes {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend(data);
        out.extend(&c);
        out.extend(crc(&c).to_be_bytes());
    }
    let raw: Vec<u8> = [0u8, 255, 0, 0, 0, 255, 0, 0u8, 0, 0, 255, 255, 255, 255].to_vec();
    let mut z = vec![0x78, 0x01, 1];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(&raw);
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(2u32.to_be_bytes());
    ihdr.extend(2u32.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// An image path on screen: `ctrl+shift+m` labels it, its label opens the menu naming
/// `apoth icat in a float`, Return opens that float, and a key ends `icat --hold` (the float
/// closes; a shell would not).
#[test]
fn image_hint_opens_into_a_float() {
    if !have_apoth("open") {
        return;
    }
    let Some(mut s) = Session::start("open") else {
        return;
    };
    // The name says nothing: the type comes from the PNG magic.
    let img = s.dir.join("home/shot.bin");
    std::fs::write(&img, png()).unwrap();
    let m0 = s.mux_until("the first pane", |m| m.panes.len() == 1);
    let first = m0.panes[0].id;
    // Clear the screen first so the only path on it is the output line.
    let p = img.to_string_lossy().into_owned();
    s.run_in(first, first, &format!("clear; printf '%s\\n' {p}"), &p);
    s.chord("m", &["ctrl", "shift"]);
    // The newest match gets the alphabet's first letter (`hints.alphabet` default).
    std::thread::sleep(Duration::from_millis(300));
    s.type_text("a");
    let path = s.mux.clone();
    let t = s.wait_file(&path, "the Open menu", |t| {
        t.contains("palette open mode Open") && t.contains("palette first ")
    });
    assert!(
        t.contains("palette first Open with apoth icat in a float"),
        "{t}"
    );
    s.chord("Return", &[]);
    let t = s.wait_file(&path, "the icat float", |t| {
        t.contains("palette closed") && !floats(t).is_empty()
    });
    let float = floats(&t)[0];
    assert_ne!(float, first);
    // `--hold` keeps it up until a key.
    std::thread::sleep(Duration::from_millis(800));
    let still = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(floats(&still).contains(&float), "the float held: {still}");
    s.type_text("q");
    s.mux_until("the float to close", |m| {
        m.panes.len() == 1 && m.panes[0].id == first
    });
    s.exit_shell();
}
