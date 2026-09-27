// SPDX-License-Identifier: AGPL-3.0-only

//! The browser's pure state: which folder is shown, its listing as `fogd`
//! sent it, and the selection. No I/O and no sorting — `order` always comes
//! from `fogd` (invariant: the UI thread never does filesystem I/O or
//! sorting). Every method returns an [`Effect`] for the app to carry out.

use fog_proto::{apply_diff, Entry, Kind, Reply};

/// What the app must do after a state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Keep row `usize` (an index into `order`) in view.
    Reveal(usize),
    /// A new folder is shown: reset the scroll, then reveal this row.
    Entered(usize),
    /// Send `ListDir` for this path.
    List(Vec<u8>),
}

/// What the selection holds on to while a listing arrives in pieces.
///
/// `fogd` answers an uncached folder with a partial snapshot (the first
/// batch, sorted on its own) and then a diff carrying the full order. Until
/// the user moves, the selection is pinned to where entering put it, not to
/// whichever name happened to be first in the partial batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// The top row of whatever order is current.
    Top,
    /// The folder we came up from, once it shows up in the listing.
    Name(Vec<u8>),
    /// The user has moved: follow the selected name across updates.
    Free,
}

/// A navigation waiting for its first snapshot. Until it arrives the old
/// listing stays on screen, so a failed open leaves you where you were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nav {
    pub path: Vec<u8>,
    /// Entry name to select once the listing arrives: the folder we came up
    /// from.
    pub select: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    /// The folder whose listing is shown (absolute, raw bytes).
    pub path: Vec<u8>,
    /// `fogd`'s id for that listing; `None` until the first snapshot.
    pub dir: Option<u64>,
    pub generation: u64,
    pub entries: Vec<Entry>,
    /// Display order, indices into `entries`, exactly as `fogd` sent it.
    pub order: Vec<u32>,
    /// Index into `order`.
    pub selected: usize,
    pub pin: Pin,
    pub complete: bool,
    pub pending: Option<Nav>,
    /// The last error for the shown or requested folder: path and errno.
    pub error: Option<(Vec<u8>, i32)>,
}

impl Browser {
    /// A browser that has asked for `path` and shows nothing yet.
    pub fn new(path: Vec<u8>) -> Self {
        Self {
            pending: Some(Nav {
                path: path.clone(),
                select: None,
            }),
            path,
            dir: None,
            generation: 0,
            entries: Vec::new(),
            order: Vec::new(),
            selected: 0,
            pin: Pin::Top,
            complete: false,
            error: None,
        }
    }

    /// The path to (re)request when a connection comes up.
    pub fn target(&self) -> &[u8] {
        self.pending.as_ref().map_or(&self.path, |n| &n.path)
    }

    /// A new `fogd` connection is a new id space: `fogd` numbers listings
    /// from 1 on every start, so the id and generation we hold mean nothing
    /// to it. Forget them, so the relist's snapshot is taken as-is.
    pub fn reconnected(&mut self) {
        self.dir = None;
        self.generation = 0;
    }

    /// Rows in display order.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// The entry shown at display row `i`.
    pub fn row(&self, i: usize) -> Option<&Entry> {
        let idx = *self.order.get(i)?;
        self.entries.get(idx as usize)
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.row(self.selected)
    }

    pub fn on_reply(&mut self, reply: Reply) -> Effect {
        match reply {
            Reply::DirSnapshot {
                path,
                dir,
                generation,
                entries,
                order,
                complete,
            } => {
                if let Some(nav) = self.pending.take_if(|n| n.path == path) {
                    self.path = path;
                    self.set_listing(dir, generation, entries, order, complete);
                    self.pin = nav.select.map_or(Pin::Top, Pin::Name);
                    self.selected = 0;
                    self.settle(None);
                    self.error = None;
                    return Effect::Entered(self.selected);
                }
                if path != self.path {
                    return Effect::None;
                }
                // A refresh of the shown folder. An older generation of the
                // same listing is a reply that lost a race; drop it.
                if self.dir == Some(dir) && generation < self.generation {
                    return Effect::None;
                }
                let keep = self.selected_name();
                self.set_listing(dir, generation, entries, order, complete);
                self.settle_moved(keep)
            }
            Reply::DirDiff {
                dir,
                generation,
                removed,
                added,
                order,
                complete,
            } => {
                if self.dir != Some(dir) {
                    return Effect::None;
                }
                if generation != self.generation + 1 {
                    // Not based on what we hold: indices would not line up.
                    return if generation > self.generation {
                        Effect::List(self.path.clone())
                    } else {
                        Effect::None
                    };
                }
                let keep = self.selected_name();
                apply_diff(&mut self.entries, &removed, &added);
                self.order = order;
                self.generation = generation;
                self.complete = complete;
                self.settle_moved(keep)
            }
            Reply::Error { path, errno } => {
                if self.pending.take_if(|n| n.path == path).is_none() && path != self.path {
                    return Effect::None;
                }
                self.error = Some((path, errno));
                Effect::None
            }
            Reply::Stat(_) => Effect::None,
        }
    }

