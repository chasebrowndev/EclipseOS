// SPDX-License-Identifier: AGPL-3.0-only
//! M32 headless gates (T-15 §5, T-16 §1.1, §6) on a hidden headless `ec-abyss`: a kitty
//! animation shows every frame for five seconds; `apoth icat` places a still that stays
//! after it exits, draws Unicode-placeholder images, and its animations go on Ctrl+C (exit
//! 130) and when the process is SIGKILLed. State is read from the debug-build hooks
//! (`APOTH_TEST_DUMP` screen text, `APOTH_TEST_IMAGES` quad geometry and `.frames` count).
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

fn b64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                o.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A stored-deflate RGB PNG (no compressor needed).
fn png(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let chunk = |out: &mut Vec<u8>, kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut raw = Vec::new();
    for row in rgb.chunks((w * 3) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    let len = raw.len() as u16;
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend_from_slice(&len.to_le_bytes());
    z.extend_from_slice(&(!len).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A looping 2x2 GIF of four solid frames, 50 ms each. LZW without compression: a clear
/// code before every two pixels keeps the code width at 3 bits.
fn gif() -> Vec<u8> {
    let mut o = b"GIF89a".to_vec();
    o.extend_from_slice(&[2, 0, 2, 0, 0x81, 0, 0]);
    o.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    o.extend_from_slice(b"\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00");
    for f in 0..4u32 {
        o.extend_from_slice(&[0x21, 0xf9, 4, 0x04, 5, 0, 0, 0]);
        o.extend_from_slice(&[0x2c, 0, 0, 0, 0, 2, 0, 2, 0, 0, 2]);
        let codes = [4, f, f, 4, f, f, 5];
        let (mut acc, mut bits, mut data) = (0u32, 0, Vec::new());
        for c in codes {
            acc |= c << bits;
            bits += 3;
            while bits >= 8 {
                data.push(acc as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        if bits > 0 {
            data.push(acc as u8);
        }
        o.push(data.len() as u8);
        o.extend_from_slice(&data);
        o.push(0);
    }
    o.push(0x3b);
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
        let Some(abyss) = find_abyss() else {
            assert!(
                std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
                "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
            );
            eprintln!("skipping {name}: ec-abyss not found");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("apm32{name}{}", std::process::id()));
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

#[test]
fn kitty_animation_shows_every_frame() {
    let Some(mut s) = Session::start("anim") else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    // Four 2x2 RGBA frames, 50 ms each, looping forever.
    let px = |i: u8| -> String { b64(&[i * 60, 255 - i * 60, 128, 255].repeat(4)) };
    let mut sh = format!(
        "printf '\\033_Ga=T,f=32,s=2,v=2,i=9,c=4,r=2,q=2;{}\\033\\\\\\r\\n\\n\\n'\n",
        px(0)
    );
    for i in 1..4 {
        sh.push_str(&format!(
            "printf '\\033_Ga=f,f=32,s=2,v=2,i=9,z=50,q=2;{}\\033\\\\'\n",
            px(i)
        ));
    }
    sh.push_str(
        "printf '\\033_Ga=a,i=9,r=1,z=50,q=2\\033\\\\\\033_Ga=a,i=9,s=3,v=1,q=2\\033\\\\'\n",
    );
    sh.push_str("echo A$((0+1))\n");
    std::fs::write(s.out().join("anim.sh"), sh).unwrap();
    let cmd = format!("sh {}/anim.sh", s.out().display());
    s.run(&cmd, "A1");
    s.quads("the animation's quad", |l| l.len() == 1);
    let n = s.frames();
    s.frames_above(n, "the animation runs");
    let (n0, t0) = (s.frames(), Instant::now());
    std::thread::sleep(Duration::from_secs(5));
    let (n1, dt) = (s.frames(), t0.elapsed());
    let want = dt.as_millis() as u64 / 50;
    let shown = n1 - n0;
    eprintln!("animation: {shown} frames shown of {want} due");
    assert!(
        shown * 100 >= want * 99 && shown <= want + 2,
        "{shown} frames in {dt:?}, {want} due"
    );
}

#[test]
fn icat_lifetime() {
    if !have_apoth("icat_lifetime") {
        return;
    }
    let Some(mut s) = Session::start("icat") else {
        return;
    };
    s.run("echo UP$((1+1))", "UP2");
    let out = s.out();
    let rgb: Vec<u8> = (0..16u8).flat_map(|i| [i * 15, 255 - i * 15, 64]).collect();
    std::fs::write(out.join("still.png"), png(4, 4, &rgb)).unwrap();
    std::fs::write(out.join("anim.gif"), gif()).unwrap();
    let (still, anim) = (out.join("still.png"), out.join("anim.gif"));

    // A still stays after icat exits.
    s.run(&format!("apoth icat {}; echo S$?S", still.display()), "S0S");
    s.quads("the still's quad", |l| {
        l.len() == 1 && l[0].starts_with("4x4 ")
    });
    std::thread::sleep(Duration::from_millis(500));
    s.quads("the still after exit", |l| l.len() == 1);
    s.run("apoth icat --clear; echo C$?C", "C0C");
    s.quads("no quads after --clear", |l| l.is_empty());

    // Unicode placeholders draw the image where the placeholder cells are.
    s.run(
        &format!(
            "apoth icat --unicode-placeholder {}; echo U$?U",
            still.display()
        ),
        "U0U",
    );
    s.quads("the placeholder quad", |l| {
        l.len() == 1 && l[0].starts_with("4x4 ")
    });
    s.run("apoth icat --clear; clear; echo D$?D", "D0D");
    s.quads("no quads", |l| l.is_empty());

    // Ctrl+C deletes the animation, restores the tty and exits 130.
    s.type_text(&format!("apoth icat {}\n", anim.display()));
    s.quads("the animation's quad", |l| {
        l.len() == 1 && l[0].starts_with("2x2 ")
    });
    let n = s.frames();
    s.frames_above(n + 2, "the GIF animates");
    s.ctrl_c();
    s.quads("no quads after Ctrl+C", |l| l.is_empty());
    s.run("echo R$?R", "R130R");
    // Echo is back on: the typed command shows on the screen.
    let dump = std::fs::read_to_string(&s.dump).unwrap_or_default();
    assert!(dump.contains("echo R$?R"), "tty restored: {dump}");
    s.run("clear; echo E$?E", "E0E");

    // SIGKILL: the terminal removes the dead invocation's placements itself.
    let pid = out.join("pid");
    s.type_text(&format!(
        "sh -c 'echo $$ > {}; exec apoth icat {}'; echo K$?K; stty sane\n",
        pid.display(),
        anim.display()
    ));
    s.quads("the second animation", |l| {
        l.len() == 1 && l[0].starts_with("2x2 ")
    });
    let pid = std::fs::read_to_string(&pid).unwrap();
    let st = Command::new("kill")
        .args(["-KILL", pid.trim()])
        .status()
        .unwrap();
    assert!(st.success());
    s.wait_file(&s.dump.clone(), "the shell after SIGKILL", |t| {
        t.lines().any(|l| l.trim() == "K137K")
    });
    s.quads("no quads after SIGKILL", |l| l.is_empty());
}
