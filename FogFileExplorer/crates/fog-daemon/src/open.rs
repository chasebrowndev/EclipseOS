// SPDX-License-Identifier: AGPL-3.0-only

//! Open with: shared-mime-info globs, `mimeapps.list`, `.desktop` Exec
//! expansion and a detached launch (FOG §Desktop interop).
//!
//! Content sniffing is phase 2 in the spec; here it is only a text-or-binary
//! guess for names no glob matches.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustix::io::Errno;

/// XDG base directories, resolved once per request.
#[derive(Debug, Clone, Default)]
pub struct Xdg {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    /// `XDG_CURRENT_DESKTOP`, lowercased, for `$desktop-mimeapps.list`.
    pub desktops: Vec<String>,
}

impl Xdg {
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let one = |var: &str, dflt: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(dflt))
        };
        let many = |var: &str, dflt: &[&str]| {
            let v: Vec<PathBuf> = std::env::var_os(var)
                .map(|s| {
                    std::env::split_paths(&s)
                        .filter(|p| p.is_absolute())
                        .collect()
                })
                .unwrap_or_default();
            if v.is_empty() {
                dflt.iter().map(PathBuf::from).collect()
            } else {
                v
            }
        };
        Self {
            config_home: one("XDG_CONFIG_HOME", ".config"),
            config_dirs: many("XDG_CONFIG_DIRS", &["/etc/xdg"]),
            data_home: one("XDG_DATA_HOME", ".local/share"),
            data_dirs: many("XDG_DATA_DIRS", &["/usr/local/share", "/usr/share"]),
            desktops: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .filter(|d| !d.is_empty())
                .map(str::to_lowercase)
                .collect(),
            home,
        }
    }

    /// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`.
    fn data(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.data_home).chain(&self.data_dirs)
    }

    /// `mimeapps.list` files, highest precedence first (mime-apps spec).
    fn mimeapps_lists(&self) -> Vec<PathBuf> {
        let dirs = std::iter::once(self.config_home.clone())
            .chain(self.config_dirs.iter().cloned())
            .chain(self.data().map(|d| d.join("applications")));
        let mut out = Vec::new();
        for dir in dirs {
            for d in &self.desktops {
                out.push(dir.join(format!("{d}-mimeapps.list")));
            }
            out.push(dir.join("mimeapps.list"));
        }
        out
    }
}

/// Open `path` (a non-directory) with `app`, else its default handler.
/// `ENOEXEC` means no usable application.
pub fn open(xdg: &Xdg, path: &Path, app: Option<&str>) -> io::Result<()> {
    let entry = match app {
        Some(id) => find_desktop(xdg, id).ok_or(Errno::NOEXEC)?,
        None => {
            let mime = mime_type(xdg, path);
            resolve(xdg, &mime).ok_or(Errno::NOEXEC)?.1
        }
    };
    let mut argv = expand_exec(&entry.exec, path)?;
    if entry.terminal {
        let term = ["xdg-terminal-exec", "foot"]
            .into_iter()
            .find_map(which)
            .ok_or(Errno::NOEXEC)?;
        argv.insert(0, term.into_os_string());
    }
    spawn(argv)
}

// ---- MIME type -------------------------------------------------------------

/// The MIME type of `path`: the best shared-mime-info glob, else a text or
/// binary guess from the first bytes.
pub fn mime_type(xdg: &Xdg, path: &Path) -> String {
    let name = path.file_name().map(OsStr::as_bytes).unwrap_or_default();
    let globs: Vec<String> = xdg
        .data()
        .filter_map(|d| fs::read_to_string(d.join("mime/globs2")).ok())
        .collect();
    if let Some(m) = glob_match(globs.iter().map(String::as_str), name) {
        return m;
    }
    let mut head = [0u8; 512];
    let n = fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    if looks_text(&head[..n]) {
        "text/plain".into()
    } else {
        "application/octet-stream".into()
    }
}

fn looks_text(b: &[u8]) -> bool {
    if b.contains(&0) {
        return false;
    }
    match std::str::from_utf8(b) {
        Ok(_) => true,
        // A multibyte char cut off by the read is still text.
        Err(e) => e.error_len().is_none(),
    }
}

