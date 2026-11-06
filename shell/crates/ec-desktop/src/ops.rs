// SPDX-License-Identifier: AGPL-3.0-only
//! File operations on the desktop folder.
//!
//! Two rules hold for every function here. **Nothing is overwritten**: a name
//! that is taken gets a ` (2)` suffix (` (3)`, ...), and creation and rename
//! are atomic about it (`O_EXCL`, `mkdir`, `renameat2(RENAME_NOREPLACE)`), so
//! a file that appears between the check and the act is still not clobbered.
//! **Nothing is deleted for good**: the only removal is [`trash`], a move to
//! the freedesktop trash (`crate::trash`). A move that would cross
//! filesystems is refused (`Error::CrossDevice`) rather than done as
//! copy-then-delete.
//!
//! Errors are kinds only (`Error::kind`); a name or path is never logged.

use std::ffi::CString;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::valid_name;

/// Why an operation did nothing (or stopped part way).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The name is empty, has a `/`, or is `.`/`..`.
    InvalidName,
    /// The thing to operate on is gone.
    Missing,
    /// A move or trash across filesystems.
    CrossDevice,
    /// A folder into itself.
    Inside,
    /// A special file (socket, fifo, device) the desktop will not copy.
    Unsupported,
    /// Anything else, by `io::ErrorKind`.
    Io(ErrorKind),
}

impl Error {
    pub fn kind(self) -> &'static str {
        match self {
            Error::InvalidName => "invalid-name",
            Error::Missing => "missing",
            Error::CrossDevice => "cross-device",
            Error::Inside => "inside-itself",
            Error::Unsupported => "unsupported",
            Error::Io(ErrorKind::PermissionDenied) => "permission-denied",
            Error::Io(ErrorKind::StorageFull) => "storage-full",
            Error::Io(_) => "io",
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.raw_os_error() == Some(libc::EXDEV) {
            Error::CrossDevice
        } else if e.kind() == ErrorKind::NotFound {
            Error::Missing
        } else {
            Error::Io(e.kind())
        }
    }
}

/// How many suffixes are tried before giving up.
const TRIES: u32 = 1000;

/// `name` for the n-th try: `name` itself for 1, then `stem (2).ext`,
/// `stem (3).ext`. A folder has no extension; a dotfile's leading dot is part
/// of its stem.
pub fn suffixed(name: &str, n: u32, is_dir: bool) -> String {
    if n <= 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !is_dir && !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// `rename(2)` that fails with `AlreadyExists` instead of replacing `to`.
pub fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let f = CString::new(from.as_os_str().as_bytes()).map_err(|_| ErrorKind::InvalidInput)?;
    let t = CString::new(to.as_os_str().as_bytes()).map_err(|_| ErrorKind::InvalidInput)?;
    // SAFETY: both pointers are NUL-terminated strings that outlive the call;
    // renameat2 reads them and touches nothing else of ours.
    let r = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // A filesystem or kernel without the flag: check, then rename.
        Some(libc::ENOSYS | libc::EINVAL) => {
            if to.symlink_metadata().is_ok() {
                Err(ErrorKind::AlreadyExists.into())
            } else {
                std::fs::rename(from, to)
            }
        }
        _ => Err(err),
    }
}

/// Run `make` on `parent/name`, `parent/name (2)`, ... until one does not
/// collide. Returns the path that was made.
fn first_free(
    parent: &Path,
    name: &str,
    is_dir: bool,
    mut make: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, Error> {
    for n in 1..=TRIES {
        let path = parent.join(suffixed(name, n, is_dir));
        match make(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Io(ErrorKind::AlreadyExists))
}

/// A new empty folder in `dir`.
pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, Error> {
    if !valid_name(name) {
        return Err(Error::InvalidName);
    }
    first_free(dir, name, true, |p| std::fs::create_dir(p))
}

/// A new empty file in `dir`.
pub fn new_file(dir: &Path, name: &str) -> Result<PathBuf, Error> {
    if !valid_name(name) {
        return Err(Error::InvalidName);
    }
    first_free(dir, name, false, |p| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(p)
            .map(drop)
    })
}

fn is_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Rename `path` within its folder. Renaming to the name it has is a no-op;
/// to a taken name, the suffixed one. Returns the new path.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, Error> {
    if !valid_name(new_name) {
        return Err(Error::InvalidName);
    }
    let parent = path.parent().ok_or(Error::InvalidName)?;
    if path.file_name().and_then(|n| n.to_str()) == Some(new_name) {
        return Ok(path.to_path_buf());
    }
    let dir = is_dir(path);
    first_free(parent, new_name, dir, |to| rename_noreplace(path, to))
}

