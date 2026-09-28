// SPDX-License-Identifier: AGPL-3.0-only

//! The places list: Home, XDG user dirs, GTK bookmarks, Trash and mounted
//! filesystems (FOG §Desktop interop). udisks2 lands in M5; until then
//! mounts come from `/proc/self/mountinfo`.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use fog_proto::{Place, PlaceKind};

use crate::open::Xdg;

/// Places for this session.
pub fn current() -> Vec<Place> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    list(&Xdg::from_env(), &mountinfo)
}

/// Home, user dirs, bookmarks, Trash, then mounts, reading the files under
/// `xdg`.
pub fn list(xdg: &Xdg, mountinfo: &str) -> Vec<Place> {
    let mut out = Vec::new();
    if xdg.home.is_absolute() {
        out.push(place(PlaceKind::Home, "Home".into(), &xdg.home));
    }
    if let Ok(t) = fs::read_to_string(xdg.config_home.join("user-dirs.dirs")) {
        out.extend(
            user_dirs(&t, &xdg.home)
                .into_iter()
                .filter(|p| p.is_dir())
                .map(|p| place(PlaceKind::UserDir, basename(&p), &p)),
        );
    }
    if let Ok(t) = fs::read(xdg.config_home.join("gtk-3.0/bookmarks")) {
        out.extend(bookmarks(&t));
    }
    out.push(place(
        PlaceKind::Trash,
        "Trash".into(),
        &xdg.data_home.join("Trash/files"),
    ));
    out.extend(mounts(mountinfo));
    out
}

fn place(kind: PlaceKind, label: String, path: &Path) -> Place {
    Place {
        kind,
        label,
        path: path.as_os_str().as_bytes().to_vec(),
    }
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// `user-dirs.dirs` paths in sidebar order. Values are `"$HOME/..."` or
/// absolute; a dir set to `$HOME` itself is disabled and skipped.
pub fn user_dirs(text: &str, home: &Path) -> Vec<PathBuf> {
    const ORDER: [&str; 8] = [
        "DESKTOP",
        "DOCUMENTS",
        "DOWNLOAD",
        "MUSIC",
        "PICTURES",
        "VIDEOS",
        "PUBLICSHARE",
        "TEMPLATES",
    ];
    let mut dirs: Vec<(usize, PathBuf)> = Vec::new();
    for line in text.lines().map(str::trim) {
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let Some(name) = key
            .strip_prefix("XDG_")
            .and_then(|k| k.strip_suffix("_DIR"))
        else {
            continue;
        };
        let Some(val) = val.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
            continue;
        };
        let val = val.replace("\\\"", "\"");
        let path = if let Some(rest) = val.strip_prefix("$HOME") {
            match rest.strip_prefix('/') {
                Some(r) if !r.is_empty() => home.join(r),
                _ => continue,
            }
        } else if val.starts_with('/') {
            PathBuf::from(val)
        } else {
            continue;
        };
        if path == home {
            continue;
        }
        let rank = ORDER.iter().position(|o| *o == name).unwrap_or(ORDER.len());
        dirs.push((rank, path));
    }
    dirs.sort_by_key(|(r, _)| *r);
    dirs.into_iter().map(|(_, p)| p).collect()
}

/// GTK bookmarks: `file://` URIs only, percent-decoded, with an optional
/// label after the first space.
pub fn bookmarks(text: &[u8]) -> Vec<Place> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let (uri, label) = match line.iter().position(|&b| b == b' ') {
                Some(i) => (&line[..i], Some(&line[i + 1..])),
                None => (line, None),
            };
            let rest = uri.strip_prefix(b"file://")?;
            let rest = rest.strip_prefix(b"localhost").unwrap_or(rest);
            if !rest.starts_with(b"/") {
                return None;
            }
            let path = percent_decode(rest)?;
            let label = label
                .map(|l| String::from_utf8_lossy(l).trim().to_string())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| basename(Path::new(OsStr::from_bytes(&path))));
            Some(Place {
                kind: PlaceKind::Bookmark,
                label,
                path,
            })
        })
        .collect()
}

