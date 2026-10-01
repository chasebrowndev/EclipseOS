// SPDX-License-Identifier: AGPL-3.0-only

//! One-line text editing for the path bar and the palette, and the path
//! editor's completion (FOG §UI and navigation).
//!
//! Plain state, not an iced `text_input`: fog-ui reads every key through
//! `keyboard::listen`, and a focused `text_input` would swallow Esc and the
//! bindings. Completion reads a `fogd` listing of the folder being typed in
//! (`ListDir`), never the filesystem: the UI thread does no I/O.

use fog_proto::{apply_diff, Entry, Kind, Reply};

/// An edit to a [`LineEdit`], as the app maps keys to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Back to the previous `/` or space.
    DeleteWord,
}

/// A line of text and a cursor (a byte offset on a char boundary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    cursor: usize,
}

impl LineEdit {
    /// `text` with the cursor at its end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text before and after the cursor.
    pub fn halves(&self) -> (&str, &str) {
        self.text.split_at(self.cursor)
    }

    pub fn apply(&mut self, e: Edit) {
        match e {
            Edit::Insert(s) => {
                let clean: String = s.chars().filter(|c| !c.is_control()).collect();
                self.text.insert_str(self.cursor, &clean);
                self.cursor += clean.len();
            }
            Edit::Backspace => {
                if let Some(c) = self.text[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                    self.text.remove(self.cursor);
                }
            }
            Edit::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            Edit::Left => {
                if let Some(c) = self.text[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                }
            }
            Edit::Right => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            Edit::Home => self.cursor = 0,
            Edit::End => self.cursor = self.text.len(),
            Edit::DeleteWord => {
                let stop = |c: char| c == '/' || c == ' ';
                let head = self.text[..self.cursor].trim_end_matches(stop);
                let start = head.rfind(stop).map_or(0, |i| i + 1);
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
        }
    }
}

/// The folder listing completion reads, as `fogd` sent it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Held {
    path: Vec<u8>,
    dir: Option<u64>,
    generation: u64,
    entries: Vec<Entry>,
    order: Vec<u32>,
}

/// The path bar in edit mode (Ctrl+L, or typing `/` or `~`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEdit {
    pub line: LineEdit,
    /// `$HOME`, for `~`.
    home: Vec<u8>,
    /// The shown folder, for relative input.
    cwd: Vec<u8>,
    held: Option<Held>,
    /// Index into [`Self::candidates`].
    pub pick: usize,
}

impl PathEdit {
    pub fn new(text: impl Into<String>, home: Vec<u8>, cwd: Vec<u8>) -> Self {
        Self {
            line: LineEdit::new(text),
            home,
            cwd,
            held: None,
            pick: 0,
        }
    }

    pub fn edit(&mut self, e: Edit) {
        self.line.apply(e);
        self.pick = 0;
    }

    /// The folder the typed text is in, when completion does not hold its
    /// listing yet: the app sends `ListDir` for it.
    pub fn want(&mut self) -> Option<Vec<u8>> {
        let (dir, _) = self.split()?;
        if self.held.as_ref().is_some_and(|h| h.path == dir) {
            return None;
        }
        self.held = Some(Held {
            path: dir.clone(),
            ..Held::default()
        });
        Some(dir)
    }

    /// A new `fogd` connection: ask for the listing again on the next
    /// [`Self::want`].
    pub fn forget(&mut self) {
        self.held = None;
    }

    /// Take a reply meant for the completion listing; others are ignored.
    pub fn on_reply(&mut self, r: &Reply) {
        let Some(h) = &mut self.held else {
            return;
        };
        match r {
            Reply::DirSnapshot {
                path,
                dir,
                generation,
                entries,
                order,
                ..
            } if *path == h.path => {
                h.dir = Some(*dir);
                h.generation = *generation;
                h.entries.clone_from(entries);
                h.order.clone_from(order);
            }
            Reply::DirDiff {
                dir,
                generation,
                removed,
                added,
                changed,
                order,
                ..
            } if h.dir == Some(*dir) && *generation == h.generation + 1 => {
                apply_diff(&mut h.entries, removed, added, changed);
                h.order.clone_from(order);
                h.generation = *generation;
            }
            Reply::Error { path, .. } if *path == h.path => {
                h.entries.clear();
                h.order.clear();
            }
            _ => {}
        }
    }

