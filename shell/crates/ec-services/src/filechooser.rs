// SPDX-License-Identifier: AGPL-3.0-only
//! `org.freedesktop.impl.portal.FileChooser`: the xdg-desktop-portal backend
//! that gives GTK, Qt, Electron and Firefox Fog as their Open and Save dialog
//! (ADR 0100).
//!
//! Each call spawns one `ec-fog-ui --pick MODE …` child. Everything the app
//! sent is passed as argv values only (no shell, ever), and its text is
//! sanitized first: control and bidi-override characters stripped, lengths
//! clamped. Fog prints the user's choice as NUL-terminated records on stdout
//! and exits 0, or exits 1 on cancel (the contract is `fog/crates/ec-fog-ui/src/pick.rs`).
//! Only what Fog reports is returned; nothing is filled in on the user's
//! behalf.
//!
//! The dialog runs on its own thread, which spawns, reads and reaps the
//! child; the bus side awaits a oneshot, so a long dialog never blocks other
//! calls. `Request.Close` kills that child by the pid it was spawned with,
//! which is safe because the thread does not reap the pid until it holds the
//! same lock (`waitid(WNOWAIT)` first, see [`run`]).

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::task::{Context, Poll, Waker};
use std::thread;

use zbus::interface;
use zbus::object_server::ObjectServer;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// Fog's UI binary, found on `PATH` (packaging installs it to /usr/bin).
pub const FOG: &str = "ec-fog-ui";

pub const RESPONSE_OK: u32 = 0;
pub const RESPONSE_CANCELLED: u32 = 1;
pub const RESPONSE_OTHER: u32 = 2;

/// Characters kept of a title, label or filter name.
const MAX_TEXT: usize = 256;
/// Characters kept of one glob or mime type.
const MAX_PATTERN: usize = 128;
const MAX_FILTERS: usize = 64;
const MAX_PATTERNS: usize = 64;
const MAX_CHOICES: usize = 16;
const MAX_OPTIONS: usize = 64;
/// `SaveFiles` names accepted in one call.
const MAX_FILES: usize = 1024;
/// PATH_MAX: longer paths are not ones Linux can open.
const MAX_PATH: usize = 4096;
/// NAME_MAX.
const MAX_NAME: usize = 255;
const MAX_PARENT: usize = 128;
/// Bytes read from Fog's stdout before the reply is refused.
const MAX_OUTPUT: u64 = 4 << 20;
/// `name (2).ext` … `name (MAX_SUFFIX).ext` are tried for a taken name.
const MAX_SUFFIX: u32 = 999;

/// Which portal method a call came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    OpenFile,
    SaveFile,
    SaveFiles,
}

/// What Fog is asked to do: `--pick <arg>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Open,
    OpenMultiple,
    Directory,
    DirectoryMultiple,
    Save,
    /// Pick a folder; the app's `files` names are joined to it here.
    SaveFiles,
}

impl Mode {
    pub fn arg(self) -> &'static str {
        match self {
            Mode::Open => "open",
            Mode::OpenMultiple => "open-multiple",
            Mode::Directory => "directory",
            Mode::DirectoryMultiple => "directory-multiple",
            Mode::Save => "save",
            Mode::SaveFiles => "save-files",
        }
    }

    fn multiple(self) -> bool {
        matches!(self, Mode::OpenMultiple | Mode::DirectoryMultiple)
    }
}

/// One filter pattern: the portal's `(us)`, 0 a glob, 1 a mime type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Glob(String),
    Mime(String),
}

/// shared-mime-info's glob table: mime type -> globs, from `globs2`. Fog lists
/// by name only, so a portal mime filter is turned into these globs here.
#[derive(Debug, Default)]
pub struct MimeGlobs(HashMap<String, Vec<String>>);

const GLOBS2: &str = "/usr/share/mime/globs2";

impl MimeGlobs {
    /// Parse `globs2` text: `weight:mime:glob[:flags]`, `#` comments.
    pub fn parse(text: &str) -> Self {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut f = line.splitn(4, ':');
            let (Some(_), Some(mime), Some(glob)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            if glob.is_empty() || glob == "__NOGLOBS__" || !is_mime(mime) {
                continue;
            }
            if !text_ok(glob) {
                continue;
            }
            m.entry(mime.to_owned()).or_default().push(glob.to_owned());
        }
        Self(m)
    }

    /// The database on this machine, read once. Missing file: empty.
    pub fn system() -> &'static Self {
        static DB: OnceLock<MimeGlobs> = OnceLock::new();
        DB.get_or_init(|| Self::parse(&std::fs::read_to_string(GLOBS2).unwrap_or_default()))
    }

    /// Globs for `mime`; `type/*` covers every type of that kind. Sorted,
    /// no repeats. Empty when the type is unknown.
    pub fn globs(&self, mime: &str) -> Vec<String> {
        let mut out: Vec<String> = match mime.split_once('/') {
            Some((t, "*")) => self
                .0
                .iter()
                .filter(|(k, _)| k.split_once('/').is_some_and(|(kt, _)| kt == t))
                .flat_map(|(_, g)| g.iter().cloned())
                .collect(),
            _ => self.0.get(mime).cloned().unwrap_or_default(),
        };
        out.sort();
        out.dedup();
        out
    }
}

fn text_ok(s: &str) -> bool {
    s.len() <= MAX_PATTERN && !s.chars().any(|c| c.is_control() || is_bidi_control(c))
}

/// A filter exactly as the app sent it: `(sa(us))`. Kept to echo back as the
/// `current_filter` result, which the app matches against its own list.
pub type RawFilter = (String, Vec<(u32, String)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub raw: RawFilter,
    /// Sanitized, for display.
    pub name: String,
    pub patterns: Vec<Pattern>,
}

/// A combo box (`options` non-empty) or a checkbox (`options` empty): the
/// portal's `(ssa(ss)s)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The app's id, returned verbatim; never shown.
    pub id: String,
    pub label: String,
    /// `(id, sanitized label)`.
    pub options: Vec<(String, String)>,
    /// Index into `options` of the initial one, or the checkbox's state.
    pub initial: Initial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initial {
    Option(usize),
    Bool(bool),
}

