// SPDX-License-Identifier: AGPL-3.0-only

//! Copy, cut and paste of files (FOG §Desktop interop: clipboard).
//!
//! Fog keeps its own clipboard of absolute paths and also publishes them to
//! the Wayland clipboard as a `text/uri-list` body. iced 0.14's clipboard
//! offers only text (`text/plain;charset=utf-8` and friends), so the body is
//! offered under the text types: GTK and Qt file managers, which look for
//! `x-special/gnome-copied-files` or `text/uri-list` by mime type, do not
//! see it as files. Reading is lenient for the same reason: a paste with
//! Fog's own clipboard empty accepts the text of a uri-list, of a
//! gnome-copied-files body (first line `copy` or `cut`), or plain absolute
//! paths, one per line.

use ec_fog_proto::{ConflictPolicy, JobSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOp {
    Copy,
    Cut,
}

/// What Copy or Cut took: absolute paths, raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    pub op: ClipOp,
    pub paths: Vec<Vec<u8>>,
}

impl Clip {
    /// The `text/uri-list` body: `file://` URIs, CRLF-terminated (RFC 2483).
    pub fn uri_list(&self) -> String {
        self.paths
            .iter()
            .map(|p| format!("{}\r\n", file_uri(p)))
            .collect()
    }

    /// Clipboard text from another app, as a clip. `None` if no line is a
    /// local file: text that is not a file list is never pasted as files.
    pub fn parse(text: &str) -> Option<Clip> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let mut op = ClipOp::Copy;
        let mut first = lines.next()?;
        match first {
            "copy" | "cut" => {
                if first == "cut" {
                    op = ClipOp::Cut;
                }
                first = lines.next()?;
            }
            _ => {}
        }
        let mut paths = Vec::new();
        for l in std::iter::once(first).chain(lines) {
            if l.starts_with('#') {
                // uri-list comment
                continue;
            }
            paths.push(to_path(l)?);
        }
        (!paths.is_empty()).then_some(Clip { op, paths })
    }

    /// The job pasting this into `dest`: collisions ask the user, except a
    /// copy back into the folder it came from, which can only mean a
    /// duplicate — keep both, as `name (2).ext`. A cut of items already in
    /// `dest` leaves them be; `None` if that is all of it.
    pub fn job(&self, dest: Vec<u8>) -> Option<JobSpec> {
        let parent = |p: &[u8]| match p.iter().rposition(|&b| b == b'/') {
            Some(0) => b"/".to_vec(),
            Some(i) => p[..i].to_vec(),
            None => Vec::new(),
        };
        let mut srcs = self.paths.clone();
        if self.op == ClipOp::Cut {
            srcs.retain(|s| parent(s) != dest);
            if srcs.is_empty() {
                return None;
            }
        }
        let duplicate = self.op == ClipOp::Copy && srcs.iter().all(|s| parent(s) == dest);
        let on_conflict = if duplicate {
            ConflictPolicy::Rename
        } else {
            ConflictPolicy::Ask
        };
        Some(match self.op {
            ClipOp::Copy => JobSpec::Copy {
                srcs,
                dest,
                on_conflict,
            },
            ClipOp::Cut => JobSpec::Move {
                srcs,
                dest,
                on_conflict,
            },
        })
    }
}

