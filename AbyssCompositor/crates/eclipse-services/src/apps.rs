// SPDX-License-Identifier: AGPL-3.0-only
//! The installed applications, read from `.desktop` files.
//!
//! In-house rather than a crate (ADR 0038): the desktop-entry format we
//! actually need is a handful of keys out of one group, and a parser for it is
//! shorter than the audit of a dependency that reads the whole specification.
//!
//! Nothing here is in the TCB. It reads world-readable files and builds an
//! argv; it is the launcher that decides to run one, and the process it spawns
//! is an ordinary child of the human's session with no capability of ours.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One launchable application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The desktop file id (`org.x.Thing.desktop`), which is also what makes
    /// two copies of the same application in two data dirs one entry.
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    /// `Exec` with its field codes resolved and its quoting undone. Never
    /// empty — an entry without a runnable command is dropped at parse time.
    pub argv: Vec<String>,
    /// `Terminal=true`. We do not own a terminal, so this is recorded and
    /// handed to the caller rather than guessed at.
    pub terminal: bool,
    /// `Keywords`, for search. Not shown.
    pub keywords: Vec<String>,
}

/// Where `.desktop` files live, in precedence order: the user's own first.
///
/// Follows the basedir spec rather than reading it from a library, for the
/// same reason the parser is in-house.
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => dirs.push(PathBuf::from(d)),
        _ => {
            if let Some(home) = home.as_ref() {
                dirs.push(home.join(".local/share"));
            }
        }
    }
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let system = if system.is_empty() {
        "/usr/local/share:/usr/share".to_owned()
    } else {
        system
    };
    for d in system.split(':').filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(d));
    }
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// Every application the session can launch, sorted by name.
///
/// An id found in an earlier directory wins: that is what lets a human shadow
/// a system entry with one of their own, which is the whole point of the
/// precedence order.
pub fn scan() -> Vec<Entry> {
    let mut found: BTreeMap<String, Entry> = BTreeMap::new();
    for dir in search_path() {
        collect(&dir, &dir, &mut found);
    }
    let mut entries: Vec<Entry> = found.into_values().collect();
    // Case-insensitive, so "Files" and "firefox" sort where a human looks for
    // them rather than where ASCII puts them.
    entries.sort_by_key(|e| (e.name.to_lowercase(), e.id.clone()));
    entries
}

/// Walks one data directory. Subdirectories are part of the id (`kde-foo.desktop`
/// lives at `kde/foo.desktop`), so the root is carried down to build it.
fn collect(root: &Path, dir: &Path, found: &mut BTreeMap<String, Entry>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        // A data dir that does not exist is the ordinary case, not an error.
        return;
    };
    for item in read.flatten() {
        let path = item.path();
        if path.is_dir() {
            collect(root, &path, found);
            continue;
        }
        if path.extension().is_none_or(|e| e != "desktop") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let id = relative.to_string_lossy().replace('/', "-");
        if found.contains_key(&id) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(entry) = parse(&id, &text) {
            found.insert(id, entry);
        }
    }
}

