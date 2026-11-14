// SPDX-License-Identifier: AGPL-3.0-only
//! Desktop entries (Desktop Entry Specification 1.5) for "Open with" (T-14 §11): the
//! `[Desktop Entry]` group of `Type=Application` files, their desktop file ids, and the
//! per-directory listing the MIME-apps association algorithm needs.
//!
//! Bounded: files over [`MAX_FILE`] bytes, more than [`MAX_FILES`] files, or deeper than
//! [`MAX_DEPTH`] directories are not read. Entry text is third-party: shown through the
//! palette's program-text style by the caller, never logged.

use std::path::{Path, PathBuf};

/// Largest `.desktop` file read.
pub(crate) const MAX_FILE: u64 = 64 * 1024;
/// Most files read over all directories.
pub(crate) const MAX_FILES: usize = 4096;
/// Deepest subdirectory walked under an `applications` directory.
const MAX_DEPTH: usize = 4;
/// Longest name kept, in bytes.
const MAX_NAME: usize = 128;

/// One application entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Desktop file id (`org.gnome.Loupe.desktop`).
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) exec: String,
    pub(crate) try_exec: Option<String>,
    pub(crate) icon: Option<String>,
    pub(crate) mime: Vec<String>,
    pub(crate) terminal: bool,
    pub(crate) no_display: bool,
    /// `Hidden=true`: as if the file did not exist (it still shadows lower directories).
    pub(crate) hidden: bool,
    pub(crate) only_show_in: Vec<String>,
    pub(crate) not_show_in: Vec<String>,
}

impl Entry {
    /// Shown in menus for the current desktops (`OnlyShowIn` / `NotShowIn`, `NoDisplay`).
    pub(crate) fn shown(&self, desktops: &[String]) -> bool {
        if self.hidden || self.no_display {
            return false;
        }
        for d in desktops {
            if self.not_show_in.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                return false;
            }
            if self.only_show_in.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                return true;
            }
        }
        self.only_show_in.is_empty()
    }
}

