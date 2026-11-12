// SPDX-License-Identifier: AGPL-3.0-only

//! FileChooser mapping, ported from `fog-portal`: portal options to picker
//! command line and picker output to portal results. The portal never touches
//! files; it only returns the paths the user picked in Fog's picker.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use crate::options::{strip_nul, Options};
use crate::request::{Reply, Response, Results};
use crate::val::Val;

/// The chooser UI contract. The portal spawns
///
/// ```text
/// ec-fog-ui --picker <open|save|dir> [--multiple] [--filter GLOB]...
///           [--name N] [--folder DIR] [--title T] --result-fd N
/// ```
///
/// `open` picks files, `dir` picks a folder, `save` picks a folder plus a file
/// name (`--name` is the suggested one). `--multiple` allows several results.
/// Each `--filter` is a glob (`*.png`); MIME rules are not passed. `--folder`
/// is the initial directory.
///
/// On accept the picker writes the chosen absolute paths to `--result-fd`,
/// newline-delimited (a save writes the full `folder/name`), and exits 0. On
/// cancel it writes nothing and exits 1. Any other exit is a failure.
pub const PICKER_BIN: &str = "ec-fog-ui";

/// Exit code a picker uses to say the user cancelled.
pub const PICKER_CANCEL_EXIT: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterRule {
    /// Type 0: a glob such as `*.png`.
    Glob(String),
    /// Type 1: a MIME type such as `image/png`.
    Mime(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub label: String,
    pub rules: Vec<FilterRule>,
}

impl Filter {
    /// From the wire form `(label, [(type, pattern)])`; unknown rule types are
    /// dropped.
    pub fn from_wire(label: String, rules: Vec<(u32, String)>) -> Self {
        Filter {
            label,
            rules: rules
                .into_iter()
                .filter_map(|(t, p)| match t {
                    0 => Some(FilterRule::Glob(p)),
                    1 => Some(FilterRule::Mime(p)),
                    _ => None,
                })
                .collect(),
        }
    }

    /// From a decoded `(sa(us))` struct; `None` if the shape is wrong.
    pub fn from_val(v: &Val) -> Option<Self> {
        let [label, rules] = v.as_struct()? else {
            return None;
        };
        let rules = rules
            .as_list()?
            .iter()
            .filter_map(|r| {
                let [t, p] = r.as_struct()? else { return None };
                Some((t.as_u32()?, p.as_str()?.to_owned()))
            })
            .collect();
        Some(Filter::from_wire(label.as_str()?.to_owned(), rules))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    OpenFile { multiple: bool, directory: bool },
    SaveFile { current_name: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChooserRequest {
    pub title: String,
    pub accept_label: Option<String>,
    pub mode: Mode,
    pub current_folder: Option<Vec<u8>>,
    pub filters: Vec<Filter>,
}

impl ChooserRequest {
    /// Request for OpenFile (`multiple`, `directory`) or SaveFile
    /// (`current_name`).
    pub fn from_options(title: &str, save: bool, o: &Options) -> Self {
        let mode = if save {
            Mode::SaveFile {
                current_name: o.str("current_name").map(str::to_owned),
            }
        } else {
            Mode::OpenFile {
                multiple: o.bool("multiple").unwrap_or(false),
                directory: o.bool("directory").unwrap_or(false),
            }
        };
        ChooserRequest {
            title: title.to_owned(),
            accept_label: o.str("accept_label").map(str::to_owned),
            mode,
            current_folder: o.bytestring("current_folder"),
            filters: o
                .get("filters")
                .and_then(Val::as_list)
                .map(|l| l.iter().filter_map(Filter::from_val).collect())
                .unwrap_or_default(),
        }
    }

    /// `SaveFiles`: pick one folder; names go under it afterwards.
    pub fn folder_for_save_files(title: &str, o: &Options) -> Self {
        ChooserRequest {
            title: title.to_owned(),
            accept_label: o.str("accept_label").map(str::to_owned),
            mode: Mode::OpenFile {
                multiple: false,
                directory: true,
            },
            current_folder: o.bytestring("current_folder"),
            filters: Vec::new(),
        }
    }

    /// Argument vector for `ec-fog-ui` (see [`PICKER_BIN`]).
    pub fn picker_args(&self, result_fd: i32) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--picker".into()];
        match &self.mode {
            Mode::OpenFile {
                directory: true, ..
            } => a.push("dir".into()),
            Mode::OpenFile { multiple, .. } => {
                a.push("open".into());
                if *multiple {
                    a.push("--multiple".into());
                }
            }
            Mode::SaveFile { current_name } => {
                a.push("save".into());
                if let Some(n) = current_name {
                    a.push("--name".into());
                    a.push(n.into());
                }
            }
        }
        if let Mode::OpenFile {
            multiple: true,
            directory: true,
        } = &self.mode
        {
            a.push("--multiple".into());
        }
        for f in &self.filters {
            for r in &f.rules {
                if let FilterRule::Glob(g) = r {
                    a.push("--filter".into());
                    a.push(g.into());
                }
            }
        }
        if let Some(d) = &self.current_folder {
            a.push("--folder".into());
            a.push(OsString::from_vec(d.clone()));
        }
        if !self.title.is_empty() {
            a.push("--title".into());
            a.push((&self.title).into());
        }
        a.push("--result-fd".into());
        a.push(result_fd.to_string().into());
        a
    }
}

/// Paths from the picker's output: newline-delimited, absolute only.
pub fn parse_picked(out: &[u8]) -> Vec<Vec<u8>> {
    out.split(|&b| b == b'\n')
        .filter(|l| l.first() == Some(&b'/'))
        .map(<[u8]>::to_vec)
        .collect()
}

/// Portal result for a finished picker: exit 0 maps its output, exit 1 is a
/// cancel, anything else (crash, signal, spawn failure = `None`) is an error.
pub fn picker_outcome(exit_code: Option<i32>, out: &[u8]) -> (Response, Vec<String>) {
    match exit_code {
        Some(0) => response(&parse_picked(out)),
        Some(PICKER_CANCEL_EXIT) => (Response::Cancelled, Vec::new()),
        _ => (Response::Other, Vec::new()),
    }
}

/// `(response, uris)`. Nothing picked is a cancel.
pub fn response(picked: &[Vec<u8>]) -> (Response, Vec<String>) {
    if picked.is_empty() {
        (Response::Cancelled, Vec::new())
    } else {
        (
            Response::Success,
            picked.iter().map(|p| path_to_uri(p)).collect(),
        )
    }
}

/// The portal reply for `(response, uris)`.
pub fn reply((response, uris): (Response, Vec<String>)) -> Reply {
    if response == Response::Success {
        Reply::ok(Results::new().with_uris("uris", uris))
    } else {
        Reply {
            response,
            results: Results::new(),
        }
    }
}

/// `SaveFiles`: each name under the chosen folder; `None` if any name is not a
/// single path component.
pub fn save_files_targets(folder: &[u8], names: &[String]) -> Option<Vec<Vec<u8>>> {
    names.iter().map(|n| save_target(folder, n)).collect()
}

/// The `files` option of SaveFiles (`aay`) as names.
pub fn save_files_names(o: &Options) -> Option<Vec<String>> {
    o.get("files")?
        .as_list()?
        .iter()
        .map(|v| String::from_utf8(strip_nul(v.as_bytes()?)).ok())
        .collect()
}

/// Save target: `folder/name`, refusing names that are not one component.
pub fn save_target(folder: &[u8], name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return None;
    }
    let mut p = folder.to_vec();
    if p.last() != Some(&b'/') {
        p.push(b'/');
    }
    p.extend_from_slice(name.as_bytes());
    Some(p)
}

/// `file://` URI for an absolute path (RFC 3986 percent-encoding).
pub fn path_to_uri(path: &[u8]) -> String {
    let mut out = String::from("file://");
    for &b in path {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Inverse of [`path_to_uri`] for `file://` URIs (also accepts `file://host/`
/// only when the host is empty or `localhost`).
pub fn uri_to_path(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Glob match with `*` and `?`, case-sensitive as the portal specifies.
pub fn glob_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while i < s.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == s[i]) && pat[p] != b'*' {
            p += 1;
            i += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some((p, i));
            p += 1;
        } else if let Some((sp, si)) = star {
            p = sp + 1;
            i = si + 1;
            star = Some((sp, si + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(b"*.png", b"a.png"));
        assert!(!glob_match(b"*.png", b"a.PNG"));
        assert!(glob_match(b"a?c*", b"abcdef"));
        assert!(!glob_match(b"a?c", b"abcd"));
        assert!(glob_match(b"*", b""));
    }

    #[test]
    fn uri_encoding_round_trip() {
        let p = "/a b/é.txt".as_bytes();
        assert_eq!(path_to_uri(p), "file:///a%20b/%C3%A9.txt");
        assert_eq!(uri_to_path(&path_to_uri(p)), Some(p.to_vec()));
        assert_eq!(uri_to_path("http://x"), None);
        assert_eq!(uri_to_path("file:///a%2"), None);
        assert_eq!(uri_to_path("file://evil/a"), None);
        assert_eq!(uri_to_path("file://localhost/a"), Some(b"/a".to_vec()));
    }

    #[test]
    fn empty_pick_is_cancel() {
        assert_eq!(response(&[]).0, Response::Cancelled);
        assert_eq!(
            response(&[b"/x".to_vec()]),
            (Response::Success, vec!["file:///x".to_string()])
        );
    }

    #[test]
    fn save_names() {
        assert_eq!(save_target(b"/d", "f"), Some(b"/d/f".to_vec()));
        assert_eq!(save_target(b"/d", "../f"), None);
        assert_eq!(save_target(b"/d", ""), None);
        assert_eq!(
            save_files_targets(b"/d", &["a".into(), "b".into()]),
            Some(vec![b"/d/a".to_vec(), b"/d/b".to_vec()])
        );
        assert_eq!(
            save_files_targets(b"/d", &["a".into(), "../b".into()]),
            None
        );
    }

    fn filter_val() -> Val {
        Val::Struct(vec![
            Val::Str("img".into()),
            Val::List(vec![
                Val::Struct(vec![Val::U32(0), Val::Str("*.png".into())]),
                Val::Struct(vec![Val::U32(1), Val::Str("image/gif".into())]),
                Val::Struct(vec![Val::U32(9), Val::Str("x".into())]),
            ]),
        ])
    }

    fn req(mode: Mode) -> ChooserRequest {
        ChooserRequest {
            title: "T".into(),
            accept_label: None,
            mode,
            current_folder: Some(b"/h".to_vec()),
            filters: vec![Filter::from_val(&filter_val()).unwrap()],
        }
    }

    #[test]
    fn options_map_to_request() {
        let o = Options::from_pairs([
            ("multiple".into(), Val::Bool(true)),
            ("current_folder".into(), Val::Bytes(b"/h\0".to_vec())),
            (
                "filters".into(),
                Val::List(vec![filter_val(), Val::Bool(true)]),
            ),
        ]);
        let r = ChooserRequest::from_options("T", false, &o);
        assert_eq!(
            r.mode,
            Mode::OpenFile {
                multiple: true,
                directory: false
            }
        );
        assert_eq!(r.current_folder, Some(b"/h".to_vec()));
        assert_eq!(r.filters.len(), 1);
        assert_eq!(r.filters[0].rules.len(), 2);
    }

    #[test]
    fn picker_arg_vectors() {
        let s = |v: Vec<OsString>| -> Vec<String> {
            v.into_iter().map(|o| o.into_string().unwrap()).collect()
        };
        assert_eq!(
            s(req(Mode::OpenFile {
                multiple: true,
                directory: false
            })
            .picker_args(7)),
            [
                "--picker",
                "open",
                "--multiple",
                "--filter",
                "*.png",
                "--folder",
                "/h",
                "--title",
                "T",
                "--result-fd",
                "7"
            ]
        );
        assert_eq!(
            s(req(Mode::SaveFile {
                current_name: Some("a.txt".into())
            })
            .picker_args(3))[..4],
            ["--picker", "save", "--name", "a.txt"]
        );
        assert_eq!(
            s(req(Mode::OpenFile {
                multiple: false,
                directory: true
            })
            .picker_args(3))[..2],
            ["--picker", "dir"]
        );
    }

    #[test]
    fn outcomes() {
        assert_eq!(
            picker_outcome(Some(0), b"/a b\n/c\n\njunk\n"),
            (
                Response::Success,
                vec!["file:///a%20b".into(), "file:///c".into()]
            )
        );
        assert_eq!(
            picker_outcome(Some(1), b"/a\n"),
            (Response::Cancelled, vec![])
        );
        assert_eq!(picker_outcome(Some(0), b""), (Response::Cancelled, vec![]));
        assert_eq!(picker_outcome(Some(101), b""), (Response::Other, vec![]));
        assert_eq!(picker_outcome(None, b"/a"), (Response::Other, vec![]));
    }

    #[test]
    fn reply_shapes() {
        let r = reply((Response::Success, vec!["file:///x".into()]));
        assert_eq!(
            r.results.get("uris"),
            Some(&Val::Strs(vec!["file:///x".into()]))
        );
        assert!(reply((Response::Cancelled, vec![])).results.is_empty());
    }

    #[test]
    fn save_files_option() {
        let o = Options::from_pairs([(
            "files".into(),
            Val::List(vec![
                Val::Bytes(b"a.txt\0".to_vec()),
                Val::Bytes(b"b".to_vec()),
            ]),
        )]);
        assert_eq!(
            save_files_names(&o),
            Some(vec!["a.txt".to_string(), "b".to_string()])
        );
        assert_eq!(save_files_names(&Options::new()), None);
    }
}
