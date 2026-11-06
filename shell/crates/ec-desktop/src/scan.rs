// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop folder: where it is, what is in it, and when it changed.
//!
//! Nothing here logs a name or a path; callers log [`Scan::len`]-style counts.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ec_services::apps;

use crate::mime;

/// What an entry is. The order is the `Kind` sort order: folders first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Dir,
    Launcher,
    File,
    Symlink,
}

/// A `.desktop` file on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    /// `Name=`, or the file stem when it has none.
    pub name: String,
    /// `Icon=`: a theme icon name or an absolute path.
    pub icon: Option<String>,
    /// Parsed by `ec_services::apps` when it is a launchable application.
    pub app: Option<apps::Entry>,
    /// Executable bit set: the human has allowed it to launch
    /// (`ops::allow_launching`). Without it, activation is refused.
    pub trusted: bool,
}

/// One icon on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file name: the key under which a position is saved.
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
    pub mime: Option<&'static str>,
    pub modified: Option<SystemTime>,
    pub size: u64,
    /// A symlink whose target is a directory.
    pub to_dir: bool,
    /// A symlink whose target is gone.
    pub broken: bool,
    pub launcher: Option<Launcher>,
}

impl Entry {
    /// The name drawn under the icon: a launcher's `Name=`, else the file name.
    pub fn label(&self) -> &str {
        self.launcher
            .as_ref()
            .map_or(self.name.as_str(), |l| l.name.as_str())
    }

    /// Does activating this open a folder rather than a file?
    pub fn is_folder(&self) -> bool {
        self.kind == Kind::Dir || (self.kind == Kind::Symlink && self.to_dir)
    }
}

/// `XDG_DESKTOP_DIR` from the text of `user-dirs.dirs`. `$HOME` expands; a
/// value of `$HOME` itself means "no desktop folder" to xdg-user-dirs and is
/// `None`.
pub fn parse_user_dirs(text: &str, home: &Path) -> Option<PathBuf> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("XDG_DESKTOP_DIR=") else {
            continue;
        };
        let value = rest.trim().trim_matches('"');
        let path = match value.strip_prefix("$HOME") {
            Some("") => home.to_path_buf(),
            Some(tail) => home.join(tail.trim_start_matches('/')),
            None => PathBuf::from(value),
        };
        if path.is_absolute() && path != home {
            return Some(path);
        }
        return None;
    }
    None
}

/// Where the desktop folder is: `$XDG_DESKTOP_DIR` if the environment sets it,
/// else `user-dirs.dirs`, else `~/Desktop`.
pub fn desktop_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    resolve_dir(
        std::env::var_os("XDG_DESKTOP_DIR").map(PathBuf::from),
        std::fs::read_to_string(config.join("user-dirs.dirs"))
            .ok()
            .as_deref(),
        &home,
    )
}

/// The pure half of [`desktop_dir`].
pub fn resolve_dir(env: Option<PathBuf>, user_dirs: Option<&str>, home: &Path) -> Option<PathBuf> {
    if let Some(p) = env.filter(|p| p.is_absolute() && p != home) {
        return Some(p);
    }
    user_dirs
        .and_then(|t| parse_user_dirs(t, home))
        .or_else(|| Some(home.join("Desktop")))
}

/// `Name` and `Icon` of a `[Desktop Entry]` group, untranslated.
fn name_and_icon(text: &str) -> (Option<String>, Option<String>) {
    let (mut name, mut icon) = (None, None);
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Name" => name = Some(v.trim().to_owned()),
                "Icon" => icon = Some(v.trim().to_owned()),
                _ => {}
            }
        }
    }
    (name, icon.filter(|i| !i.is_empty()))
}

/// A `.desktop` file's launcher record. `term` is the configured terminal, as
/// `apps::parse` wants it for `Terminal=true` entries.
pub fn parse_launcher(file_name: &str, text: &str, mode: u32, term: Option<&str>) -> Launcher {
    let (name, icon) = name_and_icon(text);
    let stem = file_name.strip_suffix(".desktop").unwrap_or(file_name);
    Launcher {
        name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| stem.to_owned()),
        icon,
        app: apps::parse(file_name, text, term),
        // The owner bit only: it is the one `ops::allow_launching` sets, and a
        // group or other bit from an unpacked archive is not the human's say.
        trusted: mode & 0o100 != 0,
    }
}

