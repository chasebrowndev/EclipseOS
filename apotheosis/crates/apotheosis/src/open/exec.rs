// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop entry `Exec` key to an argv (Desktop Entry Specification 1.5, "The Exec key"),
//! for "Open with" (T-14 §11). Never through a shell: the result is handed to `execve`.
//!
//! - The general string escapes (`\s \n \t \r \\`) are undone first, then the quoting rule:
//!   an argument may be quoted whole in `"`, inside which `\"`, `` \` ``, `\$` and `\\` stand
//!   for the character.
//! - Field codes: `%f %u` (one file or URL, may sit inside an unquoted argument), `%F %U` (a
//!   list, an argument of their own), `%i` (`--icon <Icon>`, own argument), `%c` (name), `%k`
//!   (the entry's path), `%%`; the deprecated `%d %D %n %N %v %m` are dropped; any other code
//!   makes the line invalid. At most one of `%f %u %F %U`.
//! - A field code inside a quoted argument is refused (the specification leaves it undefined,
//!   and in `sh -c "… %f"` it would put a file name into shell code).
//! - `%f`/`%F` need a local file: a non-`file:` URL is refused, never downloaded.
//! - An `Exec` with no file code gets the target appended as its own argument (the
//!   specification is silent; the user asked this app to open this target).
//!
//! Local paths are always absolute (`/…`), so a file name can never read as an option.

use std::path::Path;

/// Why an `Exec` line cannot run for a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExecError {
    Empty,
    Unterminated,
    UnknownCode(char),
    TwoFileCodes,
    NotAlone(char),
    CodeInQuotes,
    NeedsLocalFile,
}

impl ExecError {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            ExecError::Empty => "the app has no command",
            ExecError::Unterminated => "the app's command has an open quote",
            ExecError::UnknownCode(_) => "the app's command has an unknown field code",
            ExecError::TwoFileCodes => "the app's command names files twice",
            ExecError::NotAlone(_) => "the app's command misuses a list field code",
            ExecError::CodeInQuotes => "the app's command quotes a field code",
            ExecError::NeedsLocalFile => "the app opens only local files",
        }
    }
}

/// What the line is expanded for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExecTarget<'a> {
    /// The absolute local path, for a file (or a `file:` URL that names this host).
    pub(crate) path: Option<&'a Path>,
    /// The URL: `file://…` for a local file, else the link itself.
    pub(crate) url: &'a str,
}

/// The entry fields `%i %c %k` read.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ExecEntry<'a> {
    pub(crate) name: &'a str,
    pub(crate) icon: Option<&'a str>,
    pub(crate) path: Option<&'a str>,
}

/// One argument: its pieces, each quoted or not.
#[derive(Debug, Default)]
struct Arg {
    parts: Vec<(String, bool)>,
}

/// Undo the general string escapes (`\s \n \t \r \\`); any other backslash stays for the
/// quoting rule.
fn unescape_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            _ => {
                out.push('\\');
                continue;
            }
        }
        it.next();
    }
    out
}

/// Split into arguments under the quoting rule.
fn split(line: &str) -> Result<Vec<Arg>, ExecError> {
    let mut args: Vec<Arg> = Vec::new();
    let mut cur: Option<Arg> = None;
    let mut it = line.chars();
    while let Some(c) = it.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if let Some(a) = cur.take() {
                    args.push(a);
                }
            }
            '"' => {
                let mut s = String::new();
                loop {
                    match it.next() {
                        None => return Err(ExecError::Unterminated),
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(e @ ('"' | '`' | '$' | '\\')) => s.push(e),
                            Some(o) => {
                                s.push('\\');
                                s.push(o);
                            }
                            None => return Err(ExecError::Unterminated),
                        },
                        Some(o) => s.push(o),
                    }
                }
                cur.get_or_insert_with(Arg::default).parts.push((s, true));
            }
            o => {
                let a = cur.get_or_insert_with(Arg::default);
                match a.parts.last_mut() {
                    Some((s, false)) => s.push(o),
                    _ => a.parts.push((o.to_string(), false)),
                }
            }
        }
    }
    if let Some(a) = cur.take() {
        args.push(a);
    }
    Ok(args)
}

