// SPDX-License-Identifier: AGPL-3.0-only
//! Host-managed Firefox CSS (glass popups, selected tab, Home button).
//!
//! The helper runs as the user, as a child of Firefox, so it can find that
//! Firefox's profile and keep two files of its own in `<profile>/chrome/`:
//! `eclipse-glass.css` (browser chrome) and `eclipse-glass-content.css` (about:
//! pages), each pulled in by one marked `@import` line at the top of
//! `userChrome.css` / `userContent.css`. The CSS ships with the host, so a CSS
//! tweak is a package update, not a new signed add-on.
//!
//! The user's own rules in those two files are never touched. Both sheets are
//! scoped to a themed window (`:root[lwtheme]`, `:root[lwt-newtab]`), so once
//! the add-on and its theme are gone they are inert; `--remove` also takes the
//! import lines out.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const CHROME_CSS: &str = include_str!("../css/eclipse-glass.css");
pub const CONTENT_CSS: &str = include_str!("../css/eclipse-glass-content.css");

pub const CHROME_FILE: &str = "eclipse-glass.css";
pub const CONTENT_FILE: &str = "eclipse-glass-content.css";

/// Every managed import line carries this, which is how it is found again.
const MARK: &str = "eclipse-glass:managed";

fn import_line(file: &str) -> String {
    format!("@import url(\"{file}\"); /* {MARK} */")
}

/// Files only a profile directory holds, best first. Firefox keeps the first
/// open for as long as it runs.
const MARKERS: [&str; 4] = [".parentlock", "places.sqlite", "prefs.js", "cookies.sqlite"];

/// The profile named by the targets of a process's open files.
pub fn profile_from_fd_targets<I: IntoIterator<Item = PathBuf>>(targets: I) -> Option<PathBuf> {
    let targets: Vec<PathBuf> = targets.into_iter().collect();
    for marker in MARKERS {
        for t in &targets {
            if t.file_name().is_some_and(|n| n == marker) {
                return t.parent().map(Path::to_path_buf);
            }
        }
    }
    None
}

/// The profile named by a NUL-separated command line: `-profile DIR`,
/// `--profile DIR` or `--profile=DIR`.
pub fn profile_from_cmdline(cmdline: &[u8]) -> Option<PathBuf> {
    let args: Vec<&[u8]> = cmdline.split(|&b| b == 0).collect();
    let utf = |b: &[u8]| std::str::from_utf8(b).ok().map(PathBuf::from);
    for (i, a) in args.iter().enumerate() {
        if *a == b"-profile" || *a == b"--profile" {
            return args.get(i + 1).filter(|v| !v.is_empty()).and_then(|v| utf(v));
        }
        if let Some(v) = a.strip_prefix(b"--profile=") {
            return utf(v);
        }
    }
    None
}