/// The most of a `.desktop` file that is read; the rest is not a launcher.
const LAUNCHER_MAX: u64 = 64 * 1024;

/// A launcher's text, at most [`LAUNCHER_MAX`] bytes; empty if unreadable or
/// not UTF-8.
fn read_capped(path: &Path) -> String {
    use std::io::Read;
    let mut text = String::new();
    if let Ok(f) = std::fs::File::open(path) {
        if f.take(LAUNCHER_MAX).read_to_string(&mut text).is_err() {
            text.clear();
        }
    }
    text
}

/// Read one directory entry. `None` for what the desktop does not show
/// (dotfiles, unreadable entries).
fn read_entry(path: &Path, term: Option<&str>) -> Option<Entry> {
    let name = path.file_name()?.to_str()?.to_owned();
    if name.starts_with('.') {
        return None;
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    let (kind, to_dir, broken, meta) = if meta.file_type().is_symlink() {
        match std::fs::metadata(path) {
            Ok(target) => (Kind::Symlink, target.is_dir(), false, target),
            Err(_) => (Kind::Symlink, false, true, meta),
        }
    } else if meta.is_dir() {
        (Kind::Dir, false, false, meta)
    } else if name.ends_with(".desktop") && meta.is_file() {
        // Regular files only: a FIFO named `x.desktop` would block the read.
        (Kind::Launcher, false, false, meta)
    } else {
        (Kind::File, false, false, meta)
    };
    let launcher = (kind == Kind::Launcher).then(|| {
        let text = read_capped(path);
        parse_launcher(&name, &text, meta.permissions().mode(), term)
    });
    Some(Entry {
        mime: mime::of(&name),
        modified: meta.modified().ok(),
        size: meta.len(),
        name,
        path: path.to_path_buf(),
        kind,
        to_dir,
        broken,
        launcher,
    })
}

/// Every shown entry of `dir`, in directory order. A missing or unreadable
/// folder is an empty desktop, not an error: the desktop never creates it.
pub fn scan(dir: &Path, term: Option<&str>) -> Vec<Entry> {
    try_scan(dir, term).unwrap_or_default()
}

/// [`scan`], but `None` when the folder cannot be read, so a caller can tell
/// that from an empty desktop and keep what it has.
pub fn try_scan(dir: &Path, term: Option<&str>) -> Option<Vec<Entry>> {
    let read = std::fs::read_dir(dir).ok()?;
    Some(
        read.filter_map(Result::ok)
            .filter_map(|e| read_entry(&e.path(), term))
            .collect(),
    )
}

/// A hash of what [`scan`] would show: names, kinds, sizes, mtimes and modes.
/// Equal fingerprints mean nothing the desktop draws has moved. The shell
/// crates have no inotify binding and a desktop folder is tens of entries, so
/// the watcher polls this (see [`POLL`]).
pub fn fingerprint(dir: &Path) -> u64 {
    let mut rows: Vec<(String, u64, u64, u32)> = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for e in read.filter_map(Result::ok) {
            let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(m) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as u64);
            rows.push((name, m.len(), mtime, m.permissions().mode()));
        }
    }
    rows.sort();
    let mut h = DefaultHasher::new();
    rows.hash(&mut h);
    h.finish()
}