    /// The typed text with `~` expanded and made absolute.
    fn expanded(&self) -> Vec<u8> {
        let t = self.line.text().as_bytes();
        match t {
            [b'~'] => self.home.clone(),
            [b'~', b'/', rest @ ..] => [&self.home[..], b"/", rest].concat(),
            [b'/', ..] => t.to_vec(),
            _ => [&self.cwd[..], b"/", t].concat(),
        }
    }

    /// The folder being typed in, and the partial name after it.
    fn split(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let full = self.expanded();
        let cut = full.iter().rposition(|&b| b == b'/')?;
        Some((normalize(&full[..=cut]), full[cut + 1..].to_vec()))
    }

    /// Folders in the typed folder whose name starts with the partial name
    /// (ASCII case folded), in `fogd`'s order. Dot folders only when asked
    /// for, by `show_hidden` or a typed leading `.`.
    pub fn candidates(&self, show_hidden: bool) -> Vec<&Entry> {
        let (Some(h), Some((dir, part))) = (&self.held, self.split()) else {
            return Vec::new();
        };
        if h.path != dir || h.dir.is_none() {
            return Vec::new();
        }
        let dots = show_hidden || part.starts_with(b".");
        h.order
            .iter()
            .filter_map(|&i| h.entries.get(i as usize))
            .filter(|e| matches!(e.kind, Kind::Dir | Kind::Symlink))
            .filter(|e| dots || !e.name.starts_with(b"."))
            .filter(|e| {
                e.name.len() >= part.len() && e.name[..part.len()].eq_ignore_ascii_case(&part)
            })
            .collect()
    }

    /// Move the pick through `n` candidates, wrapping.
    pub fn step(&mut self, delta: isize, n: usize) {
        if n > 0 {
            self.pick = (self.pick as isize + delta).rem_euclid(n as isize) as usize;
        }
    }

    /// The rest of the picked candidate after what is typed, shown as ghost
    /// text; only when the typed part matches its case exactly.
    pub fn ghost(&self, show_hidden: bool) -> Option<String> {
        let (_, part) = self.split()?;
        let c = self.candidates(show_hidden);
        let e = c.get(self.pick.min(c.len().checked_sub(1)?))?;
        let rest = e.name.strip_prefix(part.as_slice())?;
        Some(format!("{}/", String::from_utf8_lossy(rest)))
    }

    /// Tab: complete to the picked candidate plus `/`. Returns whether
    /// anything was completed.
    pub fn accept(&mut self, show_hidden: bool) -> bool {
        let c = self.candidates(show_hidden);
        let Some(e) = c.get(self.pick.min(c.len().saturating_sub(1))) else {
            return false;
        };
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let t = self.line.text();
        let head = t.rfind('/').map_or("", |i| &t[..=i]);
        self.line = LineEdit::new(format!("{head}{name}/"));
        self.pick = 0;
        true
    }

    /// Enter: the absolute, lexically normalized path typed.
    pub fn target(&self) -> Vec<u8> {
        normalize(&self.expanded())
    }
}