/// Parses the `[Desktop Entry]` group. Returns `None` for anything that is not
/// a launchable, visible application — a link, a hidden entry, a `NoDisplay`
/// service, one whose `TryExec` is not on the system, or one with no `Exec` at
/// all. A launcher that lists those is a launcher whose rows do nothing.
pub fn parse(id: &str, text: &str) -> Option<Entry> {
    let mut in_group = false;
    let mut keys: BTreeMap<&str, &str> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Localised keys (`Name[de]`) are skipped: we render the untranslated
        // value rather than half-implementing locale matching and getting a
        // different answer from every other launcher on the machine.
        let key = key.trim();
        if key.contains('[') {
            continue;
        }
        keys.insert(key, value.trim());
    }

    if keys.get("Type").copied().unwrap_or("Application") != "Application" {
        return None;
    }
    if truthy(keys.get("NoDisplay")) || truthy(keys.get("Hidden")) {
        return None;
    }
    if let Some(try_exec) = keys.get("TryExec") {
        if !on_path(try_exec) {
            return None;
        }
    }
    let name = (*keys.get("Name")?).to_owned();
    let argv = argv(keys.get("Exec")?);
    if argv.is_empty() {
        return None;
    }
    Some(Entry {
        id: id.to_owned(),
        name,
        comment: keys.get("Comment").map(|c| (*c).to_owned()),
        argv,
        terminal: truthy(keys.get("Terminal")),
        keywords: keys
            .get("Keywords")
            .map(|k| {
                k.split(';')
                    .filter(|k| !k.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn truthy(value: Option<&&str>) -> bool {
    value.is_some_and(|v| *v == "true")
}

/// An absolute `TryExec` must exist; a bare name must be on `PATH`.
fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).exists()))
}

/// Turns an `Exec` value into an argv.
///
/// Quoting per the desktop-entry spec, and every field code dropped: `%f`,
/// `%u` and friends pass a file or a URL we were never given, and `%i`, `%c`
/// and `%k` pass metadata a launcher row has no use for. A literal percent is
/// `%%`. An argument that was *only* a field code disappears rather than
/// becoming an empty string the program would try to open.
pub fn argv(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                // Inside quotes the spec escapes `"`, `` ` ``, `$` and `\`.
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            // A `%%` is a literal percent; anything else after a `%` is a
            // field code, and is dropped whole.
            '%' => {
                if chars.next() == Some('%') {
                    current.push('%');
                    started = true;
                }
            }
            c if c.is_whitespace() && !quoted => {
                if started && !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                current.clear();
                started = false;
            }
            c => {
                current.push(c);
                started = true;
            }
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Does this entry answer that query?
///
/// A case-insensitive substring over the name, then the keywords. Not a fuzzy
/// matcher: a launcher that ranks by edit distance answers a three-letter
/// query with something surprising, and the fix is always to type more, which
/// a substring match rewards and a fuzzy one does not.
pub fn matches(entry: &Entry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    let name = entry.name.to_lowercase();
    name.contains(&query) || entry.keywords.iter().any(|k| k.to_lowercase().contains(&query))
}

/// A name that starts with the query is a better answer than one that merely
/// contains it, and an exact name is better still. Higher sorts first.
pub fn rank(entry: &Entry, query: &str) -> u8 {
    let query = query.to_lowercase();
    let name = entry.name.to_lowercase();
    if name == query {
        3
    } else if name.starts_with(&query) {
        2
    } else if name.contains(&query) {
        1
    } else {
        0
    }
}

/// Runs an entry, detached from this process.
///
/// The child is `setsid`'d by way of a new process group so that the launcher
/// exiting — which it does immediately after — does not take the application
/// with it. Nothing of ours is passed down: no capability, no socket, no
/// inherited stdin.
pub fn launch(entry: &Entry) -> std::io::Result<()> {
    if entry.terminal {
        // We do not own a terminal and will not guess at one. Saying so is
        // better than spawning a windowless process the human never sees.
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "entry wants a terminal",
        ));
    }
    let (program, args) = entry
        .argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "entry has no command"))?;
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> Option<Entry> {
        parse("test.desktop", text)
    }

    #[test]
    fn a_plain_entry_parses() {
        let e = entry("[Desktop Entry]\nType=Application\nName=Files\nExec=nautilus %U\n").unwrap();
        assert_eq!(e.name, "Files");
        assert_eq!(e.argv, ["nautilus"]);
        assert!(!e.terminal);
    }

    /// A row that does nothing when clicked is worse than no row.
    #[test]
    fn an_entry_with_nothing_to_run_is_dropped() {
        assert!(entry("[Desktop Entry]\nType=Application\nName=Nothing\n").is_none());
        assert!(entry("[Desktop Entry]\nType=Link\nName=Site\nExec=x\n").is_none());
        assert!(entry("[Desktop Entry]\nName=Hidden\nExec=x\nNoDisplay=true\n").is_none());
        assert!(entry("[Desktop Entry]\nName=Gone\nExec=x\nHidden=true\n").is_none());
    }

    /// Keys outside `[Desktop Entry]` belong to an action, not to the entry.
    #[test]
    fn a_later_group_does_not_leak_into_the_entry() {
        let e = entry(
            "[Desktop Entry]\nName=Term\nExec=alacritty\n\
             [Desktop Action New]\nName=New Window\nExec=alacritty --new\n",
        )
        .unwrap();
        assert_eq!(e.name, "Term");
        assert_eq!(e.argv, ["alacritty"]);
    }

    /// A localised name is not our name. Rendering `Name[de]` because it came
    /// last in the file would be worse than rendering the untranslated one.
    #[test]
    fn a_localised_key_does_not_replace_the_plain_one() {
        let e = entry("[Desktop Entry]\nName=Files\nName[de]=Dateien\nExec=x\n").unwrap();
        assert_eq!(e.name, "Files");
    }

    #[test]
    fn field_codes_are_dropped_and_quotes_are_undone() {
        assert_eq!(argv("foo %U bar"), ["foo", "bar"]);
        assert_eq!(
            argv(r#"env X=1 "my app" --flag"#),
            ["env", "X=1", "my app", "--flag"]
        );
        assert_eq!(argv("pct %% here"), ["pct", "%", "here"]);
        // A field code alone must not survive as an empty argument the program
        // would go looking for a file at.
        assert_eq!(argv("viewer %f"), ["viewer"]);
    }

    #[test]
    fn search_is_over_names_and_keywords() {
        let e = entry("[Desktop Entry]\nName=Files\nExec=x\nKeywords=folder;manager;\n").unwrap();
        assert!(matches(&e, ""));
        assert!(matches(&e, "fil"));
        assert!(matches(&e, "FIL"));
        assert!(matches(&e, "manager"));
        assert!(!matches(&e, "spreadsheet"));
    }

    #[test]
    fn an_exact_name_outranks_a_substring() {
        let files = entry("[Desktop Entry]\nName=Files\nExec=x\n").unwrap();
        let profiler = entry("[Desktop Entry]\nName=Profiler\nExec=x\n").unwrap();
        assert!(rank(&files, "files") > rank(&files, "ile"));
        assert!(rank(&files, "fil") > rank(&profiler, "fil"));
    }

    /// Spawning a terminal application into no terminal at all leaves a
    /// process the human cannot see or reach.
    #[test]
    fn a_terminal_entry_refuses_rather_than_disappearing() {
        let e = entry("[Desktop Entry]\nName=Top\nExec=top\nTerminal=true\n").unwrap();
        assert!(e.terminal);
        assert!(launch(&e).is_err());
    }

    /// The user's own directory comes before the system ones, or shadowing an
    /// entry is impossible.
    #[test]
    fn the_search_path_prefers_the_human() {
        let path = search_path();
        assert!(path.iter().all(|p| p.ends_with("applications")));
        assert!(path.len() >= 2);
    }
}
