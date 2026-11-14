// SPDX-License-Identifier: AGPL-3.0-only
//! "Open with" (T-14 §11): what opens a path or URL the terminal recognised (a hint, an OSC 8
//! link, a right-clicked match, a copy-mode selection, an `open.*` target), and the rows of
//! the Open menu and the Open-with picker.
//!
//! The main thread builds a [`Request`] (pane facts, config, environment) and a worker runs
//! [`resolve`]: file stat and sniff, the shared-mime-info database, desktop entries and
//! `mimeapps.list`. Nothing here touches the main loop's state, and nothing logs a path, a URL
//! or file content.
//!
//! Handler order for the default: an `open.handlers` rule (exact type, then `type/*`, then
//! the same for each parent type); the built-ins (`image/*` → `apoth icat`, `video/*` and
//! `audio/*` → `apoth play` with the desktop default as its fallback, local files only); for a
//! URL a user-set `links.open-command`; the freedesktop default; `links.open-command`.
//!
//! Security (T-18): a path from a remote pane is never opened locally; a link passes
//! `check_link` first; every launch is an argv (desktop `Exec` expanded by [`exec`]).

pub(crate) mod desktop;
pub(crate) mod exec;
pub(crate) mod mime;
pub(crate) mod mimeapps;

use std::path::{Path, PathBuf};

use crate::security::{LinkRefused, check_link};

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    /// `open.default`: launch the default.
    Default,
    /// `open.menu`: show the Open menu.
    Menu,
    /// `open.with`: show the picker.
    Picker,
}

/// Where a terminal handler shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A split pane next to the source pane.
    Inline,
    Float,
    /// A separate glass window (`--ext`).
    Window,
}

impl Placement {
    pub(crate) const ALL: [Placement; 3] = [Placement::Float, Placement::Inline, Placement::Window];

    pub(crate) fn parse(s: &str) -> Option<Placement> {
        Some(match s {
            "inline" => Placement::Inline,
            "float" => Placement::Float,
            "window" => Placement::Window,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Placement::Inline => "inline",
            Placement::Float => "float",
            Placement::Window => "window",
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            Placement::Inline => "in a split pane",
            Placement::Float => "in a float",
            Placement::Window => "in a window",
        }
    }
}

/// A built-in terminal handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Term {
    Icat,
    Play,
}

impl Term {
    pub(crate) fn rule_name(self) -> &'static str {
        match self {
            Term::Icat => "apoth-icat",
            Term::Play => "apoth-play",
        }
    }

    pub(crate) fn subcommand(self) -> &'static str {
        match self {
            Term::Icat => "icat",
            Term::Play => "play",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Term::Icat => "apoth icat",
            Term::Play => "apoth play",
        }
    }

    /// The handler for a type, when a built-in covers it.
    fn for_mime(m: &str) -> Option<Term> {
        if m.starts_with("image/") {
            Some(Term::Icat)
        } else if m.starts_with("video/") || m.starts_with("audio/") {
            Some(Term::Play)
        } else {
            None
        }
    }
}

/// Exit statuses of `apoth play` that mean "cannot play this here": 2 (usage: no `play`
/// subcommand in this build) and 69 (`EX_UNAVAILABLE`: unsupported file). The opener then
/// launches the desktop default instead (T-14 §11).
pub(crate) const PLAY_UNSUPPORTED: [i32; 2] = [2, 69];

/// Who opens a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Handler {
    /// `apoth <subcommand>` on the local file.
    Term { term: Term, file: PathBuf },
    /// A desktop application, its `Exec` already expanded for the target.
    Desktop {
        id: String,
        name: String,
        argv: Vec<String>,
        terminal: bool,
    },
    /// `links.open-command` with the target substituted.
    Opener { argv: Vec<String> },
}

/// A handler and where it shows (placement matters for terminal handlers and `Terminal=true`
/// desktop apps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) handler: Handler,
    pub(crate) placement: Placement,
}