/// One request, parsed and sanitized: what becomes Fog's argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub mode: Mode,
    pub title: Option<String>,
    pub accept_label: Option<String>,
    /// Absolute, raw bytes.
    pub current_folder: Option<Vec<u8>>,
    /// A bare file name: no `/`, not `.` or `..`.
    pub current_name: Option<String>,
    pub filters: Vec<Filter>,
    /// Index into `filters`.
    pub current_filter: Option<usize>,
    pub choices: Vec<Choice>,
    /// `SaveFiles` names, raw bytes, each a valid bare file name.
    pub files: Vec<Vec<u8>>,
    /// `wayland:<handle>` or `x11:<xid>`, a transient-for hint and nothing else.
    pub parent: Option<String>,
}

/// Why a request was refused before any dialog was shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `SaveFiles` with no names, too many, or one that is not a bare file name.
    BadFiles,
}

// --- reading the a{sv} ---------------------------------------------------

fn inner<'b, 'a>(v: &'b Value<'a>) -> &'b Value<'a> {
    match v {
        Value::Value(boxed) => inner(boxed),
        v => v,
    }
}

fn as_bool(v: &Value<'_>) -> Option<bool> {
    match inner(v) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_str<'b>(v: &'b Value<'_>) -> Option<&'b str> {
    match inner(v) {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

fn as_u32(v: &Value<'_>) -> Option<u32> {
    match inner(v) {
        Value::U32(n) => Some(*n),
        _ => None,
    }
}

fn as_array<'b, 'a>(v: &'b Value<'a>) -> Option<&'b [Value<'a>]> {
    match inner(v) {
        Value::Array(a) => Some(a.inner()),
        _ => None,
    }
}

fn as_fields<'b, 'a>(v: &'b Value<'a>) -> Option<&'b [Value<'a>]> {
    match inner(v) {
        Value::Structure(s) => Some(s.fields()),
        _ => None,
    }
}

/// An `ay`, its trailing NULs (the portal sends them NUL-terminated) dropped.
fn as_bytes(v: &Value<'_>) -> Option<Vec<u8>> {
    let mut out = as_array(v)?
        .iter()
        .map(|b| match inner(b) {
            Value::U8(b) => Some(*b),
            _ => None,
        })
        .collect::<Option<Vec<u8>>>()?;
    while out.last() == Some(&0) {
        out.pop();
    }
    Some(out)
}

// --- sanitizing ------------------------------------------------------------

/// Bidi embeddings, overrides and isolates: they can make a label read as
/// something else. Stripped with the control characters.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// App text made safe to show and to pass as one argv value: no control or
/// bidi-control characters, at most `max` characters, trimmed. `None` if
/// nothing is left.
pub fn text(s: &str, max: usize) -> Option<String> {
    let out: String = s
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .take(max)
        .collect();
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_owned())
}

/// A GTK-style mnemonic label (`_Open`, `Save __as`) as plain text: a single
/// `_` marks the next character and is dropped, `__` is a literal `_`.
pub fn strip_mnemonic(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                chars.next();
                out.push('_');
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A bare file name as raw bytes: non-empty, at most NAME_MAX, no `/`, no
/// NUL or other control byte, not `.` or `..`.
fn is_file_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != b"."
        && name != b".."
        && !name.iter().any(|&b| b == b'/' || b < 0x20 || b == 0x7f)
}

/// An absolute path the portal sent (`current_folder`, `current_file`):
/// starts with `/`, no NUL inside, at most PATH_MAX.
fn absolute(path: Vec<u8>) -> Option<Vec<u8>> {
    (path.first() == Some(&b'/') && path.len() <= MAX_PATH && !path.contains(&0)).then_some(path)
}

/// The suggested save name, sanitized; a name with a `/` is not a name.
fn file_name(s: &str) -> Option<String> {
    let t = text(s, MAX_NAME)?;
    is_file_name(t.as_bytes()).then_some(t)
}

/// A mime type as `type/subtype`, RFC 6838 restricted characters (and `*`).
fn is_mime(s: &str) -> bool {
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-*".contains(&b))
    };
    s.len() <= MAX_PATTERN && s.split_once('/').is_some_and(|(t, st)| ok(t) && ok(st))
}

/// The portal's window identifier, if it is one: `wayland:<exported
/// handle>` or `x11:<hex xid>`. Only ever a transient-for hint.
pub fn parent(s: &str) -> Option<String> {
    let (kind, id) = s.split_once(':')?;
    let ok = match kind {
        "wayland" => id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "x11" => id.bytes().all(|b| b.is_ascii_hexdigit()),
        _ => false,
    };
    (ok && !id.is_empty() && id.len() <= MAX_PARENT).then(|| s.to_owned())
}

