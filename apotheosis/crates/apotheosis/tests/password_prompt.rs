// SPDX-License-Identifier: AGPL-3.0-only
//! Password prompts through `ec-secret-prompt` (T-17 §5 auto-open, T-22 §3 routing).
//!
//! A stand-in `ec-secret-prompt` (a shell script named by the debug-only
//! `APOTH_SECRET_PROMPT`) records the context it was given and its parent pid, and
//! answers `s3cret`. The window runs on a hidden headless compositor; keys are typed
//! through its scripted input.
//!
//! - A getpass-style read (`stty -echo` + a line read) in the focused pane opens the
//!   prompt, and the answer reaches the program.
//! - A raw-mode read (`stty raw -echo`, like vim) never opens it.
//! - `security.sudo-askpass`: `$SUDO_ASKPASS` in a pane is answered through the prompt.
//!
//! "Unfocused pane" and "askpass pending" are the `Gate` unit tests in
//! `src/app/password.rs`. Scripted input cannot move focus away and back in this harness.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{BufRead, BufReader, Write};
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
    if let Some(p) = std::env::var_os("APOTH_EC_ABYSS") {
        return Some(PathBuf::from(p));
    }
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("ec-abyss"))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
            "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
        );
        eprintln!("skipping: ec-abyss not found");
    }
    found
}