/// Undo the string escapes `\s \n \t \r \\` (and `\;` inside lists).
fn unescape(v: &str, list: bool) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(';') if list => out.push(';'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split a list value on unescaped `;` (a trailing `;` is optional).
pub(crate) fn split_list(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in v.chars() {
        if esc {
            cur.push('\\');
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ';' {
            if !cur.is_empty() {
                out.push(unescape(&cur, true));
            }
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if esc {
        cur.push('\\');
    }
    if !cur.is_empty() {
        out.push(unescape(&cur, true));
    }
    out
}

/// Locale preference for `Name[…]`: `lang_COUNTRY@MODIFIER`, `lang_COUNTRY`, `lang@MODIFIER`,
/// `lang` (the specification's matching order), from an `LC_MESSAGES`-style value.
pub(crate) fn locale_keys(locale: &str) -> Vec<String> {
    // `lang_COUNTRY.ENCODING@MODIFIER`: the encoding is ignored.
    let (l, modifier) = match locale.split_once('@') {
        Some((b, m)) => (b, Some(m)),
        None => (locale, None),
    };
    let base = l.split('.').next().unwrap_or("");
    let (lang, country) = match base.split_once('_') {
        Some((a, c)) => (a, Some(c)),
        None => (base, None),
    };
    if lang.is_empty() || lang == "C" || lang == "POSIX" {
        return Vec::new();
    }
    let mut v = Vec::new();
    if let (Some(c), Some(m)) = (country, modifier) {
        v.push(format!("{lang}_{c}@{m}"));
    }
    if let Some(c) = country {
        v.push(format!("{lang}_{c}"));
    }
    if let Some(m) = modifier {
        v.push(format!("{lang}@{m}"));
    }
    v.push(lang.to_string());
    v
}

/// Parse one file. `None` when it is not an application entry or lacks `Name`/`Exec`
/// (a `Hidden=true` entry parses with whatever it has: it only shadows).
pub(crate) fn parse(text: &str, id: &str, path: &Path, locales: &[String]) -> Option<Entry> {
    let mut in_group = false;
    let mut name: Option<String> = None;
    let mut name_rank = usize::MAX;
    let mut e = Entry {
        id: id.to_string(),
        path: path.to_path_buf(),
        name: String::new(),
        exec: String::new(),
        try_exec: None,
        icon: None,
        mime: Vec::new(),
        terminal: false,
        no_display: false,
        hidden: false,
        only_show_in: Vec::new(),
        not_show_in: Vec::new(),
    };
    let mut ty = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if in_group {
                break;
            }
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim_start());
        let truthy = v == "true";
        match k {
            "Type" => ty = Some(v.to_string()),
            "Name" if name_rank == usize::MAX => name = Some(unescape(v, false)),
            "Exec" => e.exec = v.to_string(),
            "TryExec" => e.try_exec = Some(unescape(v, false)),
            "Icon" => e.icon = Some(unescape(v, false)),
            "MimeType" => e.mime = split_list(v),
            "Terminal" => e.terminal = truthy,
            "NoDisplay" => e.no_display = truthy,
            "Hidden" => e.hidden = truthy,
            "OnlyShowIn" => e.only_show_in = split_list(v),
            "NotShowIn" => e.not_show_in = split_list(v),
            _ => {
                if let Some(loc) = k.strip_prefix("Name[").and_then(|r| r.strip_suffix(']'))
                    && let Some(rank) = locales.iter().position(|l| l == loc)
                    && rank < name_rank
                {
                    name_rank = rank;
                    name = Some(unescape(v, false));
                }
            }
        }
    }
    if !e.hidden && (ty.as_deref() != Some("Application") || e.exec.is_empty()) {
        return None;
    }
    let mut n = name.unwrap_or_default();
    n.retain(|c| !c.is_control());
    if n.len() > MAX_NAME {
        let mut end = MAX_NAME;
        while !n.is_char_boundary(end) {
            end -= 1;
        }
        n.truncate(end);
    }
    if n.is_empty() && !e.hidden {
        return None;
    }
    e.name = n;
    Some(e)
}

/// Every entry found, per applications directory in precedence order (index 0 is the most
/// important: `$XDG_DATA_HOME/applications`).
#[derive(Clone, Debug, Default)]
pub(crate) struct Index {
    pub(crate) dirs: Vec<Vec<Entry>>,
}

impl Index {
    /// The effective entry for an id: the first directory that has it (a hidden one hides).
    pub(crate) fn get(&self, id: &str) -> Option<&Entry> {
        self.dirs
            .iter()
            .find_map(|d| d.iter().find(|e| e.id == id))
            .filter(|e| !e.hidden)
    }

    /// The effective entries, in directory order then file order.
    pub(crate) fn effective(&self) -> Vec<&Entry> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for d in &self.dirs {
            for e in d {
                if seen.insert(e.id.as_str()) && !e.hidden {
                    out.push(e);
                }
            }
        }
        out
    }

    /// Read the applications directories (most important first).
    pub(crate) fn scan(dirs: &[PathBuf], locales: &[String]) -> Index {
        let mut budget = MAX_FILES;
        let mut out = Index::default();
        for d in dirs {
            let mut list = Vec::new();
            walk(d, d, 0, locales, &mut budget, &mut list);
            out.dirs.push(list);
        }
        out
    }
}

fn walk(
    root: &Path,
    dir: &Path,
    depth: usize,
    locales: &[String],
    budget: &mut usize,
    out: &mut Vec<Entry>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<_> = rd.filter_map(Result::ok).collect();
    names.sort_by_key(std::fs::DirEntry::file_name);
    for de in names {
        if *budget == 0 {
            return;
        }
        let p = de.path();
        let Ok(ft) = de.file_type() else { continue };
        if ft.is_dir() {
            if depth < MAX_DEPTH {
                walk(root, &p, depth + 1, locales, budget, out);
            }
            continue;
        }
        if p.extension().is_none_or(|x| x != "desktop") {
            continue;
        }
        // Symlinks are followed (distributions link entries); the size check is on the target.
        let Ok(md) = std::fs::metadata(&p) else {
            continue;
        };
        if !md.is_file() || md.len() > MAX_FILE {
            continue;
        }
        *budget -= 1;
        let Ok(rel) = p.strip_prefix(root) else {
            continue;
        };
        let id = rel.to_string_lossy().replace('/', "-");
        if out.iter().any(|e: &Entry| e.id == id) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p)
            && let Some(e) = parse(&text, &id, &p, locales)
        {
            out.push(e);
        }
    }
}

