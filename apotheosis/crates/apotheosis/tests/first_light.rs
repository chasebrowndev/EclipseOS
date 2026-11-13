// SPDX-License-Identifier: AGPL-3.0-only
//! M11 first light: a hidden headless `ec-abyss`, the real binary, input injected through the
//! compositor's control socket, output read back from the test dump hook.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn skip(env: &str, why: &str) {
    if std::env::var_os(env).is_some() {
        panic!("{why} (required by {env})");
    }
    eprintln!("skipping first_light: {why}");
}

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

fn rpc(sock: &Path, id: u32, text: &str) {
    let Ok(mut s) = UnixStream::connect(sock) else {
        return;
    };
    let line = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"type_text\",\"params\":{{\"text\":\"{text}\"}}}}\n"
    );
    let _ = s.write_all(line.as_bytes());
    let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = std::io::Read::read(&mut s, &mut [0u8; 512]);
}

#[test]
fn types_a_command_and_exits() {
    let Some(abyss) = find_abyss() else {
        return skip("APOTH_REQUIRE_SESSION", "ec-abyss not found");
    };
    let dir = std::env::temp_dir().join(format!("apf{}", std::process::id()));
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

    let _abyss = Guard(
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
    let mut term = Guard(
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
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn apotheosis"),
    );

    let sock = dir.join("sock/abyss.sock");
    let t0 = Instant::now();
    let mut id = 0;
    let mut seen = false;
    while t0.elapsed() < Duration::from_secs(40) {
        if let Ok(Some(st)) = term.0.try_wait() {
            if st.code() == Some(69) && std::env::var_os("APOTH_REQUIRE_GPU").is_none() {
                return skip("APOTH_REQUIRE_GPU", "no usable GPU");
            }
            panic!("apotheosis exited early: {st}");
        }
        if dump.exists() {
            id += 1;
            rpc(&sock, id, "echo hi\\n");
        }
        std::thread::sleep(Duration::from_millis(700));
        if let Ok(text) = std::fs::read_to_string(&dump)
            && text.lines().any(|l| l.trim() == "hi")
        {
            seen = true;
            break;
        }
    }
    assert!(seen, "never saw the echoed output in the grid");

    let t1 = Instant::now();
    loop {
        id += 1;
        rpc(&sock, id, "exit\\n");
        if let Some(st) = term.0.try_wait().unwrap() {
            assert!(st.success(), "apotheosis exit status {st}");
            break;
        }
        assert!(t1.elapsed() < Duration::from_secs(15), "did not exit");
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