impl Choice {
    pub(crate) fn title(&self) -> String {
        match &self.handler {
            Handler::Term { term, .. } => term.title().to_string(),
            Handler::Desktop { name, .. } => name.clone(),
            Handler::Opener { argv } => Path::new(argv.first().map_or("", String::as_str))
                .file_name()
                .map_or_else(
                    || "the opener".to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
        }
    }

    /// The `open.handlers` value that makes this choice the default.
    pub(crate) fn rule_value(&self) -> Vec<String> {
        match &self.handler {
            Handler::Term { term, .. } => {
                vec![term.rule_name().into(), self.placement.name().into()]
            }
            Handler::Desktop { id, .. } => vec![id.clone()],
            Handler::Opener { .. } => vec!["opener".into()],
        }
    }

    fn shows_in_terminal(&self) -> bool {
        matches!(
            self.handler,
            Handler::Term { .. } | Handler::Desktop { terminal: true, .. }
        )
    }
}

/// How the target was recognised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawKind {
    /// An OSC 8 link (its URI; the visible text may differ).
    Link,
    Url,
    Path,
    /// A selection or an `open.*` argument: decided by its shape.
    Unknown,
}

/// The process environment the resolver reads (taken on main; cheap).
#[derive(Clone, Debug, Default)]
pub(crate) struct XdgEnv {
    pub(crate) home: PathBuf,
    pub(crate) data_home: PathBuf,
    pub(crate) data_dirs: Vec<PathBuf>,
    pub(crate) config_home: PathBuf,
    pub(crate) config_dirs: Vec<PathBuf>,
    /// `$XDG_CURRENT_DESKTOP`, lowercased, in order.
    pub(crate) desktops: Vec<String>,
    pub(crate) path: String,
    pub(crate) locales: Vec<String>,
}

impl XdgEnv {
    /// From a variable lookup (the process environment in production).
    pub(crate) fn from(get: &dyn Fn(&str) -> Option<String>) -> XdgEnv {
        let home = PathBuf::from(get("HOME").unwrap_or_else(|| "/".into()));
        let abs = |v: Option<String>| v.filter(|s| s.starts_with('/')).map(PathBuf::from);
        let list = |v: Option<String>, d: &str| -> Vec<PathBuf> {
            let s = v.filter(|s| !s.is_empty()).unwrap_or_else(|| d.to_string());
            s.split(':')
                .filter(|p| p.starts_with('/'))
                .map(PathBuf::from)
                .collect()
        };
        let locale = get("LC_ALL")
            .filter(|s| !s.is_empty())
            .or_else(|| get("LC_MESSAGES").filter(|s| !s.is_empty()))
            .or_else(|| get("LANG"))
            .unwrap_or_default();
        XdgEnv {
            data_home: abs(get("XDG_DATA_HOME")).unwrap_or_else(|| home.join(".local/share")),
            data_dirs: list(get("XDG_DATA_DIRS"), "/usr/local/share:/usr/share"),
            config_home: abs(get("XDG_CONFIG_HOME")).unwrap_or_else(|| home.join(".config")),
            config_dirs: list(get("XDG_CONFIG_DIRS"), "/etc/xdg"),
            desktops: get("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(str::to_lowercase)
                .collect(),
            path: get("PATH").unwrap_or_default(),
            locales: desktop::locale_keys(&locale),
            home,
        }
    }

    fn app_dirs(&self) -> Vec<PathBuf> {
        std::iter::once(&self.data_home)
            .chain(&self.data_dirs)
            .map(|d| d.join("applications"))
            .collect()
    }

    fn mime_dirs(&self) -> Vec<PathBuf> {
        std::iter::once(&self.data_home)
            .chain(&self.data_dirs)
            .map(|d| d.join("mime"))
            .collect()
    }
}

/// Everything [`resolve`] needs, built on main.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) intent: Intent,
    pub(crate) raw: String,
    pub(crate) kind: RawKind,
    /// The visible text of a link (empty when unknown): the URI is confirmed first when it
    /// differs (T-14 §5).
    pub(crate) visible: String,
    /// The source pane's local working directory.
    pub(crate) cwd: Option<PathBuf>,
    /// `Some(label)` when the source pane is remote (T-18 §10); the label names the host.
    pub(crate) remote: Option<String>,
    pub(crate) hostname: String,
    pub(crate) schemes: Vec<String>,
    /// `open.handlers`.
    pub(crate) rules: Vec<(String, Vec<String>)>,
    /// `open.placement`, or the action's `placement=`.
    pub(crate) placement: Placement,
    /// The action named the placement: it wins over a rule's.
    pub(crate) forced_placement: bool,
    /// `links.open-command` and whether the user set it.
    pub(crate) opener: Vec<String>,
    pub(crate) opener_custom: bool,
    pub(crate) env: XdgEnv,
}