/// `.` dropped, `..` popped, repeated and trailing `/` removed, like
/// `fog-ui`'s start-path resolution. `path` is absolute.
pub fn normalize(path: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in path.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        return b"/".to_vec();
    }
    let mut out = Vec::with_capacity(path.len());
    for p in parts {
        out.push(b'/');
        out.extend_from_slice(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(path: &str, names: &[(&str, Kind)]) -> Reply {
        Reply::DirSnapshot {
            path: path.as_bytes().to_vec(),
            dir: 7,
            generation: 0,
            entries: names
                .iter()
                .map(|&(n, k)| Entry::new(n.as_bytes().to_vec(), k))
                .collect(),
            order: (0..names.len() as u32).rev().collect(),
            complete: true,
        }
    }

    fn names(c: Vec<&Entry>) -> Vec<String> {
        c.into_iter().map(|e| e.display().into_owned()).collect()
    }

    #[test]
    fn line_edit_moves_and_deletes_on_char_boundaries() {
        let mut l = LineEdit::new("/hé");
        l.apply(Edit::Left);
        assert_eq!(l.halves(), ("/h", "é"));
        l.apply(Edit::Insert("x\u{7}".into()));
        assert_eq!(l.text(), "/hxé");
        l.apply(Edit::Delete);
        assert_eq!(l.text(), "/hx");
        l.apply(Edit::Right);
        l.apply(Edit::Home);
        l.apply(Edit::Backspace);
        assert_eq!(l.halves(), ("", "/hx"));
        l.apply(Edit::End);
        l.apply(Edit::Backspace);
        assert_eq!(l.text(), "/h");

        let mut w = LineEdit::new("/home/u/src");
        w.apply(Edit::DeleteWord);
        assert_eq!(w.text(), "/home/u/");
        w.apply(Edit::DeleteWord);
        assert_eq!(w.text(), "/home/");
        w.apply(Edit::DeleteWord);
        w.apply(Edit::DeleteWord);
        assert_eq!(w.text(), "");
    }

    #[test]
    fn path_edit_lists_the_typed_folder_once_and_completes_from_it() {
        let mut p = PathEdit::new("~/sr", b"/home/u".to_vec(), b"/x".to_vec());
        assert_eq!(p.want(), Some(b"/home/u".to_vec()));
        assert_eq!(p.want(), None, "already asked");
        assert!(p.candidates(false).is_empty(), "nothing held yet");
        // A snapshot for some other folder is not completion's.
        p.on_reply(&snap("/elsewhere", &[("src", Kind::Dir)]));
        assert!(p.candidates(false).is_empty());
        // fogd's order is reversed insertion here; completion keeps it.
        p.on_reply(&snap(
            "/home/u",
            &[
                ("src", Kind::Dir),
                ("Srv", Kind::Symlink),
                ("sr.txt", Kind::File),
                (".srx", Kind::Dir),
                ("spr", Kind::Dir),
            ],
        ));
        assert_eq!(names(p.candidates(false)), ["Srv", "src"]);
        // ".srx" does not start with "sr", hidden or not.
        assert_eq!(names(p.candidates(true)), ["Srv", "src"]);
        // "Srv" differs in case: no ghost for it, but Tab still takes it.
        assert_eq!(p.ghost(false), None);
        p.step(1, 2);
        assert_eq!(p.ghost(false).as_deref(), Some("c/"));
        p.step(1, 2);
        assert_eq!(p.pick, 0);
        p.step(-1, 2);
        assert!(p.accept(false));
        assert_eq!(p.line.text(), "~/src/");
        assert_eq!(p.want(), Some(b"/home/u/src".to_vec()));
        assert_eq!(p.target(), b"/home/u/src");
    }

    #[test]
    fn path_edit_follows_diffs_and_shows_dot_folders_on_a_dot() {
        let mut p = PathEdit::new("/d/.c", Vec::new(), Vec::new());
        p.want();
        p.on_reply(&snap("/d", &[("a", Kind::Dir)]));
        p.on_reply(&Reply::DirDiff {
            dir: 7,
            generation: 1,
            removed: vec![b"a".to_vec()],
            added: vec![Entry::new(b".config".to_vec(), Kind::Dir)],
            changed: vec![],
            order: vec![0],
            complete: true,
        });
        assert_eq!(names(p.candidates(false)), [".config"]);
        assert_eq!(p.ghost(false).as_deref(), Some("onfig/"));
        // A diff out of sequence is dropped rather than misapplied.
        p.on_reply(&Reply::DirDiff {
            dir: 7,
            generation: 5,
            removed: vec![b".config".to_vec()],
            added: vec![],
            changed: vec![],
            order: vec![],
            complete: true,
        });
        assert_eq!(names(p.candidates(false)), [".config"]);
        p.on_reply(&Reply::Error {
            path: b"/d".to_vec(),
            errno: 13,
        });
        assert!(p.candidates(false).is_empty());
        assert!(!p.accept(false));
    }

    #[test]
    fn path_edit_targets_are_absolute_and_normalized() {
        let t = |s: &str| PathEdit::new(s, b"/home/u".to_vec(), b"/srv/w".to_vec()).target();
        assert_eq!(t("~"), b"/home/u");
        assert_eq!(t("~/a/../b/"), b"/home/u/b");
        assert_eq!(t("/"), b"/");
        assert_eq!(t("//etc/./x/.."), b"/etc");
        assert_eq!(t("sub"), b"/srv/w/sub");
        assert_eq!(t("../.."), b"/");
        assert_eq!(normalize(b"/a//b/"), b"/a/b");
    }
}
