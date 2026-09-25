// SPDX-License-Identifier: AGPL-3.0-only
//! The helper's write surface on the target, as a closed enum (D-07 §6):
//! `/etc/{hostname,locale.gen,locale.conf,localtime,fstab,pacman.conf,
//! sudoers.d/wheel}` and the bootloader's two files. The user's `abyss.kdl` is
//! written by `seed`, the disk by `sgdisk`/`mkfs`, the payload by `pacstrap`;
//! there is no other way to put a byte on the target from this crate, and no
//! function here takes a path from anywhere but this enum.

use crate::error::{io, Error, Result};
use rustix::fs::OFlags;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFile {
    Hostname,
    LocaleGen,
    LocaleConf,
    Localtime,
    Fstab,
    PacmanConf,
    SudoersWheel,
    LimineConf,
    LimineStub,
    LimineHook,
}

pub const ALL: [TargetFile; 10] = [
    TargetFile::Hostname,
    TargetFile::LocaleGen,
    TargetFile::LocaleConf,
    TargetFile::Localtime,
    TargetFile::Fstab,
    TargetFile::PacmanConf,
    TargetFile::SudoersWheel,
    TargetFile::LimineConf,
    TargetFile::LimineStub,
    TargetFile::LimineHook,
];

impl TargetFile {
    /// Relative to the target root.
    pub const fn rel(self) -> &'static str {
        match self {
            TargetFile::Hostname => "etc/hostname",
            TargetFile::LocaleGen => "etc/locale.gen",
            TargetFile::LocaleConf => "etc/locale.conf",
            TargetFile::Localtime => "etc/localtime",
            TargetFile::Fstab => "etc/fstab",
            TargetFile::PacmanConf => "etc/pacman.conf",
            TargetFile::SudoersWheel => "etc/sudoers.d/wheel",
            TargetFile::LimineConf => "boot/limine.conf",
            TargetFile::LimineStub => "boot/EFI/BOOT/BOOTX64.EFI",
            TargetFile::LimineHook => "etc/pacman.d/hooks/95-limine-esp.hook",
        }
    }

    const fn mode(self) -> u32 {
        match self {
            TargetFile::SudoersWheel => 0o440,
            _ => 0o644,
        }
    }
}

/// Where `LimineStub` comes from: the `limine` package's copy inside the target.
pub const LIMINE_STUB_SRC: &str = "usr/share/limine/BOOTX64.EFI";

const MAX_READ: u64 = 4 * 1024 * 1024;

/// Walk `rel_dir` under `target`, creating missing directories and refusing a
/// symlink or a non-directory on the way, so nothing is written through a link
/// a package (or anything else) left in the tree.
fn ensure_dirs(target: &Path, rel_dir: &Path) -> Result<PathBuf> {
    let mut cur = target.to_path_buf();
    for c in rel_dir.components() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(md) if md.is_dir() => {}
            Ok(_) => return Err(Error::Refused("target path is not a directory")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&cur).map_err(io("create target dir"))?;
            }
            Err(_) => return Err(Error::Io("stat target dir")),
        }
    }
    Ok(cur)
}

fn split(f: TargetFile) -> (&'static Path, &'static std::ffi::OsStr) {
    let rel = Path::new(f.rel());
    // Every `rel()` has a parent and a file name; the enum is closed.
    (
        rel.parent().unwrap_or(Path::new("")),
        rel.file_name().unwrap_or_default(),
    )
}

fn tmp_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut t = std::ffi::OsString::from(".");
    t.push(name);
    t.push(".eclipse-new");
    t
}

/// Replace `f` atomically: write a fresh sibling (`O_EXCL`, `O_NOFOLLOW`) and
/// rename it over the name, so a symlink already sitting at the name is replaced
/// rather than followed.
pub fn write(target: &Path, f: TargetFile, bytes: &[u8]) -> Result<()> {
    if f == TargetFile::Localtime {
        return Err(Error::Refused("localtime is a link"));
    }
    let (parent, name) = split(f);
    let dir = ensure_dirs(target, parent)?;
    let tmp = dir.join(tmp_name(name));
    let _ = fs::remove_file(&tmp);
    let mut h = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .mode(f.mode())
        .open(&tmp)
        .map_err(io("create target file"))?;
    h.set_permissions(fs::Permissions::from_mode(f.mode()))
        .map_err(io("chmod target file"))?;
    h.write_all(bytes).map_err(io("write target file"))?;
    drop(h);
    fs::rename(&tmp, dir.join(name)).map_err(io("place target file"))
}

