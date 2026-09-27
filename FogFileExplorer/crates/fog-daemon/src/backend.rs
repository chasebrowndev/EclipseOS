// SPDX-License-Identifier: AGPL-3.0-only

//! Filesystem backends (FOG §Filesystem backend). M0 has `list` and `stat`
//! on [`LocalBackend`] only.

use std::io;
use std::path::Path;

use fog_proto::{Entry, Kind, StatReply};
use rustix::fs::{self as rfs, AtFlags, FileType, Mode, OFlags, RawDir, StatxFlags, CWD};

/// Entries per streamed batch.
pub const BATCH: usize = 4096;

/// `getdents64` buffer size.
const DENTS_BUF: usize = 256 << 10;

/// Filesystem access. Implementations are called from blocking threads.
pub trait Backend: Send + Sync {
    /// List `path`. Every full batch of [`BATCH`] entries goes to `batch` as
    /// soon as it is read; the remainder (fewer than `BATCH`) is returned.
    fn list(&self, path: &Path, batch: &mut dyn FnMut(Vec<Entry>)) -> io::Result<Vec<Entry>>;

    /// Metadata for `path` itself; symlinks are not followed.
    fn stat(&self, path: &Path) -> io::Result<StatReply>;
}

/// Local paths via raw syscalls: `getdents64` and `statx`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalBackend;

fn kind_of(t: FileType) -> Kind {
    match t {
        FileType::RegularFile => Kind::File,
        FileType::Directory => Kind::Dir,
        FileType::Symlink => Kind::Symlink,
        FileType::Unknown => Kind::Unknown,
        _ => Kind::Other,
    }
}

impl Backend for LocalBackend {
    fn list(&self, path: &Path, batch: &mut dyn FnMut(Vec<Entry>)) -> io::Result<Vec<Entry>> {
        let fd = rfs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut buf: Vec<u8> = Vec::with_capacity(DENTS_BUF);
        let mut dir = RawDir::new(&fd, buf.spare_capacity_mut());
        let mut cur = Vec::with_capacity(BATCH);
        while let Some(ent) = dir.next() {
            let ent = ent?;
            let name = ent.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            cur.push(Entry {
                name: name.to_vec(),
                kind: kind_of(ent.file_type()),
            });
            if cur.len() == BATCH {
                batch(std::mem::replace(&mut cur, Vec::with_capacity(BATCH)));
            }
        }
        Ok(cur)
    }

    fn stat(&self, path: &Path) -> io::Result<StatReply> {
        let st = rfs::statx(
            CWD,
            path,
            AtFlags::SYMLINK_NOFOLLOW,
            StatxFlags::TYPE | StatxFlags::SIZE | StatxFlags::MTIME | StatxFlags::INO,
        )?;
        Ok(StatReply {
            path: path.as_os_str().as_encoded_bytes().to_vec(),
            kind: kind_of(FileType::from_raw_mode(st.stx_mode.into())),
            size: st.stx_size,
            mtime_ns: i128::from(st.stx_mtime.tv_sec) * 1_000_000_000
                + i128::from(st.stx_mtime.tv_nsec),
            inode: st.stx_ino,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn batches_and_kinds() {
        let d = tempfile::tempdir().unwrap();
        let n = BATCH * 2 + 17;
        for i in 0..n {
            fs::write(d.path().join(format!("f{i}")), b"").unwrap();
        }
        fs::create_dir(d.path().join("sub")).unwrap();
        symlink("f0", d.path().join("link")).unwrap();

        let mut batches = Vec::new();
        let rest = LocalBackend
            .list(d.path(), &mut |b| batches.push(b))
            .unwrap();
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|b| b.len() == BATCH));
        assert_eq!(rest.len(), n + 2 - 2 * BATCH);

        let all: Vec<Entry> = batches.into_iter().flatten().chain(rest).collect();
        assert_eq!(all.len(), n + 2);
        let kind = |name: &[u8]| all.iter().find(|e| e.name == name).unwrap().kind;
        assert_eq!(kind(b"f0"), Kind::File);
        assert_eq!(kind(b"sub"), Kind::Dir);
        assert_eq!(kind(b"link"), Kind::Symlink);
        assert!(!all.iter().any(|e| e.name == b"." || e.name == b".."));
    }

    #[test]
    fn small_dir_has_no_batches() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), b"").unwrap();
        let mut called = false;
        let rest = LocalBackend.list(d.path(), &mut |_| called = true).unwrap();
        assert!(!called);
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn list_errors() {
        let d = tempfile::tempdir().unwrap();
        let e = LocalBackend
            .list(&d.path().join("nope"), &mut |_| {})
            .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(2));
        fs::write(d.path().join("f"), b"").unwrap();
        let e = LocalBackend
            .list(&d.path().join("f"), &mut |_| {})
            .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(20)); // ENOTDIR
    }

    #[test]
    fn stat_does_not_follow_symlinks() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"hello").unwrap();
        let l = d.path().join("l");
        symlink(&f, &l).unwrap();

        let sf = LocalBackend.stat(&f).unwrap();
        assert_eq!(sf.kind, Kind::File);
        assert_eq!(sf.size, 5);
        assert!(sf.mtime_ns > 0);
        let sl = LocalBackend.stat(&l).unwrap();
        assert_eq!(sl.kind, Kind::Symlink);
        assert_ne!(sl.inode, sf.inode);
        assert_eq!(sl.size, f.as_os_str().len() as u64);
        assert_eq!(LocalBackend.stat(d.path()).unwrap().kind, Kind::Dir);
    }
}