fn raw_filter(v: &Value<'_>) -> Option<RawFilter> {
    let [name, patterns] = as_fields(v)? else {
        return None;
    };
    let patterns = as_array(patterns)?
        .iter()
        .take(MAX_PATTERNS)
        .map(|p| match as_fields(p)? {
            [kind, pat] => Some((as_u32(kind)?, as_str(pat)?.to_owned())),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some((as_str(name)?.to_owned(), patterns))
}

/// A filter to show: sanitized name, its usable patterns. `None` if it has
/// none (unknown kinds and malformed mime types are dropped, not guessed at).
fn filter(raw: RawFilter) -> Option<Filter> {
    let patterns: Vec<Pattern> = raw
        .1
        .iter()
        .filter_map(|(kind, pat)| match kind {
            0 => text(pat, MAX_PATTERN).map(Pattern::Glob),
            1 => is_mime(pat).then(|| Pattern::Mime(pat.clone())),
            _ => None,
        })
        .collect();
    if patterns.is_empty() {
        return None;
    }
    let name = text(&raw.0, MAX_TEXT).unwrap_or_else(|| "Files".to_owned());
    Some(Filter { raw, name, patterns })
}

fn choice(v: &Value<'_>) -> Option<Choice> {
    let [id, label, options, initial] = as_fields(v)? else {
        return None;
    };
    let id = as_str(id)?.to_owned();
    if id.is_empty() {
        return None;
    }
    let label = text(as_str(label)?, MAX_TEXT).unwrap_or_else(|| id.clone());
    let options = as_array(options)?
        .iter()
        .take(MAX_OPTIONS)
        .map(|o| match as_fields(o)? {
            [oid, olabel] => {
                let oid = as_str(oid)?.to_owned();
                let olabel = text(as_str(olabel)?, MAX_TEXT).unwrap_or_else(|| oid.clone());
                Some((oid, olabel))
            }
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let initial = as_str(initial)?;
    let initial = if options.is_empty() {
        Initial::Bool(initial == "true")
    } else {
        Initial::Option(options.iter().position(|(o, _)| o == initial).unwrap_or(0))
    };
    Some(Choice {
        id,
        label,
        options,
        initial,
    })
}

impl Pick {
    /// Parse one call. Malformed or unknown options are ignored; only a
    /// `SaveFiles` call whose names cannot all be honoured is refused, since
    /// its reply must carry one URI per name.
    pub fn from_call(
        method: Method,
        title: &str,
        parent_window: &str,
        options: &HashMap<String, OwnedValue>,
    ) -> Result<Pick, Refusal> {
        let opt = |k: &str| options.get(k).map(|v| &**v);
        let flag = |k: &str| opt(k).and_then(as_bool).unwrap_or(false);
        let mode = match method {
            Method::OpenFile => match (flag("directory"), flag("multiple")) {
                (false, false) => Mode::Open,
                (false, true) => Mode::OpenMultiple,
                (true, false) => Mode::Directory,
                (true, true) => Mode::DirectoryMultiple,
            },
            Method::SaveFile => Mode::Save,
            Method::SaveFiles => Mode::SaveFiles,
        };

        let mut current_folder = opt("current_folder").and_then(as_bytes).and_then(absolute);
        let mut current_name = match mode {
            Mode::Save => opt("current_name").and_then(as_str).and_then(file_name),
            _ => None,
        };
        // Saving an existing file: open its folder with its name filled in,
        // unless the app said otherwise.
        if mode == Mode::Save {
            if let Some(file) = opt("current_file").and_then(as_bytes).and_then(absolute) {
                let cut = file.iter().rposition(|&b| b == b'/').unwrap_or(0);
                let (dir, name) = (&file[..cut], &file[cut + 1..]);
                if current_folder.is_none() {
                    current_folder = Some(if dir.is_empty() {
                        b"/".to_vec()
                    } else {
                        dir.to_vec()
                    });
                }
                if current_name.is_none() {
                    current_name = std::str::from_utf8(name).ok().and_then(file_name);
                }
            }
        }

        let raws: Vec<RawFilter> = match mode {
            Mode::SaveFiles => Vec::new(),
            _ => opt("filters")
                .and_then(as_array)
                .map(|a| a.iter().take(MAX_FILTERS).filter_map(raw_filter).collect())
                .unwrap_or_default(),
        };
        let mut filters: Vec<Filter> = raws.into_iter().filter_map(filter).collect();
        let wanted = match mode {
            Mode::SaveFiles => None,
            _ => opt("current_filter").and_then(raw_filter),
        };
        // The portal: with a list, current_filter selects from it; with no
        // list, it applies on its own.
        let current_filter = match wanted {
            Some(raw) if filters.is_empty() => filter(raw).map(|f| {
                filters.push(f);
                0
            }),
            Some(raw) => filters.iter().position(|f| f.raw == raw),
            None => None,
        };

        let choices = opt("choices")
            .and_then(as_array)
            .map(|a| a.iter().take(MAX_CHOICES).filter_map(choice).collect())
            .unwrap_or_default();

        let files = match mode {
            Mode::SaveFiles => {
                let list = opt("files").and_then(as_array).ok_or(Refusal::BadFiles)?;
                if list.is_empty() || list.len() > MAX_FILES {
                    return Err(Refusal::BadFiles);
                }
                list.iter()
                    .map(|v| as_bytes(v).filter(|n| is_file_name(n)))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(Refusal::BadFiles)?
            }
            _ => Vec::new(),
        };

        let accept_label = opt("accept_label")
            .and_then(as_str)
            .and_then(|l| text(&strip_mnemonic(l), MAX_TEXT));

        Ok(Pick {
            mode,
            title: text(title, MAX_TEXT),
            accept_label,
            current_folder,
            current_name,
            filters,
            current_filter,
            choices,
            files,
            parent: parent(parent_window),
        })
    }

    /// Fog's argv, after the program name. Values only, each its own
    /// element: nothing here is ever seen by a shell.
    pub fn argv(&self) -> Vec<OsString> {
        self.argv_with(MimeGlobs::system())
    }

    /// [`Self::argv`] with an explicit mime database. Mime patterns become
    /// `--glob`s; a type the database does not know is dropped.
    pub fn argv_with(&self, db: &MimeGlobs) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--pick".into(), self.mode.arg().into()];
        let mut push = |k: &str, v: OsString| {
            a.push(k.into());
            a.push(v);
        };
        if let Some(t) = &self.title {
            push("--title", t.into());
        }
        if let Some(l) = &self.accept_label {
            push("--accept-label", l.into());
        }
        if let Some(d) = &self.current_folder {
            push("--current-folder", OsString::from_vec(d.clone()));
        }
        if let Some(n) = &self.current_name {
            push("--current-name", n.into());
        }
        for f in &self.filters {
            push("--filter", (&f.name).into());
            for p in &f.patterns {
                match p {
                    Pattern::Glob(g) => push("--glob", g.into()),
                    Pattern::Mime(m) => {
                        let globs = db.globs(m);
                        if globs.is_empty() && std::env::var_os("EC_SERVICES_DEBUG").is_some() {
                            eprintln!("filechooser: no globs for mime type {m}; ignored");
                        }
                        for g in globs {
                            push("--glob", g.into());
                        }
                    }
                }
            }
        }
        if let Some(i) = self.current_filter {
            push("--current-filter", i.to_string().into());
        }
        for c in &self.choices {
            push("--choice", (&c.label).into());
            for (_, label) in &c.options {
                push("--choice-option", label.into());
            }
            let d = match c.initial {
                Initial::Option(i) => i.to_string(),
                Initial::Bool(b) => b.to_string(),
            };
            push("--choice-default", d.into());
        }
        for f in &self.files {
            push("--file", OsString::from_vec(f.clone()));
        }
        if let Some(p) = &self.parent {
            push("--parent", p.into());
        }
        a
    }
}

// --- Fog's answer ----------------------------------------------------------

/// What came back from one dialog.
#[derive(Debug)]
pub enum Outcome {
    /// Fog is not installed, or could not be started.
    SpawnFailed,
    /// `Request.Close` arrived; the dialog was (or never got) killed.
    Closed,
    /// It wrote more than [`MAX_OUTPUT`]; killed.
    Overflow,
    Exited {
        code: Option<i32>,
        out: Vec<u8>,
    },
}

/// The `(response, results)` of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub response: u32,
    pub uris: Vec<String>,
    pub choices: Vec<(String, String)>,
    pub current_filter: Option<RawFilter>,
}

impl Reply {
    fn bare(response: u32) -> Reply {
        Reply {
            response,
            uris: Vec::new(),
            choices: Vec::new(),
            current_filter: None,
        }
    }

    pub fn other() -> Reply {
        Reply::bare(RESPONSE_OTHER)
    }

    /// The portal's results vardict. Empty unless the response is success.
    pub fn results(self) -> HashMap<String, OwnedValue> {
        let mut out = HashMap::new();
        if self.response != RESPONSE_OK {
            return out;
        }
        let mut put = |k: &str, v: Value<'static>| {
            // Strings and structs of strings: no fds, so this cannot fail.
            if let Ok(v) = OwnedValue::try_from(v) {
                out.insert(k.to_owned(), v);
            }
        };
        put("uris", Value::from(self.uris));
        if !self.choices.is_empty() {
            put("choices", Value::from(self.choices));
        }
        if let Some(f) = self.current_filter {
            put("current_filter", Value::from(f));
        }
        out
    }
}

/// Fog's stdout, decoded. `None` if it breaks the record format.
#[derive(Debug, Default, PartialEq, Eq)]
struct Answer {
    paths: Vec<Vec<u8>>,
    filter: Option<usize>,
    /// (choice index, option index or checkbox state)
    choices: Vec<(usize, Initial)>,
}

fn records(out: &[u8]) -> Option<Answer> {
    let mut a = Answer::default();
    if out.is_empty() {
        return Some(a);
    }
    let body = out.strip_suffix(&[0])?;
    for rec in body.split(|&b| b == 0) {
        let (&tag, rest) = rec.split_first()?;
        match tag {
            b'P' => a.paths.push(rest.to_vec()),
            b'F' if a.filter.is_none() => a.filter = Some(std::str::from_utf8(rest).ok()?.parse().ok()?),
            b'C' => {
                let (i, v) = std::str::from_utf8(rest).ok()?.split_once('=')?;
                let v = match v {
                    "true" => Initial::Bool(true),
                    "false" => Initial::Bool(false),
                    n => Initial::Option(n.parse().ok()?),
                };
                a.choices.push((i.parse().ok()?, v));
            }
            _ => return None,
        }
    }
    Some(a)
}

/// An absolute, lexically clean path: `/`-led, no empty, `.` or `..`
/// component, no trailing `/` (except `/` itself), at most PATH_MAX.
fn clean_path(p: &[u8]) -> bool {
    if p == b"/" {
        return true;
    }
    p.len() <= MAX_PATH
        && p.first() == Some(&b'/')
        && p[1..]
            .split(|&b| b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b"..")
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = dir.to_vec();
    if p.last() != Some(&b'/') {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// `name` in `dir`, or `name (2)`, `name (3)`… before the extension if it is
/// taken on disk or already handed out in this reply.
fn unique(
    dir: &[u8],
    name: &[u8],
    used: &mut HashSet<Vec<u8>>,
    exists: &dyn Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let free = |p: &Vec<u8>, used: &HashSet<Vec<u8>>| !used.contains(p) && !exists(p);
    let first = join(dir, name);
    if free(&first, used) {
        used.insert(first.clone());
        return Some(first);
    }
    // A leading dot is a hidden file, not an extension.
    let (stem, ext) = match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, &b""[..]),
    };
    for n in 2..=MAX_SUFFIX {
        let mut cand = stem.to_vec();
        cand.extend_from_slice(format!(" ({n})").as_bytes());
        cand.extend_from_slice(ext);
        if cand.len() > MAX_NAME {
            return None;
        }
        let p = join(dir, &cand);
        if free(&p, used) {
            used.insert(p.clone());
            return Some(p);
        }
    }
    None
}

/// A path as a `file://` URI: every byte outside RFC 3986's unreserved set
/// and `/` percent-encoded, so any Linux file name round-trips.
pub fn file_uri(path: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut s = String::with_capacity(7 + path.len());
    s.push_str("file://");
    for &b in path {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            s.push(b as char);
        } else {
            s.push('%');
            s.push(HEX[usize::from(b >> 4)] as char);
            s.push(HEX[usize::from(b & 15)] as char);
        }
    }
    s
}

