// SPDX-License-Identifier: AGPL-3.0-only

//! Picker mode (ADR 0100, FOG §File picker): `ec-fog-ui --pick MODE …` is
//! the process `ec-portal` spawns for each
//! `org.freedesktop.impl.portal.FileChooser` request. This is the
//! non-visual half: the command line, the [`PickRequest`] the view reads,
//! and the result contract. No I/O but the final write.
//!
//! Command line (`--pick` must come first; every option takes exactly the
//! next argument as its value, whatever it starts with):
//!
//! ```text
//! --pick open|open-multiple|directory|directory-multiple|save|save-files
//! --title T  --accept-label L  --current-folder ABS  --current-name NAME
//! --filter NAME (--glob PAT | --mime TYPE)…   repeated; patterns attach to the last filter
//! --current-filter N                          index into the filters
//! --choice LABEL (--choice-option LABEL)… [--choice-default N|true|false]
//!                                             repeated; no options = a checkbox
//! --file NAME                                 save-files: the names being saved
//! --parent wayland:H|x11:X                    transient-for hint only
//! ```
//!
//! Result: on confirm, stdout carries NUL-terminated records and the process
//! exits 0; on cancel it writes nothing and exits 1; a bad command line or a
//! failed write exits 2. Records, in this order:
//!
//! - `P<absolute path>` — one per chosen path, in the user's order. For
//!   `save` the full target path (folder + name), for `save-files` the folder.
//! - `F<n>` — the filter in use, if the view shows filters.
//! - `C<i>=<n>` — combo choice `i` set to option `n`; `C<i>=true|false` a
//!   checkbox. Only choices the view actually showed.
//!
//! [`claim_stdout`] moves stdout to a private close-on-exec descriptor before
//! the UI starts and points fd 1 at /dev/null, so nothing else this process
//! or its children (custom actions) print can reach the portal.

use std::convert::Infallible;
use std::ffi::OsString;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::ffi::OsStrExt;
use std::sync::OnceLock;

/// Characters kept of any one text value. `ec-portal` clamps tighter; this
/// is for any other caller.
const MAX_TEXT: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// One file.
    Open,
    /// One or more files.
    OpenMultiple,
    /// One folder.
    Directory,
    /// One or more folders.
    DirectoryMultiple,
    /// A target path: a folder plus a typed name.
    Save,
    /// A folder to save [`PickRequest::files`] into.
    SaveFiles,
}

impl PickMode {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => PickMode::Open,
            "open-multiple" => PickMode::OpenMultiple,
            "directory" => PickMode::Directory,
            "directory-multiple" => PickMode::DirectoryMultiple,
            "save" => PickMode::Save,
            "save-files" => PickMode::SaveFiles,
            _ => return None,
        })
    }

    /// More than one path may be confirmed.
    pub fn multiple(self) -> bool {
        matches!(self, PickMode::OpenMultiple | PickMode::DirectoryMultiple)
    }

    /// The user confirms folders, not files.
    pub fn folders(self) -> bool {
        matches!(
            self,
            PickMode::Directory | PickMode::DirectoryMultiple | PickMode::SaveFiles
        )
    }

    /// The dialog needs a name field.
    pub fn named(self) -> bool {
        self == PickMode::Save
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    pub name: String,
    pub globs: Vec<String>,
    pub mimes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceValue {
    /// Index into [`Choice::options`].
    Option(usize),
    /// A checkbox's state.
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    /// Empty: a checkbox.
    pub options: Vec<String>,
    pub default: ChoiceValue,
}

/// One picker invocation, as the view reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickRequest {
    pub mode: PickMode,
    pub title: Option<String>,
    pub accept_label: Option<String>,
    /// Absolute. Where the picker opens; `None` is home.
    pub current_folder: Option<Vec<u8>>,
    /// `save`: the name field's initial text.
    pub current_name: Option<String>,
    pub filters: Vec<Filter>,
    /// Index into `filters`.
    pub current_filter: Option<usize>,
    pub choices: Vec<Choice>,
    /// `save-files`: the names to be written into the chosen folder.
    pub files: Vec<Vec<u8>>,
    /// The caller's window, for a transient-for hint. Not trusted for
    /// anything else, and not used yet (no xdg-foreign).
    pub parent: Option<String>,
}

