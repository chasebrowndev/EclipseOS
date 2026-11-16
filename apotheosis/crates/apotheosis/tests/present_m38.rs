// SPDX-License-Identifier: AGPL-3.0-only
//! M38 presentation gates on a hidden headless `ec-abyss` (T-07 §2, §3; T-23 `IDLE`; T-16 §1
//! tier A). A static screen presents no frames at all; a kitty shm video stream in the shape
//! `mpv --vo=kitty --vo-kitty-use-shm=yes` sends (anonymous `a=T,t=s,f=24,C=1` frames at the
//! same cell, 30 per second, no deletes) keeps playing after the stream has sent more than
//! the pane's image quota. The stream is generated here, so no mpv is needed.
//! State is read from the debug-build hooks (`APOTH_TEST_DUMP`, `APOTH_TEST_FRAMES`,
//! `APOTH_TEST_IMAGES`). Skips (or fails under `APOTH_REQUIRE_SESSION` and
//! `APOTH_REQUIRE_GPU`) without a compositor or GPU.
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

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn read_u64(p: &Path) -> u64 {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0)
}

/// A running compositor and terminal with an interactive `/bin/sh`. Dropped in order:
/// terminal first, then compositor.
struct Session {
    term: Guard,
    _abyss: Guard,
    dir: PathBuf,
    sock: PathBuf,
    dump: PathBuf,
    frames: PathBuf,
    images: PathBuf,
    id: u32,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Session {
    /// `None` when the environment cannot run the test (reason printed). `config` is the
    /// user's `apotheosis.kdl`.
    fn start(name: &str, config: Option<&str>) -> Option<Session> {
        let Some(abyss) = find_abyss() else {
            assert!(
                std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
                "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
            );
            eprintln!("skipping {name}: ec-abyss not found");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("apm38{name}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["sock", "cfg/eclipse", "home/.config/apotheosis"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            dir.join("cfg/eclipse/policy.kdl"),
            "misc { scripted-input #true }\n",
        )
        .unwrap();
        if let Some(c) = config {
            std::fs::write(dir.join("home/.config/apotheosis/apotheosis.kdl"), c).unwrap();
        }
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
        let t0 = Instant::now();
        while !dir.join("wayland-1").exists() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "compositor never came up"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let dump = dir.join("dump.txt");
        let frames = dir.join("frames.txt");
        let images = dir.join("images.txt");
        let term = Guard(
            Command::new(env!("CARGO_BIN_EXE_apotheosis"))
                .env_remove("APOTHEOSIS_SOCKET")
                .env(
                    "DBUS_SESSION_BUS_ADDRESS",
                    "unix:path=/nonexistent/apoth-test-bus",
                )
                .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
                .env("XDG_RUNTIME_DIR", &dir)
                .env("WAYLAND_DISPLAY", "wayland-1")
                .env("SHELL", "/bin/sh")
                .env("HOME", dir.join("home"))
                .env("XDG_CACHE_HOME", dir.join("home/.cache"))
                .env("XDG_CONFIG_HOME", dir.join("home/.config"))
                .env("APOTH_TEST_DUMP", &dump)
                .env("APOTH_TEST_FRAMES", &frames)
                .env("APOTH_TEST_IMAGES", &images)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn apotheosis"),
        );
        let mut s = Session {
            term,
            _abyss: abyss,
            sock: dir.join("sock/abyss.sock"),
            dir,
            dump,
            frames,
            images,
            id: 0,
        };
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
        let mut chunk = [0u8; 4096];
        while let Ok(n) = s.read(&mut chunk) {
            if n == 0 || chunk[..n].ends_with(b"\n") {
                break;
            }
        }
    }

    fn type_text(&mut self, text: &str) {
        self.rpc("type_text", &format!("{{\"text\":{}}}", json_str(text)));
    }

    /// Type `cmd` and wait for a screen line that is exactly `marker`.
    fn run(&mut self, cmd: &str, marker: &str) {
        self.type_text(&format!("{cmd}\n"));
        let t0 = Instant::now();
        loop {
            if let Ok(Some(st)) = self.term.0.try_wait() {
                panic!("apotheosis exited: {st}");
            }
            let seen = std::fs::read_to_string(&self.dump)
                .is_ok_and(|t| t.lines().any(|l| l.trim() == marker));
            if seen {
                return;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "timed out waiting for {marker}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Frames presented since start.
    fn presented(&self) -> u64 {
        read_u64(&self.frames)
    }

    /// Distinct image frames drawn (`APOTH_TEST_IMAGES` `.frames`).
    fn image_frames(&self) -> u64 {
        read_u64(&self.images.with_extension("frames"))
    }

    /// Wait until no frame has been presented for `quiet`.
    fn settle(&self, quiet: Duration) -> u64 {
        let t0 = Instant::now();
        let (mut n, mut since) = (self.presented(), Instant::now());
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let m = self.presented();
            if m != n {
                (n, since) = (m, Instant::now());
            } else if since.elapsed() >= quiet {
                return n;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "never settled");
        }
    }
}

/// T-07 §2 / T-23 `IDLE`: with a static screen, no blink and no animation the window presents
/// nothing; output presents again (the hook counts).
#[test]
fn static_screen_presents_no_frames() {
    let Some(mut s) = Session::start("idle", None) else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    let n = s.settle(Duration::from_millis(500));
    assert!(n > 0, "the hook counted no frames");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(s.presented(), n, "frames presented on an idle screen");
    s.run("echo OK$((2+2))", "OK4");
    assert!(s.presented() > n, "output presented nothing");
}

/// Writes `count` shm objects of a `w`x`h` RGB24 picture (each a distinct colour) and
/// returns their names, as `mpv --vo=kitty --vo-kitty-use-shm=yes` hands them over.
fn shm_frames(tag: &str, count: usize, w: usize, h: usize) -> Vec<String> {
    (0..count)
        .map(|i| {
            let name = format!("apothm38-{tag}-{}-{i}", std::process::id());
            let v = (i * 7 % 251) as u8;
            let px: Vec<u8> = [v, 255 - v, 128].repeat(w * h);
            std::fs::write(Path::new("/dev/shm").join(&name), px).unwrap();
            name
        })
        .collect()
}

fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= c.len() {
                o.push(T[(n >> (18 - 6 * k)) as usize & 63] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

/// The tier A stream shape of `mpv --vo=kitty --vo-kitty-use-shm=yes` (T-16 §1): 120
/// anonymous frames at 30 fps over shm, five times the pane quota, keep playing. Each frame
/// covers the one before, so the covered frames are evicted instead of filling the quota
/// (T-15 §4, owner decision at M38); before that the picture froze once the quota was full
/// (19 of 120 here, 82 % with mpv). The bound is 95 %, not the gate's 99 %: a `sleep` loop
/// on a loaded host can land two frames in one frame interval, and T-07 §2 renders only the
/// latest. The 99 % gate itself is `tier_a_mpv_kitty_shm_1080p30` (media_m34, manual).
#[test]
fn kitty_shm_stream_plays_past_the_quota() {
    const N: usize = 120;
    // 160x90 RGBA is 57.6 KB; 1.2 MiB holds about 21 of them.
    let cfg = "graphics {\n    quota 1258291\n}\n\
               security {\n    sequence {\n        graphics-file \"allow\" \"deny\"\n    }\n}\n";
    let Some(mut s) = Session::start("rate", Some(cfg)) else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    let names = shm_frames("rate", N, 160, 90);
    let list = s.dir.join("frames.list");
    std::fs::write(
        &list,
        names.iter().map(|n| b64(n) + "\n").collect::<String>(),
    )
    .unwrap();
    s.run(
        &format!(
            "clear; while read f; do printf '\\033[3;1H\\033_Ga=T,t=s,f=24,s=160,v=90,C=1,q=2;%s\\033\\\\' \"$f\"; \
             sleep 0.033; done < {}; clear; echo D$?D",
            list.display()
        ),
        "D0D",
    );
    let shown = s.image_frames();
    for n in &names {
        let _ = std::fs::remove_file(Path::new("/dev/shm").join(n));
    }
    eprintln!("kitty shm stream: {shown} of {N} frames shown");
    assert!(shown * 100 >= N as u64 * 95, "{shown} of {N} frames shown");
}
