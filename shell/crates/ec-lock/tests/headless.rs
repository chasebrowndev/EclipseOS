// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-lock` against a real compositor: `ec-abyss --backend headless` in a
//! private `XDG_RUNTIME_DIR`, so nothing here touches the live session.
//!
//! Locks, checks the compositor reports the session locked, kills the locker
//! by the pid this test spawned, and checks the session is *still* locked
//! (ADR 0024). Also checks a second locker refuses to start beside the first.
//!
//! The compositor is another workspace's binary, so this is ignored by
//! default. Run it with
//!
//! ```text
//! (cd abyss && cargo build -p ec-abyss)
//! EC_ABYSS_BIN=$CARGO_TARGET_DIR/debug/ec-abyss cargo test -p ec-lock --test headless -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Kills the captured pid on drop, so a failed assertion leaves nothing behind.
struct Spawned(Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for<T>(what: &str, within: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < within, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The compositor's Wayland socket name in `dir`, once it exists.
fn wayland_socket(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let name = e.file_name().into_string().ok()?;
        (name.starts_with("wayland-") && !name.ends_with(".lock")).then_some(name)
    })
}

/// `Some(true)` if the compositor refuses a focus command because the session
/// is locked, `Some(false)` if it answers otherwise, `None` if unreachable.
fn locked(sock: &Path) -> Option<bool> {
    let mut c = ec_ipc::Client::connect_to(sock).ok()?;
    match c.call("focus_window", serde_json::json!({"id": 0})) {
        Err(ec_ipc::Error::Rpc { message, .. }) => Some(message.contains("session is locked")),
        Ok(_) => Some(false),
        Err(_) => None,
    }
}

#[test]
#[ignore = "needs EC_ABYSS_BIN pointing at an ec-abyss build"]
fn killing_the_locker_leaves_the_session_locked() {
    let Some(abyss) = std::env::var_os("EC_ABYSS_BIN").map(PathBuf::from) else {
        eprintln!("skipped: EC_ABYSS_BIN is not set");
        return;
    };
    assert!(abyss.is_file(), "EC_ABYSS_BIN={} is not a file", abyss.display());
    if ec_lock::service::find(&ec_lock::service::DIRS, ec_lock::PAM_SERVICE).is_none() {
        // ec-lock refuses to lock without it (tests/refusals.rs covers that).
        eprintln!("skipped: no /etc/pam.d/ec-lock; install packaging/pam.d/ec-lock first");
        return;
    }

    // Short path: a Wayland socket path must fit in `SUN_LEN`.
    let dir = std::env::temp_dir().join(format!("el{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let log = std::fs::File::create(dir.join("abyss.log")).unwrap();
    let compositor = Spawned(
        Command::new(&abyss)
            .args(["--backend", "headless"])
            .env("XDG_RUNTIME_DIR", &dir)
            .env_remove("ECLIPSE_SOCKET_DIR")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );

    let socket = wait_for("the headless Wayland socket", Duration::from_secs(20), || {
        wayland_socket(&dir)
    });
    // Headless isolates its control socket beside its Wayland socket.
    let ctl_dir = dir.join(format!("eclipse-headless-{socket}"));
    let ctl = ctl_dir.join("abyss.sock");
    let before = wait_for("the control socket", Duration::from_secs(20), || locked(&ctl));
    assert!(!before, "the session is locked before any locker ran");

    let locker = |dir: &Path| {
        Command::new(env!("CARGO_BIN_EXE_ec-lock"))
            .env("XDG_RUNTIME_DIR", dir)
            .env("WAYLAND_DISPLAY", &socket)
            .env("ECLIPSE_SOCKET_DIR", &ctl_dir)
            .env_remove("DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };

    let mut first = Spawned(locker(&dir));
    wait_for("the session to lock", Duration::from_secs(20), || {
        locked(&ctl).filter(|l| *l)
    });

    // A second locker finds the claim held and exits at once, locking nothing new.
    let mut second = Spawned(locker(&dir));
    let status = wait_for("the second locker to exit", Duration::from_secs(10), || {
        second.0.try_wait().unwrap()
    });
    assert!(status.success(), "second locker: {status}");

    // Kill the first by the pid we hold. SIGKILL: no handler, no cleanup.
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        locked(&ctl),
        Some(true),
        "a dead locker must not unlock (ADR 0024)"
    );

    drop(compositor);
    let _ = std::fs::remove_dir_all(&dir);
}