/// Map a dialog's outcome to the portal reply. `exists` is asked only for
/// `SaveFiles` names. Exit 0 with a well-formed answer for the mode is
/// success, exit 1 is cancelled, everything else (including a malformed
/// answer) is "other": a reply never carries a path Fog did not report.
pub fn reply(pick: &Pick, outcome: Outcome, exists: &dyn Fn(&[u8]) -> bool) -> Reply {
    let out = match outcome {
        Outcome::Exited { code: Some(0), out } => out,
        Outcome::Exited { code: Some(1), .. } => return Reply::bare(RESPONSE_CANCELLED),
        Outcome::SpawnFailed | Outcome::Closed | Outcome::Overflow | Outcome::Exited { .. } => {
            return Reply::other()
        }
    };
    let Some(a) = records(&out) else {
        return Reply::other();
    };
    let count_ok = match pick.mode {
        Mode::OpenMultiple | Mode::DirectoryMultiple => !a.paths.is_empty(),
        _ => a.paths.len() == 1,
    };
    if !count_ok || !a.paths.iter().all(|p| clean_path(p)) {
        return Reply::other();
    }
    if pick.mode == Mode::Save && a.paths[0] == b"/" {
        return Reply::other();
    }
    debug_assert!(pick.mode.multiple() || a.paths.len() == 1);

    let uris = if pick.mode == Mode::SaveFiles {
        let mut used = HashSet::new();
        let Some(paths) = pick
            .files
            .iter()
            .map(|n| unique(&a.paths[0], n, &mut used, exists))
            .collect::<Option<Vec<_>>>()
        else {
            return Reply::other();
        };
        paths.iter().map(|p| file_uri(p)).collect()
    } else {
        a.paths.iter().map(|p| file_uri(p)).collect()
    };

    let current_filter = match a.filter {
        Some(i) => match pick.filters.get(i) {
            Some(f) => Some(f.raw.clone()),
            None => return Reply::other(),
        },
        None => None,
    };
    let mut choices = Vec::new();
    for (i, v) in a.choices {
        let Some(c) = pick.choices.get(i) else {
            return Reply::other();
        };
        let value = match (v, c.options.is_empty()) {
            (Initial::Bool(b), true) => b.to_string(),
            (Initial::Option(n), false) => match c.options.get(n) {
                Some((id, _)) => id.clone(),
                None => return Reply::other(),
            },
            _ => return Reply::other(),
        };
        choices.push((c.id.clone(), value));
    }
    Reply {
        response: RESPONSE_OK,
        uris,
        choices,
        current_filter,
    }
}

