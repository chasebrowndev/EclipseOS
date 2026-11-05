// SPDX-License-Identifier: AGPL-3.0-only
//! `--kiosk` end to end on the headless backend: abyss hosts one client and
//! exits when it does. Each run gets a private `XDG_RUNTIME_DIR`, so nothing
//! here touches the live session's sockets. DRM is not covered.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Run {
    code: Option<i32>,
    stderr: String,
}

/// Run `ec-abyss --backend headless <args>` and wait up to 20 s, killing the
/// captured pid on timeout.
fn run(args: &[&str]) -> Run {
    // Short path: a wayland socket path must fit in `SUN_LEN`.
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ek{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_ec-abyss"))
        .args(["--backend", "headless"])
        .args(args)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("ECLIPSE_SOCKET_DIR")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.is_some(), "ec-abyss did not exit on its own: {stderr}");
    Run {
        code: status.and_then(|s| s.code()),
        stderr,
    }
}

/// No EGL device (some CI hosts): the compositor never gets as far as the
/// client, so there is nothing to test.
fn no_egl(r: &Run) -> bool {
    r.stderr.contains("EGL") || r.stderr.contains("egl")
}

#[test]
fn exits_zero_when_the_client_does() {
    let r = run(&["--kiosk", "--", "sh", "-c", "sleep 0.5; exit 0"]);
    if no_egl(&r) {
        eprintln!("skipped: headless EGL unavailable");
        return;
    }
    assert_eq!(r.code, Some(0), "{}", r.stderr);
}

#[test]
fn exits_nonzero_when_the_client_cannot_start() {
    let r = run(&["--kiosk", "--", "/nonexistent/binary"]);
    if no_egl(&r) {
        eprintln!("skipped: headless EGL unavailable");
        return;
    }
    assert_eq!(r.code, Some(1), "{}", r.stderr);
}

#[test]
fn kiosk_with_session_is_refused() {
    let r = run(&["--kiosk", "--session", "--", "true"]);
    assert_eq!(r.code, Some(1));
    assert!(r.stderr.contains("--kiosk"), "{}", r.stderr);
}

#[test]
fn kiosk_without_a_client_is_refused() {
    let r = run(&["--kiosk"]);
    assert_eq!(r.code, Some(1));
    let r = run(&["--kiosk", "--"]);
    assert_eq!(r.code, Some(1));
}

#[test]
fn double_dash_without_kiosk_is_refused() {
    let r = run(&["--", "true"]);
    assert_eq!(r.code, Some(1));
}
