// SPDX-License-Identifier: AGPL-3.0-only
//! M34 headless gates (T-16 §2, §1.1, ADR 0118) on a hidden headless `ec-abyss`: `apoth
//! play` shows a video inline whose frames advance and which goes when it ends; Ctrl+C
//! removes it and exits 130; SIGKILL of a foreground or a background play removes it; and
//! `--ext` exits 0 on Escape in its window and 137 when killed. No sound server is reachable here
//! (`XDG_RUNTIME_DIR` is the test's own), so audio reports unavailable and video plays.
//! State is read from the debug-build hooks (`APOTH_TEST_DUMP`, `APOTH_TEST_IMAGES`).
//! Skips (or fails under `APOTH_REQUIRE_SESSION` and `APOTH_REQUIRE_GPU`) without a
//! compositor or GPU.
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

/// A running compositor and terminal with an interactive `/bin/sh`. Dropped in order:
/// terminal first, then compositor.
struct Session {
    term: Guard,
    _abyss: Guard,
    dir: PathBuf,
    sock: PathBuf,
    dump: PathBuf,
    images: PathBuf,
    id: u32,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Session {
    /// `None` when the environment cannot run the test (reason printed).
    fn start(name: &str) -> Option<Session> {
        Session::start_with(name, None)
    }

    /// With `config` as the user's `apotheosis.kdl`.
    fn start_with(name: &str, config: Option<&str>) -> Option<Session> {
        let Some(abyss) = find_abyss() else {
            assert!(
                std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
                "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
            );
            eprintln!("skipping {name}: ec-abyss not found");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("apm34{name}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["sock", "cfg/eclipse", "home", "out"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
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
        let t0 = Instant::now();
        while !dir.join("wayland-1").exists() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "compositor never came up"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let dump = dir.join("dump.txt");
        let images = dir.join("images.txt");
        if let Some(c) = config {
            std::fs::create_dir_all(dir.join("home/.config/apotheosis")).unwrap();
            std::fs::write(dir.join("home/.config/apotheosis/apotheosis.kdl"), c).unwrap();
        }
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
                .env("PATH", bin_path())
                .env("HOME", dir.join("home"))
                .env("XDG_CACHE_HOME", dir.join("home/.cache"))
                .env("XDG_CONFIG_HOME", dir.join("home/.config"))
                .env("APOTH_TEST_DUMP", &dump)
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

    fn out(&self) -> PathBuf {
        self.dir.join("out")
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

    fn ctrl_c(&mut self) {
        self.rpc("press_key", "{\"key\":\"c\",\"modifiers\":[\"ctrl\"]}");
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
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("timed out waiting for {what}; last content:\n{last}");
    }

    /// Type `cmd` and wait for a screen line that is exactly `marker`.
    fn run(&mut self, cmd: &str, marker: &str) {
        self.type_text(&format!("{cmd}\n"));
        let path = self.dump.clone();
        self.wait_file(&path, marker, |t| t.lines().any(|l| l.trim() == marker));
    }

    fn quads(&mut self, what: &str, pred: impl Fn(&[&str]) -> bool) -> String {
        let path = self.images.clone();
        self.wait_file(&path, what, |t| pred(&t.lines().collect::<Vec<_>>()))
    }

    fn frames(&self) -> u64 {
        std::fs::read_to_string(self.images.with_extension("frames"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    fn frames_above(&mut self, n: u64, what: &str) {
        let t0 = Instant::now();
        while self.frames() <= n {
            assert!(t0.elapsed() < Duration::from_secs(20), "{what}: no frames");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn fixture(s: &Session) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../apoth-media/tests/fixtures/av1-opus-160x90.webm");
    let dst = s.out().join("clip.webm");
    std::fs::copy(src, &dst).unwrap();
    dst
}

fn kill(pid_file: &Path) {
    let pid = std::fs::read_to_string(pid_file).unwrap();
    let st = Command::new("kill")
        .args(["-KILL", pid.trim()])
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn play_shows_frames_and_cleans_up() {
    if !have_apoth("play_shows_frames_and_cleans_up") {
        return;
    }
    let Some(mut s) = Session::start("play") else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    let clip = fixture(&s);

    // Inline: a 160x90 video's frames are drawn and advance; at the end it goes, exit 0.
    s.type_text(&format!(
        "apoth play --size 16x5 {}; echo P$?P\n",
        clip.display()
    ));
    // Fitted into the 16x5 box at its aspect, centred.
    s.quads("the video's quad", |l| {
        l.len() == 1 && l[0].starts_with("160x90 ")
    });
    let n = s.frames();
    s.frames_above(n + 5, "the video plays");
    s.wait_file(&s.dump.clone(), "play to end", |t| {
        t.lines().any(|l| l.trim() == "P0P")
    });
    s.quads("no quads after the end", |l| l.is_empty());
    s.run("clear; echo E$?E", "E0E");

    // Ctrl+C removes it, restores the tty and exits 130.
    s.type_text(&format!(
        "apoth play --loop --size 16x5 {}\n",
        clip.display()
    ));
    s.quads("the looping video", |l| l.len() == 1);
    let n = s.frames();
    s.frames_above(n + 3, "the loop plays");
    s.ctrl_c();
    s.quads("no quads after Ctrl+C", |l| l.is_empty());
    s.run("echo R$?R", "R130R");
    let dump = std::fs::read_to_string(&s.dump).unwrap_or_default();
    assert!(dump.contains("echo R$?R"), "tty restored: {dump}");
    s.run("clear; echo F$?F", "F0F");

    // SIGKILL in the foreground: the session ends with the process.
    let pid = s.out().join("pid");
    s.type_text(&format!(
        "sh -c 'echo $$ > {}; exec apoth play --loop --size 16x5 {}'; echo K$?K; stty sane\n",
        pid.display(),
        clip.display()
    ));
    s.quads("the killed video", |l| l.len() == 1);
    kill(&pid);
    s.wait_file(&s.dump.clone(), "the shell after SIGKILL", |t| {
        t.lines().any(|l| l.trim() == "K137K")
    });
    s.quads("no quads after SIGKILL", |l| l.is_empty());
    s.run("clear; echo G$?G", "G0G");

    // In the background (stopped at its tty change, placement up): SIGKILL removes it.
    let bg = s.out().join("bg");
    s.run(
        &format!(
            "apoth play --loop --size 16x5 {} & echo $! > {}; echo B$((1+2))B",
            clip.display(),
            bg.display()
        ),
        "B3B",
    );
    s.quads("the background video", |l| l.len() == 1);
    kill(&bg);
    s.quads("no quads after the background SIGKILL", |l| l.is_empty());
}

#[test]
fn play_ext_exits_on_escape_and_sigkill() {
    if !have_apoth("play_ext_exits_on_escape_and_sigkill") {
        return;
    }
    let Some(mut s) = Session::start("playx") else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    let clip = fixture(&s);
    let pid = s.out().join("pid");
    let started = s.out().join("started");
    // The window opens, takes the focus and plays; Escape in it closes it and play exits 0.
    s.type_text(&format!(
        "touch {}; apoth play -x --loop --size 16x5 {}; echo X$?X\n",
        started.display(),
        clip.display()
    ));
    s.wait_file(&started, "play -x started", |_| true);
    std::thread::sleep(Duration::from_secs(2));
    s.rpc("press_key", "{\"key\":\"Escape\",\"modifiers\":[]}");
    s.wait_file(&s.dump.clone(), "Escape exit", |t| {
        t.lines().any(|l| l.trim() == "X0X")
    });
    // SIGKILL: the connection drops, the window and its player go, the shell sees 137.
    s.type_text(&format!(
        "sh -c 'echo $$ > {}; exec apoth play -x --loop --size 16x5 {}'; echo K$?K; stty sane\n",
        pid.display(),
        clip.display()
    ));
    s.wait_file(&pid, "the pid", |t| !t.trim().is_empty());
    std::thread::sleep(Duration::from_secs(2));
    kill(&pid);
    s.wait_file(&s.dump.clone(), "SIGKILL exit", |t| {
        t.lines().any(|l| l.trim() == "K137K")
    });
    s.run("echo OK$((2+2))", "OK4");
}

/// Tier A gate (T-16 §1, ADR 0118): `mpv --vo=kitty --vo-kitty-use-shm=yes` plays a 1080p30
/// clip in the pane with under 1% of frames dropped. Manual: needs mpv and a clip, named by
/// `APOTH_TIER_A_CLIP` (the test makes none; `ffmpeg -f lavfi -i testsrc2=size=1920x1080:
/// rate=30 -t 12 -c:v libx264 -preset ultrafast clip.mkv`). Shared memory needs the
/// `graphics-file` class allowed locally, which the session's config does.
#[test]
#[ignore = "manual: needs mpv and APOTH_TIER_A_CLIP"]
fn tier_a_mpv_kitty_shm_1080p30() {
    let Some(clip) = std::env::var_os("APOTH_TIER_A_CLIP").map(PathBuf::from) else {
        eprintln!("skipping: APOTH_TIER_A_CLIP is not set");
        return;
    };
    let cfg = "security {\n    sequence {\n        graphics-file \"allow\" \"deny\"\n    }\n}\n";
    let Some(mut s) = Session::start_with("tiera", Some(cfg)) else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    let stats = s.out().join("stats");
    let lua = s.out().join("drops.lua");
    std::fs::write(
        &lua,
        format!(
            "local best = {{0, 0, 0}}\n\
             local function w()\n\
             local v = {{mp.get_property_number('frame-drop-count'), \
             mp.get_property_number('decoder-frame-drop-count'), \
             mp.get_property_number('estimated-frame-number')}}\n\
             if v[3] == nil then return end\n\
             for i = 1, 3 do best[i] = math.max(best[i], v[i] or 0) end\n\
             local f = io.open({:?}, 'w')\n\
             f:write(string.format('%d %d %d\\n', best[1], best[2], best[3]))\n\
             f:close()\n\
             end\n\
             mp.add_periodic_timer(0.25, w)\n",
            stats.display().to_string()
        ),
    )
    .unwrap();
    s.type_text(&format!(
        "mpv --no-config --vo=kitty --vo-kitty-use-shm={} --no-audio --really-quiet \
         --length=10 --log-file={} --script={} {}; echo M$?M\n",
        std::env::var("APOTH_TIER_A_SHM").unwrap_or_else(|_| "yes".into()),
        s.out().join("mpv.log").display(),
        lua.display(),
        clip.display()
    ));
    let t0 = Instant::now();
    loop {
        let done = std::fs::read_to_string(&s.dump).is_ok_and(|t| {
            t.lines()
                .any(|l| l.trim().starts_with('M') && l.trim().ends_with('M'))
        });
        if done {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "mpv never finished");
        std::thread::sleep(Duration::from_millis(200));
    }
    let shown = s.frames();
    if std::env::var_os("APOTH_TIER_A_DEBUG").is_some() {
        eprintln!("{}", std::fs::read_to_string(&s.dump).unwrap_or_default());
        let log = std::fs::read_to_string(s.out().join("mpv.log")).unwrap_or_default();
        let lines: Vec<&str> = log
            .lines()
            .filter(|l| !l.contains("[osd/") && !l.contains("[d]"))
            .collect();
        eprintln!("{}", lines[lines.len().saturating_sub(60)..].join("\n"));
    }
    let st = std::fs::read_to_string(&stats).unwrap_or_default();
    let v: Vec<u64> = st
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    eprintln!(
        "tier A: mpv vo drops, decoder drops, frames = {st:?}; terminal frames presented {shown}"
    );
    assert_eq!(v.len(), 3, "mpv stats: {st:?}");
    let (vo, dec, n) = (v[0], v[1], v[2]);
    assert!(n >= 250, "only {n} frames played");
    assert!((vo + dec) * 100 < n, "{vo}+{dec} of {n} frames dropped");
}