/// Best `globs2` match: literal names beat patterns, then weight, then the
/// longest pattern. Earlier files win ties.
pub fn glob_match<'a>(files: impl IntoIterator<Item = &'a str>, name: &[u8]) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let mut best: Option<((bool, u32, usize), &str)> = None;
    for line in files.into_iter().flat_map(str::lines) {
        if line.starts_with('#') {
            continue;
        }
        let mut f = line.split(':');
        let (Some(w), Some(mime), Some(glob)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if glob == "__NOGLOBS__" {
            continue;
        }
        let cs = f.next().is_some_and(|fl| fl.split(',').any(|x| x == "cs"));
        let hay: &[u8] = if cs { name } else { &lower };
        let pat = if cs {
            glob.as_bytes().to_vec()
        } else {
            glob.as_bytes().to_ascii_lowercase()
        };
        if !fnmatch(&pat, hay) {
            continue;
        }
        let literal = !glob.contains(['*', '?', '[']);
        let key = (literal, w.parse().unwrap_or(50), glob.len());
        if best.is_none_or(|(k, _)| key > k) {
            best = Some((key, mime));
        }
    }
    best.map(|(_, m)| m.to_string())
}

/// Shell-style glob: `*`, `?`, `[...]` (with `!` negation and ranges).
fn fnmatch(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    star = Some((pi, si));
                    pi += 1;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                b'[' => {
                    if let Some((ok, len)) = class(&p[pi..], s[si]) {
                        if ok {
                            pi += len;
                            si += 1;
                            continue;
                        }
                    } else if s[si] == b'[' {
                        pi += 1;
                        si += 1;
                        continue;
                    }
                }
                c if c == s[si] => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, ss)) => {
                pi = sp + 1;
                si = ss + 1;
                star = Some((sp, ss + 1));
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Match `c` against the bracket class at the start of `p`; returns whether it
/// matched and the class length, or `None` for an unterminated `[`.
fn class(p: &[u8], c: u8) -> Option<(bool, usize)> {
    let mut i = 1;
    let neg = matches!(p.get(i), Some(b'!' | b'^'));
    if neg {
        i += 1;
    }
    let mut hit = false;
    let mut first = true;
    loop {
        let lo = *p.get(i)?;
        if lo == b']' && !first {
            return Some((hit != neg, i + 1));
        }
        first = false;
        if p.get(i + 1) == Some(&b'-') && p.get(i + 2).is_some_and(|&h| h != b']') {
            hit |= (lo..=p[i + 2]).contains(&c);
            i += 3;
        } else {
            hit |= lo == c;
            i += 1;
        }
    }
}

/// `mime` and its ancestors from `mime/subclasses`, nearest first. Every
/// `text/*` falls back to `text/plain`.
fn lineage(xdg: &Xdg, mime: &str) -> Vec<String> {
    let subclasses: Vec<String> = xdg
        .data()
        .filter_map(|d| fs::read_to_string(d.join("mime/subclasses")).ok())
        .collect();
    let mut out = vec![mime.to_string()];
    let mut i = 0;
    while i < out.len() {
        for line in subclasses.iter().flat_map(|s| s.lines()) {
            if let Some((child, parent)) = line.split_once(' ') {
                if child == out[i] && !out.iter().any(|m| m == parent) {
                    out.push(parent.to_string());
                }
            }
        }
        i += 1;
    }
    if mime.starts_with("text/") && !out.iter().any(|m| m == "text/plain") {
        out.push("text/plain".into());
    }
    out
}

// ---- Handler resolution -----------------------------------------------------

/// Parsed `mimeapps.list` or `mimeinfo.cache` groups relevant to one type.
#[derive(Default)]
struct Assoc {
    default: Vec<String>,
    added: Vec<String>,
    removed: Vec<String>,
}

fn assoc(text: &str, mime: &str) -> Assoc {
    let mut a = Assoc::default();
    let mut group = "";
    for line in text.lines().map(str::trim) {
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = g;
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != mime {
            continue;
        }
        let ids = v.split(';').map(str::trim).filter(|s| !s.is_empty());
        let dest = match group {
            "Default Applications" => &mut a.default,
            "Added Associations" | "MIME Cache" => &mut a.added,
            "Removed Associations" => &mut a.removed,
            _ => continue,
        };
        dest.extend(ids.map(String::from));
    }
    a
}

/// The handler for `mime` (or its nearest ancestor that has one): its
/// desktop id and entry.
pub fn resolve(xdg: &Xdg, mime: &str) -> Option<(String, DesktopEntry)> {
    let lists: Vec<String> = xdg
        .mimeapps_lists()
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    let caches: Vec<String> = xdg
        .data()
        .filter_map(|d| fs::read_to_string(d.join("applications/mimeinfo.cache")).ok())
        .collect();
    lineage(xdg, mime)
        .iter()
        .find_map(|m| resolve_one(xdg, &lists, &caches, m))
}

fn resolve_one(
    xdg: &Xdg,
    lists: &[String],
    caches: &[String],
    mime: &str,
) -> Option<(String, DesktopEntry)> {
    let usable = |id: &String| find_desktop(xdg, id).map(|e| (id.clone(), e));
    let parsed: Vec<Assoc> = lists.iter().map(|t| assoc(t, mime)).collect();
    // Default Applications, in precedence order.
    if let Some(hit) = parsed.iter().flat_map(|a| &a.default).find_map(usable) {
        return Some(hit);
    }
    // Added Associations; a file's removals hide lower-precedence additions.
    let mut removed: HashSet<&str> = HashSet::new();
    for a in &parsed {
        removed.extend(a.removed.iter().map(String::as_str));
        if let Some(hit) = a
            .added
            .iter()
            .filter(|id| !removed.contains(id.as_str()))
            .find_map(usable)
        {
            return Some(hit);
        }
    }
    caches
        .iter()
        .flat_map(|t| assoc(t, mime).added)
        .filter(|id| !removed.contains(id.as_str()))
        .find_map(|id| usable(&id))
}

/// The fields of a `.desktop` file Fog uses to launch it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    /// `Exec`, string-unescaped; field codes still in place.
    pub exec: String,
    pub terminal: bool,
}