fn percent_decode(s: &[u8]) -> Option<Vec<u8>> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let hi = hex(*s.get(i + 1)?)?;
            let lo = hex(*s.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Filesystem types that are never a user's volume.
const PSEUDO: &[&str] = &[
    "autofs",
    "binfmt_misc",
    "bpf",
    "cgroup",
    "cgroup2",
    "configfs",
    "debugfs",
    "devpts",
    "devtmpfs",
    "efivarfs",
    "fusectl",
    "fuse.gvfsd-fuse",
    "fuse.portal",
    "hugetlbfs",
    "mqueue",
    "nsfs",
    "overlay",
    "proc",
    "pstore",
    "ramfs",
    "rpc_pipefs",
    "securityfs",
    "selinuxfs",
    "squashfs",
    "sysfs",
    "tmpfs",
    "tracefs",
];

/// Mount points that belong to the system, with everything under them.
const SYSTEM: &[&str] = &[
    "/boot", "/efi", "/home", "/nix", "/opt", "/proc", "/root", "/run", "/snap", "/srv", "/sys",
    "/tmp", "/usr", "/var",
];

/// User-relevant mounts from `mountinfo`: anything non-pseudo under
/// `/run/media`, `/media` or `/mnt`, plus other block-device mounts outside
/// `/` and the system trees. Deduplicated by mount point.
pub fn mounts(mountinfo: &str) -> Vec<Place> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for line in mountinfo.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let (Some(mp), Some(fstype), Some(source)) = (
            pre.split(' ').nth(4),
            post.split(' ').next(),
            post.split(' ').nth(1),
        ) else {
            continue;
        };
        let mp = unoctal(mp);
        let under = |root: &[u8]| {
            mp.strip_prefix(root)
                .is_some_and(|r| r.is_empty() || r.starts_with(b"/"))
        };
        if PSEUDO.contains(&fstype) {
            continue;
        }
        let user = [&b"/run/media"[..], b"/media", b"/mnt"]
            .into_iter()
            .any(|r| under(r) && mp.len() > r.len());
        let block = source.starts_with("/dev/")
            && mp != b"/"
            && !SYSTEM.iter().any(|s| under(s.as_bytes()));
        if !(user || block) || !seen.insert(mp.clone()) {
            continue;
        }
        let path = Path::new(OsStr::from_bytes(&mp));
        out.push(place(PlaceKind::Mount, basename(path), path));
    }
    out
}