/// What the user confirmed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Answer {
    /// Absolute paths, in the user's order.
    pub paths: Vec<Vec<u8>>,
    /// The filter in use, an index into [`PickRequest::filters`].
    pub filter: Option<usize>,
    /// `(choice index, value)` for each choice shown.
    pub choices: Vec<(usize, ChoiceValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgError {
    UnknownMode(String),
    UnknownOption(String),
    MissingValue(&'static str),
    NotUtf8(&'static str),
    NotAbsolute,
    /// `--glob`/`--mime` before any `--filter`, `--choice-*` before `--choice`.
    Orphan(&'static str),
    BadIndex(&'static str),
    Repeated(&'static str),
    /// `--file` outside save-files, or save-files without one.
    Files,
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgError::UnknownMode(m) => write!(f, "unknown --pick mode {m:?}"),
            ArgError::UnknownOption(o) => write!(f, "unknown option {o:?}"),
            ArgError::MissingValue(o) => write!(f, "{o} needs a value"),
            ArgError::NotUtf8(o) => write!(f, "{o} is not UTF-8"),
            ArgError::NotAbsolute => write!(f, "--current-folder must be absolute"),
            ArgError::Orphan(o) => write!(f, "{o} has nothing to attach to"),
            ArgError::BadIndex(o) => write!(f, "{o} is out of range"),
            ArgError::Repeated(o) => write!(f, "{o} given twice"),
            ArgError::Files => write!(f, "--file belongs to, and is required by, save-files"),
        }
    }
}

/// Why [`confirm`] would not answer. The view shows [`Refused::hint`] and
/// stays open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    NothingChosen,
    OnlyOne,
    NotAbsolute,
    /// `save` needs a name, not a bare folder.
    NoName,
    BadFilter,
    BadChoice,
}

impl Refused {
    pub fn hint(self) -> &'static str {
        match self {
            Refused::NothingChosen => "nothing chosen",
            Refused::OnlyOne => "choose one item",
            Refused::NotAbsolute => "not a full path",
            Refused::NoName => "type a name",
            Refused::BadFilter => "no such filter",
            Refused::BadChoice => "no such option",
        }
    }
}

fn once<T>(slot: &Option<T>, key: &'static str) -> Result<(), ArgError> {
    match slot {
        Some(_) => Err(ArgError::Repeated(key)),
        None => Ok(()),
    }
}

/// App text as one safe display line: control characters dropped, clamped.
fn text(s: String) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect()
}

/// Parse the arguments after the program name. `Ok(None)` if this is not a
/// picker invocation (the first argument is not `--pick`), so the normal
/// `ec-fog-ui [PATH]` path is untouched.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<PickRequest>, ArgError> {
    let mut args = args.into_iter();
    match args.next() {
        Some(a) if a == "--pick" => {}
        _ => return Ok(None),
    }
    let mode = args.next().ok_or(ArgError::MissingValue("--pick"))?;
    let mode = mode
        .to_str()
        .and_then(PickMode::parse)
        .ok_or_else(|| ArgError::UnknownMode(mode.to_string_lossy().into_owned()))?;
    let mut req = PickRequest {
        mode,
        title: None,
        accept_label: None,
        current_folder: None,
        current_name: None,
        filters: Vec::new(),
        current_filter: None,
        choices: Vec::new(),
        files: Vec::new(),
        parent: None,
    };
    // Raw defaults, resolved once each choice's options are all in.
    let mut defaults: Vec<Option<String>> = Vec::new();
    let mut current_filter: Option<String> = None;

    while let Some(opt) = args.next() {
        let Some(name) = opt.to_str() else {
            return Err(ArgError::UnknownOption(opt.to_string_lossy().into_owned()));
        };
        let key: &'static str = match name {
            "--title" => "--title",
            "--accept-label" => "--accept-label",
            "--current-folder" => "--current-folder",
            "--current-name" => "--current-name",
            "--filter" => "--filter",
            "--glob" => "--glob",
            "--mime" => "--mime",
            "--current-filter" => "--current-filter",
            "--choice" => "--choice",
            "--choice-option" => "--choice-option",
            "--choice-default" => "--choice-default",
            "--file" => "--file",
            "--parent" => "--parent",
            other => return Err(ArgError::UnknownOption(other.to_owned())),
        };
        let raw = args.next().ok_or(ArgError::MissingValue(key))?;
        let string = |raw: OsString| raw.into_string().map_err(|_| ArgError::NotUtf8(key));
        match key {
            "--title" => {
                once(&req.title, key)?;
                req.title = Some(text(string(raw)?));
            }
            "--accept-label" => {
                once(&req.accept_label, key)?;
                req.accept_label = Some(text(string(raw)?));
            }
            "--current-folder" => {
                once(&req.current_folder, key)?;
                let p = raw.as_bytes().to_vec();
                if p.first() != Some(&b'/') {
                    return Err(ArgError::NotAbsolute);
                }
                req.current_folder = Some(p);
            }
            "--current-name" => {
                once(&req.current_name, key)?;
                req.current_name = Some(text(string(raw)?));
            }
            "--filter" => req.filters.push(Filter {
                name: text(string(raw)?),
                ..Filter::default()
            }),
            "--glob" => {
                let v = text(string(raw)?);
                req.filters
                    .last_mut()
                    .ok_or(ArgError::Orphan(key))?
                    .globs
                    .push(v);
            }
            "--mime" => {
                let v = text(string(raw)?);
                req.filters
                    .last_mut()
                    .ok_or(ArgError::Orphan(key))?
                    .mimes
                    .push(v);
            }
            "--current-filter" => {
                once(&current_filter, key)?;
                current_filter = Some(string(raw)?);
            }
            "--choice" => {
                req.choices.push(Choice {
                    label: text(string(raw)?),
                    options: Vec::new(),
                    default: ChoiceValue::Bool(false),
                });
                defaults.push(None);
            }
            "--choice-option" => {
                let v = text(string(raw)?);
                req.choices
                    .last_mut()
                    .ok_or(ArgError::Orphan(key))?
                    .options
                    .push(v);
            }
            "--choice-default" => {
                let slot = defaults.last_mut().ok_or(ArgError::Orphan(key))?;
                once(slot, key)?;
                *slot = Some(string(raw)?);
            }
            "--file" => req.files.push(raw.as_bytes().to_vec()),
            "--parent" => {
                once(&req.parent, key)?;
                req.parent = Some(text(string(raw)?));
            }
            _ => unreachable!("every key is matched above"),
        }
    }

    if let Some(n) = current_filter {
        let i: usize = n
            .parse()
            .map_err(|_| ArgError::BadIndex("--current-filter"))?;
        if i >= req.filters.len() {
            return Err(ArgError::BadIndex("--current-filter"));
        }
        req.current_filter = Some(i);
    }
    for (c, d) in req.choices.iter_mut().zip(defaults) {
        c.default = match (d.as_deref(), c.options.is_empty()) {
            (None, true) => ChoiceValue::Bool(false),
            (None, false) => ChoiceValue::Option(0),
            (Some("true"), true) => ChoiceValue::Bool(true),
            (Some("false"), true) => ChoiceValue::Bool(false),
            (Some(n), false) => match n.parse::<usize>() {
                Ok(i) if i < c.options.len() => ChoiceValue::Option(i),
                _ => return Err(ArgError::BadIndex("--choice-default")),
            },
            (Some(_), true) => return Err(ArgError::BadIndex("--choice-default")),
        };
    }
    if (req.mode == PickMode::SaveFiles) == req.files.is_empty() {
        return Err(ArgError::Files);
    }
    Ok(Some(req))
}