/// `TryExec` (or `Exec`'s program) resolves to an executable: absolute, or on `path`.
pub(crate) fn installed(prog: &str, path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let exe = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if prog.contains('/') {
        return Path::new(prog).is_absolute() && exe(Path::new(prog));
    }
    !prog.is_empty()
        && path
            .split(':')
            .any(|d| !d.is_empty() && exe(&Path::new(d).join(prog)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUPE: &str = "\
# comment
[Desktop Entry]
Type=Application
Name=Image Viewer
Name[de]=Bildbetrachter
Name[de_AT]=Bildanzeige
Exec=loupe %U
Icon=org.gnome.Loupe
MimeType=image/png;image/jpeg;image/x\\;odd;
Terminal=false
NotShowIn=KDE;

[Desktop Action new-window]
Name=New Window
Exec=loupe --new %U
";

    #[test]
    fn parses_the_desktop_entry_group_only() {
        let e = parse(LOUPE, "org.gnome.Loupe.desktop", Path::new("/x"), &[]).unwrap();
        assert_eq!(e.name, "Image Viewer");
        assert_eq!(e.exec, "loupe %U");
        assert_eq!(e.mime, ["image/png", "image/jpeg", "image/x;odd"]);
        assert!(!e.terminal);
        assert_eq!(e.not_show_in, ["KDE"]);
        assert!(e.shown(&["abyss".into()]));
        assert!(!e.shown(&["kde".into()]));
    }

    #[test]
    fn localized_name_follows_the_matching_order() {
        let keys = locale_keys("de_AT.UTF-8@euro");
        assert_eq!(keys, ["de_AT@euro", "de_AT", "de@euro", "de"]);
        let e = parse(LOUPE, "l.desktop", Path::new("/x"), &keys).unwrap();
        assert_eq!(e.name, "Bildanzeige");
        let e = parse(LOUPE, "l.desktop", Path::new("/x"), &locale_keys("de_CH")).unwrap();
        assert_eq!(e.name, "Bildbetrachter");
        assert!(locale_keys("C.UTF-8").is_empty());
    }

    #[test]
    fn rejects_non_applications_and_keeps_hidden_shadows() {
        assert!(
            parse(
                "[Desktop Entry]\nType=Link\nName=x\nURL=y\n",
                "a",
                Path::new("/"),
                &[]
            )
            .is_none()
        );
        assert!(
            parse(
                "[Desktop Entry]\nType=Application\nName=x\n",
                "a",
                Path::new("/"),
                &[]
            )
            .is_none()
        );
        let h = parse(
            "[Desktop Entry]\nHidden=true\n",
            "a.desktop",
            Path::new("/"),
            &[],
        )
        .unwrap();
        assert!(h.hidden);
        let idx = Index {
            dirs: vec![
                vec![h],
                vec![parse(LOUPE, "a.desktop", Path::new("/"), &[]).unwrap()],
            ],
        };
        assert!(
            idx.get("a.desktop").is_none(),
            "a hidden entry hides lower ones"
        );
        assert!(idx.effective().is_empty());
    }

    #[test]
    fn names_are_bounded_and_clean() {
        let t = format!(
            "[Desktop Entry]\nType=Application\nExec=x\nName=a\\nb{}\n",
            "é".repeat(200)
        );
        let e = parse(&t, "x.desktop", Path::new("/"), &[]).unwrap();
        assert!(e.name.len() <= MAX_NAME);
        assert!(e.name.starts_with("ab"));
    }

    #[test]
    fn scan_builds_ids_from_subdirectories() {
        let dir = std::env::temp_dir().join(format!("apoth-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(dir.join("vendor/app.desktop"), LOUPE).unwrap();
        std::fs::write(dir.join("big.desktop"), "x".repeat(MAX_FILE as usize + 1)).unwrap();
        std::fs::write(dir.join("readme.txt"), LOUPE).unwrap();
        let idx = Index::scan(std::slice::from_ref(&dir), &[]);
        let ids: Vec<&str> = idx.dirs[0].iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["vendor-app.desktop"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_checks_path_and_absolute() {
        assert!(installed("sh", "/usr/bin:/bin"));
        assert!(!installed("definitely-not-a-program-xyz", "/usr/bin:/bin"));
        assert!(!installed("relative/sh", "/usr/bin"));
        assert!(installed("/bin/sh", ""));
    }
}