/// Expand an `Exec` value for one target.
pub(crate) fn expand(
    raw: &str,
    target: &ExecTarget<'_>,
    entry: &ExecEntry<'_>,
) -> Result<Vec<String>, ExecError> {
    let line = unescape_string(raw);
    let args = split(&line)?;
    let mut out: Vec<String> = Vec::new();
    let mut file_codes = 0;
    let file = |code: char| -> Result<String, ExecError> {
        match code {
            'f' | 'F' => target
                .path
                .map(|p| p.to_string_lossy().into_owned())
                .ok_or(ExecError::NeedsLocalFile),
            // Local files go as paths: every app understands those.
            _ => Ok(target.path.map_or_else(
                || target.url.to_string(),
                |p| p.to_string_lossy().into_owned(),
            )),
        }
    };
    for a in &args {
        // An argument that is exactly one list code expands to its own arguments.
        if let [(s, false)] = a.parts.as_slice()
            && s.len() == 2
            && s.starts_with('%')
        {
            let code = s.as_bytes()[1] as char;
            match code {
                'F' | 'U' | 'f' | 'u' => {
                    file_codes += 1;
                    out.push(file(code)?);
                    continue;
                }
                'i' => {
                    if let Some(icon) = entry.icon.filter(|i| !i.is_empty()) {
                        out.push("--icon".into());
                        out.push(icon.into());
                    }
                    continue;
                }
                'd' | 'D' | 'n' | 'N' | 'v' | 'm' => continue,
                _ => {}
            }
        }
        let mut s = String::new();
        let mut dropped_only = true;
        for (part, quoted) in &a.parts {
            if *quoted {
                if part.contains('%') {
                    let stripped = part.replace("%%", "");
                    if stripped.contains('%') {
                        return Err(ExecError::CodeInQuotes);
                    }
                }
                s.push_str(&part.replace("%%", "%"));
                dropped_only = false;
                continue;
            }
            let mut it = part.chars();
            while let Some(c) = it.next() {
                if c != '%' {
                    s.push(c);
                    dropped_only = false;
                    continue;
                }
                let Some(code) = it.next() else {
                    return Err(ExecError::UnknownCode('%'));
                };
                match code {
                    '%' => {
                        s.push('%');
                        dropped_only = false;
                    }
                    'f' | 'u' => {
                        file_codes += 1;
                        s.push_str(&file(code)?);
                        dropped_only = false;
                    }
                    'F' | 'U' | 'i' => return Err(ExecError::NotAlone(code)),
                    'c' => {
                        s.push_str(entry.name);
                        dropped_only = false;
                    }
                    'k' => {
                        s.push_str(entry.path.unwrap_or(""));
                        dropped_only = false;
                    }
                    'd' | 'D' | 'n' | 'N' | 'v' | 'm' => {}
                    o => return Err(ExecError::UnknownCode(o)),
                }
            }
        }
        // An argument that was only deprecated codes disappears.
        if !(dropped_only && s.is_empty()) {
            out.push(s);
        }
    }
    if file_codes > 1 {
        return Err(ExecError::TwoFileCodes);
    }
    if out.first().is_none_or(String::is_empty) {
        return Err(ExecError::Empty);
    }
    if file_codes == 0 {
        out.push(file('u')?);
    }
    Ok(out)
}

