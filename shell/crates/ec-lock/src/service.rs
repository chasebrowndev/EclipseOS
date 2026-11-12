// SPDX-License-Identifier: AGPL-3.0-only
//! Is the PAM service installed? Asked before the lock is taken.
//!
//! Without `/etc/pam.d/ec-lock` libpam falls back to the `other` service,
//! which on Arch is `pam_deny`: every password would fail and the human would
//! be stranded behind a locker that can never say yes. So a missing file is a
//! refusal to lock, not a lock that cannot open (ADR 0095).

use std::path::{Path, PathBuf};

/// Where libpam looks, in its own order: the admin's directory, then the
/// vendor one.
pub const DIRS: [&str; 2] = ["/etc/pam.d", "/usr/lib/pam.d"];

/// The first `dir/service` that exists as a file, if any.
pub fn find<P: AsRef<Path>>(dirs: &[P], service: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.as_ref().join(service))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ec-lock-pam-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("etc")).unwrap();
        fs::create_dir_all(dir.join("usr")).unwrap();
        dir
    }

    #[test]
    fn absent_everywhere_is_none() {
        let d = scratch("absent");
        assert_eq!(find(&[d.join("etc"), d.join("usr")], "ec-lock"), None);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn either_directory_is_enough_and_etc_wins() {
        let d = scratch("either");
        let dirs = [d.join("etc"), d.join("usr")];
        fs::write(d.join("usr/ec-lock"), "auth include system-auth\n").unwrap();
        assert_eq!(find(&dirs, "ec-lock"), Some(d.join("usr/ec-lock")));
        fs::write(d.join("etc/ec-lock"), "auth include system-auth\n").unwrap();
        assert_eq!(find(&dirs, "ec-lock"), Some(d.join("etc/ec-lock")));
        fs::remove_dir_all(d).unwrap();
    }

    /// A directory with the service's name is not a service file.
    #[test]
    fn a_directory_does_not_count() {
        let d = scratch("dir");
        fs::create_dir_all(d.join("etc/ec-lock")).unwrap();
        assert_eq!(find(&[d.join("etc")], "ec-lock"), None);
        fs::remove_dir_all(d).unwrap();
    }

    /// Another service's file is not ours (`other` is the deny fallback).
    #[test]
    fn only_the_named_service_counts() {
        let d = scratch("other");
        fs::write(d.join("etc/other"), "auth required pam_deny.so\n").unwrap();
        assert_eq!(find(&[d.join("etc")], "ec-lock"), None);
        fs::remove_dir_all(d).unwrap();
    }
}
