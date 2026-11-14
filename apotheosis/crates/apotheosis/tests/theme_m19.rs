// SPDX-License-Identifier: AGPL-3.0-only
//! M19 headless gate (T-17 §1, §2; T-10 §7): on a hidden headless `ec-abyss`, the terminal
//! detects Abyss over its control socket, takes the Abyss theme as its default colours
//! (the OSC 11 answer is the theme's base), and a theme flip reaches the running program
//! as the mode-2031 report with the new colours behind it. A shell script inside the pane
//! records what the terminal sends it; nothing else is read. Skips (or fails under
//! `APOTH_REQUIRE_SESSION` and `APOTH_REQUIRE_GPU`) without a compositor or GPU.
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

/// The pane's program: report-mode 2031 on, then query the background until the light
/// Abyss base answers; wait for `go`, query once more. Everything the terminal sends goes
/// to `log` (raw mode, so the replies arrive unechoed).
const SCRIPT: &str = r#"D="$1"
stty raw -echo
printf '\033[?2031h'
q() { printf '\033]11;?\033\\'; timeout --foreground 0.3 cat >> "$D/log"; }
i=0
until grep -q 'f4f4/f1f1/eaea' "$D/log" 2>/dev/null || [ $i -ge 60 ]; do q; i=$((i+1)); done
touch "$D/ready"
i=0
until [ -e "$D/go" ] || [ $i -ge 300 ]; do timeout --foreground 0.1 cat >> "$D/log"; i=$((i+1)); done
q
touch "$D/done"
sleep 30
"#;

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool, term: &mut Guard) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(secs) {
        if f() {
            return true;
        }
        if let Ok(Some(st)) = term.0.try_wait() {
            if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                eprintln!("skipping theme_m19: no usable GPU");
                return false;
            }
            panic!("apotheosis exited early ({what}): {st}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

fn log(dir: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(dir.join("log")).unwrap_or_default()).into_owned()
}

#[test]
fn abyss_theme_is_the_default_and_a_flip_reports_2031() {
    let Some(abyss) = find_abyss() else {
        assert!(
            std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
            "ec-abyss not found (required by APOTH_REQUIRE_SESSION)"
        );
        eprintln!("skipping theme_m19: ec-abyss not found");
        return;
    };
    let dir = std::env::temp_dir().join(format!("apm19theme{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["sock", "cfg/eclipse", "home", "out"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let dir = Dir(dir);
    let d = &dir.0;
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        d.join("cfg/eclipse/policy.kdl"),
        "misc { scripted-input #true }\n",
    )
    .unwrap();
    let abyss_kdl = d.join("cfg/eclipse/abyss.kdl");
    std::fs::write(&abyss_kdl, "general { color-scheme \"light\"; }\n").unwrap();
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
    while !d.join("wayland-1").exists() || !d.join("sock/abyss.sock").exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "compositor never came up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = d.join("out");
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn apotheosis"),
    );

    // Detection: the default background is the light Abyss base, not the stock dark one.
    if !wait_for(
        "the ready mark",
        60,
        || out.join("ready").exists(),
        &mut term,
    ) {
        return;
    }
    let l = log(&out);
    assert!(
        l.contains("\x1b]11;rgb:f4f4/f1f1/eaea"),
        "OSC 11 answers the Abyss base: {l:?}"
    );

    // A flip to dark: the program hears the 2031 report, and the background follows.
    std::fs::write(&abyss_kdl, "general { color-scheme \"dark\"; }\n").unwrap();
    wait_for(
        "the 2031 report",
        20,
        || log(&out).contains("\x1b[?997;1n"),
        &mut term,
    );
    std::fs::write(out.join("go"), b"").unwrap();
    wait_for(
        "the second query",
        20,
        || out.join("done").exists(),
        &mut term,
    );
    let l = log(&out);
    let flip = l.find("\x1b[?997;1n").unwrap();
    assert!(
        l[flip..].contains("\x1b]11;rgb:0b0b/0909/0606"),
        "after the report, OSC 11 answers the dark base: {l:?}"
    );
}
