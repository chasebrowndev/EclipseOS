// SPDX-License-Identifier: AGPL-3.0-only

//! Spawning children that inherit one end of a socketpair.

use std::ffi::OsString;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::process::Stdio;
use std::sync::Mutex;

use rustix::io::{fcntl_setfd, FdFlags};
use tokio::process::{Child, Command};

/// Rust opens fds CLOEXEC; the child's end must survive exec. Hold this so a
/// concurrent spawn cannot inherit it too.
static SPAWN: Mutex<()> = Mutex::new(());

/// Start `program` with `args(fd)` where `fd` is the child's end of a fresh
/// socketpair. Returns the child (killed if dropped) and our end, already
/// non-blocking. Must be called inside the tokio runtime.
pub fn spawn_with_fd(
    program: &str,
    args: impl FnOnce(i32) -> Vec<OsString>,
) -> std::io::Result<(Child, StdUnixStream)> {
    let (mine, theirs) = StdUnixStream::pair()?;
    let _guard = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
    fcntl_setfd(&theirs, FdFlags::empty())?;
    mine.set_nonblocking(true)?;
    let child = Command::new(program)
        .args(args(theirs.as_raw_fd()))
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    drop(theirs);
    Ok((child, mine))
}