/// `file://` + the path, percent-encoding every byte outside RFC 3986's
/// unreserved set and `/`.
pub fn file_uri(path: &[u8]) -> String {
    let mut s = String::from("file://");
    for &b in path {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// A line as an absolute path: a local `file://` URI, percent-decoded, or
/// an absolute path as-is. Anything else is `None`.
fn to_path(line: &str) -> Option<Vec<u8>> {
    let rest = match line.strip_prefix("file://") {
        Some(r) => r.strip_prefix("localhost").unwrap_or(r),
        None if line.starts_with('/') => return Some(line.as_bytes().to_vec()),
        None => return None,
    };
    if !rest.starts_with('/') {
        return None;
    }
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(op: ClipOp, paths: &[&[u8]]) -> Clip {
        Clip {
            op,
            paths: paths.iter().map(|p| p.to_vec()).collect(),
        }
    }

    #[test]
    fn encodes_a_uri_list() {
        let c = clip(ClipOp::Cut, &[b"/home/u/a b.txt", b"/x/\xff%"]);
        assert_eq!(
            c.uri_list(),
            "file:///home/u/a%20b.txt\r\nfile:///x/%FF%25\r\n"
        );
    }

    #[test]
    fn parses_what_it_writes_and_what_others_write() {
        let c = clip(ClipOp::Cut, &[b"/a b", b"/\xfe"]);
        // gnome-copied-files, as GTK file managers write it.
        assert_eq!(
            Clip::parse("cut\nfile:///a%20b\nfile:///%FE"),
            Some(c.clone())
        );
        // A uri-list carries no cut flag: it pastes as a copy.
        assert_eq!(
            Clip::parse(&c.uri_list()),
            Some(Clip {
                op: ClipOp::Copy,
                ..c
            })
        );
        assert_eq!(
            Clip::parse("# comment\nfile://localhost/etc/hosts\n/tmp/x\n"),
            Some(clip(ClipOp::Copy, &[b"/etc/hosts", b"/tmp/x"]))
        );
        // Prose, remote URIs and relative paths are not files.
        assert_eq!(Clip::parse("hello world"), None);
        assert_eq!(Clip::parse("file:///a\nhttps://x.org/b"), None);
        assert_eq!(Clip::parse("copy\n"), None);
        assert_eq!(Clip::parse(""), None);
    }

    #[test]
    fn paste_asks_on_conflict() {
        let d = b"/dest".to_vec();
        assert_eq!(
            clip(ClipOp::Copy, &[b"/a"]).job(d.clone()),
            Some(JobSpec::Copy {
                srcs: vec![b"/a".to_vec()],
                dest: d.clone(),
                on_conflict: ConflictPolicy::Ask
            })
        );
        assert!(matches!(
            clip(ClipOp::Cut, &[b"/a"]).job(d),
            Some(JobSpec::Move {
                on_conflict: ConflictPolicy::Ask,
                ..
            })
        ));
    }

    #[test]
    fn copying_into_its_own_folder_keeps_both() {
        let own = |c: &Clip, d: &[u8]| match c.job(d.to_vec()).unwrap() {
            JobSpec::Copy { on_conflict, .. } | JobSpec::Move { on_conflict, .. } => on_conflict,
            _ => unreachable!(),
        };
        let c = clip(ClipOp::Copy, &[b"/w/a", b"/w/b"]);
        assert_eq!(own(&c, b"/w"), ConflictPolicy::Rename);
        assert_eq!(
            own(&clip(ClipOp::Copy, &[b"/a"]), b"/"),
            ConflictPolicy::Rename
        );
        // One from elsewhere, and a clash is a real question.
        let mixed = clip(ClipOp::Copy, &[b"/w/a", b"/x/b"]);
        assert_eq!(own(&mixed, b"/w"), ConflictPolicy::Ask);
    }

    #[test]
    fn cutting_into_its_own_folder_moves_nothing_there() {
        assert_eq!(
            clip(ClipOp::Cut, &[b"/w/a", b"/w/b"]).job(b"/w".to_vec()),
            None
        );
        assert_eq!(clip(ClipOp::Cut, &[b"/a"]).job(b"/".to_vec()), None);
        // Only the ones from elsewhere move; a clash there still asks.
        assert_eq!(
            clip(ClipOp::Cut, &[b"/w/a", b"/x/b"]).job(b"/w".to_vec()),
            Some(JobSpec::Move {
                srcs: vec![b"/x/b".to_vec()],
                dest: b"/w".to_vec(),
                on_conflict: ConflictPolicy::Ask
            })
        );
    }
}
