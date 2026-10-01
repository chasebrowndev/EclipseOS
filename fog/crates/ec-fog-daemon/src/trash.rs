// SPDX-License-Identifier: AGPL-3.0-only

//! The freedesktop Trash, in-house (FOG §Filesystem backend/Trash).
//!
//! Files on the home filesystem go to `$XDG_DATA_HOME/Trash`; files on any
//! other mount go to `$topdir/.Trash-$uid`, with `$topdir` taken from
//! `/proc/self/mountinfo`. The `.trashinfo` is created `O_EXCL` and synced
//! before the file moves, so a crash never leaves a trashed file without its
//! original path. `directorysizes` is not maintained (it is an optional
//! cache in the spec).

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use fog_proto::{Kind, TrashItem};
use rustix::io::Errno;

use crate::ops::{self, is_errno};

/// A file just moved into a trash directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trashed {
    pub files: PathBuf,
    pub info: PathBuf,
}

/// A trash entry resolved from its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub files: PathBuf,
    pub info: PathBuf,
    pub original: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Trash {
    /// `$XDG_DATA_HOME/Trash`.
    home: PathBuf,
    uid: u32,
}

impl Trash {
    pub fn new(data_home: &Path) -> Self {
        Self {
            home: data_home.join("Trash"),
            uid: rustix::process::getuid().as_raw(),
        }
    }

    /// Move `path` into the trash of its filesystem.
    pub fn trash(&self, path: &Path) -> io::Result<Trashed> {
        let name = ops::name_of(path)?.as_bytes().to_vec();
        let st = ops::lstat(path)?;
        let full = fs::canonicalize(ops::parent_of(path)?)?.join(OsStr::from_bytes(&name));
        let (dir, topdir) = self.dir_for(st.st_dev, &full)?;
        for sub in ["files", "info"] {
            mkdir_private(&dir.join(sub))?;
        }
        let recorded = match &topdir {
            Some(t) => full.strip_prefix(t).unwrap_or(&full).to_path_buf(),
            None => full.clone(),
        };
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode(recorded.as_os_str().as_bytes()),
            local_date(now_s()),
        );
        for n in 1u32.. {
            let mut cand = name.clone();
            if n > 1 {
                cand.extend_from_slice(format!(".{n}").as_bytes());
            }
            let cand = OsStr::from_bytes(&cand);
            let info = dir.join("info").join(info_name(cand));
            let files = dir.join("files").join(cand);
            let mut f = match File::options()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&info)
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            };
            if let Err(e) = f.write_all(body.as_bytes()).and_then(|()| f.sync_all()) {
                let _ = fs::remove_file(&info);
                return Err(e);
            }
            match ops::rename_noreplace(path, &files) {
                Ok(()) => return Ok(Trashed { files, info }),
                Err(e) => {
                    let _ = fs::remove_file(&info);
                    if !is_errno(&e, Errno::EXIST) {
                        return Err(e);
                    }
                }
            }
        }
        unreachable!("u32 names exhausted")
    }

    /// The trash directory for a file on device `dev` at canonical `full`,
    /// and the topdir its `Path=` is relative to (`None` for home).
    fn dir_for(&self, dev: u64, full: &Path) -> io::Result<(PathBuf, Option<PathBuf>)> {
        mkdir_private(&self.home)?;
        if ops::lstat(&self.home)?.st_dev == dev {
            return Ok((self.home.clone(), None));
        }
        let top = topdir(dev, full).ok_or_else(|| io::Error::from(Errno::XDEV))?;
        let dir = top.join(format!(".Trash-{}", self.uid));
        match DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let m = fs::symlink_metadata(&dir)?;
        if !m.is_dir() || m.uid() != self.uid {
            return Err(Errno::PERM.into());
        }
        Ok((dir, Some(top)))
    }

    /// Every trash directory that may hold our items, with its topdir.
    fn dirs(&self) -> Vec<(PathBuf, Option<PathBuf>)> {
        let mut out = vec![(self.home.clone(), None)];
        for m in mounts() {
            for d in [
                m.point.join(format!(".Trash-{}", self.uid)),
                m.point.join(".Trash").join(self.uid.to_string()),
            ] {
                if d != self.home && !out.iter().any(|(o, _)| *o == d) && d.is_dir() {
                    out.push((d, Some(m.point.clone())));
                }
            }
        }
        out
    }

    /// Everything in every trash we can see (`ListTrash`).
    pub fn list(&self) -> Vec<TrashItem> {
        let mut out = Vec::new();
        for (dir, top) in self.dirs() {
            let Ok(rd) = fs::read_dir(dir.join("info")) else {
                continue;
            };
            for e in rd.flatten() {
                let fname = e.file_name();
                let Some(name) = fname.as_bytes().strip_suffix(b".trashinfo") else {
                    continue;
                };
                let files = dir.join("files").join(OsStr::from_bytes(name));
                let Ok(st) = ops::lstat(&files) else { continue };
                let Some((orig, date)) = read_info(&e.path(), top.as_deref()) else {
                    continue;
                };
                let kind = match ops::file_type(&st) {
                    rustix::fs::FileType::RegularFile => Kind::File,
                    rustix::fs::FileType::Directory => Kind::Dir,
                    rustix::fs::FileType::Symlink => Kind::Symlink,
                    _ => Kind::Other,
                };
                out.push(TrashItem {
                    id: ops::raw(&files),
                    original_path: ops::raw(&orig),
                    deleted_s: date,
                    size: (kind != Kind::Dir).then_some(st.st_size as u64),
                    kind,
                });
            }
        }
        out
    }

    /// Resolve a [`TrashItem::id`]. Refuses anything not inside a trash
    /// directory of ours.
    pub fn find(&self, id: &[u8]) -> io::Result<Found> {
        let files = ops::path_of(id);
        let name = ops::name_of(&files)?;
        let fdir = ops::parent_of(&files)?;
        let dir = ops::parent_of(fdir)?;
        let uid = self.uid.to_string();
        let top = if dir == self.home {
            None
        } else if dir.file_name() == Some(OsStr::new(&format!(".Trash-{uid}"))) {
            Some(ops::parent_of(dir)?.to_path_buf())
        } else if dir.file_name() == Some(OsStr::new(&uid))
            && dir.parent().and_then(Path::file_name) == Some(OsStr::new(".Trash"))
        {
            Some(ops::parent_of(ops::parent_of(dir)?)?.to_path_buf())
        } else {
            return Err(Errno::INVAL.into());
        };
        if fdir.file_name() != Some(OsStr::new("files")) || !ops::valid_name(name.as_bytes()) {
            return Err(Errno::INVAL.into());
        }
        let info = dir.join("info").join(info_name(name));
        let (original, _) = read_info(&info, top.as_deref()).ok_or(Errno::NOENT)?;
        ops::lstat(&files)?;
        Ok(Found {
            files: files.clone(),
            info,
            original,
        })
    }
}

