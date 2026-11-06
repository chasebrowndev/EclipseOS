// SPDX-License-Identifier: AGPL-3.0-only
//! Delete to the trash, per the freedesktop trash specification: the file
//! moves to `$XDG_DATA_HOME/Trash/files/<name>` and
//! `Trash/info/<name>.trashinfo` records its `Path` (percent-encoded) and
//! `DeletionDate` (local time, `YYYY-MM-DDThh:mm:ss`).
//!
//! The home trash only: a file on another filesystem is refused with
//! `Error::CrossDevice` rather than moved across (that is a copy and a
//! delete). There is no permanent delete here or anywhere in the crate.

use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::ops::{rename_noreplace, suffixed, Error};

/// `$XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// Percent-encode a path for `Path=`: every byte but unreserved characters
/// and `/`.
pub fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::new();
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The text of a `.trashinfo` file.
pub fn trashinfo(original: &Path, date: &str) -> String {
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        encode_path(original)
    )
}

/// `YYYY-MM-DDThh:mm:ss` from broken-down local time (`tm_year` is years since
/// 1900 and `tm_mon` is 0-based, as in C).
pub fn format_date(year: i32, mon: i32, day: i32, hour: i32, min: i32, sec: i32) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        year + 1900,
        mon + 1,
        day,
        hour,
        min,
        sec
    )
}

/// Now, in local time.
pub fn local_date() -> String {
    // SAFETY: `time` accepts a null out-pointer; `localtime_r` writes only to
    // the `tm` we hand it, which is zero-initialised plain data.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format_date(
        tm.tm_year, tm.tm_mon, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec,
    )
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Move `path` to the trash under `data_home`. Returns the path it has in
/// `Trash/files`.
pub fn trash(data_home: &Path, path: &Path) -> Result<PathBuf, Error> {
    // The original's absolute path, without following a final symlink: the
    // link is what is trashed.
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(Error::InvalidName)?;
    let meta = std::fs::symlink_metadata(path).map_err(Error::from)?;
    let parent = path
        .parent()
        .unwrap_or(Path::new("/"))
        .canonicalize()
        .map_err(Error::from)?;
    let original = parent.join(name);

    let root = data_home.join("Trash");
    let (files, info) = (root.join("files"), root.join("info"));
    private_dir(&files).map_err(Error::from)?;
    private_dir(&info).map_err(Error::from)?;

    let date = local_date();
    for n in 1..=1000 {
        let candidate = suffixed(name, n, meta.is_dir());
        let info_path = info.join(format!("{candidate}.trashinfo"));
        // The info file is created first and exclusively: it reserves the name.
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        };
        let written = f.write_all(trashinfo(&original, &date).as_bytes());
        let dest = files.join(&candidate);
        let moved = written.and_then(|()| rename_noreplace(path, &dest));
        match moved {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // A file of that name is in files/ with no info: leave it, next name.
                let _ = std::fs::remove_file(&info_path);
                continue;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&info_path);
                return Err(e.into());
            }
        }
    }
    Err(Error::Io(ErrorKind::AlreadyExists))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp;

    #[test]
    fn path_encoding_keeps_slashes_and_escapes_the_rest() {
        assert_eq!(
            encode_path(Path::new("/home/u/a b%é.txt")),
            "/home/u/a%20b%25%C3%A9.txt"
        );
    }

    #[test]
    fn date_is_iso_local_without_zone() {
        assert_eq!(format_date(126, 10, 5, 9, 3, 7), "2026-11-05T09:03:07");
        let now = local_date();
        assert_eq!(now.len(), 19);
        assert_eq!(&now[10..11], "T");
    }

    #[test]
    fn trashing_moves_the_file_and_writes_the_info() {
        let data = tmp("trash-data");
        let desk = tmp("trash-desk");
        std::fs::write(desk.join("old note.txt"), "bye").unwrap();
        let dest = trash(&data, &desk.join("old note.txt")).unwrap();
        assert_eq!(dest, data.join("Trash/files/old note.txt"));
        assert!(!desk.join("old note.txt").exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "bye");
        let info = std::fs::read_to_string(data.join("Trash/info/old note.txt.trashinfo")).unwrap();
        let mut lines = info.lines();
        assert_eq!(lines.next(), Some("[Trash Info]"));
        let want = format!(
            "Path={}/old%20note.txt",
            encode_path(&desk.canonicalize().unwrap())
        );
        assert_eq!(lines.next(), Some(want.as_str()));
        assert!(lines.next().unwrap().starts_with("DeletionDate=20"));
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&desk);
    }

    #[test]
    fn a_second_file_of_the_same_name_does_not_replace_the_first() {
        let data = tmp("trash2-data");
        let desk = tmp("trash2-desk");
        std::fs::write(desk.join("a.txt"), "1").unwrap();
        let one = trash(&data, &desk.join("a.txt")).unwrap();
        std::fs::write(desk.join("a.txt"), "2").unwrap();
        let two = trash(&data, &desk.join("a.txt")).unwrap();
        assert_eq!(two, data.join("Trash/files/a (2).txt"));
        assert_eq!(std::fs::read_to_string(one).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(two).unwrap(), "2");
        assert!(data.join("Trash/info/a (2).txt.trashinfo").exists());
        assert_eq!(trash(&data, &desk.join("gone")), Err(Error::Missing));
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&desk);
    }

    #[test]
    fn a_symlink_is_trashed_not_its_target() {
        let data = tmp("trash3-data");
        let desk = tmp("trash3-desk");
        std::fs::write(desk.join("real"), "r").unwrap();
        std::os::unix::fs::symlink(desk.join("real"), desk.join("ln")).unwrap();
        trash(&data, &desk.join("ln")).unwrap();
        assert!(desk.join("real").exists());
        assert!(data.join("Trash/files/ln").symlink_metadata().is_ok());
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&desk);
    }
}