impl DesktopEntry {
    /// `None` unless the file is a visible `Type=Application` with `Exec`.
    pub fn parse(text: &str) -> Option<Self> {
        let (mut exec, mut ty, mut hidden, mut terminal) = (None, None, false, false);
        let mut in_main = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_main = line == "[Desktop Entry]";
                continue;
            }
            if !in_main || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "Exec" => exec = Some(unescape(v)),
                "Type" => ty = Some(v.to_string()),
                "Hidden" => hidden = v == "true",
                "Terminal" => terminal = v == "true",
                _ => {}
            }
        }
        (ty.as_deref() == Some("Application") && !hidden)
            .then_some(())
            .and(exec)
            .filter(|e| !e.trim().is_empty())
            .map(|exec| Self { exec, terminal })
    }
}

/// Desktop entry string unescaping: `\s \n \t \r \\`.
fn unescape(v: &str) -> String {
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
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Load `id` from `$XDG_DATA_HOME/applications` then `$XDG_DATA_DIRS`.
/// Ids that could escape the applications directory are refused.
pub fn find_desktop(xdg: &Xdg, id: &str) -> Option<DesktopEntry> {
    if !id.ends_with(".desktop") || id.starts_with('.') || id.contains('/') {
        return None;
    }
    xdg.data()
        .find_map(|d| fs::read_to_string(d.join("applications").join(id)).ok())
        .and_then(|t| DesktopEntry::parse(&t))
}

// ---- Exec expansion and launch ----------------------------------------------

/// Split `exec` per the desktop entry quoting rules and expand field codes
/// for one local file: `%f %F` to the path, `%u %U` to its `file://` URI,
/// `%%` to `%`; `%i %c %k` and the deprecated codes are dropped. With no
/// file code the path is appended.
pub fn expand_exec(exec: &str, path: &Path) -> io::Result<Vec<OsString>> {
    let file = path.as_os_str().as_bytes();
    let uri = file_uri(file);
    let mut used = false;
    let mut argv = Vec::new();
    for tok in split_exec(exec).ok_or(Errno::NOEXEC)? {
        let t = tok.as_bytes();
        if matches!(t, b"%i" | b"%c" | b"%k") {
            continue;
        }
        let mut out = Vec::with_capacity(t.len());
        let mut i = 0;
        while i < t.len() {
            if t[i] != b'%' || i + 1 == t.len() {
                out.push(t[i]);
                i += 1;
                continue;
            }
            match t[i + 1] {
                b'f' | b'F' => {
                    out.extend_from_slice(file);
                    used = true;
                }
                b'u' | b'U' => {
                    out.extend_from_slice(uri.as_bytes());
                    used = true;
                }
                b'%' => out.push(b'%'),
                _ => {}
            }
            i += 2;
        }
        // A token that was only a dropped code vanishes.
        if !out.is_empty() || t.is_empty() {
            argv.push(OsString::from_vec(out));
        }
    }
    if argv.is_empty() {
        return Err(Errno::NOEXEC.into());
    }
    if !used {
        argv.push(path.as_os_str().to_owned());
    }
    Ok(argv)
}

/// Tokenize an `Exec` value. Double quotes group; inside them `\` escapes
/// the next char. `None` for an unterminated quote.
fn split_exec(exec: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut have) = (false, false);
    let mut it = exec.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                quoted = !quoted;
                have = true;
            }
            '\\' if quoted => cur.push(it.next()?),
            c if c.is_whitespace() && !quoted => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if have {
        out.push(cur);
    }
    Some(out)
}

