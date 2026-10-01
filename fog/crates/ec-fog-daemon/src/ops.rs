// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem primitives for the job queue (FOG §Filesystem backend/
//! LocalBackend, §File operations). Nothing here overwrites: renames are
//! `RENAME_NOREPLACE` and creations are `O_EXCL`.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use rustix::fs::{
    self as rfs, AtFlags, FileType, RenameFlags, SeekFrom, Timespec, Timestamps, CWD,
};
use rustix::io::Errno;

/// Chunk size for copies; pause and cancel are checked between chunks.
pub const CHUNK: usize = 8 << 20;

/// `lstat`: never follows a final symlink.
pub fn lstat(p: &Path) -> io::Result<rfs::Stat> {
    Ok(rfs::lstat(p)?)
}

pub fn file_type(st: &rfs::Stat) -> FileType {
    FileType::from_raw_mode(st.st_mode)
}

pub fn is_dir(st: &rfs::Stat) -> bool {
    file_type(st) == FileType::Directory
}

pub fn mtime_ns(st: &rfs::Stat) -> i128 {
    i128::from(st.st_mtime) * 1_000_000_000 + i128::from(st.st_mtime_nsec)
}

/// `renameat2(RENAME_NOREPLACE)`: `EEXIST` if `to` exists, never replaces.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    Ok(rfs::renameat_with(
        CWD,
        from,
        CWD,
        to,
        RenameFlags::NOREPLACE,
    )?)
}

pub fn is_errno(e: &io::Error, n: Errno) -> bool {
    e.raw_os_error() == Some(n.raw_os_error())
}

/// `name (n).ext`; a leading dot is not an extension, directories have none.
pub fn keep_both_name(name: &[u8], n: u32, dir: bool) -> Vec<u8> {
    let split = if dir {
        None
    } else {
        name.iter().rposition(|&b| b == b'.').filter(|&i| i > 0)
    };
    let (base, ext) = match split {
        Some(i) => name.split_at(i),
        None => (name, &b""[..]),
    };
    let mut out = base.to_vec();
    out.extend_from_slice(format!(" ({n})").as_bytes());
    out.extend_from_slice(ext);
    out
}

/// Bytes and non-directory entries under `p` (itself included).
pub fn tree_size(p: &Path) -> (u64, u64) {
    let Ok(st) = lstat(p) else { return (0, 0) };
    if !is_dir(&st) {
        return (st.st_size as u64, 1);
    }
    let mut acc = (0, 0);
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let (b, f) = tree_size(&e.path());
            acc.0 += b;
            acc.1 += f;
        }
    }
    acc
}

/// Remove `p` and, if it is a real directory, everything under it.
/// Symlinks are removed, never followed.
pub fn remove_tree(p: &Path) -> io::Result<()> {
    if is_dir(&lstat(p)?) {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// Whether `inner` is `outer` or below it, by canonical path.
pub fn is_within(inner: &Path, outer: &Path) -> bool {
    match (fs::canonicalize(inner), fs::canonicalize(outer)) {
        (Ok(i), Ok(o)) => i.starts_with(o),
        _ => false,
    }
}

pub fn name_of(p: &Path) -> io::Result<&OsStr> {
    p.file_name().ok_or_else(|| Errno::INVAL.into())
}

pub fn parent_of(p: &Path) -> io::Result<&Path> {
    p.parent()
        .filter(|q| !q.as_os_str().is_empty())
        .ok_or_else(|| Errno::INVAL.into())
}

/// A single, real path component.
pub fn valid_name(n: &[u8]) -> bool {
    !n.is_empty() && n != b"." && n != b".." && !n.contains(&b'/') && !n.contains(&0)
}

pub fn path_of(raw: &[u8]) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(raw))
}