/// `/etc/localtime` -> `/usr/share/zoneinfo/<tz>`. `tz` has already been through
/// `validate::valid_timezone`; it is re-checked here as a path component list
/// because it is about to be a link target.
pub fn link_localtime(target: &Path, tz: &str) -> Result<()> {
    if tz.is_empty()
        || !tz
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\0'))
    {
        return Err(Error::Refused("timezone"));
    }
    let (parent, name) = split(TargetFile::Localtime);
    let dir = ensure_dirs(target, parent)?;
    let tmp = dir.join(tmp_name(name));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(format!("/usr/share/zoneinfo/{tz}"), &tmp)
        .map_err(io("create localtime link"))?;
    fs::rename(&tmp, dir.join(name)).map_err(io("place localtime link"))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(io("read target file"))?;
    let mut buf = Vec::new();
    f.take(MAX_READ + 1)
        .read_to_end(&mut buf)
        .map_err(io("read target file"))?;
    if buf.len() as u64 > MAX_READ {
        return Err(Error::Io("target file too large"));
    }
    Ok(buf)
}

/// Current bytes of one of the surface's files (for the ones that are edited
/// rather than replaced). A missing file reads as empty only for `Fstab`.
pub fn read(target: &Path, f: TargetFile) -> Result<Vec<u8>> {
    match read_bounded(&target.join(f.rel())) {
        Ok(b) => Ok(b),
        Err(_) if f == TargetFile::Fstab && !target.join(f.rel()).exists() => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// The stub the `limine` package installed into the target.
pub fn read_limine_stub(target: &Path) -> Result<Vec<u8>> {
    read_bounded(&target.join(LIMINE_STUB_SRC))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::{symlink, MetadataExt};

    #[test]
    fn every_path_is_relative_inside_etc_or_boot_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for f in ALL {
            let r = Path::new(f.rel());
            assert!(r.is_relative());
            assert!(r
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))));
            assert!(r.starts_with("etc") || r.starts_with("boot"));
            assert!(seen.insert(f.rel()));
        }
    }

    #[test]
    fn writes_with_mode_creating_parents() {
        let t = TempDir::new();
        write(t.path(), TargetFile::SudoersWheel, b"%wheel ALL=(ALL:ALL) ALL\n").unwrap();
        write(t.path(), TargetFile::LimineStub, b"MZ").unwrap();
        let md = fs::metadata(t.path().join("etc/sudoers.d/wheel")).unwrap();
        assert_eq!(md.mode() & 0o777, 0o440);
        assert_eq!(
            fs::read(t.path().join("boot/EFI/BOOT/BOOTX64.EFI")).unwrap(),
            b"MZ"
        );
        // No temp file left behind.
        let names: Vec<_> = fs::read_dir(t.path().join("etc/sudoers.d")).unwrap().collect();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn a_symlink_at_the_name_is_replaced_not_followed() {
        let t = TempDir::new();
        let outside = t.path().join("outside");
        fs::write(&outside, "keep").unwrap();
        fs::create_dir_all(t.path().join("mnt/etc")).unwrap();
        let target = t.path().join("mnt");
        symlink(&outside, target.join("etc/hostname")).unwrap();
        write(&target, TargetFile::Hostname, b"eclipse\n").unwrap();
        assert_eq!(fs::read_to_string(&outside).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(target.join("etc/hostname")).unwrap(),
            "eclipse\n"
        );
    }

    #[test]
    fn a_symlinked_directory_on_the_way_is_refused() {
        let t = TempDir::new();
        let outside = t.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(t.path().join("mnt")).unwrap();
        let target = t.path().join("mnt");
        symlink(&outside, target.join("etc")).unwrap();
        assert!(write(&target, TargetFile::Hostname, b"x").is_err());
        assert!(link_localtime(&target, "UTC").is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
    }

    #[test]
    fn localtime_is_a_link_into_zoneinfo_and_refuses_traversal() {
        let t = TempDir::new();
        link_localtime(t.path(), "America/New_York").unwrap();
        assert_eq!(
            fs::read_link(t.path().join("etc/localtime")).unwrap(),
            Path::new("/usr/share/zoneinfo/America/New_York")
        );
        for bad in ["", "../x", "a/../b", "/etc/passwd", "a//b"] {
            assert!(link_localtime(t.path(), bad).is_err(), "{bad:?}");
        }
        assert!(write(t.path(), TargetFile::Localtime, b"x").is_err());
    }

    #[test]
    fn reads_are_bounded_and_do_not_follow_links() {
        let t = TempDir::new();
        assert!(read(t.path(), TargetFile::Fstab).unwrap().is_empty());
        assert!(read(t.path(), TargetFile::Hostname).is_err());
        fs::create_dir_all(t.path().join("etc")).unwrap();
        symlink("/etc/passwd", t.path().join("etc/locale.gen")).unwrap();
        assert!(read(t.path(), TargetFile::LocaleGen).is_err());
    }
}
