// SPDX-License-Identifier: AGPL-3.0-only
//! One sheet at a time, and the bind closes it.
//!
//! A spawned bind cannot toggle: the compositor runs the command again on a
//! second press. So a new instance finds the sheet already open through a pid
//! file in the runtime directory, asks it to close, and exits without drawing.
//! The recorded pid is trusted only when `/proc/<pid>/comm` still names this
//! binary, so a stale file never signals an unrelated process.

use std::path::PathBuf;

const NAME: &str = "ec-cheatsheet";

fn pid_file() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("ec-cheatsheet.pid"))
}

fn recorded_pid(path: &PathBuf) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn is_this_binary(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|comm| comm.trim() == NAME)
}

/// Claim the sheet. `false` means an open sheet was just closed instead, and
/// this process should exit without drawing.
pub fn claim() -> bool {
    let Some(path) = pid_file() else {
        return true;
    };
    let me = std::process::id() as i32;
    if let Some(open) = recorded_pid(&path) {
        if open != me && is_this_binary(open) {
            // SAFETY: `kill` takes a pid and a signal number and touches no
            // memory of ours. The pid came from our own pid file and was
            // checked against `/proc` just above.
            unsafe {
                libc::kill(open, libc::SIGTERM);
            }
            let _ = std::fs::remove_file(&path);
            return false;
        }
    }
    let _ = std::fs::write(&path, me.to_string());
    true
}

/// Forget this sheet's pid on a normal close, so the next press opens one.
pub fn release() {
    if let Some(path) = pid_file() {
        if recorded_pid(&path) == Some(std::process::id() as i32) {
            let _ = std::fs::remove_file(&path);
        }
    }
}
