// SPDX-License-Identifier: AGPL-3.0-only
//! A cancel flag for `account login`.
//!
//! SIGINT, SIGTERM and SIGHUP only set a flag; the login loop polls it every
//! 100 ms and unwinds, so `claude` is killed and its temp config directory
//! removed. (SIGKILL cannot be caught: the directory is then left behind and
//! `claude` is orphaned until its own timeout.)
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    REQUESTED.store(true, Ordering::Relaxed);
}

pub fn install() {
    for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `on_signal` is async-signal-safe (one atomic store) and has
        // the signature `signal` expects.
        unsafe { libc::signal(s, on_signal as *const () as libc::sighandler_t) };
    }
}

pub fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// Asks the kernel to SIGKILL the child when this process dies, however it
/// dies (the drop guards cannot run on SIGKILL).
pub fn die_with_parent(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs between fork and exec and only makes one
    // async-signal-safe prctl call.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
            Ok(())
        });
    }
}