/// Move `src` into the folder `dest_dir`, which is a drag onto a folder. A
/// move into the folder it is already in changes nothing.
pub fn move_to(src: &Path, dest_dir: &Path) -> Result<PathBuf, Error> {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(Error::InvalidName)?;
    if src.symlink_metadata().is_err() {
        return Err(Error::Missing);
    }
    if src.parent() == Some(dest_dir) {
        return Ok(src.to_path_buf());
    }
    if inside(src, dest_dir) {
        return Err(Error::Inside);
    }
    let dir = is_dir(src);
    first_free(dest_dir, name, dir, |to| rename_noreplace(src, to))
}

/// Copy `src` into `dest_dir` (a drag with copy, or paste). Folders copy
/// recursively; symlinks copy as links and are never followed. Copying into
/// the folder it is already in makes `name (2)`.
pub fn copy(src: &Path, dest_dir: &Path) -> Result<PathBuf, Error> {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(Error::InvalidName)?;
    let meta = std::fs::symlink_metadata(src).map_err(Error::from)?;
    if meta.is_dir() && inside(src, dest_dir) {
        return Err(Error::Inside);
    }
    first_free(dest_dir, name, meta.is_dir(), |to| copy_one(src, to))
}

/// Is `dest_dir` `src` or somewhere under it?
fn inside(src: &Path, dest_dir: &Path) -> bool {
    if !is_dir(src) {
        return false;
    }
    match (src.canonicalize(), dest_dir.canonicalize()) {
        (Ok(s), Ok(d)) => d.starts_with(s),
        _ => false,
    }
}

/// Copy one tree to `to`, which must not exist.
fn copy_one(src: &Path, to: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, to)
    } else if ft.is_dir() {
        std::fs::create_dir(to)?;
        for child in std::fs::read_dir(src)? {
            let child = child?;
            copy_one(&child.path(), &to.join(child.file_name()))?;
        }
        std::fs::set_permissions(to, meta.permissions())
    } else if ft.is_file() {
        let mut from = std::fs::File::open(src)?;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(meta.permissions().mode() & 0o777)
            .open(to)?;
        std::io::copy(&mut from, &mut out).map(drop)
    } else {
        Err(ErrorKind::Unsupported.into())
    }
}

/// Delete to the trash: the only delete there is (`crate::trash`).
pub fn trash(path: &Path) -> Result<PathBuf, Error> {
    let data = crate::trash::data_home().ok_or(Error::Missing)?;
    crate::trash::trash(&data, path)
}