/// What [`resolve`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) intent: Intent,
    /// The target as shown to the user (program text).
    pub(crate) shown: String,
    /// What "Copy" copies (the target as recognised).
    pub(crate) copy: String,
    /// The absolute local path, for "Copy path".
    pub(crate) copy_path: Option<String>,
    pub(crate) mime: Option<String>,
    /// Why nothing can open it (Copy still works).
    pub(crate) refusal: Option<String>,
    /// The link's URI differs from its visible text: confirm before a default open.
    pub(crate) reveal: bool,
    pub(crate) default: Option<Choice>,
    /// Launched when the default is `apoth play` and it reports "unsupported".
    pub(crate) fallback: Option<Choice>,
    pub(crate) terminal: Vec<Choice>,
    pub(crate) associated: Vec<Choice>,
    pub(crate) others: Vec<Choice>,
}

impl Resolved {
    /// The type key a "Remember" writes (`image/png`, `x-scheme-handler/https`).
    pub(crate) fn remember_key(&self) -> Option<&str> {
        self.mime
            .as_deref()
            .filter(|m| *m != "application/octet-stream")
    }

    fn refused(intent: Intent, shown: String, copy: String, why: String) -> Resolved {
        Resolved {
            intent,
            shown,
            copy,
            copy_path: None,
            mime: None,
            refusal: Some(why),
            reveal: false,
            default: None,
            fallback: None,
            terminal: Vec::new(),
            associated: Vec::new(),
            others: Vec::new(),
        }
    }
}

/// The recognised target before the file system is consulted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Url(String),
    /// A local path as written (maybe relative, with `~`, with `:line:col`).
    Path(String),
}

/// Why a target is not opened (Copy still offered).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A path from a remote pane, or a `file://` link naming another host.
    Remote(String),
    Link(LinkRefused),
}

impl Refusal {
    pub(crate) fn message(&self) -> String {
        match self {
            Refusal::Remote(h) => format!("path is on {h}"),
            Refusal::Link(l) => l.reason().to_string(),
        }
    }
}

fn has_scheme(s: &str) -> bool {
    let Some(i) = s.find(':') else { return false };
    let scheme = &s[..i];
    !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        && (s[i + 1..].starts_with("//") || scheme.eq_ignore_ascii_case("mailto"))
}

/// Classify a recognised string (T-14 §11): a URL is held to `check_link`; a `file://` link
/// naming this host becomes its path; a path (or a `file://` URL) from a remote pane is
/// refused with the host's name.
pub(crate) fn classify(
    raw: &str,
    kind: RawKind,
    remote: Option<&str>,
    schemes: &[String],
    hostname: &str,
) -> Result<Target, Refusal> {
    let raw = raw.trim();
    let url = match kind {
        RawKind::Link | RawKind::Url => true,
        RawKind::Path => false,
        RawKind::Unknown => has_scheme(raw),
    };
    if !url {
        return match remote {
            Some(h) => Err(Refusal::Remote(h.to_string())),
            None => Ok(Target::Path(raw.to_string())),
        };
    }
    if let Err(e) = check_link(raw, schemes, hostname) {
        if e == LinkRefused::RemoteFile {
            let host = raw
                .get(7..)
                .and_then(|r| r.split('/').next())
                .map_or_else(|| "another host".to_string(), clean_host);
            return Err(Refusal::Remote(host));
        }
        return Err(Refusal::Link(e));
    }
    if raw
        .get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("file:"))
    {
        if let Some(h) = remote {
            return Err(Refusal::Remote(h.to_string()));
        }
        return match crate::ctl::osc7::local_path(raw, hostname) {
            Some(p) => Ok(Target::Path(p.to_string_lossy().into_owned())),
            None => Err(Refusal::Link(LinkRefused::Malformed)),
        };
    }
    Ok(Target::Url(raw.to_string()))
}