    /// Move the selection by `delta` rows, clamped.
    pub fn step(&mut self, delta: isize) -> Effect {
        if self.order.is_empty() {
            return Effect::None;
        }
        let last = self.order.len() - 1;
        self.pin = Pin::Free;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        Effect::Reveal(self.selected)
    }

    pub fn first(&mut self) -> Effect {
        self.jump(0)
    }

    pub fn last(&mut self) -> Effect {
        self.jump(self.order.len().saturating_sub(1))
    }

    fn jump(&mut self, i: usize) -> Effect {
        if self.order.is_empty() {
            return Effect::None;
        }
        self.pin = Pin::Free;
        self.selected = i;
        Effect::Reveal(i)
    }

    /// Open the selected entry if it may be a folder. Symlinks and unknown
    /// kinds are tried; `fogd` answers `ENOTDIR` if they are not.
    pub fn open(&mut self) -> Effect {
        let Some(e) = self.selected_entry() else {
            return Effect::None;
        };
        if !matches!(e.kind, Kind::Dir | Kind::Symlink | Kind::Unknown) {
            return Effect::None;
        }
        let path = join(&self.path, &e.name);
        self.navigate(path, None)
    }

    /// Go to the parent folder and reselect the one we left. Lexical, like a
    /// shell's `cd ..`: leaving a symlinked folder returns to where the link
    /// is, not to the target's parent.
    pub fn parent(&mut self) -> Effect {
        let Some((parent, name)) = split_parent(&self.path) else {
            return Effect::None;
        };
        self.navigate(parent, Some(name))
    }

    fn navigate(&mut self, path: Vec<u8>, select: Option<Vec<u8>>) -> Effect {
        self.pending = Some(Nav {
            path: path.clone(),
            select,
        });
        Effect::List(path)
    }

    fn set_listing(
        &mut self,
        dir: u64,
        generation: u64,
        entries: Vec<Entry>,
        order: Vec<u32>,
        complete: bool,
    ) {
        self.dir = Some(dir);
        self.generation = generation;
        self.entries = entries;
        self.order = order;
        self.complete = complete;
    }

    fn selected_name(&self) -> Option<Vec<u8>> {
        self.selected_entry().map(|e| e.name.clone())
    }

    /// Place the selection in a new order by the [`Pin`]. `keep` is the name
    /// that was selected before, which a free selection follows; if it is
    /// gone, the index stays, clamped.
    fn settle(&mut self, keep: Option<Vec<u8>>) {
        let found = match &self.pin {
            Pin::Top => Some(0),
            Pin::Name(n) => Some(self.position(n).unwrap_or(0)),
            Pin::Free => keep.and_then(|n| self.position(&n)),
        };
        self.selected = found
            .unwrap_or(self.selected)
            .min(self.order.len().saturating_sub(1));
    }

    /// [`Self::settle`] for an update of the shown folder: if the selected
    /// row moved, it is scrolled back into view.
    fn settle_moved(&mut self, keep: Option<Vec<u8>>) -> Effect {
        let before = self.selected;
        self.settle(keep);
        if self.selected == before {
            Effect::None
        } else {
            Effect::Reveal(self.selected)
        }
    }

    fn position(&self, name: &[u8]) -> Option<usize> {
        (0..self.order.len()).find(|&i| self.row(i).is_some_and(|e| e.name == name))
    }
}