/// The bytes [`confirm`] writes for `answer`, or why it would not.
pub fn encode(req: &PickRequest, answer: &Answer) -> Result<Vec<u8>, Refused> {
    match answer.paths.len() {
        0 => return Err(Refused::NothingChosen),
        1 => {}
        _ if !req.mode.multiple() => return Err(Refused::OnlyOne),
        _ => {}
    }
    for p in &answer.paths {
        if p.first() != Some(&b'/') || p.contains(&0) {
            return Err(Refused::NotAbsolute);
        }
    }
    if req.mode == PickMode::Save {
        let p = &answer.paths[0];
        if p.len() < 2 || p.ends_with(b"/") {
            return Err(Refused::NoName);
        }
    }
    let mut out = Vec::new();
    for p in &answer.paths {
        out.push(b'P');
        out.extend_from_slice(p);
        out.push(0);
    }
    if let Some(i) = answer.filter {
        if i >= req.filters.len() {
            return Err(Refused::BadFilter);
        }
        out.extend_from_slice(format!("F{i}").as_bytes());
        out.push(0);
    }
    for &(i, v) in &answer.choices {
        let c = req.choices.get(i).ok_or(Refused::BadChoice)?;
        let v = match (v, c.options.is_empty()) {
            (ChoiceValue::Bool(b), true) => b.to_string(),
            (ChoiceValue::Option(n), false) if n < c.options.len() => n.to_string(),
            _ => return Err(Refused::BadChoice),
        };
        out.extend_from_slice(format!("C{i}={v}").as_bytes());
        out.push(0);
    }
    Ok(out)
}