/// A host name for display: `[A-Za-z0-9.-]`, at most 64 bytes (program text, T-18 §6).
pub(crate) fn clean_host(h: &str) -> String {
    let s: String = h
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .take(64)
        .collect();
    if s.is_empty() {
        "a remote host".to_string()
    } else {
        s
    }
}

/// `~`, `~/x` and relative paths made absolute; `None` for a relative path with no cwd.
pub(crate) fn absolute(p: &str, cwd: Option<&Path>, home: &Path) -> Option<PathBuf> {
    let pb = if p == "~" {
        home.to_path_buf()
    } else if let Some(r) = p.strip_prefix("~/") {
        home.join(r)
    } else if p.starts_with('/') {
        PathBuf::from(p)
    } else {
        cwd?.join(p)
    };
    Some(normalize(&pb))
}

/// Lexical `.`/`..` removal (no symlink resolution: the path is shown and copied as written).
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
            _ => {}
        }
    }
    out
}

/// `path:12` / `path:12:5` → `(path, line, col)`; `None` when there is no suffix.
pub(crate) fn split_line_col(p: &str) -> Option<(&str, u32, Option<u32>)> {
    let num = |s: &str| {
        (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let (a, last) = p.rsplit_once(':')?;
    let last: u32 = num(last)?;
    match a.rsplit_once(':') {
        Some((path, line)) if !path.is_empty() => match num(line) {
            Some(l) => Some((path, l, Some(last))),
            None => Some((a, last, None)),
        },
        _ if !a.is_empty() => Some((a, last, None)),
        _ => None,
    }
}

/// The file system facts about a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    /// A FIFO, socket or device: never opened for a sniff.
    Special,
}

fn stat(p: &Path) -> Option<Kind> {
    let m = std::fs::metadata(p).ok()?;
    Some(if m.is_file() {
        Kind::File
    } else if m.is_dir() {
        Kind::Dir
    } else {
        Kind::Special
    })
}

/// First bytes of a regular file (opened non-blocking so a raced swap to a FIFO cannot hang).
fn head(p: &Path) -> Vec<u8> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(p)
    else {
        return Vec::new();
    };
    let mut buf = Vec::with_capacity(mime::SNIFF_BYTES);
    let _ = f.take(mime::SNIFF_BYTES as u64).read_to_end(&mut buf);
    buf
}

fn read_capped(p: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let m = std::fs::metadata(p).ok()?;
    if !m.is_file() || m.len() > cap {
        return None;
    }
    let mut v = Vec::new();
    std::fs::File::open(p)
        .ok()?
        .take(cap)
        .read_to_end(&mut v)
        .ok()?;
    Some(v)
}

/// Load the shared-mime-info database (most important directory first), or the built-in
/// table when none is installed.
pub(crate) fn load_mime(env: &XdgEnv) -> mime::MimeDb {
    let cap = mime::MAX_DB_FILE as u64;
    let mut db = mime::MimeDb::default();
    let mut any = false;
    for d in env.mime_dirs() {
        let text = |n: &str| {
            read_capped(&d.join(n), cap).map(|b| String::from_utf8_lossy(&b).into_owned())
        };
        if let Some(t) = text("globs2") {
            db.add_globs2(&t);
            any = true;
        }
        if let Some(b) = read_capped(&d.join("magic"), cap) {
            db.add_magic(&b);
            any = true;
        }
        if let Some(t) = text("subclasses") {
            db.add_subclasses(&t);
        }
        if let Some(t) = text("aliases") {
            db.add_aliases(&t);
        }
    }
    if any { db } else { mime::MimeDb::builtin() }
}

