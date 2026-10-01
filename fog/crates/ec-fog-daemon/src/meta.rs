// SPDX-License-Identifier: AGPL-3.0-only

//! Phase-2 metadata: `statx` per entry, relative to the directory fd, never
//! following symlinks (FOG §Performance model, technique 1).
//!
//! Plain `statx` on the blocking pool; io_uring batching is not needed while
//! fog-bench stays inside its budgets.

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

use fog_proto::{Entry, Kind};
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags, Statx, StatxFlags, CWD};

/// Entries in the first phase-2 batch: enough for any visible window, so
/// the rows on screen get their metadata first.
pub const FIRST: usize = 256;

const MASK: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::MODE)
    .union(StatxFlags::SIZE)
    .union(StatxFlags::MTIME);

fn mtime(st: &Statx) -> i128 {
    i128::from(st.stx_mtime.tv_sec) * 1_000_000_000 + i128::from(st.stx_mtime.tv_nsec)
}

/// The directory's own mtime, with one `statx` (following a symlinked path,
/// as listing does).
pub fn dir_mtime(path: &Path) -> io::Result<i128> {
    Ok(mtime(&rfs::statx(
        CWD,
        path,
        AtFlags::empty(),
        StatxFlags::MTIME,
    )?))
}

/// An fd for `statx` relative lookups in `path`.
pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    Ok(rfs::open(
        path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

/// `e` with size, mtime and mode filled from `statx` on `dir/e.name`. An
/// `Unknown` kind (no `d_type`) is resolved. `None` if the entry is gone or
/// cannot be stat'ed; the watcher or the next rescan deals with it.
pub fn fill(dir: &OwnedFd, e: &Entry) -> Option<Entry> {
    let st = rfs::statx(dir, e.name.as_slice(), AtFlags::SYMLINK_NOFOLLOW, MASK).ok()?;
    let mode = u32::from(st.stx_mode);
    let kind = match e.kind {
        Kind::Unknown => match FileType::from_raw_mode(mode) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Dir,
            FileType::Symlink => Kind::Symlink,
            _ => Kind::Other,
        },
        k => k,
    };
    Some(Entry {
        name: e.name.clone(),
        kind,
        size: Some(st.stx_size),
        mtime_ns: Some(mtime(&st)),
        mode: Some(mode),
    })
}

/// Whether phase 2 still owes this entry its metadata.
pub fn missing(e: &Entry) -> bool {
    e.mode.is_none()
}

/// Indices of `entries` lacking metadata, in display `order`: visible rows
/// (the top of the order) come first.
pub fn todo(entries: &[Entry], order: &[u32]) -> Vec<usize> {
    order
        .iter()
        .map(|&i| i as usize)
        .filter(|&i| entries.get(i).is_some_and(missing))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn fills_without_following_symlinks() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("f"), b"hello").unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        symlink("f", d.path().join("l")).unwrap();
        let fd = open_dir(d.path()).unwrap();

        let f = fill(&fd, &Entry::new(b"f".to_vec(), Kind::File)).unwrap();
        assert_eq!(f.size, Some(5));
        assert_eq!(f.mode.unwrap() & 0o170000, 0o100000);
        assert!(f.mtime_ns.unwrap() > 0);
        let l = fill(&fd, &Entry::new(b"l".to_vec(), Kind::Symlink)).unwrap();
        assert_eq!(l.mode.unwrap() & 0o170000, 0o120000);
        assert_eq!(l.size, Some(1));
        let s = fill(&fd, &Entry::new(b"sub".to_vec(), Kind::Unknown)).unwrap();
        assert_eq!(s.kind, Kind::Dir);
        assert!(fill(&fd, &Entry::new(b"gone".to_vec(), Kind::File)).is_none());
        assert!(dir_mtime(d.path()).unwrap() > 0);
    }

    #[test]
    fn todo_follows_display_order() {
        let mut v = vec![
            Entry::new(b"a".to_vec(), Kind::File),
            Entry::new(b"b".to_vec(), Kind::File),
            Entry::new(b"c".to_vec(), Kind::File),
        ];
        v[1].mode = Some(0o100644);
        assert_eq!(todo(&v, &[2, 1, 0]), [2, 0]);
    }
}