/// The real stdout, once [`claim_stdout`] has moved it aside.
static OUT: OnceLock<File> = OnceLock::new();

/// Move stdout aside for the result. Call first thing in `main`, before any
/// thread starts: afterwards fd 1 is /dev/null, so stray prints and spawned
/// custom actions cannot corrupt the answer or hold the pipe open.
pub fn claim_stdout() -> std::io::Result<()> {
    // F_DUPFD_CLOEXEC: the copy is not inherited by anything we spawn.
    let private = std::io::stdout().as_fd().try_clone_to_owned()?;
    let null = File::options().write(true).open("/dev/null")?;
    // SAFETY: both descriptors are open and ours; no other thread exists yet
    // to race on fd 1.
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let _ = OUT.set(File::from(private));
    Ok(())
}

/// The user confirmed. Writes the answer and exits 0; on a refusal returns
/// it, and the picker stays open. A write that fails exits 2: the portal
/// then answers "other", never a partial list.
pub fn confirm(req: &PickRequest, answer: &Answer) -> Result<Infallible, Refused> {
    let bytes = encode(req, answer)?;
    let written = match OUT.get() {
        Some(mut f) => f.write_all(&bytes).and_then(|()| f.flush()),
        None => {
            let mut s = std::io::stdout().lock();
            s.write_all(&bytes).and_then(|()| s.flush())
        }
    };
    std::process::exit(if written.is_ok() { 0 } else { 2 })
}