pub fn raw(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

/// Copy `len` bytes of `src` into the freshly created `dst`, preserving
/// holes (`SEEK_DATA`/`SEEK_HOLE`) and using `copy_file_range` with a
/// buffered fallback. `chunk` runs after every chunk with the bytes copied
/// and may abort the copy by returning an error.
pub fn copy_data(
    src: &File,
    dst: &File,
    len: u64,
    chunk: &mut dyn FnMut(u64) -> io::Result<()>,
) -> io::Result<()> {
    let mut pos = 0u64;
    let mut cfr = true;
    let mut buf = Vec::new();
    while pos < len {
        let data = match rfs::seek(src, SeekFrom::Data(pos)) {
            Ok(d) => d,
            Err(Errno::NXIO) => break, // only a hole remains
            Err(Errno::INVAL) => pos,  // no hole support: all data
            Err(e) => return Err(e.into()),
        };
        let end = match rfs::seek(src, SeekFrom::Hole(data)) {
            Ok(h) => h.min(len),
            Err(_) => len,
        };
        let mut off = data;
        while off < end {
            let want = ((end - off) as usize).min(CHUNK);
            let n = if cfr {
                let (mut i, mut o) = (off, off);
                match rfs::copy_file_range(src, Some(&mut i), dst, Some(&mut o), want) {
                    Ok(n) => n,
                    Err(Errno::XDEV | Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {
                        cfr = false;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
            } else {
                buf.resize(want.min(1 << 20), 0);
                let mut done = 0;
                while done < want {
                    let k = (want - done).min(buf.len());
                    let r = src.read_at(&mut buf[..k], off + done as u64)?;
                    if r == 0 {
                        break;
                    }
                    dst.write_all_at(&buf[..r], off + done as u64)?;
                    done += r;
                }
                done
            };
            if n == 0 {
                // The source shrank underneath us.
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source shrank during copy",
                ));
            }
            off += n as u64;
            chunk(n as u64)?;
        }
        pos = end;
    }
    dst.set_len(len)?;
    Ok(())
}

/// Set a node's atime/mtime from `st` without following symlinks.
pub fn set_times(p: &Path, st: &rfs::Stat) -> io::Result<()> {
    let ts = Timestamps {
        last_access: Timespec {
            tv_sec: st.st_atime,
            tv_nsec: st.st_atime_nsec as _,
        },
        last_modification: Timespec {
            tv_sec: st.st_mtime,
            tv_nsec: st.st_mtime_nsec as _,
        },
    };
    Ok(rfs::utimensat(CWD, p, &ts, AtFlags::SYMLINK_NOFOLLOW)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_both_names() {
        assert_eq!(keep_both_name(b"a.txt", 2, false), b"a (2).txt");
        assert_eq!(keep_both_name(b".bashrc", 2, false), b".bashrc (2)");
        assert_eq!(keep_both_name(b"a.b.c", 3, false), b"a.b (3).c");
        assert_eq!(keep_both_name(b"dir.d", 2, true), b"dir.d (2)");
    }

    #[test]
    fn noreplace_refuses() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::write(&a, b"A").unwrap();
        fs::write(&b, b"B").unwrap();
        let e = rename_noreplace(&a, &b).unwrap_err();
        assert!(is_errno(&e, Errno::EXIST));
        assert_eq!(fs::read(&b).unwrap(), b"B");
        assert_eq!(fs::read(&a).unwrap(), b"A");
    }

    #[test]
    fn sparse_copy_keeps_holes() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        let f = File::create(&a).unwrap();
        let len = 64u64 << 20;
        f.set_len(len).unwrap();
        f.write_all_at(b"head", 0).unwrap();
        f.write_all_at(b"tail", len - 4).unwrap();
        let src = File::open(&a).unwrap();
        let dst = File::options()
            .write(true)
            .create_new(true)
            .open(&b)
            .unwrap();
        let mut copied = 0;
        copy_data(&src, &dst, len, &mut |n| {
            copied += n;
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
        let sb = lstat(&b).unwrap();
        assert_eq!(sb.st_size as u64, len);
        // Only the data extents are copied when holes are reported.
        if copied < len {
            assert!((sb.st_blocks as u64) * 512 < len);
        }
    }
}