/// How often the watcher looks at the folder.
pub const POLL: Duration = Duration::from_millis(1000);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp;

    #[test]
    fn user_dirs_expands_home_and_refuses_home_itself() {
        let home = Path::new("/home/u");
        assert_eq!(
            parse_user_dirs("XDG_DESKTOP_DIR=\"$HOME/Skrivbord\"\n", home),
            Some(PathBuf::from("/home/u/Skrivbord"))
        );
        assert_eq!(
            parse_user_dirs("XDG_DESKTOP_DIR=\"/data/desk\"", home),
            Some(PathBuf::from("/data/desk"))
        );
        assert_eq!(parse_user_dirs("XDG_DESKTOP_DIR=\"$HOME\"", home), None);
        assert_eq!(parse_user_dirs("XDG_MUSIC_DIR=\"$HOME/Music\"", home), None);
    }

    #[test]
    fn dir_resolution_prefers_env_then_user_dirs_then_desktop() {
        let home = Path::new("/home/u");
        assert_eq!(
            resolve_dir(Some("/x".into()), Some("XDG_DESKTOP_DIR=\"$HOME/D\""), home),
            Some("/x".into())
        );
        assert_eq!(
            resolve_dir(None, Some("XDG_DESKTOP_DIR=\"$HOME/D\""), home),
            Some("/home/u/D".into())
        );
        assert_eq!(resolve_dir(None, None, home), Some("/home/u/Desktop".into()));
        // "no desktop" in user-dirs.dirs falls back rather than showing $HOME.
        assert_eq!(
            resolve_dir(None, Some("XDG_DESKTOP_DIR=\"$HOME\""), home),
            Some("/home/u/Desktop".into())
        );
    }

    #[test]
    fn scan_classifies_and_skips_dotfiles() {
        let d = tmp("scan");
        std::fs::create_dir(d.join("folder")).unwrap();
        std::fs::write(d.join("a.txt"), "hi").unwrap();
        std::fs::write(d.join(".hidden"), "").unwrap();
        std::fs::write(
            d.join("app.desktop"),
            "[Desktop Entry]\nType=Application\nName=An App\nIcon=app-icon\nExec=true\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(d.join("folder"), d.join("link-to-folder")).unwrap();
        std::os::unix::fs::symlink(d.join("gone"), d.join("dangling")).unwrap();
        let mut found = scan(&d, None);
        found.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = found.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["a.txt", "app.desktop", "dangling", "folder", "link-to-folder"]
        );
        let by = |n: &str| found.iter().find(|e| e.name == n).unwrap();
        assert_eq!(by("a.txt").kind, Kind::File);
        assert_eq!(by("a.txt").mime, Some("text/plain"));
        assert_eq!(by("a.txt").size, 2);
        assert_eq!(by("folder").kind, Kind::Dir);
        assert!(by("link-to-folder").is_folder());
        assert!(by("dangling").broken);
        let l = by("app.desktop").launcher.as_ref().unwrap();
        assert_eq!(by("app.desktop").label(), "An App");
        assert_eq!(l.icon.as_deref(), Some("app-icon"));
        assert!(l.app.is_some());
        assert!(!l.trusted, "a fresh file has no exec bit");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_folder_is_an_empty_desktop() {
        assert!(scan(Path::new("/nonexistent/ec-desktop"), None).is_empty());
    }

    #[test]
    fn only_the_owner_exec_bit_is_trust() {
        for (mode, trusted) in [(0o601, false), (0o611, false), (0o744, true), (0o700, true)] {
            let l = parse_launcher("a.desktop", "[Desktop Entry]\nName=A\nExec=true\n", mode, None);
            assert_eq!(l.trusted, trusted, "{mode:o}");
        }
    }

    #[test]
    fn a_fifo_named_desktop_does_not_block_the_scan() {
        let d = tmp("fifo");
        let ok = std::process::Command::new("mkfifo")
            .arg(d.join("x.desktop"))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "mkfifo");
        let found = scan(&d, None);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::File);
        assert!(found[0].launcher.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_huge_launcher_is_read_only_to_the_cap() {
        let d = tmp("big");
        let mut text = String::from("[Desktop Entry]\nName=Big\nExec=true\n");
        text.push_str(&"#".repeat(LAUNCHER_MAX as usize * 2));
        std::fs::write(d.join("big.desktop"), &text).unwrap();
        assert!(read_capped(&d.join("big.desktop")).len() as u64 <= LAUNCHER_MAX);
        let found = scan(&d, None);
        assert_eq!(found[0].label(), "Big");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unreadable_folder_is_not_an_empty_one() {
        assert!(try_scan(Path::new("/nonexistent/ec-desktop"), None).is_none());
        let d = tmp("empty");
        assert_eq!(try_scan(&d, None).map(|v| v.len()), Some(0));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn launcher_without_a_name_uses_its_stem_and_exec_bit_is_trust() {
        let l = parse_launcher("thing.desktop", "[Desktop Entry]\nExec=true\n", 0o755, None);
        assert_eq!(l.name, "thing");
        assert!(l.trusted);
        assert!(l.app.is_none(), "no Name: apps::parse refuses it");
    }

    #[test]
    fn fingerprint_moves_with_the_folder() {
        let d = tmp("fp");
        let empty = fingerprint(&d);
        assert_eq!(empty, fingerprint(&d));
        std::fs::write(d.join("a"), "x").unwrap();
        let one = fingerprint(&d);
        assert_ne!(empty, one);
        std::fs::write(d.join("a"), "xyz").unwrap();
        assert_ne!(one, fingerprint(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}
