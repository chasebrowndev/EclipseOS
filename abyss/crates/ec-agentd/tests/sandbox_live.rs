// SPDX-License-Identifier: AGPL-3.0-only
//! The live sandbox: runs the real `ec-agentd --sandbox-init` helper (Landlock,
//! seccomp, `no_new_privs`) around a probe and checks what the probe cannot do.
//!
//! The probe is this test binary run again with `EC_SANDBOX_PROBE` set. It
//! tries to read a fake `~/.ssh`, to write outside its scratch, to `ptrace`
//! and to open a packet socket, and records what happened in a file in its
//! scratch. bwrap is not involved (the namespaces are not the subject here),
//! so this needs no unprivileged user namespaces, only a kernel with Landlock.
//!
//! Gated on `dev-unsandboxed` (the headless-test feature) because it runs
//! processes under real kernel filters. Skipped, loudly, when the kernel has no
//! Landlock: the launcher refuses to launch there (fail closed), and
//! `the_preflight_matches_the_kernel` in the unit tests covers that side.
#![cfg(feature = "dev-unsandboxed")]

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_agentd::sandbox::{self, Decl, FsPolicy};

const PROBE: &str = "EC_SANDBOX_PROBE";

/// The probe itself. A no-op unless run by the helper below.
#[test]
fn probe_child() {
    let Some(scratch) = std::env::var_os(PROBE).map(PathBuf::from) else {
        return;
    };
    let secret = std::env::var_os("EC_PROBE_SECRET").map(PathBuf::from).unwrap();
    let outside = std::env::var_os("EC_PROBE_OUTSIDE").map(PathBuf::from).unwrap();
    let mut out = String::new();
    let mut note = |name: &str, allowed: bool| {
        out.push_str(&format!(
            "{name}={}\n",
            if allowed { "allowed" } else { "denied" }
        ))
    };

    note("read_secret", std::fs::read(&secret).is_ok());
    note(
        "list_secret_dir",
        std::fs::read_dir(secret.parent().unwrap()).is_ok(),
    );
    note("write_outside", std::fs::write(&outside, b"x").is_ok());
    note("write_scratch", std::fs::write(scratch.join("ok"), b"x").is_ok());
    // SAFETY: plain syscalls with integer arguments.
    let (ptrace_ok, packet_ok, raw_ok, userns_ok, tcp_ok) = unsafe {
        (
            libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) == 0,
            libc::socket(libc::AF_PACKET, libc::SOCK_RAW, 0) >= 0,
            libc::socket(libc::AF_INET, libc::SOCK_RAW, 1) >= 0,
            libc::unshare(libc::CLONE_NEWUSER) == 0,
            libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) >= 0,
        )
    };
    note("ptrace", ptrace_ok);
    note("packet_socket", packet_ok);
    note("raw_socket", raw_ok);
    note("unshare_user", userns_ok);
    // An ordinary socket still works: the filter is a deny-list, not a wall.
    note("tcp_socket", tcp_ok);
    std::fs::write(scratch.join("result"), out).unwrap();
}

#[test]
fn a_sandboxed_child_cannot_read_ssh_ptrace_or_open_raw_sockets() {
    if let Err(why) = sandbox::landlock_abi() {
        eprintln!("skipped: {why}");
        return;
    }
    let base = ec_agentd::scratch_dir("sbx-live");
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_ed25519"), b"not a real key").unwrap();
    let scratch = base.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let outside = base.join("outside.txt");

    // The runtime and the test binary's own directory, read-only; the scratch
    // directory read-write; nothing else (so not `home`, not `base`).
    let me = std::env::current_exe().unwrap();
    let mut policy = FsPolicy::for_package(me.parent().unwrap(), &Decl::default());
    policy.rw = vec![scratch.clone()];
    let cfg = base.join("sandbox.json");
    std::fs::write(&cfg, policy.to_json().to_string()).unwrap();

    let st = Command::new(env!("CARGO_BIN_EXE_ec-agentd"))
        .arg("--sandbox-init")
        .arg(&cfg)
        .arg("--")
        .arg(&me)
        .args(["--exact", "probe_child", "--nocapture", "--test-threads=1"])
        .env(PROBE, &scratch)
        .env("EC_PROBE_SECRET", home.join(".ssh/id_ed25519"))
        .env("EC_PROBE_OUTSIDE", &outside)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "helper failed ({}): {}",
        st.status,
        String::from_utf8_lossy(&st.stderr)
    );
    let result = std::fs::read_to_string(scratch.join("result")).unwrap_or_else(|e| {
        panic!(
            "probe wrote no result ({e}): {}",
            String::from_utf8_lossy(&st.stdout)
        )
    });
    let got = |k: &str| -> String {
        result
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .unwrap_or_else(|| panic!("{k} missing in {result}"))
            .to_owned()
    };
    for k in [
        "read_secret",
        "list_secret_dir",
        "write_outside",
        "ptrace",
        "packet_socket",
        "raw_socket",
        "unshare_user",
    ] {
        assert_eq!(got(k), "denied", "{k}\n{result}");
    }
    assert_eq!(got("write_scratch"), "allowed", "{result}");
    assert_eq!(got("tcp_socket"), "allowed", "{result}");
    assert!(!Path::new(&outside).exists());
}

#[test]
fn a_missing_entrypoint_exits_with_the_sandbox_status_not_zero() {
    if sandbox::landlock_abi().is_err() {
        eprintln!("skipped: no landlock");
        return;
    }
    let base = ec_agentd::scratch_dir("sbx-live-exec");
    let cfg = base.join("sandbox.json");
    std::fs::write(
        &cfg,
        FsPolicy::for_package(Path::new("/nonexistent"), &Decl::default())
            .to_json()
            .to_string(),
    )
    .unwrap();
    let st = Command::new(env!("CARGO_BIN_EXE_ec-agentd"))
        .arg("--sandbox-init")
        .arg(&cfg)
        .args(["--", "/nonexistent/agent"])
        .output()
        .unwrap();
    assert_eq!(st.status.code(), Some(sandbox::EXIT_SANDBOX));
    assert!(String::from_utf8_lossy(&st.stderr).contains("cannot exec"));
}
