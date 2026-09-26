// SPDX-License-Identifier: AGPL-3.0-only
//! Spawning and killing widget commands.
//!
//! Every widget command is argv-exec (no shell), in its own process group,
//! with `PR_SET_PDEATHSIG(SIGTERM)` so it dies with the thread that spawned
//! it, stdin and stderr on `/dev/null`. Killing signals the whole group, so a
//! `sh -c '… &'` fixture's background job goes too.

use std::io;
use std::os::raw::c_int;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

// libc is linked by std on every unix target; the crate adds no `libc`
// dependency just for these three calls.
extern "C" {
    fn prctl(option: c_int, ...) -> c_int;
    fn getppid() -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

const PR_SET_PDEATHSIG: c_int = 1;
const SIGKILL: c_int = 9;
const SIGTERM: c_int = 15;
const ESRCH: i32 = 3;

/// Spawns `argv` as a widget command. stdout is piped when `piped`.
pub(super) fn spawn(argv: &[String], piped: bool) -> io::Result<Child> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let parent = std::process::id() as c_int;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(if piped { Stdio::piped() } else { Stdio::null() })
        // stderr is discarded unread: it is the command's text, and we never
        // log that.
        .stderr(Stdio::null())
        .process_group(0);
    // SAFETY: the closure runs between fork and exec and calls only
    // async-signal-safe functions (prctl, getppid), allocating nothing.
    unsafe {
        cmd.pre_exec(move || {
            if prctl(PR_SET_PDEATHSIG, SIGTERM as std::os::raw::c_ulong) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The parent may have died before prctl took effect; then the
            // signal will never come, so refuse to run orphaned.
            if getppid() != parent {
                return Err(io::Error::from_raw_os_error(ESRCH));
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// SIGKILLs `child`'s process group and reaps `child`. Call only while
/// `child` is unreaped, so its pid still names its group.
pub(super) fn kill_and_reap(child: &mut Child) {
    kill_group(child);
    let _ = child.wait();
}

/// SIGKILLs `child`'s process group (the child leads it; see [`spawn`]).
pub(super) fn kill_group(child: &Child) {
    let Ok(pid) = c_int::try_from(child.id()) else {
        return;
    };
    // SAFETY: plain syscall wrapper; a stale or empty group gives ESRCH.
    unsafe {
        kill(-pid, SIGKILL);
    }
}

/// Fire-and-forget `argv` (a click or scroll action). Detached: its own
/// process group, no parent-death signal and no stdio, so reloading the bar
/// does not take down what the user just launched. A reaper thread waits on
/// it so it never lingers as a zombie; the caller never blocks.
pub(super) fn run_detached(argv: &[String]) -> io::Result<()> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    std::thread::Builder::new()
        .name("eclipse-custom-reap".into())
        .spawn(move || {
            let _ = child.wait();
        })?;
    Ok(())
}