/// The human's "allow launching" on a `.desktop` file: give the owner execute
/// permission, which is what `launch::plan` checks.
pub fn allow_launching(path: &Path) -> Result<(), Error> {
    let meta = std::fs::symlink_metadata(path).map_err(Error::from)?;
    if !meta.is_file() || !path.extension().is_some_and(|e| e == "desktop") {
        return Err(Error::Unsupported);
    }
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() | 0o100);
    std::fs::set_permissions(path, perms).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp;

    #[test]
    fn suffix_goes_before_the_extension_but_not_on_folders_or_dotfiles() {
        assert_eq!(suffixed("a.txt", 1, false), "a.txt");
        assert_eq!(suffixed("a.txt", 2, false), "a (2).txt");
        assert_eq!(suffixed("v1.2", 3, true), "v1.2 (3)");
        assert_eq!(suffixed(".rc", 2, false), ".rc (2)");
        assert_eq!(suffixed("plain", 2, false), "plain (2)");
    }

    #[test]
    fn creating_never_overwrites() {
        let d = tmp("create");
        let a = new_file(&d, "n.txt").unwrap();
        std::fs::write(&a, "keep").unwrap();
        let b = new_file(&d, "n.txt").unwrap();
        assert_eq!(b.file_name().unwrap(), "n (2).txt");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "keep");
        let f1 = new_folder(&d, "dir").unwrap();
        let f2 = new_folder(&d, "dir").unwrap();
        assert_ne!(f1, f2);
        assert_eq!(f2.file_name().unwrap(), "dir (2)");
        assert_eq!(new_folder(&d, "a/b"), Err(Error::InvalidName));
        assert_eq!(new_file(&d, ""), Err(Error::InvalidName));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_suffixes_a_taken_name_and_keeps_the_other_file() {
        let d = tmp("rename");
        std::fs::write(d.join("a"), "A").unwrap();
        std::fs::write(d.join("b"), "B").unwrap();
        let to = rename(&d.join("a"), "b").unwrap();
        assert_eq!(to, d.join("b (2)"));
        assert_eq!(std::fs::read_to_string(d.join("b")).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "A");
        assert_eq!(rename(&to, "b (2)").unwrap(), to);
        assert_eq!(rename(&to, "x/y"), Err(Error::InvalidName));
        assert_eq!(rename(&d.join("gone"), "z"), Err(Error::Missing));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn noreplace_refuses_an_existing_target() {
        let d = tmp("noreplace");
        std::fs::write(d.join("a"), "A").unwrap();
        std::fs::write(d.join("b"), "B").unwrap();
        let e = rename_noreplace(&d.join("a"), &d.join("b")).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(d.join("b")).unwrap(), "B");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn move_into_a_folder_and_collision_and_self() {
        let d = tmp("move");
        std::fs::create_dir(d.join("box")).unwrap();
        std::fs::write(d.join("f"), "1").unwrap();
        std::fs::write(d.join("box/f"), "0").unwrap();
        let to = move_to(&d.join("f"), &d.join("box")).unwrap();
        assert_eq!(to, d.join("box/f (2)"));
        assert_eq!(std::fs::read_to_string(d.join("box/f")).unwrap(), "0");
        assert!(!d.join("f").exists());
        // Same folder: nothing happens.
        assert_eq!(move_to(&d.join("box"), &d).unwrap(), d.join("box"));
        // A folder into itself or below.
        std::fs::create_dir(d.join("box/inner")).unwrap();
        assert_eq!(move_to(&d.join("box"), &d.join("box/inner")), Err(Error::Inside));
        assert_eq!(move_to(&d.join("box"), &d.join("box")), Err(Error::Inside));
        assert_eq!(move_to(&d.join("nope"), &d.join("box")), Err(Error::Missing));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_is_recursive_keeps_links_and_never_overwrites() {
        let d = tmp("copy");
        std::fs::create_dir_all(d.join("src/sub")).unwrap();
        std::fs::write(d.join("src/sub/x"), "X").unwrap();
        std::os::unix::fs::symlink("sub/x", d.join("src/ln")).unwrap();
        std::fs::create_dir(d.join("dest")).unwrap();
        let c = copy(&d.join("src"), &d.join("dest")).unwrap();
        assert_eq!(c, d.join("dest/src"));
        assert_eq!(std::fs::read_to_string(c.join("sub/x")).unwrap(), "X");
        assert_eq!(std::fs::read_link(c.join("ln")).unwrap(), Path::new("sub/x"));
        // Copy onto itself makes " (2)".
        let again = copy(&d.join("src"), &d).unwrap();
        assert_eq!(again, d.join("src (2)"));
        // Into itself is refused.
        assert_eq!(copy(&d.join("src"), &d.join("src/sub")), Err(Error::Inside));
        // A file copy keeps the original.
        std::fs::write(d.join("f"), "F").unwrap();
        std::fs::write(d.join("dest/f"), "OLD").unwrap();
        let f = copy(&d.join("f"), &d.join("dest")).unwrap();
        assert_eq!(f, d.join("dest/f (2)"));
        assert_eq!(std::fs::read_to_string(d.join("dest/f")).unwrap(), "OLD");
        assert_eq!(std::fs::read_to_string(f).unwrap(), "F");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn allow_launching_sets_the_owner_exec_bit_on_desktop_files_only() {
        let d = tmp("allow");
        let f = d.join("a.desktop");
        std::fs::write(&f, "[Desktop Entry]").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        allow_launching(&f).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o744);
        std::fs::write(d.join("a.txt"), "").unwrap();
        assert_eq!(allow_launching(&d.join("a.txt")), Err(Error::Unsupported));
        let _ = std::fs::remove_dir_all(&d);
    }
}
