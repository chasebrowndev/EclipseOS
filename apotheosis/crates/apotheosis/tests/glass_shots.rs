// SPDX-License-Identifier: AGPL-3.0-only
//! Interface screenshots on a hidden headless `ec-abyss` with real blur (T-08 §6, STYLE.md).
//! Not a gate: it does nothing unless `APOTH_SHOT_DIR=<dir>` is set, and then writes the
//! compositor's own composed frame (`EC_ABYSS_HEADLESS_DUMP`) per surface, theme and scale as
//! `<dir>/<surface>-<dark|light>-<1|2>x.png`. `APOTH_SHOT_ONLY=a,b` limits the surfaces.
//! The backdrop is a generated wallpaper so the blur has something to blur. Paths and
//! content are the test's own; nothing here touches the live session.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn find_abyss() -> Option<PathBuf> {
    std::env::var_os("APOTH_EC_ABYSS").map(PathBuf::from)
}

fn bin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_apotheosis"))
        .parent()
        .unwrap()
        .to_owned()
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

/// A busy backdrop: bands, discs and a grid in warm and cool colours, as an uncompressed PNG.
fn wallpaper(w: usize, h: usize, light: bool) -> Vec<u8> {
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let mut c = if light {
                [0.80 + 0.12 * fx, 0.78 + 0.1 * fy, 0.70]
            } else {
                [0.10 + 0.10 * fx, 0.09 + 0.06 * fy, 0.12]
            };
            for (cx, cy, r, col) in [
                (0.25, 0.30, 0.20, [0.95, 0.70, 0.20]),
                (0.70, 0.65, 0.25, [0.20, 0.55, 0.90]),
                (0.85, 0.20, 0.12, [0.85, 0.25, 0.40]),
                (0.45, 0.85, 0.15, [0.30, 0.80, 0.50]),
            ] {
                let d = ((fx - cx) * 1.6).hypot(fy - cy);
                if d < r {
                    c = col;
                }
            }
            if (x / 40 + y / 40) % 2 == 0 {
                for v in &mut c {
                    *v *= 0.82;
                }
            }
            for (i, v) in c.iter().enumerate() {
                rgb[(y * w + x) * 3 + i] = (v.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }
    png(w, h, &rgb)
}

fn png(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffff_u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for row in rgb.chunks_exact(w * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

struct Shots {
    term: Guard,
    _abyss: Guard,
    dir: PathBuf,
    sock: PathBuf,
    dump: PathBuf,
    mux: PathBuf,
    frame: PathBuf,
    out: PathBuf,
    tag: String,
    id: u32,
    /// Where the pointer rests while a shot settles.
    nudge: (f64, f64),
}

impl Drop for Shots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Shots {
    fn start(light: bool, scale: u32, out: &Path) -> Option<Shots> {
        let abyss = find_abyss()?;
        let tag = format!("{}-{scale}x", if light { "light" } else { "dark" });
        let dir = std::env::temp_dir().join(format!("apshot{}{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["sock", "cfg/eclipse", "home/.config/apotheosis", "home/out"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            dir.join("cfg/eclipse/policy.kdl"),
            "misc { scripted-input #true }\n",
        )
        .unwrap();
        let (lw, lh) = (900usize, 560usize);
        let wp = dir.join("wall.png");
        std::fs::write(
            &wp,
            wallpaper(lw * scale as usize, lh * scale as usize, light),
        )
        .unwrap();
        std::fs::write(
            dir.join("cfg/eclipse/abyss.kdl"),
            format!(
                "general {{ color-scheme \"{}\"; }}\nwallpaper {{ path \"{}\"; mode \"fill\"; }}\noutput \"*\" {{ scale {scale}; }}\n",
                if light { "light" } else { "dark" },
                wp.display()
            ),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("home/.config/eclipse")).unwrap();
        std::fs::write(
            dir.join("home/.config/eclipse/apotheosis.kdl"),
            "version 1\nprofile \"staging\" {\n    env { APOTH_SHOT \"1\"; }\n}\nrule \"open\" {\n    on \"user-var-changed\"\n    where \"name:go\"\n    do { tab.new profile=\"staging\"; }\n}\nrule \"spawn\" {\n    on \"user-var-changed\"\n    where \"name:spawn\"\n    allow-spawn #true\n    do { tab.new { argv \"/bin/sh\"; }; }\n}\n",
        )
        .unwrap();
        let frame = dir.join("frame.png");
        let abyss = Guard(
            Command::new(abyss)
                .args([
                    "--backend",
                    "headless",
                    "--size",
                    &format!("{}x{}", lw * scale as usize, lh * scale as usize),
                ])
                .env("XDG_RUNTIME_DIR", &dir)
                .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
                .env("XDG_CONFIG_HOME", dir.join("cfg"))
                .env("EC_ABYSS_HEADLESS_DUMP", &frame)
                .env_remove("WAYLAND_DISPLAY")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn ec-abyss"),
        );
        let t0 = Instant::now();
        while !dir.join("wayland-1").exists() {
            assert!(t0.elapsed() < Duration::from_secs(20), "no compositor");
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut path = vec![bin_dir()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let dump = dir.join("dump.txt");
        let mux = dir.join("mux.txt");
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
                .env("PATH", std::env::join_paths(path).unwrap())
                .env("HOME", dir.join("home"))
                .env("XDG_CACHE_HOME", dir.join("home/.cache"))
                .env("XDG_CONFIG_HOME", dir.join("home/.config"))
                .env("APOTH_TEST_DUMP", &dump)
                .env("APOTH_TEST_MUX", &mux)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn apotheosis"),
        );
        let mut s = Shots {
            nudge: (3.0, 3.0),
            term,
            _abyss: abyss,
            sock: dir.join("sock/abyss.sock"),
            dir,
            dump,
            mux,
            frame,
            out: out.to_owned(),
            tag,
            id: 0,
        };
        let t0 = Instant::now();
        while !s.dump.exists() {
            if let Ok(Some(st)) = s.term.0.try_wait() {
                panic!("apotheosis exited early: {st}");
            }
            assert!(t0.elapsed() < Duration::from_secs(40), "no first frame");
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

    fn key(&mut self, key: &str, mods: &[&str]) {
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

    /// Run a shell line in the focused pane.
    fn sh(&mut self, line: &str) {
        self.type_text(&format!("{line}\n"));
    }

    /// Move the pointer to compositor-logical `(x, y)` (a zero scroll).
    fn hover(&mut self, x: f64, y: f64) {
        self.rpc("scroll_at", &format!("{{\"x\":{x},\"y\":{y},\"dy\":0}}"));
    }

    fn click(&mut self, x: f64, y: f64, button: &str) {
        self.click_with(x, y, button, &[]);
    }

    fn click_with(&mut self, x: f64, y: f64, button: &str, mods: &[&str]) {
        let m: Vec<String> = mods.iter().map(|m| json_str(m)).collect();
        self.rpc(
            "click_at",
            &format!(
                "{{\"x\":{x},\"y\":{y},\"button\":\"{button}\",\"modifiers\":[{}]}}",
                m.join(",")
            ),
        );
    }

    fn pause(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn mux_has(&self, needle: &str) -> bool {
        std::fs::read_to_string(&self.mux).is_ok_and(|t| t.contains(needle))
    }

    fn until(&self, what: &str, f: impl Fn(&Shots) -> bool) {
        let t0 = Instant::now();
        while !f(self) {
            assert!(t0.elapsed() < Duration::from_secs(25), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    fn screen_has(&self, needle: &str) -> bool {
        std::fs::read_to_string(&self.dump).is_ok_and(|t| t.contains(needle))
    }

    /// Settle, then keep the compositor's next two frame dumps and save the later one.
    fn shot(&mut self, name: &str) {
        self.pause(700);
        let seen = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let mut last = seen(&self.frame);
        let mut fresh = 0;
        let t0 = Instant::now();
        while fresh < 2 && t0.elapsed() < Duration::from_secs(12) {
            let now = seen(&self.frame);
            if now != last {
                fresh += 1;
                last = now;
            }
            // Keep frames coming: a nudge of the pointer redraws the output.
            let (x, y) = self.nudge;
            self.hover(x, y);
            std::thread::sleep(Duration::from_millis(150));
        }
        std::fs::create_dir_all(&self.out).unwrap();
        let to = self.out.join(format!("{name}-{}.png", self.tag));
        std::fs::copy(&self.frame, &to).expect("a frame dump");
        println!("shot {}", to.display());
    }
}

fn wanted(name: &str) -> bool {
    std::env::var("APOTH_SHOT_ONLY")
        .map(|v| v.split(',').any(|w| w == name))
        .unwrap_or(true)
}

fn run_cases(s: &mut Shots) {
    s.until("the shell", |s| s.screen_has("$") || s.screen_has("%"));
    let home = s.dir.join("home");
    let clip = home.join("out/clip.webm");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../apoth-media/tests/fixtures/av1-opus-160x90.webm"),
        &clip,
    )
    .unwrap();
    std::fs::write(
        home.join("out/pic.png"),
        png(2, 2, &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]),
    )
    .unwrap();
    let apps = home.join(".local/share/applications");
    std::fs::create_dir_all(&apps).unwrap();
    for (id, name, mime, term) in [
        ("lightbox", "Lightbox", "image/png;image/jpeg;", "false"),
        (
            "pixelwright",
            "Pixelwright",
            "image/png;image/gif;",
            "false",
        ),
        (
            "orbit-player",
            "Orbit Player",
            "video/webm;video/mp4;audio/flac;",
            "false",
        ),
        ("quill", "Quill", "text/plain;text/markdown;", "false"),
        ("hexbench", "Hexbench", "application/octet-stream;", "true"),
        ("fieldnotes", "Field Notes", "text/plain;", "false"),
    ] {
        std::fs::write(
            apps.join(format!("{id}.desktop")),
            format!("[Desktop Entry]\nType=Application\nName={name}\nExec=true %f\nMimeType={mime}\nIcon={id}\nTerminal={term}\n"),
        )
        .unwrap();
    }
    std::fs::write(
        home.join(".config/mimeapps.list"),
        "[Default Applications]\nvideo/webm=orbit-player.desktop\ntext/plain=quill.desktop\n",
    )
    .unwrap();
    std::fs::write(home.join("out/notes.txt"), "hello\n").unwrap();
    if wanted("terminal") {
        s.sh("clear; ls /");
        s.pause(500);
        s.shot("terminal");
    }
    if wanted("open-menu") {
        s.sh("clear");
        s.sh("apoth do \"open.menu $HOME/out/pic.png\"");
        s.until("the Open menu", |s| s.mux_has("palette open mode Open"));
        s.shot("open-menu");
        s.key("Escape", &[]);
        s.until("menu closed", |s| s.mux_has("palette closed"));
    }
    if wanted("open-picker") {
        s.sh("clear");
        s.sh("apoth do \"open.with $HOME/out/pic.png\"");
        s.until("the picker", |s| s.mux_has("palette open mode Open"));
        s.shot("open-picker");
        s.key("Escape", &[]);
        s.until("picker closed", |s| s.mux_has("palette closed"));
    }
    if wanted("open-picker-text") {
        s.sh("clear");
        s.sh("apoth do \"open.with $HOME/out/notes.txt\"");
        s.until("the picker", |s| s.mux_has("palette open mode Open"));
        s.shot("open-picker-text");
        s.key("Escape", &[]);
        s.until("picker closed", |s| s.mux_has("palette closed"));
    }
    if wanted("float") {
        s.sh("clear; ls -la /usr/share | head -30");
        s.pause(500);
        s.sh("apoth do pane.float");
        s.until("a float", |s| s.mux_has(" float"));
        s.pause(500);
        s.sh("echo focused float; ls /");
        s.shot("float");
        s.sh("exit");
        s.until("float gone", |s| !s.mux_has(" float"));
    }
    if wanted("play-inline") {
        s.sh("clear; echo 'Inline video:'; apoth play --loop \"$HOME/out/clip.webm\"");
        s.pause(2500);
        s.shot("play-inline");
        // Pointer over the picture: the hover controls (T-08 §4).
        s.nudge = (120.0, 90.0);
        s.hover(120.0, 90.0);
        s.pause(600);
        s.shot("play-inline-hover");
        s.nudge = (3.0, 3.0);
        s.hover(3.0, 3.0);
        s.key("c", &["ctrl"]);
        s.pause(500);
    }
    if wanted("play-muted") {
        // The session has no audio output: the player plays muted and says so, for 4 s,
        // in a pill at the pane's top right (T-08 §4, T-16 §3).
        s.sh("clear; echo 'Inline video:'; apoth play --loop \"$HOME/out/clip.webm\"");
        s.pause(900);
        s.shot("play-muted");
        s.key("c", &["ctrl"]);
        s.pause(4500);
    }
    if wanted("play-ext") {
        s.sh("clear; apoth play --ext --loop --size 56x16 \"$HOME/out/clip.webm\"");
        s.pause(3500);
        s.shot("play-ext");
        // Pointer over the picture: focus rim, hover close button and the hint.
        s.nudge = (400.0, 200.0);
        s.click(400.0, 200.0, "left");
        s.pause(600);
        s.shot("play-ext-hover");
        s.nudge = (3.0, 3.0);
        s.key("Escape", &[]);
        s.pause(800);
    }
    let uservar = |name: &str| {
        format!("printf '\\033]1337;SetUserVar={name}=%s\\007' $(printf yes | base64)")
    };
    if wanted("tab-profile") {
        s.sh("clear");
        s.sh(&uservar("go"));
        s.until("a second tab", |s| s.mux_has("tabs 2"));
        s.pause(600);
        s.sh("printf '\\033]9;4;1;45\\033\\\\'");
        s.pause(500);
        s.shot("tab-profile-progress");
        s.sh("exit");
        s.pause(800);
    }
    if wanted("rule-confirm") {
        s.sh("clear");
        s.sh(&uservar("spawn"));
        s.pause(1200);
        s.shot("rule-confirm");
        s.key("Escape", &[]);
        s.pause(500);
    }
    if wanted("link-confirm") {
        s.sh("clear; printf '\\033]8;;https://update.example.net/x\\033\\\\Download the update\\033]8;;\\033\\\\\\n'");
        s.pause(800);
        s.nudge = (60.0, 25.0);
        s.hover(60.0, 25.0);
        s.pause(500);
        s.click_with(60.0, 25.0, "left", &["ctrl"]);
        s.pause(1000);
        s.shot("link-hover");
        s.nudge = (3.0, 3.0);
        s.key("Escape", &[]);
        s.pause(500);
    }
    if wanted("bell") {
        s.sh("clear; echo 'build finished'; (while :; do printf '\\a'; sleep 0.05; done) & echo $! > $HOME/bell.pid");
        s.pause(800);
        s.shot("bell");
        s.sh("kill $(cat $HOME/bell.pid)");
        s.pause(500);
    }
    if wanted("copy-mode") {
        s.sh("clear; echo one two three; echo four five six");
        s.pause(500);
        s.sh("apoth do copy-mode.enter");
        s.pause(500);
        s.key("Up", &[]);
        s.pause(300);
        s.shot("copy-mode");
        s.key("Escape", &[]);
        s.pause(400);
    }
    if wanted("open-menu-remote") {
        s.sh("clear; printf '\\033]7;file://build-box.lan/srv/data\\033\\\\'");
        s.pause(400);
        s.sh("apoth do \"open.menu /srv/data/report.pdf\"");
        s.until("the Open menu", |s| s.mux_has("palette open mode Open"));
        s.shot("open-menu-remote");
        s.key("Escape", &[]);
        s.until("menu closed", |s| s.mux_has("palette closed"));
    }
}

#[test]
fn glass_shots() {
    let Some(out) = std::env::var_os("APOTH_SHOT_DIR").map(PathBuf::from) else {
        return;
    };
    let only: Option<Vec<String>> = std::env::var("APOTH_SHOT_VARIANTS")
        .ok()
        .map(|v| v.split(',').map(str::to_owned).collect());
    for (light, scale) in [(false, 1), (true, 1), (false, 2), (true, 2)] {
        let tag = format!("{}-{scale}x", if light { "light" } else { "dark" });
        if only.as_ref().is_some_and(|o| !o.contains(&tag)) {
            continue;
        }
        let Some(mut s) = Shots::start(light, scale, &out) else {
            eprintln!("APOTH_EC_ABYSS is not set");
            return;
        };
        run_cases(&mut s);
    }
}