/// The desktop entries and `mimeapps.list` files.
pub(crate) fn load_apps(env: &XdgEnv) -> (desktop::Index, Vec<mimeapps::Located>) {
    let app_dirs = env.app_dirs();
    let index = desktop::Index::scan(&app_dirs, &env.locales);
    let mut lists = Vec::new();
    for (p, level, ds) in
        mimeapps::locations(&env.config_home, &env.config_dirs, &app_dirs, &env.desktops)
    {
        if let Some(b) = read_capped(&p, mimeapps::MAX_FILE) {
            lists.push(mimeapps::Located {
                level,
                desktop_specific: ds,
                list: mimeapps::List::parse(&String::from_utf8_lossy(&b), ds),
            });
        }
    }
    (index, lists)
}

/// The databases a resolve reads (loaded on the worker; tests build them by hand).
#[derive(Debug)]
pub(crate) struct Dbs {
    pub(crate) mime: mime::MimeDb,
    pub(crate) index: desktop::Index,
    pub(crate) lists: Vec<mimeapps::Located>,
}

impl Dbs {
    pub(crate) fn load(env: &XdgEnv) -> Dbs {
        let (index, lists) = load_apps(env);
        Dbs {
            mime: load_mime(env),
            index,
            lists,
        }
    }
}

/// Resolve a request against the file system and the databases (worker thread).
pub(crate) fn resolve(req: &Request) -> Resolved {
    resolve_with(req, &Dbs::load(&req.env))
}