fn apoth_bin() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_BIN_EXE_apotheosis"))
        .parent()?
        .join("apoth");
    if p.is_file() {
        return std::fs::canonicalize(p).ok();
    }
    assert!(
        std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
        "the apoth binary is not built next to apotheosis"
    );
    eprintln!("skipping: the apoth binary is not built");
    None
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("appw{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("home/.config/eclipse")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// The stand-in prompt: context to `ctx.N`, its parent pid to `ppid.N`, answer `s3cret`.
const PROMPT: &str = r#"#!/bin/sh
d=$(dirname "$0")/..
[ "$1" = terminal ] || exit 2
n=$(ls "$d" | grep -c '^ctx\.')
cat > "$d/ctx.$n"
echo "$PPID" > "$d/ppid.$n"
printf '\000s3cret'
"#;

const GETPASS: &str = r#"stty -echo
printf 'Password: '
read pw
stty echo
echo
[ "$pw" = s3cret ] && : > "$(dirname "$0")/getpass.ok"
"#;

const RAW: &str = r#"stty raw -echo
dd bs=1 count=1 >/dev/null 2>&1
stty sane
: > "$(dirname "$0")/raw.done"
"#;

fn write_exec(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn start_abyss(abyss: &Path, dir: &Path) -> Guard {
    std::fs::create_dir_all(dir.join("sock")).unwrap();
    std::fs::create_dir_all(dir.join("cfg/eclipse")).unwrap();
    std::fs::write(
        dir.join("cfg/eclipse/policy.kdl"),
        "misc { scripted-input #true }\n",
    )
    .unwrap();
    let g = Guard(
        Command::new(abyss)
            .args(["--backend", "headless", "--size", "800x600"])
            .env("XDG_RUNTIME_DIR", dir)
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
    g
}

fn gui(dir: &Path) -> Guard {
    let path = format!(
        "{}:{}",
        dir.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Guard(
        Command::new(env!("CARGO_BIN_EXE_apotheosis"))
            .env("XDG_RUNTIME_DIR", dir)
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("home/.config"))
            .env("XDG_CACHE_HOME", dir.join("home/.cache"))
            .env("XDG_STATE_HOME", dir.join("home/.state"))
            .env("SHELL", "/bin/sh")
            .env("ENV", "/dev/null")
            .env("PATH", path)
            .env("WAYLAND_DISPLAY", "wayland-1")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/apoth-test-bus",
            )
            .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
            .env("APOTH_SECRET_PROMPT", dir.join("bin/ec-secret-prompt"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn apotheosis"),
    )
}

fn type_text(dir: &Path, text: &str) {
    let Ok(s) = UnixStream::connect(dir.join("sock/abyss.sock")) else {
        return;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let mut s = BufReader::new(s);
    let body = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"type_text\",\"params\":{{\"text\":\"{body}\"}}}}\n"
    );
    let _ = s.get_mut().write_all(line.as_bytes());
    let mut out = String::new();
    let _ = s.read_line(&mut out);
}

/// Wait for `p`; `None` when the window found no GPU (skip), panics if it died otherwise.
fn wait_file(g: &mut Guard, p: &Path, secs: u64) -> Option<bool> {
    let t0 = Instant::now();
    loop {
        if p.exists() {
            return Some(true);
        }
        if let Ok(Some(st)) = g.0.try_wait() {
            if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                eprintln!("skipping: no usable GPU");
                return None;
            }
            panic!("apotheosis exited: {st}");
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            return Some(false);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Type `: > ready` until the shell runs it (the first keys may land before the prompt).
fn shell_ready(g: &mut Guard, dir: &Path) -> Option<bool> {
    let ready = dir.join("ready");
    for _ in 0..15 {
        type_text(dir, &format!(": > {}\n", ready.display()));
        if wait_file(g, &ready, 2)? {
            return Some(true);
        }
    }
    Some(false)
}

fn prompts(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("ctx."))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn a_getpass_read_opens_the_prompt_and_a_raw_read_never_does() {
    let Some(abyss) = find_abyss() else { return };
    let dir = temp_dir("auto");
    write_exec(&dir.join("bin/ec-secret-prompt"), PROMPT);
    std::fs::write(dir.join("getpass.sh"), GETPASS).unwrap();
    std::fs::write(dir.join("raw.sh"), RAW).unwrap();
    let _abyss = start_abyss(&abyss, &dir);
    let mut g = gui(&dir);
    let Some(ready) = shell_ready(&mut g, &dir) else {
        return;
    };
    assert!(ready, "the pane's shell never ran a command");

    // Raw mode (vim-like): ECHO off but ICANON off too. Never a prompt.
    type_text(&dir, &format!("sh {}\n", dir.join("raw.sh").display()));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(prompts(&dir), 0, "a raw-mode read opened the prompt");
    type_text(&dir, "x");
    assert_eq!(wait_file(&mut g, &dir.join("raw.done"), 10), Some(true));
    assert_eq!(prompts(&dir), 0);

    // getpass-style: the prompt opens by itself and its answer reaches the program.
    type_text(&dir, &format!("sh {}\n", dir.join("getpass.sh").display()));
    assert_eq!(
        wait_file(&mut g, &dir.join("getpass.ok"), 15),
        Some(true),
        "the answer never reached the getpass read"
    );
    assert_eq!(prompts(&dir), 1, "exactly one prompt for one read");
    let ctx = std::fs::read_to_string(dir.join("ctx.0")).unwrap();
    assert!(ctx.contains("\"via\":\"pty\""), "{ctx}");
    assert!(ctx.contains("\"in_foreground\":true"), "{ctx}");
    assert!(ctx.contains("Password:"), "{ctx}");
    // The prompt's parent is the window itself (what its verified check relies on).
    let ppid: u32 = std::fs::read_to_string(dir.join("ppid.0"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(ppid, g.0.id());
    drop(g);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sudo_askpass_in_a_pane_is_answered_through_the_prompt() {
    let Some(abyss) = find_abyss() else { return };
    let Some(apoth) = apoth_bin() else { return };
    let dir = temp_dir("sudo");
    write_exec(&dir.join("bin/ec-secret-prompt"), PROMPT);
    std::os::unix::fs::symlink(&apoth, dir.join("bin/apoth")).unwrap();
    std::fs::write(
        dir.join("home/.config/eclipse/apotheosis.kdl"),
        "security {\n  sudo-askpass #true\n}\n",
    )
    .unwrap();
    let _abyss = start_abyss(&abyss, &dir);
    let mut g = gui(&dir);
    let Some(ready) = shell_ready(&mut g, &dir) else {
        return;
    };
    assert!(ready, "the pane's shell never ran a command");
    let out = dir.join("askpass.out");
    // What `sudo -A` does: run `$SUDO_ASKPASS PROMPT` and read its stdout.
    type_text(
        &dir,
        &format!(
            "\"$SUDO_ASKPASS\" '[sudo] password for u: ' > {o}.tmp && mv {o}.tmp {o}\n",
            o = out.display()
        ),
    );
    assert_eq!(
        wait_file(&mut g, &out, 15),
        Some(true),
        "askpass never answered"
    );
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "s3cret\n");
    assert_eq!(prompts(&dir), 1);
    let ctx = std::fs::read_to_string(dir.join("ctx.0")).unwrap();
    assert!(ctx.contains("\"via\":\"askpass\""), "{ctx}");
    assert!(ctx.contains("[sudo] password for u: "), "{ctx}");
    assert!(ctx.contains("\"host\":null"), "{ctx}");
    drop(g);
    let _ = std::fs::remove_dir_all(&dir);
}