/// The user cancelled, or closed the picker window: nothing written, exit 1.
pub fn cancel() -> ! {
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn req(v: &[&str]) -> PickRequest {
        parse(args(v)).unwrap().unwrap()
    }

    #[test]
    fn not_a_pick_leaves_the_normal_path_alone() {
        assert_eq!(parse(args(&[])), Ok(None));
        assert_eq!(parse(args(&["/tmp"])), Ok(None));
        assert_eq!(parse(args(&["/tmp", "--pick", "open"])), Ok(None));
    }

    #[test]
    fn modes_parse() {
        assert_eq!(req(&["--pick", "open"]).mode, PickMode::Open);
        assert!(req(&["--pick", "open-multiple"]).mode.multiple());
        assert!(req(&["--pick", "directory"]).mode.folders());
        assert!(req(&["--pick", "save"]).mode.named());
        assert!(matches!(
            parse(args(&["--pick", "explode"])),
            Err(ArgError::UnknownMode(_))
        ));
        assert_eq!(
            parse(args(&["--pick"])),
            Err(ArgError::MissingValue("--pick"))
        );
    }

    #[test]
    fn values_are_taken_verbatim_even_when_they_look_like_options() {
        let r = req(&[
            "--pick",
            "open",
            "--title",
            "--accept-label",
            "--accept-label",
            "-x",
        ]);
        assert_eq!(r.title.as_deref(), Some("--accept-label"));
        assert_eq!(r.accept_label.as_deref(), Some("-x"));
        assert_eq!(
            parse(args(&["--pick", "open", "--title"])),
            Err(ArgError::MissingValue("--title"))
        );
        assert!(matches!(
            parse(args(&["--pick", "open", "--bogus", "x"])),
            Err(ArgError::UnknownOption(_))
        ));
        assert_eq!(
            parse(args(&["--pick", "open", "--title", "a", "--title", "b"])),
            Err(ArgError::Repeated("--title"))
        );
    }

    #[test]
    fn control_characters_are_stripped() {
        let r = req(&["--pick", "open", "--title", "a\u{1b}[2Jb\nc"]);
        assert_eq!(r.title.as_deref(), Some("a[2Jbc"));
    }

    #[test]
    fn filters_and_choices_attach_to_the_last_one() {
        let r = req(&[
            "--pick",
            "open",
            "--filter",
            "Images",
            "--glob",
            "*.png",
            "--mime",
            "image/jpeg",
            "--filter",
            "All",
            "--glob",
            "*",
            "--current-filter",
            "1",
            "--choice",
            "Enc",
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
            "--choice",
            "Plain",
        ]);
        assert_eq!(r.filters.len(), 2);
        assert_eq!(r.filters[0].globs, ["*.png"]);
        assert_eq!(r.filters[0].mimes, ["image/jpeg"]);
        assert_eq!(r.current_filter, Some(1));
        assert_eq!(r.choices[0].default, ChoiceValue::Option(1));
        assert_eq!(r.choices[1].default, ChoiceValue::Bool(true));
        assert_eq!(r.choices[2].default, ChoiceValue::Bool(false));
        assert_eq!(
            parse(args(&["--pick", "open", "--glob", "*"])),
            Err(ArgError::Orphan("--glob"))
        );
        assert_eq!(
            parse(args(&["--pick", "open", "--current-filter", "0"])),
            Err(ArgError::BadIndex("--current-filter"))
        );
        assert_eq!(
            parse(args(&[
                "--pick",
                "open",
                "--choice",
                "x",
                "--choice-option",
                "a",
                "--choice-default",
                "3"
            ])),
            Err(ArgError::BadIndex("--choice-default"))
        );
        assert_eq!(
            parse(args(&[
                "--pick",
                "open",
                "--choice",
                "x",
                "--choice-default",
                "maybe"
            ])),
            Err(ArgError::BadIndex("--choice-default"))
        );
    }

    #[test]
    fn folders_must_be_absolute_and_files_belong_to_save_files() {
        let r = req(&[
            "--pick",
            "save",
            "--current-folder",
            "/tmp",
            "--current-name",
            "a.txt",
        ]);
        assert_eq!(r.current_folder.as_deref(), Some(&b"/tmp"[..]));
        assert_eq!(
            parse(args(&["--pick", "open", "--current-folder", "tmp"])),
            Err(ArgError::NotAbsolute)
        );
        assert_eq!(parse(args(&["--pick", "save-files"])), Err(ArgError::Files));
        assert_eq!(
            parse(args(&["--pick", "open", "--file", "a"])),
            Err(ArgError::Files)
        );
        assert_eq!(
            req(&["--pick", "save-files", "--file", "a", "--file", "b"])
                .files
                .len(),
            2
        );
    }

    fn answer(paths: &[&[u8]]) -> Answer {
        Answer {
            paths: paths.iter().map(|p| p.to_vec()).collect(),
            ..Answer::default()
        }
    }

    #[test]
    fn encode_writes_nul_terminated_records() {
        let r = req(&[
            "--pick",
            "open-multiple",
            "--filter",
            "A",
            "--glob",
            "*",
            "--choice",
            "c",
            "--choice-option",
            "x",
            "--choice",
            "ro",
        ]);
        let mut a = answer(&[b"/a b", b"/c\nd"]);
        a.filter = Some(0);
        a.choices = vec![(0, ChoiceValue::Option(0)), (1, ChoiceValue::Bool(true))];
        assert_eq!(
            encode(&r, &a).unwrap(),
            b"P/a b\0P/c\nd\0F0\0C0=0\0C1=true\0"
        );
    }

    #[test]
    fn encode_refuses_what_the_mode_does_not_allow() {
        let open = req(&["--pick", "open"]);
        assert_eq!(encode(&open, &answer(&[])), Err(Refused::NothingChosen));
        assert_eq!(
            encode(&open, &answer(&[b"/a", b"/b"])),
            Err(Refused::OnlyOne)
        );
        assert_eq!(encode(&open, &answer(&[b"a"])), Err(Refused::NotAbsolute));
        assert_eq!(
            encode(&open, &answer(&[b"/a\0b"])),
            Err(Refused::NotAbsolute)
        );
        let mut a = answer(&[b"/a"]);
        a.filter = Some(0);
        assert_eq!(encode(&open, &a), Err(Refused::BadFilter));
        let mut a = answer(&[b"/a"]);
        a.choices = vec![(0, ChoiceValue::Bool(true))];
        assert_eq!(encode(&open, &a), Err(Refused::BadChoice));
        let save = req(&["--pick", "save"]);
        assert_eq!(encode(&save, &answer(&[b"/"])), Err(Refused::NoName));
        assert_eq!(encode(&save, &answer(&[b"/tmp/"])), Err(Refused::NoName));
        assert!(encode(&save, &answer(&[b"/tmp/a.txt"])).is_ok());
        let checkbox = req(&["--pick", "open", "--choice", "ro"]);
        let mut a = answer(&[b"/a"]);
        a.choices = vec![(0, ChoiceValue::Option(0))];
        assert_eq!(encode(&checkbox, &a), Err(Refused::BadChoice));
    }
}
