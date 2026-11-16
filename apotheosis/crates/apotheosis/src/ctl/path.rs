// SPDX-License-Identifier: AGPL-3.0-only
//! Where the socket lives (T-12 §1): `$XDG_RUNTIME_DIR/apotheosis/<instance>.sock`, the
//! directory `0700` and ours, the socket `0600`. No TCP, no abstract namespace.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// `$XDG_RUNTIME_DIR/apotheosis`, or `None` without an absolute runtime dir.
#[must_use]
pub(crate) fn socket_dir(runtime: Option<&Path>) -> Option<PathBuf> {
    runtime
        .filter(|p| p.is_absolute())
        .map(|p| p.join("apotheosis"))
}

/// Create the directory `0700` if missing; refuse one that is not ours or has group or
/// other bits.
pub(crate) fn ensure_dir(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let md = fs::symlink_metadata(dir)?;
    if !md.is_dir() {
        return Err(io::Error::other("socket directory is not a directory"));
    }
    if md.uid() != rustix::process::getuid().as_raw() {
        return Err(io::Error::other("socket directory belongs to another user"));
    }
    if md.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "socket directory is open to group or others",
        ));
    }
    Ok(())
}

/// Bind `<dir>/<instance>.sock` (`0600`), non-blocking. A stale socket of the same name
/// (no listener behind it) is replaced.
pub(crate) fn bind(dir: &Path, instance: &str) -> io::Result<(UnixListener, PathBuf)> {
    ensure_dir(dir)?;
    let path = dir.join(format!("{instance}.sock"));
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "another instance owns this socket",
            ));
        }
        fs::remove_file(&path)?;
    }
    let l = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    l.set_nonblocking(true)?;
    Ok((l, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apoth-ctl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn dir_is_private_and_checked() {
        assert_eq!(socket_dir(Some(Path::new("rel"))), None);
        let base = tmp("dir");
        fs::create_dir_all(&base).unwrap();
        let d = base.join("apotheosis");
        ensure_dir(&d).unwrap();
        assert_eq!(fs::metadata(&d).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(ensure_dir(&d).is_err());
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        let (l, p) = bind(&d, "1").unwrap();
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        // A live listener is not stolen; a stale one is replaced.
        assert!(bind(&d, "1").is_err());
        drop(l);
        assert!(bind(&d, "1").is_ok());
        let _ = fs::remove_dir_all(&base);
    }
}