fn info_name(name: &OsStr) -> PathBuf {
    let mut n = name.as_bytes().to_vec();
    n.extend_from_slice(b".trashinfo");
    ops::path_of(&n)
}

fn mkdir_private(p: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(p)
}

/// Parse a `.trashinfo`: the original absolute path and `DeletionDate`.
fn read_info(p: &Path, top: Option<&Path>) -> Option<(PathBuf, Option<i64>)> {
    let text = fs::read(p).ok()?;
    let mut in_group = false;
    let (mut path, mut date) = (None, None);
    for line in text.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(b"[") {
            in_group = line == b"[Trash Info]";
        } else if in_group {
            if let Some(v) = line.strip_prefix(b"Path=") {
                path = Some(decode(v)?);
            } else if let Some(v) = line.strip_prefix(b"DeletionDate=") {
                date = std::str::from_utf8(v).ok().and_then(parse_date);
            }
        }
    }
    let p = ops::path_of(&path?);
    let full = match top {
        Some(t) if p.is_relative() => t.join(p),
        None if p.is_relative() => return None,
        _ => p,
    };
    Some((full, date))
}

/// Percent-encode a path for `Path=`: everything but unreserved and `/`.
pub fn encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len());
    for &c in b {
        if c.is_ascii_alphanumeric() || b"-_.~/".contains(&c) {
            s.push(c as char);
        } else {
            s.push_str(&format!("%{c:02X}"));
        }
    }
    s
}