/// [`resolve`] over given databases.
pub(crate) fn resolve_with(req: &Request, dbs: &Dbs) -> Resolved {
    let shown = req.raw.trim().to_string();
    let target = match classify(
        &req.raw,
        req.kind,
        req.remote.as_deref(),
        &req.schemes,
        &req.hostname,
    ) {
        Ok(t) => t,
        Err(r) => return Resolved::refused(req.intent, shown.clone(), shown, r.message()),
    };
    // The file, its type and the URL handed to apps.
    let (path, kind, mime, url) = match &target {
        Target::Url(u) => {
            let scheme = u.split(':').next().unwrap_or("").to_ascii_lowercase();
            (None, None, format!("x-scheme-handler/{scheme}"), u.clone())
        }
        Target::Path(p) => {
            let found = absolute(p, req.cwd.as_deref(), &req.env.home)
                .and_then(|a| stat(&a).map(|k| (a, k)))
                .or_else(|| {
                    let (bare, _, _) = split_line_col(p)?;
                    let a = absolute(bare, req.cwd.as_deref(), &req.env.home)?;
                    stat(&a).map(|k| (a, k))
                });
            let Some((abs, k)) = found else {
                let mut r = Resolved::refused(
                    req.intent,
                    shown.clone(),
                    shown,
                    "file not found".to_string(),
                );
                r.copy_path = absolute(p, req.cwd.as_deref(), &req.env.home)
                    .map(|a| a.to_string_lossy().into_owned());
                return r;
            };
            let name = abs
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let m = match k {
                Kind::Dir => "inode/directory".to_string(),
                Kind::Special => "inode/x-special".to_string(),
                Kind::File => dbs.mime.detect(&name, &head(&abs)),
            };
            let url = exec::file_url(&abs);
            (Some(abs), Some(k), m, url)
        }
    };
    let mut chain = vec![mime.clone()];
    if path.is_some() {
        chain.extend(dbs.mime.ancestors(&mime));
    }
    let resolver = mimeapps::Resolver {
        index: &dbs.index,
        lists: &dbs.lists,
        path: &req.env.path,
    };
    let local_file = kind == Some(Kind::File);
    let desktop = |id: &str| -> Option<Choice> {
        let e = dbs.index.get(id)?;
        if e.try_exec
            .as_deref()
            .is_some_and(|t| !desktop::installed(t, &req.env.path))
        {
            return None;
        }
        let path_s = e.path.to_string_lossy();
        let argv = exec::expand(
            &e.exec,
            &exec::ExecTarget {
                path: path.as_deref(),
                url: &url,
            },
            &exec::ExecEntry {
                name: &e.name,
                icon: e.icon.as_deref(),
                path: Some(&path_s),
            },
        )
        .map_err(|e| tracing::debug!(reason = e.reason(), "desktop entry skipped for this target"))
        .ok()?;
        Some(Choice {
            handler: Handler::Desktop {
                id: e.id.clone(),
                name: e.name.clone(),
                argv,
                terminal: e.terminal,
            },
            placement: req.placement,
        })
    };
    let opener = || -> Option<Choice> {
        let target = path
            .as_ref()
            .map_or_else(|| url.clone(), |p| p.to_string_lossy().into_owned());
        crate::security::open_argv(&req.opener, &target).map(|argv| Choice {
            handler: Handler::Opener { argv },
            placement: req.placement,
        })
    };
    let term = |t: Term, placement: Placement| -> Option<Choice> {
        let file = path.clone().filter(|_| local_file)?;
        Some(Choice {
            handler: Handler::Term { term: t, file },
            placement,
        })
    };
    let from_rule = |args: &[String]| -> Option<Choice> {
        let h = args.first()?;
        let placement = if req.forced_placement {
            req.placement
        } else {
            args.get(1)
                .and_then(|p| Placement::parse(p))
                .unwrap_or(req.placement)
        };
        match h.as_str() {
            "apoth-icat" => term(Term::Icat, placement),
            "apoth-play" => term(Term::Play, placement),
            "opener" => opener(),
            id => desktop(id).map(|mut c| {
                c.placement = placement;
                c
            }),
        }
    };
    // 1. Rules, most specific type first; exact before wildcard.
    let mut default = None;
    'types: for m in &chain {
        for exact in [true, false] {
            for (pat, args) in &req.rules {
                let hit = if exact {
                    pat.eq_ignore_ascii_case(m)
                } else {
                    pat.ends_with("/*") && mime::pattern_covers(pat, m)
                };
                if hit && let Some(c) = from_rule(args) {
                    default = Some(c);
                    break 'types;
                }
            }
        }
    }
    let desktop_default = resolver.default_through(&chain).and_then(|id| desktop(&id));
    // 2. Built-ins for local media files.
    let builtin = Term::for_mime(&mime).and_then(|t| term(t, req.placement));
    if default.is_none() {
        default = builtin.clone();
    }
    // 3. A user-set opener for URLs, 4. the desktop default, 5. the opener.
    if default.is_none() && path.is_none() && req.opener_custom {
        default = opener();
    }
    if default.is_none() {
        default = desktop_default.clone();
    }
    if default.is_none() {
        default = opener();
    }
    let fallback = match &default {
        Some(Choice {
            handler: Handler::Term {
                term: Term::Play, ..
            },
            ..
        }) => desktop_default.clone().or_else(opener),
        _ => None,
    };
    // Picker lists.
    let mut terminal = Vec::new();
    if let Some(t) = Term::for_mime(&mime) {
        for p in Placement::ALL {
            terminal.extend(term(t, p));
        }
    }
    let mut assoc_ids: Vec<String> = Vec::new();
    for m in &chain {
        for id in resolver.associated(m) {
            if !assoc_ids.contains(&id) {
                assoc_ids.push(id);
            }
        }
    }
    let mut associated: Vec<Choice> = assoc_ids.iter().filter_map(|id| desktop(id)).collect();
    associated.extend(opener());
    let mut others: Vec<Choice> = dbs
        .index
        .effective()
        .into_iter()
        .filter(|e| e.shown(&req.env.desktops) && !assoc_ids.contains(&e.id))
        .filter_map(|e| desktop(&e.id))
        .collect();
    others.sort_by_key(|c| c.title().to_lowercase());
    let reveal = req.kind == RawKind::Link && crate::security::needs_reveal(&req.raw, &req.visible);
    Resolved {
        intent: req.intent,
        shown: shown.clone(),
        copy: shown,
        copy_path: path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        mime: Some(mime),
        refusal: None,
        reveal,
        default,
        fallback,
        terminal,
        associated,
        others,
    }
}

/// What a menu or picker row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OpenPick {
    /// Launch `choices[i]` of the open session.
    Launch(usize),
    /// Show the picker for the same target.
    Picker,
    Copy(String),
    ToggleRemember,
    /// Shown with its reason; picking rings the bell.
    Disabled,
}