/// `file://` URL of an absolute local path, percent-encoding everything but the RFC 3986
/// unreserved set and `/`.
pub(crate) fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn file_t(p: &Path) -> (String, PathBuf) {
        (file_url(p), p.to_path_buf())
    }

    fn run(raw: &str, path: Option<&str>, url: &str) -> Result<Vec<String>, ExecError> {
        let p = path.map(PathBuf::from);
        let t = ExecTarget {
            path: p.as_deref(),
            url,
        };
        let e = ExecEntry {
            name: "Viewer",
            icon: Some("viewer"),
            path: Some("/usr/share/applications/viewer.desktop"),
        };
        expand(raw, &t, &e)
    }

    #[test]
    fn single_and_list_codes() {
        let f = Some("/tmp/a b.png");
        let u = "file:///tmp/a%20b.png";
        assert_eq!(run("viewer %f", f, u).unwrap(), ["viewer", "/tmp/a b.png"]);
        assert_eq!(run("viewer %F", f, u).unwrap(), ["viewer", "/tmp/a b.png"]);
        assert_eq!(run("viewer %u", f, u).unwrap(), ["viewer", "/tmp/a b.png"]);
        assert_eq!(run("viewer %U", f, u).unwrap(), ["viewer", "/tmp/a b.png"]);
        assert_eq!(
            run("browser %u", None, "https://x.org/?a=1&b").unwrap(),
            ["browser", "https://x.org/?a=1&b"]
        );
        assert_eq!(
            run("viewer --file=%f", f, u).unwrap(),
            ["viewer", "--file=/tmp/a b.png"]
        );
        // %f with a remote URL is refused, never downloaded.
        assert_eq!(
            run("viewer %f", None, "https://x"),
            Err(ExecError::NeedsLocalFile)
        );
        assert_eq!(
            run("viewer %F", None, "https://x"),
            Err(ExecError::NeedsLocalFile)
        );
    }

    #[test]
    fn other_codes() {
        let f = Some("/tmp/x");
        assert_eq!(
            run("viewer %i --name %c %k %f", f, "file:///tmp/x").unwrap(),
            [
                "viewer",
                "--icon",
                "viewer",
                "--name",
                "Viewer",
                "/usr/share/applications/viewer.desktop",
                "/tmp/x"
            ]
        );
        assert_eq!(
            run("app 100%% %f", f, "").unwrap(),
            ["app", "100%", "/tmp/x"]
        );
        // Deprecated codes vanish, alone or inside an argument.
        assert_eq!(
            run("app %d %m -x%n %f", f, "").unwrap(),
            ["app", "-x", "/tmp/x"]
        );
        assert_eq!(run("app %z %f", f, ""), Err(ExecError::UnknownCode('z')));
        assert_eq!(run("app %f %u", f, ""), Err(ExecError::TwoFileCodes));
        assert_eq!(run("app --x=%F", f, ""), Err(ExecError::NotAlone('F')));
        assert_eq!(run("app -%i", f, ""), Err(ExecError::NotAlone('i')));
        assert_eq!(run("app %", f, ""), Err(ExecError::UnknownCode('%')));
    }

    #[test]
    fn quoting_and_escapes() {
        let f = Some("/tmp/x");
        // `"a \"b\" \$c \\d"` in the file: the general escape turns `\\\\` into `\\` first.
        let raw = r#""/opt/my app/bin" "say \"hi\" \\$HOME \\\\d" %f"#;
        assert_eq!(
            run(raw, f, "").unwrap(),
            ["/opt/my app/bin", "say \"hi\" $HOME \\d", "/tmp/x"]
        );
        // `\s` is a space before the quoting rule runs: it separates unless quoted.
        assert_eq!(
            run(r"app\sname %f", f, "").unwrap(),
            ["app", "name", "/tmp/x"]
        );
        assert_eq!(
            run(r#""app\sname" %f"#, f, "").unwrap(),
            ["app name", "/tmp/x"]
        );
        assert_eq!(run("app \"open %f", f, ""), Err(ExecError::Unterminated));
        // Field codes inside quotes are refused: `sh -c "… %f"` would run the file name.
        assert_eq!(
            run("sh -c \"viewer %f\"", f, ""),
            Err(ExecError::CodeInQuotes)
        );
        assert_eq!(
            run("app \"100%%\" %f", f, "").unwrap(),
            ["app", "100%", "/tmp/x"]
        );
        // Shell metacharacters are plain bytes: there is no shell.
        assert_eq!(
            run("app ; rm %f", f, "").unwrap(),
            ["app", ";", "rm", "/tmp/x"]
        );
    }

    #[test]
    fn no_file_code_appends_and_empty_fails() {
        assert_eq!(run("app", Some("/tmp/x"), "").unwrap(), ["app", "/tmp/x"]);
        assert_eq!(run("app", None, "https://x").unwrap(), ["app", "https://x"]);
        assert_eq!(run("", Some("/tmp/x"), ""), Err(ExecError::Empty));
        assert_eq!(run("%d", Some("/tmp/x"), ""), Err(ExecError::Empty));
        // A hostile file name stays one argument.
        let evil = "/tmp/$(rm -rf ~) `x`;.png";
        assert_eq!(run("viewer %f", Some(evil), "").unwrap(), ["viewer", evil]);
    }

    #[test]
    fn file_urls_encode() {
        let (u, _) = file_t(Path::new("/tmp/a b/ü#?.png"));
        assert_eq!(u, "file:///tmp/a%20b/%C3%BC%23%3F.png");
    }
}