// --- the child -------------------------------------------------------------

/// One dialog's lifetime, shared by its thread and its `Request` object.
#[derive(Debug, Default)]
struct Life {
    pid: Option<i32>,
    /// Set, under this lock, just before the pid is reaped: after that it may
    /// belong to someone else and is never signalled.
    reaped: bool,
    closed: bool,
}

fn lock(m: &Mutex<Life>) -> MutexGuard<'_, Life> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn kill_unreaped(l: &Life) {
    if let (Some(pid), false) = (l.pid, l.reaped) {
        // SAFETY: plain signal. `pid` is our own child and, with the lock
        // held and `reaped` false, not yet reaped, so it cannot have been
        // reused by another process.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Block until `pid` has exited, leaving it unreaped (a zombie keeps the pid
/// ours, so a racing `Close` can still signal it harmlessly).
fn wait_exited(pid: i32) {
    loop {
        // SAFETY: siginfo_t is plain data; zeroed is a valid value.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid out-pointer for the duration of the call.
        let r = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }
    }
}

/// Spawn Fog, read its answer, reap it. Runs on the dialog's own thread.
fn run(argv: Vec<OsString>, life: &Mutex<Life>) -> Outcome {
    let mut child = {
        let mut l = lock(life);
        if l.closed {
            return Outcome::Closed;
        }
        let spawned = Command::new(FOG)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn();
        match spawned {
            Ok(c) => {
                l.pid = i32::try_from(c.id()).ok();
                c
            }
            Err(_) => return Outcome::SpawnFailed,
        }
    };
    let pid = i32::try_from(child.id()).unwrap_or(0);
    let mut out = Vec::new();
    let overflow = match child.stdout.take() {
        Some(s) => s.take(MAX_OUTPUT + 1).read_to_end(&mut out).is_err() || out.len() as u64 > MAX_OUTPUT,
        None => true,
    };
    if overflow {
        kill_unreaped(&lock(life));
    }
    if pid > 0 {
        wait_exited(pid);
    }
    let closed = {
        let mut l = lock(life);
        l.reaped = true;
        l.closed
    };
    let status = child.wait();
    if closed {
        return Outcome::Closed;
    }
    if overflow {
        return Outcome::Overflow;
    }
    Outcome::Exited {
        code: status.ok().and_then(|s| s.code()),
        out,
    }
}

// --- a oneshot the bus side can await, std only ----------------------------

struct Slot<T> {
    value: Option<T>,
    done: bool,
    waker: Option<Waker>,
}

struct Sender<T>(Arc<Mutex<Slot<T>>>);
struct Receiver<T>(Arc<Mutex<Slot<T>>>);

fn oneshot<T>() -> (Sender<T>, Receiver<T>) {
    let slot = Arc::new(Mutex::new(Slot {
        value: None,
        done: false,
        waker: None,
    }));
    (Sender(Arc::clone(&slot)), Receiver(slot))
}

impl<T> Sender<T> {
    fn send(self, v: T) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).value = Some(v);
        // Drop finishes the send: marks done and wakes.
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
            s.done = true;
            s.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