/// One row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuRow {
    pub(crate) title: String,
    pub(crate) hint: Option<String>,
    pub(crate) pick: OpenPick,
    /// The title holds third-party text (an app name): drawn in the program-text style.
    pub(crate) program: bool,
}

fn row(title: String, hint: Option<String>, pick: OpenPick, program: bool) -> MenuRow {
    MenuRow {
        title,
        hint,
        pick,
        program,
    }
}

/// The Open menu: Open (the default, named), Open with…, placement variants of the terminal
/// handler, Copy and Copy path. Returns the rows and the choices `Launch(i)` indexes.
pub(crate) fn menu_rows(r: &Resolved) -> (Vec<MenuRow>, Vec<Choice>) {
    let mut rows = Vec::new();
    let mut choices = Vec::new();
    let mime = r.mime.clone();
    match (&r.refusal, &r.default) {
        (None, Some(d)) => {
            choices.push(d.clone());
            let title = if d.shows_in_terminal() && matches!(d.handler, Handler::Term { .. }) {
                format!("Open with {} {}", d.title(), d.placement.phrase())
            } else {
                format!("Open with {}", d.title())
            };
            rows.push(row(title, mime.clone(), OpenPick::Launch(0), true));
            rows.push(row(
                "Open with\u{2026}".into(),
                Some("choose an app".into()),
                OpenPick::Picker,
                false,
            ));
            for c in &r.terminal {
                if c == d {
                    continue;
                }
                choices.push(c.clone());
                rows.push(row(
                    format!("Open with {} {}", c.title(), c.placement.phrase()),
                    None,
                    OpenPick::Launch(choices.len() - 1),
                    false,
                ));
            }
        }
        (why, _) => {
            let why = why.clone().unwrap_or_else(|| "no app opens this".into());
            rows.push(row(
                "Open".into(),
                Some(why.clone()),
                OpenPick::Disabled,
                false,
            ));
            rows.push(row(
                "Open with\u{2026}".into(),
                Some(why),
                OpenPick::Disabled,
                false,
            ));
        }
    }
    let is_path = r.copy_path.is_some();
    rows.push(row(
        if is_path { "Copy" } else { "Copy link" }.into(),
        None,
        OpenPick::Copy(r.copy.clone()),
        false,
    ));
    if let Some(p) = &r.copy_path
        && p != &r.copy
    {
        rows.push(row(
            "Copy path".into(),
            None,
            OpenPick::Copy(p.clone()),
            false,
        ));
    }
    (rows, choices)
}

/// The picker: a pinned "Remember for <type>" toggle, then terminal handlers, the apps that
/// declare the type, the rest.
pub(crate) fn picker_rows(
    r: &Resolved,
    remember: bool,
) -> (Vec<MenuRow>, Vec<MenuRow>, Vec<Choice>) {
    let mut pinned = Vec::new();
    if let Some(k) = r.remember_key() {
        pinned.push(row(
            format!("Remember for {k}"),
            Some(if remember { "on" } else { "off" }.into()),
            OpenPick::ToggleRemember,
            false,
        ));
    }
    let mut rows = Vec::new();
    let mut choices = Vec::new();
    let mut push = |c: &Choice, hint: String, rows: &mut Vec<MenuRow>| {
        choices.push(c.clone());
        let title = match c.handler {
            Handler::Term { .. } => format!("{} {}", c.title(), c.placement.phrase()),
            _ => c.title(),
        };
        let hint = if r.default.as_ref() == Some(c) {
            format!("{hint} \u{b7} default")
        } else {
            hint
        };
        rows.push(row(
            title,
            Some(hint),
            OpenPick::Launch(choices.len() - 1),
            true,
        ));
    };
    for c in &r.terminal {
        push(c, "in the terminal".into(), &mut rows);
    }
    let mime = r.mime.clone().unwrap_or_default();
    for c in &r.associated {
        let hint = if matches!(c.handler, Handler::Opener { .. }) {
            "opener".to_string()
        } else {
            format!("for {mime}")
        };
        push(c, hint, &mut rows);
    }
    for c in &r.others {
        push(c, "other app".into(), &mut rows);
    }
    (pinned, rows, choices)
}

#[cfg(test)]
mod tests;