/// Undo mountinfo's `\NNN` octal escapes.
fn unoctal(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.len() >= i + 4 {
            let oct = &b[i + 1..i + 4];
            if oct.iter().all(|c| (b'0'..=b'7').contains(c)) {
                out.push(oct.iter().fold(0u8, |a, c| a.wrapping_mul(8) + (c - b'0')));
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = "\
22 1 259:2 / / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,subvol=/@
23 22 259:2 /@home /home rw,relatime shared:2 - btrfs /dev/nvme0n1p2 rw,subvol=/@home
24 22 259:1 / /boot rw,relatime shared:3 - vfat /dev/nvme0n1p1 rw
25 22 0:22 / /proc rw,nosuid shared:4 - proc proc rw
26 22 0:23 / /tmp rw shared:5 - tmpfs tmpfs rw
27 22 0:24 / /run rw shared:6 - tmpfs run rw
28 27 8:17 / /run/media/u/My\\040Disk rw,nosuid shared:7 - exfat /dev/sdb1 rw
29 22 7:0 / /snap/core/1 ro shared:8 - squashfs /dev/loop0 ro
30 22 8:1 / /data rw shared:9 - ext4 /dev/sda1 rw
31 22 8:1 / /data rw shared:10 - ext4 /dev/sda1 rw
32 22 0:40 / /mnt/nas rw shared:11 - nfs4 nas:/export rw
33 22 0:41 / /mnt rw shared:12 - ext4 /dev/sdc1 rw
34 27 0:42 / /run/user/1000/doc rw shared:13 - fuse.portal portal rw
35 22 0:43 / /media/tmp rw shared:14 - tmpfs tmpfs rw
36 22 259:3 / /var/lib/docker rw shared:15 - ext4 /dev/nvme0n1p3 rw
";

    #[test]
    fn mountinfo_filtering() {
        let got: Vec<(String, Vec<u8>)> = mounts(MOUNTINFO)
            .into_iter()
            .map(|p| {
                assert_eq!(p.kind, PlaceKind::Mount);
                (p.label, p.path)
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("My Disk".into(), b"/run/media/u/My Disk".to_vec()),
                ("data".into(), b"/data".to_vec()),
                ("nas".into(), b"/mnt/nas".to_vec()),
                ("mnt".into(), b"/mnt".to_vec()),
            ]
        );
    }

    #[test]
    fn user_dirs_parse() {
        let home = Path::new("/home/u");
        let text = "# generated\nXDG_DOWNLOAD_DIR=\"$HOME/Downloads\"\nXDG_DESKTOP_DIR=\"$HOME/\"\n\
                    XDG_TEMPLATES_DIR=\"$HOME\"\nXDG_MUSIC_DIR=\"/srv/music\"\n\
                    XDG_DOCUMENTS_DIR=\"$HOME/My Docs\"\nXDG_BOGUS=\"$HOME/x\"\nXDG_VIDEOS_DIR=rel\n";
        assert_eq!(
            user_dirs(text, home),
            vec![
                PathBuf::from("/home/u/My Docs"),
                PathBuf::from("/home/u/Downloads"),
                PathBuf::from("/srv/music"),
            ]
        );
    }

    #[test]
    fn bookmarks_parse() {
        let text = b"file:///home/u/src\nfile:///home/u/a%20b%FF Work stuff\n\
                     sftp://host/x remote\nfile://localhost/opt\r\nfile:///bad%zz\n\nfile:///\n";
        let got: Vec<(String, Vec<u8>)> = bookmarks(text)
            .into_iter()
            .map(|p| (p.label, p.path))
            .collect();
        assert_eq!(
            got,
            vec![
                ("src".into(), b"/home/u/src".to_vec()),
                ("Work stuff".into(), b"/home/u/a b\xff".to_vec()),
                ("opt".into(), b"/opt".to_vec()),
                ("/".into(), b"/".to_vec()),
            ]
        );
    }

    #[test]
    fn full_list_order() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        fs::create_dir_all(home.join("Documents")).unwrap();
        let x = Xdg {
            config_home: home.join(".config"),
            data_home: home.join(".local/share"),
            home: home.clone(),
            ..Xdg::default()
        };
        fs::create_dir_all(x.config_home.join("gtk-3.0")).unwrap();
        fs::write(
            x.config_home.join("user-dirs.dirs"),
            "XDG_DOCUMENTS_DIR=\"$HOME/Documents\"\nXDG_MUSIC_DIR=\"$HOME/Music\"\n",
        )
        .unwrap();
        fs::write(x.config_home.join("gtk-3.0/bookmarks"), "file:///tmp T\n").unwrap();
        let kinds: Vec<(PlaceKind, String)> = list(&x, MOUNTINFO)
            .into_iter()
            .map(|p| (p.kind, p.label))
            .take(5)
            .collect();
        assert_eq!(
            kinds,
            vec![
                (PlaceKind::Home, "Home".into()),
                (PlaceKind::UserDir, "Documents".into()),
                (PlaceKind::Bookmark, "T".into()),
                (PlaceKind::Trash, "Trash".into()),
                (PlaceKind::Mount, "My Disk".into()),
            ]
        );
    }
}
