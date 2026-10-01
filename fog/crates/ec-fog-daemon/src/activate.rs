// SPDX-License-Identifier: AGPL-3.0-only

//! systemd socket activation for fogd (FOG §Architecture: Lifecycle).
//! sd_listen_fds(3) in a few lines instead of a dependency.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixListener;

/// First fd systemd passes (`SD_LISTEN_FDS_START`).
const LISTEN_FDS_START: i32 = 3;

/// Whether `LISTEN_PID`/`LISTEN_FDS` hand this process (`pid`) at least one
/// socket.
fn activated(listen_pid: Option<&str>, listen_fds: Option<&str>, pid: u32) -> bool {
    listen_pid.and_then(|p| p.parse::<u32>().ok()) == Some(pid)
        && listen_fds.and_then(|n| n.parse::<u32>().ok()).unwrap_or(0) >= 1
}

/// The listening socket systemd passed on fd 3 (fogd.socket), if any. The
/// `LISTEN_*` variables are cleared so children don't inherit them. Call
/// before spawning threads; wrap with `tokio::net::UnixListener::from_std`.
pub fn inherited_listener() -> io::Result<Option<UnixListener>> {
    let var = |k| std::env::var(k).ok();
    let ok = activated(
        var("LISTEN_PID").as_deref(),
        var("LISTEN_FDS").as_deref(),
        std::process::id(),
    );
    for k in ["LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES"] {
        std::env::remove_var(k);
    }
    if !ok {
        return Ok(None);
    }
    // SAFETY: systemd hands fd 3 to this process (LISTEN_PID matched) and
    // nothing else in fogd opens or owns it; ownership is taken exactly once
    // because the variables were just cleared.
    let fd = unsafe { OwnedFd::from_raw_fd(LISTEN_FDS_START) };
    if !rustix::fs::FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode).is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LISTEN_FDS fd 3 is not a socket",
        ));
    }
    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
    let listener = UnixListener::from(fd);
    listener.set_nonblocking(true)?;
    Ok(Some(listener))
}

#[cfg(test)]
mod tests {
    use super::activated;

    #[test]
    fn socket_activation_needs_our_pid_and_a_fd() {
        assert!(activated(Some("42"), Some("1"), 42));
        assert!(!activated(Some("41"), Some("1"), 42));
        assert!(!activated(Some("42"), Some("0"), 42));
        assert!(!activated(None, Some("1"), 42));
        assert!(!activated(Some("42"), None, 42));
        assert!(!activated(Some("x"), Some("1"), 42));
    }
}
