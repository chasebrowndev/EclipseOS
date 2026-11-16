// SPDX-License-Identifier: AGPL-3.0-only
//! M25 headless gate (T-15 §1, §5, §6): on a hidden headless `ec-abyss`, a program in the
//! pane transmits a PNG (decoded on a worker while the pane's output waits) and a raw RGB
//! image under the text. The program hears the kitty OK, the text after the images lands
//! after them, and the frame draws both quads (the debug-build `APOTH_TEST_IMAGES` dump:
//! sizes and geometry only). Skips (or fails under `APOTH_REQUIRE_SESSION` and
//! `APOTH_REQUIRE_GPU`) without a compositor or GPU.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::os::unix::fs::PermissionsExt;
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

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The pane's program: a 4x2 PNG at z 0 (id 7, replies on), the same pixels raw at z -1
/// (id 8, quiet), then text. Everything the terminal sends goes to `log`.
const SCRIPT: &str = r#"D="$1"
stty raw -echo
P=$(base64 -w0 "$D/img.png")
R=$(base64 -w0 "$D/img.rgb")
printf '\033_Ga=T,f=100,i=7;%s\033\\' "$P"
printf '\033_Ga=T,f=24,s=4,v=2,i=8,z=-1,q=2,C=1;%s\033\\after' "$R"
timeout --foreground 2 cat >> "$D/log"
touch "$D/done"
sleep 30
"#;

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

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool, term: &mut Guard) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if f() {
            return true;
        }
        if let Ok(Some(st)) = term.0.try_wait() {
            if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                eprintln!("skipping graphics_m25: no usable GPU");
                return false;
            }
            panic!("apotheosis exited early ({what}): {st}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

fn read(p: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned()
}

#[test]
fn kitty_images_decode_off_the_io_thread_and_draw() {
    let Some(abyss) = find_abyss() else {
        assert!(
            std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
            "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
        );
        eprintln!("skipping graphics_m25: ec-abyss not found");
        return;
    };
    let dir = std::env::temp_dir().join(format!("apm25gfx{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["sock", "cfg", "home", "out"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let dir = Dir(dir);
    let d = &dir.0;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    let out = d.join("out");
    let rgb: Vec<u8> = (0..8u8).flat_map(|i| [i * 30, 255 - i * 30, 128]).collect();
    std::fs::write(out.join("img.rgb"), &rgb).unwrap();
    std::fs::write(out.join("img.png"), png(4, 2, &rgb)).unwrap();
    std::fs::write(d.join("probe.sh"), SCRIPT).unwrap();

    let _abyss = Guard(
        Command::new(abyss)
            .args(["--backend", "headless", "--size", "800x600"])
            .env("XDG_RUNTIME_DIR", d)
            .env("ECLIPSE_SOCKET_DIR", d.join("sock"))
            .env("XDG_CONFIG_HOME", d.join("cfg"))
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ec-abyss"),
    );
    let t0 = Instant::now();
    while !d.join("wayland-1").exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "compositor never came up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (dump, images) = (d.join("dump.txt"), d.join("images.txt"));
    let mut term = Guard(
        Command::new(env!("CARGO_BIN_EXE_apotheosis"))
            .args(["-e", "/bin/sh"])
            .arg(d.join("probe.sh"))
            .arg(&out)
            .env_remove("APOTHEOSIS_SOCKET")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/apoth-test-bus",
            )
            .env("ECLIPSE_SOCKET_DIR", d.join("sock"))
            .env("XDG_RUNTIME_DIR", d)
            .env("WAYLAND_DISPLAY", "wayland-1")
            .env("SHELL", "/bin/sh")
            .env("HOME", d.join("home"))
            .env("XDG_CACHE_HOME", d.join("home/.cache"))
            .env("XDG_CONFIG_HOME", d.join("home/.config"))
            .env("APOTH_TEST_DUMP", &dump)
            .env("APOTH_TEST_IMAGES", &images)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn apotheosis"),
    );

    if !wait_for("the done mark", 60, || out.join("done").exists(), &mut term) {
        return;
    }
    let l = read(&out.join("log"));
    assert_eq!(l, "\x1b_Gi=7;OK\x1b\\", "only the PNG replies, once");
    wait_for(
        "the text after the images",
        20,
        || read(&dump).contains("after"),
        &mut term,
    );
    wait_for(
        "both quads in the frame",
        20,
        || read(&images).lines().count() == 2,
        &mut term,
    );
    let quads = read(&images);
    let lines: Vec<&str> = quads.lines().collect();
    assert!(
        lines[0].starts_with("4x2 dst=") && lines[0].ends_with(" z=-1"),
        "under the text first: {quads:?}"
    );
    assert!(
        lines[1].starts_with("4x2 dst=") && lines[1].ends_with(" z=0"),
        "then over it: {quads:?}"
    );
    // Both at the top-left cell; the PNG moved the cursor, the quiet one (C=1) did not.
    assert!(
        lines[1].contains("dst=0,0,4,2"),
        "the PNG at the origin: {quads:?}"
    );
}