pub fn decode(b: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

pub fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `YYYY-MM-DDThh:mm:ss` in local time, as the Trash spec asks.
fn local_date(t: i64) -> String {
    // SAFETY: localtime_r writes only into `tm`, which we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tt = t as libc::time_t;
    if unsafe { libc::localtime_r(&tt, &mut tm) }.is_null() {
        return "1970-01-01T00:00:00".into();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let (d, t) = s.split_once('T')?;
    let mut d = d.split('-').map(str::parse::<i32>);
    let mut t = t.split(':').map(|x| x.get(..2).unwrap_or(x).parse::<i32>());
    // SAFETY: an all-zero tm is valid input for mktime.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = d.next()?.ok()? - 1900;
    tm.tm_mon = d.next()?.ok()? - 1;
    tm.tm_mday = d.next()?.ok()?;
    tm.tm_hour = t.next()?.ok()?;
    tm.tm_min = t.next()?.ok()?;
    tm.tm_sec = t.next()?.ok()?;
    tm.tm_isdst = -1;
    // SAFETY: mktime reads and normalises `tm`, which we own.
    let r = unsafe { libc::mktime(&mut tm) };
    (r != -1).then_some(r as i64)
}

/// One `/proc/self/mountinfo` line, reduced.
#[derive(Debug, Clone)]
pub struct Mount {
    pub dev: u64,
    pub point: PathBuf,
}

pub fn mounts() -> Vec<Mount> {
    fs::read("/proc/self/mountinfo")
        .map(|t| parse_mountinfo(&t))
        .unwrap_or_default()
}

pub fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    let mut out = Vec::new();
    for line in text.split(|&b| b == b'\n') {
        let mut f = line.split(|&b| b == b' ');
        let (Some(_), Some(_), Some(mm), Some(_), Some(point)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let Some((ma, mi)) = std::str::from_utf8(mm).ok().and_then(|s| s.split_once(':')) else {
            continue;
        };
        let (Ok(ma), Ok(mi)) = (ma.parse(), mi.parse()) else {
            continue;
        };
        out.push(Mount {
            dev: rustix::fs::makedev(ma, mi),
            point: ops::path_of(&unoctal(point)),
        });
    }
    out
}

/// mountinfo escapes space, tab, newline and backslash as `\ooo`.
fn unoctal(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Some(v) = std::str::from_utf8(&b[i + 1..i + 4])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 8).ok())
            {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// The mount point holding `full` (canonical): the longest mount point
/// prefix on device `dev`, else the longest prefix at all.
fn topdir(dev: u64, full: &Path) -> Option<PathBuf> {
    let ms = mounts();
    let best = |same: bool| {
        ms.iter()
            .filter(|m| full.starts_with(&m.point) && (!same || m.dev == dev))
            .max_by_key(|m| m.point.as_os_str().len())
            .map(|m| m.point.clone())
    };
    best(true).or_else(|| best(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_round_trip() {
        let p = b"/home/u/a b/\xff%x.txt";
        let e = encode(p);
        assert_eq!(e, "/home/u/a%20b/%FF%25x.txt");
        assert_eq!(decode(e.as_bytes()).unwrap(), p);
        assert!(decode(b"%G1").is_none());
    }

    #[test]
    fn date_round_trip() {
        let t = 1_790_000_000;
        assert_eq!(parse_date(&local_date(t)), Some(t));
    }

    #[test]
    fn mountinfo_parses_escapes() {
        let t = b"36 35 98:0 / /mnt/a\\040b rw,noatime master:1 - ext4 /dev/sda1 rw\n\
                  37 35 0:5 / /dev/shm rw - tmpfs tmpfs rw\n";
        let m = parse_mountinfo(t);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].point, Path::new("/mnt/a b"));
        assert_eq!(m[0].dev, rustix::fs::makedev(98, 0));
        assert_eq!(m[1].point, Path::new("/dev/shm"));
    }

    #[test]
    fn find_refuses_foreign_ids() {
        let d = tempfile::tempdir().unwrap();
        let t = Trash::new(d.path());
        assert!(t.find(b"/etc/passwd").is_err());
        assert!(t.find(b"/tmp/files/x").is_err());
    }

    #[test]
    fn trash_writes_info_first_and_unique_names() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path().join("data");
        let t = Trash::new(&data);
        let w = d.path().join("w");
        fs::create_dir(&w).unwrap();
        let f = w.join("a b.txt");
        fs::write(&f, b"1").unwrap();
        let one = t.trash(&f).unwrap();
        fs::write(&f, b"2").unwrap();
        let two = t.trash(&f).unwrap();
        assert_ne!(one.files, two.files);
        assert!(two.files.ends_with("a b.txt.2"));
        let info = fs::read_to_string(&one.info).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"));
        assert!(info.contains("a%20b.txt\nDeletionDate="));
        let found = t.find(&ops::raw(&two.files)).unwrap();
        assert_eq!(
            found.original,
            fs::canonicalize(&w).unwrap().join("a b.txt")
        );
        assert!(!f.exists());
        let l: Vec<_> = t
            .list()
            .into_iter()
            .filter(|i| i.id.starts_with(data.as_os_str().as_bytes()))
            .collect();
        assert_eq!(l.len(), 2);
        assert!(l.iter().all(|i| i.kind == Kind::File && i.size == Some(1)));
    }
}