/// `dir` + `/` + `name`, on raw bytes.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = dir.to_vec();
    if !p.ends_with(b"/") {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

/// `/a/b` into (`/a`, `b`); `/a` into (`/`, `a`); `/` has no parent.
pub fn split_parent(path: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let end = path.iter().rposition(|&b| b != b'/')?;
    let trimmed = &path[..=end];
    let cut = trimmed.iter().rposition(|&b| b == b'/')?;
    let parent = if cut == 0 {
        b"/".to_vec()
    } else {
        trimmed[..cut].to_vec()
    };
    Some((parent, trimmed[cut + 1..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.as_bytes().to_vec(),
            kind,
        }
    }

    fn snap(path: &str, dir: u64, gen: u64, names: &[(&str, Kind)], order: &[u32]) -> Reply {
        Reply::DirSnapshot {
            path: path.as_bytes().to_vec(),
            dir,
            generation: gen,
            entries: names.iter().map(|&(n, k)| e(n, k)).collect(),
            order: order.to_vec(),
            complete: true,
        }
    }

    fn names(b: &Browser) -> Vec<String> {
        (0..b.len())
            .map(|i| b.row(i).unwrap().display().into_owned())
            .collect()
    }

    fn sel(b: &Browser) -> String {
        b.selected_entry().unwrap().display().into_owned()
    }

    #[test]
    fn snapshot_for_requested_path_is_applied_in_fogd_order() {
        let mut b = Browser::new(b"/home".to_vec());
        assert_eq!(b.target(), b"/home");
        let fx = b.on_reply(snap(
            "/home",
            1,
            0,
            &[("b", Kind::File), ("a", Kind::Dir)],
            &[1, 0],
        ));
        assert_eq!(fx, Effect::Entered(0));
        assert_eq!(names(&b), ["a", "b"]);
        assert_eq!(b.dir, Some(1));
        assert!(b.pending.is_none());
    }

    #[test]
    fn stale_path_snapshot_is_rejected() {
        let mut b = Browser::new(b"/home".to_vec());
        b.on_reply(snap("/home", 1, 0, &[("a", Kind::Dir)], &[0]));
        b.open();
        // A late reply for somewhere we are neither in nor going to.
        let before = b.clone();
        assert_eq!(
            b.on_reply(snap("/etc", 9, 0, &[("x", Kind::File)], &[0])),
            Effect::None
        );
        assert_eq!(b, before);
        // A diff for another listing id is dropped too.
        b.on_reply(Reply::DirDiff {
            dir: 9,
            generation: 1,
            removed: vec![b"a".to_vec()],
            added: vec![],
            order: vec![],
            complete: true,
        });
        assert_eq!(b, before);
    }

    #[test]
    fn partial_snapshot_then_diff_completes() {
        let mut b = Browser::new(b"/d".to_vec());
        let mut first = snap("/d", 4, 0, &[("m", Kind::File), ("c", Kind::File)], &[1, 0]);
        if let Reply::DirSnapshot { complete, .. } = &mut first {
            *complete = false;
        }
        b.on_reply(first);
        assert!(!b.complete);
        b.on_reply(Reply::DirDiff {
            dir: 4,
            generation: 1,
            removed: vec![],
            added: vec![e("a", Kind::Dir), e("z", Kind::File)],
            order: vec![2, 1, 0, 3],
            complete: true,
        });
        assert!(b.complete);
        assert_eq!(names(&b), ["a", "c", "m", "z"]);
    }

    fn partial(path: &str, dir: u64, names: &[(&str, Kind)], order: &[u32]) -> Reply {
        let mut r = snap(path, dir, 0, names, order);
        if let Reply::DirSnapshot { complete, .. } = &mut r {
            *complete = false;
        }
        r
    }

    #[test]
    fn cold_open_keeps_the_top_row_selected() {
        // The partial batch holds "m" and "c"; the full order puts both
        // far from the top.
        let mut b = Browser::new(b"/big".to_vec());
        let fx = b.on_reply(partial(
            "/big",
            1,
            &[("m", Kind::File), ("c", Kind::File)],
            &[1, 0],
        ));
        assert_eq!(fx, Effect::Entered(0));
        assert_eq!(sel(&b), "c");
        let fx = b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![],
            added: vec![e("a", Kind::File), e("b", Kind::File)],
            order: vec![2, 3, 1, 0],
            complete: true,
        });
        assert_eq!(names(&b), ["a", "b", "c", "m"]);
        assert_eq!(b.selected, 0);
        assert_eq!(sel(&b), "a");
        assert_eq!(fx, Effect::None);
    }

    #[test]
    fn cold_open_follows_a_moved_selection_and_reveals_it() {
        let mut b = Browser::new(b"/big".to_vec());
        b.on_reply(partial(
            "/big",
            1,
            &[("m", Kind::File), ("c", Kind::File)],
            &[1, 0],
        ));
        b.step(1);
        assert_eq!(sel(&b), "m");
        let fx = b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![],
            added: vec![e("a", Kind::File), e("b", Kind::File)],
            order: vec![2, 3, 1, 0],
            complete: true,
        });
        assert_eq!(sel(&b), "m");
        assert_eq!(fx, Effect::Reveal(3));
    }

    #[test]
    fn cold_parent_reselects_the_folder_we_left_once_it_arrives() {
        let mut b = Browser::new(b"/p/src".to_vec());
        b.on_reply(snap("/p/src", 1, 0, &[("x", Kind::File)], &[0]));
        b.parent();
        // "src" is not in the first batch.
        let fx = b.on_reply(partial("/p", 2, &[("m", Kind::File)], &[0]));
        assert_eq!(fx, Effect::Entered(0));
        let fx = b.on_reply(Reply::DirDiff {
            dir: 2,
            generation: 1,
            removed: vec![],
            added: vec![e("a", Kind::Dir), e("src", Kind::Dir)],
            order: vec![1, 2, 0],
            complete: true,
        });
        assert_eq!(sel(&b), "src");
        assert_eq!(fx, Effect::Reveal(1));
    }

    #[test]
    fn reconnect_accepts_a_relist_that_reuses_the_old_id() {
        let mut b = Browser::new(b"/t".to_vec());
        b.on_reply(snap("/t", 1, 0, &[("a", Kind::File)], &[0]));
        b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![],
            added: vec![e("b", Kind::File)],
            order: vec![0, 1],
            complete: true,
        });
        b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 2,
            removed: vec![],
            added: vec![e("c", Kind::File)],
            order: vec![0, 1, 2],
            complete: true,
        });
        assert_eq!(b.generation, 2);
        // fogd restarts: ids begin at 1 again and generations at 0.
        b.reconnected();
        b.on_reply(partial("/t", 1, &[("a", Kind::File)], &[0]));
        assert_eq!(names(&b), ["a"]);
        assert!(!b.complete);
        b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![],
            added: vec![e("d", Kind::File)],
            order: vec![0, 1],
            complete: true,
        });
        assert_eq!(names(&b), ["a", "d"]);
        assert!(b.complete);
    }

    #[test]
    fn diff_keeps_selection_on_the_same_name() {
        let mut b = Browser::new(b"/d".to_vec());
        b.on_reply(snap(
            "/d",
            2,
            0,
            &[("a", Kind::File), ("b", Kind::File), ("c", Kind::File)],
            &[0, 1, 2],
        ));
        b.step(2);
        assert_eq!(sel(&b), "c");
        // "0" is added and sorts first; "a" is removed. "c" moves but stays
        // selected.
        b.on_reply(Reply::DirDiff {
            dir: 2,
            generation: 1,
            removed: vec![b"a".to_vec()],
            added: vec![e("0", Kind::File), e("bb", Kind::File)],
            order: vec![2, 0, 3, 1],
            complete: true,
        });
        assert_eq!(names(&b), ["0", "b", "bb", "c"]);
        assert_eq!(sel(&b), "c");
        assert_eq!(b.selected, 3);
        // The selected entry itself goes away: the index stays, clamped.
        b.on_reply(Reply::DirDiff {
            dir: 2,
            generation: 2,
            removed: vec![b"c".to_vec()],
            added: vec![],
            // entries are now [b, 0, bb]
            order: vec![1, 0, 2],
            complete: true,
        });
        assert_eq!(b.selected, 2);
        assert_eq!(sel(&b), "bb");
    }

    #[test]
    fn diff_with_a_generation_gap_asks_for_a_resync() {
        let mut b = Browser::new(b"/d".to_vec());
        b.on_reply(snap("/d", 2, 3, &[("a", Kind::File)], &[0]));
        let before = b.clone();
        let fx = b.on_reply(Reply::DirDiff {
            dir: 2,
            generation: 7,
            removed: vec![],
            added: vec![e("b", Kind::File)],
            order: vec![0, 1],
            complete: true,
        });
        assert_eq!(fx, Effect::List(b"/d".to_vec()));
        assert_eq!(b, before);
    }

    #[test]
    fn refresh_snapshot_keeps_selection_and_drops_older_generations() {
        let mut b = Browser::new(b"/d".to_vec());
        b.on_reply(snap(
            "/d",
            2,
            1,
            &[("a", Kind::File), ("b", Kind::File)],
            &[0, 1],
        ));
        b.step(1);
        assert_eq!(
            b.on_reply(snap("/d", 2, 0, &[("zz", Kind::File)], &[0])),
            Effect::None
        );
        assert_eq!(names(&b), ["a", "b"]);
        b.on_reply(snap(
            "/d",
            2,
            2,
            &[("a", Kind::File), ("b", Kind::File), ("0", Kind::File)],
            &[2, 0, 1],
        ));
        assert_eq!(sel(&b), "b");
    }

    #[test]
    fn open_and_parent_reselect_the_folder_we_came_from() {
        let mut b = Browser::new(b"/home/u".to_vec());
        b.on_reply(snap(
            "/home/u",
            1,
            0,
            &[("f", Kind::File), ("src", Kind::Dir), ("docs", Kind::Dir)],
            &[2, 1, 0],
        ));
        b.step(1);
        assert_eq!(b.open(), Effect::List(b"/home/u/src".to_vec()));
        // Until the listing arrives, the old one stays.
        assert_eq!(b.path, b"/home/u");
        b.on_reply(snap("/home/u/src", 2, 0, &[("main.rs", Kind::File)], &[0]));
        assert_eq!(b.path, b"/home/u/src");

        assert_eq!(b.parent(), Effect::List(b"/home/u".to_vec()));
        let fx = b.on_reply(snap(
            "/home/u",
            1,
            0,
            &[("f", Kind::File), ("src", Kind::Dir), ("docs", Kind::Dir)],
            &[2, 1, 0],
        ));
        assert_eq!(fx, Effect::Entered(1));
        assert_eq!(sel(&b), "src");
    }

    #[test]
    fn open_ignores_files_and_error_keeps_the_old_listing() {
        let mut b = Browser::new(b"/".to_vec());
        b.on_reply(snap(
            "/",
            1,
            0,
            &[("f", Kind::File), ("l", Kind::Symlink)],
            &[0, 1],
        ));
        assert_eq!(b.open(), Effect::None);
        b.step(1);
        assert_eq!(b.open(), Effect::List(b"/l".to_vec()));
        b.on_reply(Reply::Error {
            path: b"/l".to_vec(),
            errno: 20,
        });
        assert!(b.pending.is_none());
        assert_eq!(b.path, b"/");
        assert_eq!(b.error, Some((b"/l".to_vec(), 20)));
        assert_eq!(names(&b), ["f", "l"]);
        // An error for an unrelated path is ignored.
        b.on_reply(Reply::Error {
            path: b"/x".to_vec(),
            errno: 13,
        });
        assert_eq!(b.error, Some((b"/l".to_vec(), 20)));
    }

    #[test]
    fn movement_clamps() {
        let mut b = Browser::new(b"/".to_vec());
        assert_eq!(b.step(1), Effect::None);
        b.on_reply(snap(
            "/",
            1,
            0,
            &[("a", Kind::File), ("b", Kind::File), ("c", Kind::File)],
            &[0, 1, 2],
        ));
        assert_eq!(b.step(-1), Effect::Reveal(0));
        assert_eq!(b.step(10), Effect::Reveal(2));
        assert_eq!(b.first(), Effect::Reveal(0));
        assert_eq!(b.last(), Effect::Reveal(2));
    }

    #[test]
    fn path_helpers() {
        assert_eq!(join(b"/", b"a"), b"/a");
        assert_eq!(join(b"/a", b"b"), b"/a/b");
        assert_eq!(split_parent(b"/"), None);
        assert_eq!(split_parent(b"/a"), Some((b"/".to_vec(), b"a".to_vec())));
        assert_eq!(
            split_parent(b"/a/b/"),
            Some((b"/a".to_vec(), b"b".to_vec()))
        );
        let mut b = Browser::new(b"/".to_vec());
        assert_eq!(b.parent(), Effect::None);
    }
}