/// `file://` URI with everything outside RFC 3986 unreserved and `/`
/// percent-encoded.
pub fn file_uri(path: &[u8]) -> String {
    let mut s = String::from("file://");
    for &b in path {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// An executable named `name` on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// Launch `argv` outside fogd's service cgroup: in a transient user scope
/// when a systemd user manager is running, else in a new session. stdio is
/// /dev/null and no fogd descriptor survives exec. The child is reaped on a
/// detached thread.
fn spawn(mut argv: Vec<OsString>) -> io::Result<()> {
    let manager = std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|d| Path::new(&d).join("systemd/private").exists());
    if let Some(run) = which("systemd-run").filter(|_| manager) {
        let pre = [
            run.into_os_string(),
            "--user".into(),
            "--scope".into(),
            "--collect".into(),
            "--quiet".into(),
            "--".into(),
        ];
        argv.splice(0..0, pre);
    }
    cloexec_all();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()?;
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Mark every descriptor above stderr close-on-exec. std and tokio open
/// theirs with `O_CLOEXEC` already; this covers inherited ones, such as a
/// socket-activation fd.
fn cloexec_all() {
    let Ok(dir) = fs::read_dir("/proc/self/fd") else {
        return;
    };
    let fds: Vec<i32> = dir
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|&fd| fd > 2)
        .collect();
    for fd in fds {
        // SAFETY: fcntl on a possibly stale number only fails with EBADF.
        let fd = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
        if let Ok(fl) = rustix::io::fcntl_getfd(fd) {
            let _ = rustix::io::fcntl_setfd(fd, fl | rustix::io::FdFlags::CLOEXEC);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xdg(root: &Path) -> Xdg {
        Xdg {
            home: root.join("home"),
            config_home: root.join("config"),
            config_dirs: vec![root.join("etc")],
            data_home: root.join("data"),
            data_dirs: vec![root.join("usr")],
            desktops: vec!["abyss".into()],
        }
    }

    fn put(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn app(id: &str, dir: &Path) {
        put(
            &dir.join("applications").join(id),
            &format!("[Desktop Entry]\nType=Application\nName={id}\nExec={id} %f\n"),
        );
    }

    fn resolved(x: &Xdg, mime: &str) -> Option<String> {
        resolve(x, mime).map(|(id, _)| id)
    }

    #[test]
    fn mimeapps_precedence() {
        let t = tempfile::tempdir().unwrap();
        let x = xdg(t.path());
        for id in [
            "cache.desktop",
            "sys.desktop",
            "user.desktop",
            "added.desktop",
            "de.desktop",
        ] {
            app(id, &x.data_dirs[0]);
        }
        let cache = x.data_dirs[0].join("applications/mimeinfo.cache");
        put(&cache, "[MIME Cache]\ntext/plain=cache.desktop;\n");
        assert_eq!(resolved(&x, "text/plain").as_deref(), Some("cache.desktop"));

        // Added Associations beat mimeinfo.cache.
        put(
            &x.config_dirs[0].join("mimeapps.list"),
            "[Added Associations]\ntext/plain=added.desktop;\n",
        );
        assert_eq!(resolved(&x, "text/plain").as_deref(), Some("added.desktop"));

        // A default anywhere beats every added association.
        put(
            &x.data_dirs[0].join("applications/mimeapps.list"),
            "[Default Applications]\ntext/plain=sys.desktop\n",
        );
        assert_eq!(resolved(&x, "text/plain").as_deref(), Some("sys.desktop"));

        // XDG_CONFIG_HOME beats the data dirs; a missing app is skipped.
        put(
            &x.config_home.join("mimeapps.list"),
            "[Default Applications]\ntext/plain=gone.desktop;user.desktop;\n",
        );
        assert_eq!(resolved(&x, "text/plain").as_deref(), Some("user.desktop"));

        // $desktop-mimeapps.list beats plain mimeapps.list in the same dir.
        put(
            &x.config_home.join("abyss-mimeapps.list"),
            "[Default Applications]\ntext/plain=de.desktop\n",
        );
        assert_eq!(resolved(&x, "text/plain").as_deref(), Some("de.desktop"));
    }

    #[test]
    fn removed_hides_lower_associations() {
        let t = tempfile::tempdir().unwrap();
        let x = xdg(t.path());
        app("a.desktop", &x.data_home);
        app("b.desktop", &x.data_home);
        put(
            &x.config_home.join("mimeapps.list"),
            "[Removed Associations]\nimage/png=a.desktop;\n",
        );
        put(
            &x.data_dirs[0].join("applications/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=a.desktop;b.desktop;\n",
        );
        assert_eq!(resolved(&x, "image/png").as_deref(), Some("b.desktop"));
    }

    #[test]
    fn falls_back_to_parent_type() {
        let t = tempfile::tempdir().unwrap();
        let x = xdg(t.path());
        app("ed.desktop", &x.data_home);
        put(
            &x.data_dirs[0].join("mime/subclasses"),
            "text/x-rust text/plain\n",
        );
        put(
            &x.config_home.join("mimeapps.list"),
            "[Default Applications]\ntext/plain=ed.desktop\n",
        );
        assert_eq!(resolved(&x, "text/x-rust").as_deref(), Some("ed.desktop"));
        assert_eq!(resolved(&x, "text/x-other").as_deref(), Some("ed.desktop"));
        assert_eq!(resolved(&x, "image/png"), None);
    }

    #[test]
    fn desktop_entry_parse() {
        let e = DesktopEntry::parse(
            "# c\n[Desktop Entry]\nType=Application\nName[de]=X\nExec=foo\\sbar %U\nTerminal=true\n\
             [Desktop Action new]\nExec=other\n",
        )
        .unwrap();
        assert_eq!(e.exec, "foo bar %U");
        assert!(e.terminal);
        assert!(DesktopEntry::parse("[Desktop Entry]\nType=Link\nExec=x\n").is_none());
        assert!(
            DesktopEntry::parse("[Desktop Entry]\nType=Application\nHidden=true\nExec=x\n")
                .is_none()
        );
        assert!(DesktopEntry::parse("[Desktop Entry]\nType=Application\n").is_none());
        let t = tempfile::tempdir().unwrap();
        let x = xdg(t.path());
        assert!(find_desktop(&x, "../../etc/passwd").is_none());
        assert!(find_desktop(&x, "a/b.desktop").is_none());
    }

    fn argv(exec: &str, path: &[u8]) -> Vec<Vec<u8>> {
        expand_exec(exec, Path::new(OsStr::from_bytes(path)))
            .unwrap()
            .into_iter()
            .map(OsString::into_vec)
            .collect()
    }

    #[test]
    fn exec_field_codes() {
        let v = |xs: &[&[u8]]| xs.iter().map(|x| x.to_vec()).collect::<Vec<_>>();
        assert_eq!(argv("micro %f", b"/a b"), v(&[b"micro", b"/a b"]));
        assert_eq!(argv("mpv -- %F", b"/x"), v(&[b"mpv", b"--", b"/x"]));
        assert_eq!(
            argv("firefox %u", b"/a b/\xff.html"),
            v(&[b"firefox", b"file:///a%20b/%FF.html"])
        );
        assert_eq!(argv("app %U", b"/x"), v(&[b"app", b"file:///x"]));
        assert_eq!(
            argv("app %i %c %k --name=%c %f", b"/x"),
            v(&[b"app", b"--name=", b"/x"])
        );
        assert_eq!(argv("app 100%% %f", b"/x"), v(&[b"app", b"100%", b"/x"]));
        assert_eq!(argv("app --file=%f", b"/x"), v(&[b"app", b"--file=/x"]));
        // No file code: the path is appended.
        assert_eq!(argv("viewer", b"/x"), v(&[b"viewer", b"/x"]));
        assert_eq!(
            argv(r#""/opt/my app/run" "a \"q\" \\ b" %f"#, b"/x"),
            v(&[b"/opt/my app/run", br#"a "q" \ b"#, b"/x"])
        );
        assert_eq!(
            argv(r#"sh -c "" %f"#, b"/x"),
            v(&[b"sh", b"-c", b"", b"/x"])
        );
        assert!(expand_exec("app \"open", Path::new("/x")).is_err());
        assert!(expand_exec("%i", Path::new("/x")).is_err());
    }

    #[test]
    fn globs() {
        let g = "# c\n50:text/plain:*.txt\n50:text/x-c++src:*.C:cs\n50:text/x-csrc:*.c\n\
                 80:application/x-compressed-tar:*.tar.gz\n50:application/gzip:*.gz\n\
                 10:text/x-makefile:makefile\n50:text/x-makefile:Makefile\n60:image/x-z:*.[ab]z\n";
        let m = |n: &[u8]| glob_match([g], n);
        assert_eq!(m(b"a.TXT").as_deref(), Some("text/plain"));
        assert_eq!(m(b"a.C").as_deref(), Some("text/x-c++src"));
        assert_eq!(m(b"a.c").as_deref(), Some("text/x-csrc"));
        assert_eq!(
            m(b"x.tar.gz").as_deref(),
            Some("application/x-compressed-tar")
        );
        assert_eq!(m(b"x.gz").as_deref(), Some("application/gzip"));
        // Literal beats a heavier pattern.
        assert_eq!(m(b"Makefile").as_deref(), Some("text/x-makefile"));
        assert_eq!(m(b"q.bz").as_deref(), Some("image/x-z"));
        assert_eq!(m(b"q.cz"), None);
        assert!(fnmatch(b"[!a]*", b"bc"));
        assert!(!fnmatch(b"[!a]*", b"ac"));
        assert!(fnmatch(b"*.[0-9]", b"x.7"));
    }

    #[test]
    fn mime_type_sniffs_unknown_names() {
        let t = tempfile::tempdir().unwrap();
        let x = xdg(t.path());
        put(
            &x.data_dirs[0].join("mime/globs2"),
            "50:text/markdown:*.md\n",
        );
        let md = t.path().join("r.md");
        let txt = t.path().join("README");
        let bin = t.path().join("blob");
        fs::write(&md, b"\0").unwrap();
        fs::write(&txt, "héllo").unwrap();
        fs::write(&bin, b"\x7fELF\0\0").unwrap();
        assert_eq!(mime_type(&x, &md), "text/markdown");
        assert_eq!(mime_type(&x, &txt), "text/plain");
        assert_eq!(mime_type(&x, &bin), "application/octet-stream");
    }
}