impl<T> Future for Receiver<T> {
    /// `None` if the sender went away without sending (its thread panicked).
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut s = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if s.done {
            return Poll::Ready(s.value.take());
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

// --- the bus objects -------------------------------------------------------

/// `org.freedesktop.impl.portal.Request`, exported at the call's handle for
/// as long as its dialog is open.
struct Request {
    life: Arc<Mutex<Life>>,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    fn close(&self) {
        let mut l = lock(&self.life);
        l.closed = true;
        kill_unreaped(&l);
    }
}

/// `org.freedesktop.impl.portal.FileChooser`, served beside Settings.
pub struct FileChooser;

fn exists(p: &[u8]) -> bool {
    std::fs::symlink_metadata(OsStr::from_bytes(p)).is_ok()
}

async fn serve(
    server: &ObjectServer,
    handle: OwnedObjectPath,
    method: Method,
    parent_window: &str,
    title: &str,
    options: &HashMap<String, OwnedValue>,
) -> (u32, HashMap<String, OwnedValue>) {
    let Ok(pick) = Pick::from_call(method, title, parent_window, options) else {
        return (RESPONSE_OTHER, HashMap::new());
    };
    let life = Arc::new(Mutex::new(Life::default()));
    let _ = server
        .at(
            &handle,
            Request {
                life: Arc::clone(&life),
            },
        )
        .await;
    let (tx, rx) = oneshot();
    let argv = pick.argv();
    let thread_life = Arc::clone(&life);
    // If the thread cannot start, `tx` is dropped with the closure and the
    // receiver resolves to `None`: answered "other", not hung.
    let _ = thread::Builder::new()
        .name("ec-portal-pick".into())
        .spawn(move || {
            let outcome = run(argv, &thread_life);
            tx.send(reply(&pick, outcome, &exists));
        });
    let r = rx.await.unwrap_or_else(Reply::other);
    let _ = server.remove::<Request, _>(&handle).await;
    (r.response, r.results())
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    #[allow(clippy::too_many_arguments)]
    async fn open_file(
        &self,
        handle: OwnedObjectPath,
        _app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, HashMap<String, OwnedValue>) {
        serve(server, handle, Method::OpenFile, &parent_window, &title, &options).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn save_file(
        &self,
        handle: OwnedObjectPath,
        _app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, HashMap<String, OwnedValue>) {
        serve(server, handle, Method::SaveFile, &parent_window, &title, &options).await
    }

    #[allow(clippy::too_many_arguments)]
    async fn save_files(
        &self,
        handle: OwnedObjectPath,
        _app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> (u32, HashMap<String, OwnedValue>) {
        serve(
            server,
            handle,
            Method::SaveFiles,
            &parent_window,
            &title,
            &options,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ov(v: Value<'static>) -> OwnedValue {
        OwnedValue::try_from(v).unwrap()
    }

    fn bytes(b: &[u8]) -> Value<'static> {
        Value::from(b.to_vec())
    }

    fn opts(pairs: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), ov(v))).collect()
    }

    fn filt(name: &str, pats: &[(u32, &str)]) -> Value<'static> {
        let pats: Vec<(u32, String)> = pats.iter().map(|(k, p)| (*k, (*p).to_owned())).collect();
        Value::from((name.to_owned(), pats))
    }

    fn open(o: Vec<(&str, Value<'static>)>) -> Pick {
        Pick::from_call(Method::OpenFile, "Open", "", &opts(o)).unwrap()
    }

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn ok(out: &[u8]) -> Outcome {
        Outcome::Exited {
            code: Some(0),
            out: out.to_vec(),
        }
    }

    fn none(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn mode_follows_method_and_flags() {
        assert_eq!(open(vec![]).mode, Mode::Open);
        assert_eq!(
            open(vec![("multiple", Value::Bool(true))]).mode,
            Mode::OpenMultiple
        );
        assert_eq!(open(vec![("directory", Value::Bool(true))]).mode, Mode::Directory);
        assert_eq!(
            open(vec![
                ("directory", Value::Bool(true)),
                ("multiple", Value::Bool(true))
            ])
            .mode,
            Mode::DirectoryMultiple
        );
        // A wrongly typed flag is ignored, not read as true.
        assert_eq!(open(vec![("multiple", Value::U32(1))]).mode, Mode::Open);
        let save = Pick::from_call(Method::SaveFile, "", "", &HashMap::new()).unwrap();
        assert_eq!(save.mode, Mode::Save);
    }

    #[test]
    fn argv_is_values_only_and_untrusted_text_is_cleaned() {
        let p = Pick::from_call(
            Method::OpenFile,
            "Pick\n; rm -rf ~ \u{202e}gpj.exe",
            "wayland:abc-123_X",
            &opts(vec![
                ("accept_label", Value::from("_Open")),
                ("current_folder", bytes(b"/home/u/a b\0")),
                (
                    "filters",
                    Value::from(vec![(
                        "Images".to_owned(),
                        vec![(0u32, "*.png".to_owned()), (1, "image/jpeg".to_owned())],
                    )]),
                ),
            ]),
        )
        .unwrap();
        let want = os(&[
            "--pick",
            "open",
            "--title",
            "Pick; rm -rf ~ gpj.exe",
            "--accept-label",
            "Open",
            "--current-folder",
            "/home/u/a b",
            "--filter",
            "Images",
            "--glob",
            "*.png",
            "--glob",
            "*.jpeg",
            "--parent",
            "wayland:abc-123_X",
        ]);
        assert_eq!(p.argv_with(&MimeGlobs::parse("50:image/jpeg:*.jpeg\n")), want);
        // Long titles are clamped.
        let long = "x".repeat(10_000);
        let p = Pick::from_call(Method::OpenFile, &long, "", &HashMap::new()).unwrap();
        assert_eq!(p.title.unwrap().chars().count(), MAX_TEXT);
        // A title of only control characters is no title.
        let p = Pick::from_call(Method::OpenFile, "\u{1b}\u{7}", "", &HashMap::new()).unwrap();
        assert_eq!(p.argv(), os(&["--pick", "open"]));
    }

    #[test]
    fn mnemonics_are_dropped() {
        assert_eq!(strip_mnemonic("_Open"), "Open");
        assert_eq!(strip_mnemonic("Save __as"), "Save _as");
        assert_eq!(strip_mnemonic("plain"), "plain");
    }

    #[test]
    fn parent_handles_are_only_accepted_in_their_two_shapes() {
        assert_eq!(parent("x11:1c00007").as_deref(), Some("x11:1c00007"));
        assert!(parent("x11:zz").is_none());
        assert!(parent("wayland:").is_none());
        assert!(parent("wayland:a b").is_none());
        assert!(parent("wayland:--title").is_some()); // only ever an argv value
        assert!(parent("").is_none());
        assert!(parent("mir:1").is_none());
        assert!(parent(&format!("wayland:{}", "a".repeat(200))).is_none());
    }

    #[test]
    fn folders_must_be_absolute() {
        assert_eq!(
            open(vec![("current_folder", bytes(b"/tmp\0"))])
                .current_folder
                .as_deref(),
            Some(&b"/tmp"[..])
        );
        assert!(open(vec![("current_folder", bytes(b"tmp\0"))])
            .current_folder
            .is_none());
        assert!(open(vec![("current_folder", bytes(b"/a\0b\0"))])
            .current_folder
            .is_none());
        assert!(open(vec![("current_folder", Value::from("/tmp"))])
            .current_folder
            .is_none());
    }

    #[test]
    fn current_file_fills_folder_and_name() {
        let p = Pick::from_call(
            Method::SaveFile,
            "",
            "",
            &opts(vec![("current_file", bytes(b"/home/u/notes.txt\0"))]),
        )
        .unwrap();
        assert_eq!(p.current_folder.as_deref(), Some(&b"/home/u"[..]));
        assert_eq!(p.current_name.as_deref(), Some("notes.txt"));
        let p = Pick::from_call(
            Method::SaveFile,
            "",
            "",
            &opts(vec![
                ("current_file", bytes(b"/home/u/notes.txt\0")),
                ("current_name", Value::from("new.txt")),
                ("current_folder", bytes(b"/tmp\0")),
            ]),
        )
        .unwrap();
        assert_eq!(p.current_folder.as_deref(), Some(&b"/tmp"[..]));
        assert_eq!(p.current_name.as_deref(), Some("new.txt"));
    }

    #[test]
    fn save_names_with_a_slash_are_not_names() {
        let name = |n: &str| {
            Pick::from_call(
                Method::SaveFile,
                "",
                "",
                &opts(vec![("current_name", Value::from(n.to_owned()))]),
            )
            .unwrap()
            .current_name
        };
        assert_eq!(name("a.png").as_deref(), Some("a.png"));
        assert!(name("../../.bashrc").is_none());
        assert!(name("..").is_none());
        assert_eq!(name("a\nb").as_deref(), Some("ab"));
        // current_name is a Save option; Open ignores it.
        assert!(open(vec![("current_name", Value::from("a"))])
            .current_name
            .is_none());
    }

    #[test]
    fn filters_translate_and_current_filter_selects() {
        let list = Value::from(vec![
            ("Images".to_owned(), vec![(0u32, "*.png".to_owned())]),
            (
                "Bogus".to_owned(),
                vec![(7u32, "x".to_owned()), (1, "notmime".to_owned())],
            ),
            ("Text".to_owned(), vec![(1u32, "text/plain".to_owned())]),
        ]);
        let p = open(vec![
            ("filters", list.try_clone().unwrap()),
            ("current_filter", filt("Text", &[(1, "text/plain")])),
        ]);
        // The filter with no usable pattern is dropped, indices follow.
        assert_eq!(p.filters.len(), 2);
        assert_eq!(p.current_filter, Some(1));
        let p = open(vec![
            ("filters", list),
            ("current_filter", filt("Other", &[(0, "*")])),
        ]);
        assert_eq!(p.current_filter, None);
        // With no list, current_filter applies on its own.
        let p = open(vec![("current_filter", filt("Docs", &[(0, "*.pdf")]))]);
        assert_eq!(p.filters.len(), 1);
        assert_eq!(p.current_filter, Some(0));
        assert!(p.argv().ends_with(&os(&[
            "--filter",
            "Docs",
            "--glob",
            "*.pdf",
            "--current-filter",
            "0"
        ])));
    }

    const GLOBS2_FIXTURE: &str = "# Generated by update-mime-database\n\
        50:image/png:*.png\n\
        50:image/jpeg:*.jpg\n\
        50:image/jpeg:*.jpeg:cs\n\
        50:image/svg+xml:*.svg\n\
        50:text/plain:*.txt\n\
        bogus line\n\
        50:application/x-none:__NOGLOBS__\n";

    #[test]
    fn mime_globs_parse_and_expand() {
        let db = MimeGlobs::parse(GLOBS2_FIXTURE);
        assert_eq!(db.globs("image/png"), ["*.png"]);
        assert_eq!(db.globs("image/jpeg"), ["*.jpeg", "*.jpg"]);
        assert_eq!(db.globs("image/*"), ["*.jpeg", "*.jpg", "*.png", "*.svg"]);
        assert!(db.globs("video/mp4").is_empty());
        assert!(db.globs("application/x-none").is_empty());
    }

    #[test]
    fn mime_filter_becomes_globs_and_unknown_is_ignored() {
        let list = Value::from(vec![(
            "Pics".to_owned(),
            vec![
                (1u32, "image/png".to_owned()),
                (1, "video/mp4".to_owned()),
                (0, "*.gif".to_owned()),
            ],
        )]);
        let p = open(vec![("filters", list)]);
        let a = p.argv_with(&MimeGlobs::parse(GLOBS2_FIXTURE));
        assert!(a.ends_with(&os(&["--filter", "Pics", "--glob", "*.png", "--glob", "*.gif"])));
        assert!(!a.contains(&OsString::from("--mime")));
    }

    #[test]
    fn choices_pass_labels_and_map_back_to_ids() {
        let choices = Value::from(vec![
            (
                "enc".to_owned(),
                "Encoding".to_owned(),
                vec![
                    ("utf8".to_owned(), "UTF-8".to_owned()),
                    ("l1".to_owned(), "Latin-1".to_owned()),
                ],
                "l1".to_owned(),
            ),
            ("ro".to_owned(), "Read only".to_owned(), vec![], "true".to_owned()),
        ]);
        let p = open(vec![("choices", choices)]);
        assert_eq!(
            p.argv(),
            os(&[
                "--pick",
                "open",
                "--title",
                "Open",
                "--choice",
                "Encoding",
                "--choice-option",
                "UTF-8",
                "--choice-option",
                "Latin-1",
                "--choice-default",
                "1",
                "--choice",
                "Read only",
                "--choice-default",
                "true",
            ])
        );
        let r = reply(&p, ok(b"P/a\0C0=0\0C1=false\0"), &none);
        assert_eq!(r.response, RESPONSE_OK);
        assert_eq!(
            r.choices,
            vec![
                ("enc".to_owned(), "utf8".to_owned()),
                ("ro".to_owned(), "false".to_owned())
            ]
        );
        // Unreported choices are not filled in.
        assert!(reply(&p, ok(b"P/a\0"), &none).choices.is_empty());
        // Out of range, or the wrong kind of value: other.
        assert_eq!(reply(&p, ok(b"P/a\0C0=9\0"), &none).response, RESPONSE_OTHER);
        assert_eq!(reply(&p, ok(b"P/a\0C0=true\0"), &none).response, RESPONSE_OTHER);
        assert_eq!(reply(&p, ok(b"P/a\0C5=0\0"), &none).response, RESPONSE_OTHER);
    }

    #[test]
    fn uris_are_percent_encoded() {
        assert_eq!(
            file_uri(b"/home/u/a b#c%d.txt"),
            "file:///home/u/a%20b%23c%25d.txt"
        );
        assert_eq!(file_uri("/tmp/é".as_bytes()), "file:///tmp/%C3%A9");
        assert_eq!(file_uri(b"/x/\xff\n"), "file:///x/%FF%0A");
        assert_eq!(file_uri(b"/a-b_c.d~e"), "file:///a-b_c.d~e");
    }

    #[test]
    fn exit_codes_map_to_responses() {
        let p = open(vec![]);
        assert_eq!(
            reply(&p, ok(b"P/home/u/a.png\0"), &none).uris,
            ["file:///home/u/a.png"]
        );
        let code = |c| Outcome::Exited {
            code: c,
            out: Vec::new(),
        };
        assert_eq!(reply(&p, code(Some(1)), &none).response, RESPONSE_CANCELLED);
        assert_eq!(reply(&p, code(Some(2)), &none).response, RESPONSE_OTHER);
        assert_eq!(reply(&p, code(None), &none).response, RESPONSE_OTHER);
        assert_eq!(reply(&p, Outcome::SpawnFailed, &none).response, RESPONSE_OTHER);
        assert_eq!(reply(&p, Outcome::Closed, &none).response, RESPONSE_OTHER);
        assert_eq!(reply(&p, Outcome::Overflow, &none).response, RESPONSE_OTHER);
        // Exit 0 with nothing chosen is not a success.
        assert_eq!(reply(&p, ok(b""), &none).response, RESPONSE_OTHER);
    }

    #[test]
    fn malformed_answers_are_never_success() {
        let p = open(vec![]);
        for bad in [
            &b"P/a"[..],    // unterminated
            b"Prelative\0", // not absolute
            b"P/a/../b\0",  // not clean
            b"P/a//b\0",
            b"P/a/\0",
            b"X/a\0",      // unknown record
            b"\0",         // empty record
            b"P/a\0P/b\0", // two for a single pick
            b"P/a\0F0\0",  // no filter 0
        ] {
            assert_eq!(reply(&p, ok(bad), &none).response, RESPONSE_OTHER, "{bad:?}");
        }
        let multi = open(vec![("multiple", Value::Bool(true))]);
        assert_eq!(reply(&multi, ok(b"P/a\0P/b\0"), &none).uris.len(), 2);
        let save = Pick::from_call(Method::SaveFile, "", "", &HashMap::new()).unwrap();
        assert_eq!(reply(&save, ok(b"P/\0"), &none).response, RESPONSE_OTHER);
    }

    #[test]
    fn current_filter_result_echoes_the_apps_filter() {
        let p = open(vec![(
            "filters",
            Value::from(vec![("\u{1b}Imgs".to_owned(), vec![(0u32, "*.png".to_owned())])]),
        )]);
        let r = reply(&p, ok(b"P/a.png\0F0\0"), &none);
        assert_eq!(
            r.current_filter,
            Some(("\u{1b}Imgs".to_owned(), vec![(0, "*.png".to_owned())]))
        );
        assert_eq!(p.filters[0].name, "Imgs");
    }

    fn save_files(names: &[&[u8]]) -> Result<Pick, Refusal> {
        let files: Vec<Vec<u8>> = names.iter().map(|n| n.to_vec()).collect();
        Pick::from_call(
            Method::SaveFiles,
            "",
            "",
            &opts(vec![("files", Value::from(files))]),
        )
    }

    #[test]
    fn save_files_joins_names_and_avoids_collisions() {
        let p = save_files(&[b"a.txt\0", b"a.txt\0", b".rc\0", b"noext\0"]).unwrap();
        assert!(p.argv().ends_with(&os(&[
            "--file", "a.txt", "--file", "a.txt", "--file", ".rc", "--file", "noext"
        ])));
        let taken = |p: &[u8]| p == b"/d/noext" || p == b"/d/.rc";
        let r = reply(&p, ok(b"P/d\0"), &taken);
        assert_eq!(r.response, RESPONSE_OK);
        assert_eq!(
            r.uris,
            [
                "file:///d/a.txt",
                "file:///d/a%20%282%29.txt",
                "file:///d/.rc%20%282%29",
                "file:///d/noext%20%282%29"
            ]
        );
        // The folder is the one answer; two is malformed.
        assert_eq!(reply(&p, ok(b"P/d\0P/e\0"), &taken).response, RESPONSE_OTHER);
    }

    #[test]
    fn save_files_refuses_names_it_cannot_honour() {
        assert_eq!(save_files(&[]), Err(Refusal::BadFiles));
        assert_eq!(save_files(&[b"../x\0"]), Err(Refusal::BadFiles));
        assert_eq!(save_files(&[b"a/b\0"]), Err(Refusal::BadFiles));
        assert_eq!(save_files(&[b"..\0"]), Err(Refusal::BadFiles));
        assert_eq!(save_files(&[b"\0"]), Err(Refusal::BadFiles));
        assert_eq!(save_files(&[b"a\nb\0"]), Err(Refusal::BadFiles));
        assert!(Pick::from_call(Method::SaveFiles, "", "", &HashMap::new()).is_err());
        assert!(save_files(&[b"ok\0"]).is_ok());
    }

    #[test]
    fn results_are_empty_unless_success() {
        let mut r = Reply::bare(RESPONSE_OK);
        r.uris = vec!["file:///a".into()];
        let m = r.clone().results();
        assert!(m.contains_key("uris"));
        assert!(!m.contains_key("choices"));
        r.response = RESPONSE_CANCELLED;
        assert!(r.results().is_empty());
    }

    #[test]
    fn oneshot_resolves_when_the_sender_is_dropped() {
        let mut cx = Context::from_waker(Waker::noop());
        let (tx, mut rx) = oneshot::<u8>();
        assert!(Pin::new(&mut rx).poll(&mut cx).is_pending());
        tx.send(7);
        assert_eq!(Pin::new(&mut rx).poll(&mut cx), Poll::Ready(Some(7)));
        let (tx, mut rx) = oneshot::<u8>();
        drop(tx);
        assert_eq!(Pin::new(&mut rx).poll(&mut cx), Poll::Ready(None));
    }

    #[test]
    fn a_request_closed_before_spawn_never_starts_fog() {
        let life = Mutex::new(Life::default());
        lock(&life).closed = true;
        assert!(matches!(run(Vec::new(), &life), Outcome::Closed));
    }
}
