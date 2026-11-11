// SPDX-License-Identifier: AGPL-3.0-only
//! One locker at a time.
//!
//! The idle timeout, the lock bind and `loginctl lock-session` can all fire at
//! once. A second `ext_session_lock_v1` while one is held gains nothing and,
//! with abyss today, the first unlock would clear both lockers' surfaces. So
//! the locker holds an `flock` on `$XDG_RUNTIME_DIR/ec-lock.lock` for its whole
//! life; a second instance finds it held and exits without locking anything.
//!
//! The kernel drops the lock when the holder dies, however it dies, so a
//! crashed locker never blocks the one started to recover from it.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Held for the process's lifetime. Dropping it releases the claim.
pub struct Claim {
    _file: Option<File>,
}

pub enum Outcome {
    /// We are the locker.
    Claimed(Claim),
    /// Another live locker holds the session.
    Running,
}

/// The lock file, or `None` without `XDG_RUNTIME_DIR`.
pub fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("ec-lock.lock"))
}

/// Claim the session. With no runtime directory, or a lock file that cannot be
/// opened, the claim is skipped rather than refused: failing to lock the
/// screen is worse than a second locker.
pub fn claim() -> Outcome {
    match path() {
        Some(p) => claim_at(&p),
        None => Outcome::Claimed(Claim { _file: None }),
    }
}

pub fn claim_at(path: &Path) -> Outcome {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
    else {
        eprintln!(
            "ec-lock: cannot open {}; running without the one-locker guard",
            path.display()
        );
        return Outcome::Claimed(Claim { _file: None });
    };
    // SAFETY: a valid fd we own; flock does not touch memory.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Outcome::Claimed(Claim { _file: Some(file) });
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
        return Outcome::Running;
    }
    eprintln!(
        "ec-lock: cannot lock {}; running without the one-locker guard",
        path.display()
    );
    Outcome::Claimed(Claim { _file: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_claim_is_refused_until_the_first_is_dropped() {
        let dir = std::env::temp_dir().join(format!("ec-lock-instance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ec-lock.lock");
        let first = claim_at(&p);
        assert!(matches!(first, Outcome::Claimed(Claim { _file: Some(_) })));
        assert!(matches!(claim_at(&p), Outcome::Running));
        drop(first);
        assert!(matches!(claim_at(&p), Outcome::Claimed(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
