// SPDX-License-Identifier: AGPL-3.0-only
//! The binary's pre-lock refusals, run without any compositor: the
//! environment points at a Wayland display that does not exist, so even a
//! refusal that failed to fire could never take a lock here.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let dir = std::env::temp_dir().join(format!("ec-lock-refusals-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    Command::new(env!("CARGO_BIN_EXE_ec-lock"))
        .args(args)
        .env("XDG_RUNTIME_DIR", &dir)
        .env("WAYLAND_DISPLAY", "ec-lock-test-no-such-display")
        .env_remove("EC_ABYSS_SOCKET")
        .output()
        .unwrap()
}

#[test]
fn an_unexpected_argument_is_a_usage_error() {
    let out = run(&["--user", "root"]);
    assert_eq!(out.status.code(), Some(2));
}

/// Without its PAM service file ec-lock must refuse before locking. Only
/// checkable on a host that lacks the file; the lookup itself is unit-tested
/// with injected directories (`service.rs`).
#[test]
fn no_pam_service_file_refuses_to_lock() {
    if ec_lock::service::find(&ec_lock::service::DIRS, ec_lock::PAM_SERVICE).is_some() {
        eprintln!("skipped: this host has the ec-lock PAM service installed");
        return;
    }
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no PAM service file"), "{err}");
    assert!(err.contains("not locking"), "{err}");
}