/// The profile of the process `pid` (Firefox), if it can be determined.
pub fn find_profile(pid: u32) -> Option<PathBuf> {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    let fds = fs::read_dir(proc.join("fd"))
        .map(|d| {
            d.filter_map(|e| fs::read_link(e.ok()?.path()).ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    profile_from_fd_targets(fds)
        .or_else(|| profile_from_cmdline(&fs::read(proc.join("cmdline")).unwrap_or_default()))
        .filter(|p| p.is_dir())
}

/// `text` with exactly one managed import line at the top (after a leading
/// `@charset`, which must stay first). `None` when it already is.
pub fn with_import(text: &str, file: &str) -> Option<String> {
    let line = import_line(file);
    let rest = without_import(text);
    let (head, tail) = split_charset(&rest);
    let out = format!("{head}{line}\n{tail}");
    (out != text).then_some(out)
}

/// `text` without the managed import line. `None` when there is none.
pub fn remove_import(text: &str) -> Option<String> {
    let out = without_import(text);
    (out != text).then_some(out)
}

fn without_import(text: &str) -> String {
    text.split_inclusive('\n').filter(|l| !l.contains(MARK)).collect()
}

fn split_charset(text: &str) -> (&str, &str) {
    if text.trim_start().starts_with("@charset") {
        if let Some(end) = text.find('\n') {
            return text.split_at(end + 1);
        }
        return (text, "");
    }
    ("", text)
}

fn write_if_changed(path: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    fs::write(path, content)
}

fn keep_import(chrome: &Path, user_file: &str, file: &str) -> io::Result<()> {
    let path = chrome.join(user_file);
    let old = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    match with_import(&old, file) {
        Some(new) => fs::write(&path, new),
        None => Ok(()),
    }
}

/// Write the two sheets (only when they differ) and keep the import lines.
/// Firefox reads these at startup, so a first install needs one restart.
pub fn install(profile: &Path) -> io::Result<()> {
    let chrome = profile.join("chrome");
    fs::create_dir_all(&chrome)?;
    write_if_changed(&chrome.join(CHROME_FILE), CHROME_CSS)?;
    write_if_changed(&chrome.join(CONTENT_FILE), CONTENT_CSS)?;
    keep_import(&chrome, "userChrome.css", CHROME_FILE)?;
    keep_import(&chrome, "userContent.css", CONTENT_FILE)
}

/// Take our import lines and sheets out of a profile, leaving the user's rules.
pub fn uninstall(profile: &Path) -> io::Result<()> {
    let chrome = profile.join("chrome");
    for user_file in ["userChrome.css", "userContent.css"] {
        let path = chrome.join(user_file);
        if let Ok(old) = fs::read_to_string(&path) {
            if let Some(new) = remove_import(&old) {
                fs::write(&path, new)?;
            }
        }
    }
    for f in [CHROME_FILE, CONTENT_FILE] {
        match fs::remove_file(chrome.join(f)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMP: &str = "@import url(\"eclipse-glass.css\"); /* eclipse-glass:managed */\n";

    #[test]
    fn import_is_inserted_above_the_users_rules() {
        let out = with_import("a { b: c }\n", CHROME_FILE).unwrap();
        assert_eq!(out, format!("{IMP}a {{ b: c }}\n"));
        assert_eq!(with_import("", CHROME_FILE).unwrap(), IMP);
    }

    #[test]
    fn import_is_kept_once_and_moved_back_to_the_top() {
        let have = format!("{IMP}a {{ b: c }}\n");
        assert_eq!(with_import(&have, CHROME_FILE), None);
        let moved = format!("a {{ b: c }}\n{IMP}d {{}}\n");
        assert_eq!(
            with_import(&moved, CHROME_FILE).unwrap(),
            format!("{IMP}a {{ b: c }}\nd {{}}\n")
        );
        let twice = format!("{IMP}{IMP}x\n");
        assert_eq!(with_import(&twice, CHROME_FILE).unwrap(), format!("{IMP}x\n"));
    }

    #[test]
    fn charset_stays_first() {
        let out = with_import("@charset \"utf-8\";\nx {}\n", CHROME_FILE).unwrap();
        assert_eq!(out, format!("@charset \"utf-8\";\n{IMP}x {{}}\n"));
        assert_eq!(with_import(&out, CHROME_FILE), None);
    }

    #[test]
    fn removal_restores_the_users_file_exactly() {
        let user = "@namespace url(x);\na { b: c }";
        let with = with_import(user, CHROME_FILE).unwrap();
        assert_eq!(remove_import(&with).unwrap(), user);
        assert_eq!(remove_import(user), None);
    }

    #[test]
    fn profile_comes_from_the_lock_file_before_other_files() {
        let t = vec![
            PathBuf::from("/usr/lib/firefox/libxul.so"),
            PathBuf::from("/home/u/.mozilla/firefox/x.default/places.sqlite"),
            PathBuf::from("/home/u/.mozilla/firefox/x.default/.parentlock"),
        ];
        assert_eq!(
            profile_from_fd_targets(t),
            Some(PathBuf::from("/home/u/.mozilla/firefox/x.default"))
        );
        assert_eq!(profile_from_fd_targets([PathBuf::from("/tmp/x")]), None);
    }

    #[test]
    fn profile_comes_from_the_command_line() {
        let p = |s: &[u8]| profile_from_cmdline(s);
        assert_eq!(p(b"firefox\0--profile\0/p/one\0"), Some(PathBuf::from("/p/one")));
        assert_eq!(p(b"firefox\0-profile\0/p/two"), Some(PathBuf::from("/p/two")));
        assert_eq!(
            p(b"firefox\0--profile=/p/three\0"),
            Some(PathBuf::from("/p/three"))
        );
        assert_eq!(p(b"firefox\0--new-instance\0"), None);
        assert_eq!(p(b"firefox\0--profile"), None);
    }

    #[test]
    fn install_creates_keeps_and_uninstall_restores() {
        let dir = std::env::temp_dir().join(format!("ec-glass-host-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("chrome")).unwrap();
        fs::write(dir.join("chrome/userChrome.css"), "mine { x: y }\n").unwrap();
        install(&dir).unwrap();
        let chrome = fs::read_to_string(dir.join("chrome/userChrome.css")).unwrap();
        assert!(chrome.starts_with("@import") && chrome.ends_with("mine { x: y }\n"));
        let content = fs::read_to_string(dir.join("chrome/userContent.css")).unwrap();
        assert!(content.contains("eclipse-glass-content.css"));
        assert_eq!(
            fs::read_to_string(dir.join("chrome/eclipse-glass.css")).unwrap(),
            CHROME_CSS
        );
        let before = fs::metadata(dir.join("chrome/eclipse-glass.css"))
            .unwrap()
            .modified()
            .unwrap();
        install(&dir).unwrap();
        assert_eq!(
            fs::metadata(dir.join("chrome/eclipse-glass.css"))
                .unwrap()
                .modified()
                .unwrap(),
            before
        );
        uninstall(&dir).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join("chrome/userChrome.css")).unwrap(),
            "mine { x: y }\n"
        );
        assert!(!dir.join("chrome/eclipse-glass.css").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
