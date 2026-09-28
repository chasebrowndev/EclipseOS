// SPDX-License-Identifier: AGPL-3.0-only

//! The browser's pure state: which folder each tab shows, its listing as
//! `fogd` sent it, the filter and the selection. No I/O and no sorting —
//! `order` always comes from `fogd` (invariant: the UI thread never does
//! filesystem I/O or sorting). The filter and hidden-file toggle only drop
//! rows from that order; they never reorder it. Every method returns an
//! [`Effect`] for the app to carry out.

use std::collections::{BTreeSet, HashSet};

use fog_proto::{apply_diff, Entry, Kind, Reply, Sort, SortKey};

/// What the app must do after a state change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Keep row `usize` (an index into the visible rows) in view.
    Reveal(usize),
    /// A new folder is shown: reset the scroll, then reveal this row.
    Entered(usize),
    /// Send `Subscribe` for this path: a listing, then pushed diffs.
    List(Vec<u8>),
    /// Stop the pushes for a listing we no longer show.
    Unsubscribe(u64),
    /// Send `Open` for this file: `fogd` launches its handler.
    Open(Vec<u8>),
    /// Send `SetSort`: `fogd` reorders the listing and says `Sorted`.
    SetSort(u64, Sort),
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

/// A one-line report for the status line, replaced by the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `fogd` launched a handler for this file.
    Opened(Vec<u8>),
    /// Opening this file failed with this errno.
    OpenFailed(Vec<u8>, i32),
    /// A custom action started.
    Ran(String),
    /// A custom action could not be started: name and reason.
    RunFailed(String, String),
    /// An action this build has no behaviour for yet.
    Unavailable(&'static str),
}

/// How a row was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Move the cursor there and drop the marks.
    Only,
    /// Ctrl: toggle that row's mark.
    Toggle,
    /// Shift: mark from the cursor to there.
    Extend,
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
    /// `order` without hidden or filtered-out rows. What is drawn.
    visible: Vec<u32>,
    /// Index into the visible rows: the cursor.
    pub selected: usize,
    pub pin: Pin,
    pub complete: bool,
    pub pending: Option<Nav>,
    /// The last error for the shown or requested folder: path and errno.
    pub error: Option<(Vec<u8>, i32)>,
    /// How `fogd` orders this listing, from its last `Sorted`.
    pub sort: Sort,
    pub show_hidden: bool,
    /// Type-to-filter text: a fuzzy match on names. Empty is no filter.
    pub filter: String,
    /// Marked names (Ctrl+click, select-all, a finished visual range).
    pub marked: BTreeSet<Vec<u8>>,
    /// Visual mode: the name the range started on. The range runs from it
    /// to the cursor.
    pub anchor: Option<Vec<u8>>,
    /// A file sent to `fogd` with `Open`, until it answers.
    pub opening: Option<Vec<u8>>,
    pub notice: Option<Notice>,
    /// Free and total bytes on the shown folder's filesystem.
    pub space: Option<(u64, u64)>,
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
            visible: Vec::new(),
            selected: 0,
            pin: Pin::Top,
            complete: false,
            error: None,
            sort: Sort::default(),
            show_hidden: false,
            filter: String::new(),
            marked: BTreeSet::new(),
            anchor: None,
            opening: None,
            notice: None,
            space: None,
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

    /// Visible rows.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    /// Every row of the listing, hidden and filtered ones included.
    pub fn total(&self) -> usize {
        self.order.len()
    }

    /// Rows the hidden-files setting shows, before the filter narrows them.
    pub fn unfiltered(&self) -> usize {
        if self.show_hidden {
            return self.total();
        }
        self.order
            .iter()
            .filter_map(|&i| self.entries.get(i as usize))
            .filter(|e| !e.name.starts_with(b"."))
            .count()
    }

    /// The entry shown at visible row `i`.
    pub fn row(&self, i: usize) -> Option<&Entry> {
        let idx = *self.visible.get(i)?;
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
                    self.filter.clear();
                    self.marked.clear();
                    self.anchor = None;
                    self.space = None;
                    self.set_listing(dir, generation, entries, order, complete);
                    self.pin = nav.select.map_or(Pin::Top, Pin::Name);
                    self.selected = 0;
                    self.settle(None);
                    self.error = None;
                    return Effect::Entered(self.selected);
                }
                if path != self.path {
                    // A navigation we moved on from: its subscription is ours
                    // to end.
                    return Effect::Unsubscribe(dir);
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
                changed,
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
                apply_diff(&mut self.entries, &removed, &added, &changed);
                for gone in &removed {
                    self.marked.remove(gone);
                }
                self.order = order;
                self.generation = generation;
                self.complete = complete;
                self.refilter();
                self.settle_moved(keep)
            }
            Reply::Error { path, errno } => {
                if self.opening.take_if(|p| *p == path).is_some() {
                    self.notice = Some(Notice::OpenFailed(path, errno));
                    return Effect::None;
                }
                let navigating = self.pending.take_if(|n| n.path == path).is_some();
                if !navigating && path != self.path {
                    return Effect::None;
                }
                let not_dir = std::io::Error::from_raw_os_error(errno).kind()
                    == std::io::ErrorKind::NotADirectory;
                if not_dir && navigating {
                    // The start path is a file (`fog-ui FILE`, or a file://
                    // URI from the desktop entry): nothing is shown yet, so
                    // open its parent with the file selected.
                    if self.dir.is_none() {
                        if let Some((parent, name)) = split_parent(&path) {
                            self.path = parent.clone();
                            return self.navigate(parent, Some(name));
                        }
                    }
                    // A symlink or unknown kind we tried as a folder is a
                    // file after all: open it instead.
                    self.opening = Some(path.clone());
                    return Effect::Open(path);
                }
                self.error = Some((path, errno));
                Effect::None
            }
            Reply::Opened { path } => {
                if self.opening.take_if(|p| *p == path).is_some() {
                    self.notice = Some(Notice::Opened(path));
                }
                Effect::None
            }
            Reply::Sorted { dir, sort } => {
                if self.dir == Some(dir) {
                    self.sort = sort;
                }
                Effect::None
            }
            Reply::FsInfo { path, free, total } => {
                if path == self.path {
                    self.space = Some((free, total));
                }
                Effect::None
            }
            // Jobs, trash and config errors get views in later units; places
            // belong to the window, not a tab.
            Reply::Stat(_)
            | Reply::JobAccepted { .. }
            | Reply::JobProgress { .. }
            | Reply::JobState { .. }
            | Reply::UndoResult { .. }
            | Reply::TrashList(_)
            | Reply::PlacesList(_)
            | Reply::ConfigError { .. } => Effect::None,
        }
    }

    /// Move the selection by `delta` rows, clamped.
    pub fn step(&mut self, delta: isize) -> Effect {
        if self.visible.is_empty() {
            return Effect::None;
        }
        let last = self.visible.len() - 1;
        self.pin = Pin::Free;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        Effect::Reveal(self.selected)
    }

    pub fn first(&mut self) -> Effect {
        self.jump(0)
    }

    pub fn last(&mut self) -> Effect {
        self.jump(self.visible.len().saturating_sub(1))
    }

    fn jump(&mut self, i: usize) -> Effect {
        if self.visible.is_empty() {
            return Effect::None;
        }
        self.pin = Pin::Free;
        self.selected = i;
        Effect::Reveal(i)
    }

    /// Open the selected entry: a folder (or a symlink or unknown kind,
    /// which `fogd` answers `ENOTDIR` for if it is not one) is entered, a
    /// file is handed to `fogd`'s `Open`.
    pub fn open(&mut self) -> Effect {
        let Some(e) = self.selected_entry() else {
            return Effect::None;
        };
        let path = join(&self.path, &e.name);
        if matches!(e.kind, Kind::Dir | Kind::Symlink | Kind::Unknown) {
            return self.navigate(path, None);
        }
        self.opening = Some(path.clone());
        Effect::Open(path)
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

    /// Go to `path` (a place, a breadcrumb, the edited path bar).
    pub fn go(&mut self, path: Vec<u8>) -> Effect {
        self.navigate(path, None)
    }

    fn navigate(&mut self, path: Vec<u8>, select: Option<Vec<u8>>) -> Effect {
        self.pending = Some(Nav {
            path: path.clone(),
            select,
        });
        Effect::List(path)
    }

    // Filter and hidden files.

    /// Type-to-filter: add `s` and put the cursor on the first match.
    pub fn push_filter(&mut self, s: &str) -> Effect {
        self.filter.push_str(s);
        self.refilter();
        self.jump(0)
    }

    /// Backspace in the filter. The cursor stays on its entry if it still
    /// matches.
    pub fn pop_filter(&mut self) -> Effect {
        let keep = self.selected_name();
        self.filter.pop();
        self.refilter();
        self.follow(keep)
    }

    /// Esc: drop the filter; the cursor stays on the entry it was on.
    pub fn clear_filter(&mut self) -> Effect {
        let keep = self.selected_name();
        self.filter.clear();
        self.refilter();
        self.follow(keep)
    }

    pub fn toggle_hidden(&mut self) -> Effect {
        let keep = self.selected_name();
        self.show_hidden = !self.show_hidden;
        self.refilter();
        self.follow(keep)
    }

    /// Esc, innermost first: the filter, then a visual range, then marks.
    pub fn escape(&mut self) -> Effect {
        if !self.filter.is_empty() {
            return self.clear_filter();
        }
        if self.anchor.take().is_none() {
            self.marked.clear();
        }
        Effect::None
    }

    fn refilter(&mut self) {
        let shown: Vec<u32> = self
            .order
            .iter()
            .copied()
            .filter(|&i| {
                self.entries.get(i as usize).is_some_and(|e| {
                    (self.show_hidden || !e.name.starts_with(b".")) && fuzzy(&self.filter, &e.name)
                })
            })
            .collect();
        self.visible = shown;
    }

    /// Put the cursor back on `keep` after the visible rows changed, else
    /// keep the index, clamped; reveal it.
    fn follow(&mut self, keep: Option<Vec<u8>>) -> Effect {
        if let Some(i) = keep.and_then(|n| self.position(&n)) {
            self.selected = i;
        }
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        if self.visible.is_empty() {
            Effect::None
        } else {
            Effect::Reveal(self.selected)
        }
    }

    // Selection.

    /// `v`: start a range at the cursor, or mark the range and end it.
    pub fn visual(&mut self) {
        if self.anchor.is_none() {
            self.anchor = self.selected_name();
        } else {
            let names = self.range_names();
            self.marked.extend(names);
            self.anchor = None;
        }
    }

    /// Mark every visible row; if all are marked already, unmark them.
    pub fn select_all(&mut self) {
        self.anchor = None;
        let names: Vec<Vec<u8>> = (0..self.len())
            .filter_map(|i| self.row(i).map(|e| e.name.clone()))
            .collect();
        if names.iter().all(|n| self.marked.contains(n)) {
            self.marked.clear();
        } else {
            self.marked.extend(names);
        }
    }

    pub fn click(&mut self, i: usize, how: Click) {
        let Some(name) = self.row(i).map(|e| e.name.clone()) else {
            return;
        };
        match how {
            Click::Only => {
                self.marked.clear();
                self.anchor = None;
            }
            Click::Toggle => {
                if !self.marked.remove(&name) {
                    self.marked.insert(name);
                }
            }
            Click::Extend => {
                let (a, b) = (self.selected.min(i), self.selected.max(i));
                let names: Vec<Vec<u8>> = (a..=b)
                    .filter_map(|r| self.row(r).map(|e| e.name.clone()))
                    .collect();
                self.marked.extend(names);
            }
        }
        self.pin = Pin::Free;
        self.selected = i;
    }

    /// The visual range as visible-row bounds, while one is open.
    pub fn range(&self) -> Option<(usize, usize)> {
        let a = self.position(self.anchor.as_deref()?)?;
        Some((a.min(self.selected), a.max(self.selected)))
    }

    fn range_names(&self) -> Vec<Vec<u8>> {
        self.range().map_or_else(Vec::new, |(a, b)| {
            (a..=b)
                .filter_map(|r| self.row(r).map(|e| e.name.clone()))
                .collect()
        })
    }

    /// Whether visible row `i` is marked or inside the visual range.
    pub fn is_marked(&self, i: usize) -> bool {
        self.range().is_some_and(|(a, b)| (a..=b).contains(&i))
            || self.row(i).is_some_and(|e| self.marked.contains(&e.name))
    }

    /// Marked entries plus the visual range, in display order. Rows the
    /// filter hides stay selected.
    pub fn selection(&self) -> Vec<&Entry> {
        let range: HashSet<Vec<u8>> = self.range_names().into_iter().collect();
        self.order
            .iter()
            .filter_map(|&i| self.entries.get(i as usize))
            .filter(|e| self.marked.contains(&e.name) || range.contains(&e.name))
            .collect()
    }

    /// What a custom action runs on: the selection, else the cursor's
    /// entry, as absolute paths.
    pub fn targets(&self) -> Vec<Vec<u8>> {
        let sel = self.selection();
        let picked = if sel.is_empty() {
            self.selected_entry().into_iter().collect()
        } else {
            sel
        };
        picked.iter().map(|e| join(&self.path, &e.name)).collect()
    }

    // Sorting: asked of `fogd`, never done here.

    /// Order by `key`. The key already in use flips direction; a new size or
    /// date key starts largest or newest first, a name or type key A to Z.
    pub fn sort_by(&mut self, key: SortKey) -> Effect {
        let reverse = if key == self.sort.key {
            !self.sort.reverse
        } else {
            matches!(key, SortKey::Size | SortKey::Modified)
        };
        self.resort(Sort {
            key,
            reverse,
            ..self.sort
        })
    }

    pub fn sort_reverse(&mut self) -> Effect {
        self.resort(Sort {
            reverse: !self.sort.reverse,
            ..self.sort
        })
    }

    pub fn sort_dirs_first(&mut self) -> Effect {
        self.resort(Sort {
            dirs_first: !self.sort.dirs_first,
            ..self.sort
        })
    }

    fn resort(&mut self, sort: Sort) -> Effect {
        self.pin = Pin::Free;
        match self.dir {
            Some(dir) if self.pending.is_none() => Effect::SetSort(dir, sort),
            _ => Effect::None,
        }
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
        self.refilter();
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
            .min(self.visible.len().saturating_sub(1));
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
        (0..self.len()).find(|&i| self.row(i).is_some_and(|e| e.name == name))
    }
}

/// The window's tabs (FOG §UI and navigation). One `fogd` connection serves
/// them all, so a reply is offered to every tab and each keeps what is its
/// own; a subscription ends only when no tab still shows that listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tabs {
    tabs: Vec<Browser>,
    active: usize,
}

impl Tabs {
    pub fn new(first: Browser) -> Self {
        Self {
            tabs: vec![first],
            active: 0,
        }
    }

    pub fn active(&self) -> &Browser {
        &self.tabs[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Browser {
        &mut self.tabs[self.active]
    }

    pub fn index(&self) -> usize {
        self.active
    }

    pub fn get(&self, i: usize) -> Option<&Browser> {
        self.tabs.get(i)
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Browser> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Browser> {
        self.tabs.iter_mut()
    }

    /// Offer `reply` to every tab. Returns each tab's effect, by tab index,
    /// with the subscriptions that no tab holds any more ended (the folders
    /// tabs left, and replies no tab wanted).
    pub fn on_reply(&mut self, reply: Reply) -> Vec<(usize, Effect)> {
        let before: Vec<Option<u64>> = self.tabs.iter().map(|b| b.dir).collect();
        let mut out: Vec<(usize, Effect)> = Vec::new();
        for (i, b) in self.tabs.iter_mut().enumerate() {
            match b.on_reply(reply.clone()) {
                Effect::None => {}
                fx => out.push((i, fx)),
            }
        }
        let left = before
            .into_iter()
            .zip(self.tabs.iter().map(|b| b.dir))
            .filter_map(|(was, now)| was.filter(|&w| Some(w) != now));
        let mut ended: Vec<u64> = out
            .iter()
            .filter_map(|(_, fx)| match fx {
                Effect::Unsubscribe(d) => Some(*d),
                _ => None,
            })
            .chain(left)
            .filter(|d| !self.holds(*d))
            .collect();
        ended.sort_unstable();
        ended.dedup();
        out.retain(|(_, fx)| !matches!(fx, Effect::Unsubscribe(_)));
        out.extend(
            ended
                .into_iter()
                .map(|d| (self.active, Effect::Unsubscribe(d))),
        );
        out
    }

    fn holds(&self, dir: u64) -> bool {
        self.tabs.iter().any(|b| b.dir == Some(dir))
    }

    /// A new tab on the active tab's folder, placed after it and made
    /// active.
    pub fn open(&mut self) -> Effect {
        let cur = self.active();
        let mut b = Browser::new(cur.path.clone());
        b.show_hidden = cur.show_hidden;
        self.active += 1;
        self.tabs.insert(self.active, b);
        Effect::List(self.active().path.clone())
    }

    /// Close the active tab. `None` when it was the last one: the window
    /// closes.
    pub fn close(&mut self) -> Option<Effect> {
        if self.tabs.len() == 1 {
            return None;
        }
        let gone = self.tabs.remove(self.active);
        self.active = self.active.min(self.tabs.len() - 1);
        Some(match gone.dir {
            Some(d) if !self.holds(d) => Effect::Unsubscribe(d),
            _ => Effect::None,
        })
    }

    pub fn select(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    /// Next (`1`) or previous (`-1`) tab, wrapping.
    pub fn cycle(&mut self, delta: isize) {
        let n = self.tabs.len() as isize;
        self.active = (self.active as isize + delta).rem_euclid(n) as usize;
    }
}

/// Fuzzy match for the filter and the palette: every byte of `needle`, in
/// order, somewhere in `hay`, ASCII case folded. Only a yes or no; nothing
/// is ranked, so matches keep `fogd`'s order.
pub fn fuzzy(needle: &str, hay: &[u8]) -> bool {
    let mut hay = hay.iter();
    needle
        .as_bytes()
        .iter()
        .all(|n| hay.any(|h| h.eq_ignore_ascii_case(n)))
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
        Entry::new(name.as_bytes().to_vec(), kind)
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
            Effect::Unsubscribe(9)
        );
        assert_eq!(b, before);
        // A diff for another listing id is dropped too.
        b.on_reply(Reply::DirDiff {
            dir: 9,
            generation: 1,
            removed: vec![b"a".to_vec()],
            added: vec![],
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
            order: vec![0, 1],
            complete: true,
        });
        b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 2,
            removed: vec![],
            added: vec![e("c", Kind::File)],
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
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
            changed: vec![],
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
    fn starting_on_a_file_opens_its_parent_with_the_file_selected() {
        let mut b = Browser::new(b"/d/f".to_vec());
        let fx = b.on_reply(Reply::Error {
            path: b"/d/f".to_vec(),
            errno: 20,
        });
        assert_eq!(fx, Effect::List(b"/d".to_vec()));
        assert_eq!(b.target(), b"/d");
        assert_eq!(b.error, None);
        b.on_reply(snap(
            "/d",
            1,
            0,
            &[("a", Kind::File), ("f", Kind::File)],
            &[0, 1],
        ));
        assert_eq!(sel(&b), "f");
    }

    #[test]
    fn open_hands_files_to_fogd_and_reports_the_outcome() {
        let mut b = Browser::new(b"/".to_vec());
        b.on_reply(snap(
            "/",
            1,
            0,
            &[("f", Kind::File), ("l", Kind::Symlink), ("d", Kind::Dir)],
            &[0, 1, 2],
        ));
        assert_eq!(b.open(), Effect::Open(b"/f".to_vec()));
        b.on_reply(Reply::Opened {
            path: b"/f".to_vec(),
        });
        assert_eq!(b.notice, Some(Notice::Opened(b"/f".to_vec())));
        assert_eq!(b.opening, None);

        b.open();
        b.on_reply(Reply::Error {
            path: b"/f".to_vec(),
            errno: 2,
        });
        assert_eq!(b.notice, Some(Notice::OpenFailed(b"/f".to_vec(), 2)));
        assert_eq!(b.error, None, "a failed open is not a listing error");

        // A symlink is tried as a folder; ENOTDIR means it is a file.
        b.step(1);
        assert_eq!(b.open(), Effect::List(b"/l".to_vec()));
        let fx = b.on_reply(Reply::Error {
            path: b"/l".to_vec(),
            errno: 20,
        });
        assert_eq!(fx, Effect::Open(b"/l".to_vec()));
        assert!(b.pending.is_none());
        assert_eq!(b.path, b"/");
        assert_eq!(names(&b), ["f", "l", "d"]);

        // Any other failure to enter keeps the old listing and says why.
        b.step(1);
        b.open();
        b.on_reply(Reply::Error {
            path: b"/d".to_vec(),
            errno: 13,
        });
        assert_eq!(b.error, Some((b"/d".to_vec(), 13)));
        assert_eq!(b.path, b"/");
        // An error for an unrelated path is ignored.
        b.on_reply(Reply::Error {
            path: b"/x".to_vec(),
            errno: 2,
        });
        assert_eq!(b.error, Some((b"/d".to_vec(), 13)));
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

    fn listed(names: &[(&str, Kind)]) -> Browser {
        let mut b = Browser::new(b"/w".to_vec());
        let order: Vec<u32> = (0..names.len() as u32).collect();
        b.on_reply(snap("/w", 1, 0, names, &order));
        b
    }

    fn marked(b: &Browser) -> Vec<String> {
        b.selection()
            .iter()
            .map(|e| e.display().into_owned())
            .collect()
    }

    #[test]
    fn filter_narrows_in_fogd_order_and_esc_restores() {
        let mut b = listed(&[
            ("Cargo.toml", Kind::File),
            ("crates", Kind::Dir),
            ("docs", Kind::Dir),
            ("README.md", Kind::File),
            (".git", Kind::Dir),
        ]);
        assert_eq!(names(&b), ["Cargo.toml", "crates", "docs", "README.md"]);
        assert_eq!(b.total(), 5);
        b.step(2);
        assert_eq!(sel(&b), "docs");

        // Fuzzy, case-insensitive; the order is never changed.
        assert_eq!(b.push_filter("c"), Effect::Reveal(0));
        assert_eq!(names(&b), ["Cargo.toml", "crates", "docs"]);
        b.push_filter("r");
        assert_eq!(names(&b), ["Cargo.toml", "crates"]);
        b.step(1);
        assert_eq!(sel(&b), "crates");
        b.push_filter("zz");
        assert_eq!(b.len(), 0);
        assert_eq!(b.open(), Effect::None);
        b.pop_filter();
        b.pop_filter();
        assert_eq!(names(&b), ["Cargo.toml", "crates"]);

        // A diff while filtering is filtered too.
        b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![],
            added: vec![e("scripts", Kind::Dir)],
            changed: vec![],
            order: vec![5, 0, 1, 2, 3, 4],
            complete: true,
        });
        assert_eq!(names(&b), ["scripts", "Cargo.toml", "crates"]);

        // Esc drops the filter and the cursor stays on its entry.
        b.step(2);
        assert_eq!(b.escape(), Effect::Reveal(2));
        assert_eq!(b.filter, "");
        assert_eq!(sel(&b), "crates");
        assert_eq!(b.len(), 5);

        // Hidden files: shown and hidden again around the cursor.
        assert_eq!(b.toggle_hidden(), Effect::Reveal(2));
        assert_eq!(b.len(), 6);
        b.last();
        assert_eq!(sel(&b), ".git");
        b.toggle_hidden();
        assert_eq!(b.selected, 4);

        // Entering a folder ends the filter.
        b.push_filter("do");
        assert_eq!(sel(&b), "docs");
        b.open();
        b.on_reply(snap("/w/docs", 2, 0, &[("x", Kind::File)], &[0]));
        assert_eq!(b.filter, "");
        assert_eq!(names(&b), ["x"]);
    }

    #[test]
    fn visual_marks_a_range_and_esc_unwinds() {
        let mut b = listed(&[
            ("a", Kind::File),
            ("b", Kind::File),
            ("c", Kind::File),
            ("d", Kind::File),
            ("e", Kind::File),
        ]);
        b.step(1);
        b.visual();
        assert_eq!(b.anchor.as_deref(), Some(&b"b"[..]));
        b.step(2);
        assert_eq!(b.range(), Some((1, 3)));
        assert!(b.is_marked(2) && !b.is_marked(0));
        assert_eq!(marked(&b), ["b", "c", "d"]);
        // Moving back past the anchor flips the range.
        b.first();
        assert_eq!(marked(&b), ["a", "b"]);
        // A second `v` keeps the range as marks.
        b.visual();
        assert_eq!(b.anchor, None);
        b.last();
        assert_eq!(marked(&b), ["a", "b"]);
        assert_eq!(b.targets(), [b"/w/a".to_vec(), b"/w/b".to_vec()]);

        // Ctrl+click toggles one, Shift+click extends from the cursor.
        b.click(2, Click::Toggle);
        assert_eq!(marked(&b), ["a", "b", "c"]);
        b.click(4, Click::Extend);
        assert_eq!(marked(&b), ["a", "b", "c", "d", "e"]);
        b.click(0, Click::Toggle);
        assert_eq!(marked(&b), ["b", "c", "d", "e"]);

        // A marked entry that is deleted is no longer marked.
        b.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![b"c".to_vec()],
            added: vec![],
            changed: vec![],
            order: vec![0, 1, 2, 3],
            complete: true,
        });
        assert_eq!(marked(&b), ["b", "d", "e"]);

        // Esc: an open range first, then the marks.
        b.visual();
        assert!(b.anchor.is_some());
        b.escape();
        assert_eq!(b.anchor, None);
        assert_eq!(marked(&b), ["b", "d", "e"]);
        b.escape();
        assert!(marked(&b).is_empty());
        // Nothing marked: an action runs on the cursor.
        assert_eq!(b.targets(), [b"/w/a".to_vec()]);

        b.select_all();
        assert_eq!(marked(&b), ["a", "b", "d", "e"]);
        b.select_all();
        assert!(marked(&b).is_empty());
        b.click(3, Click::Toggle);
        b.click(1, Click::Only);
        assert!(marked(&b).is_empty());
        assert_eq!(sel(&b), "b");
    }

    #[test]
    fn sort_asks_fogd_and_follows_sorted() {
        let mut b = Browser::new(b"/w".to_vec());
        assert_eq!(b.sort_by(SortKey::Size), Effect::None, "nothing listed");
        b.on_reply(snap("/w", 4, 0, &[("a", Kind::File)], &[0]));
        let size_desc = Sort {
            key: SortKey::Size,
            reverse: true,
            dirs_first: true,
        };
        assert_eq!(b.sort_by(SortKey::Size), Effect::SetSort(4, size_desc));
        // Nothing changes until fogd says so.
        assert_eq!(b.sort, Sort::default());
        b.on_reply(Reply::Sorted {
            dir: 9,
            sort: size_desc,
        });
        assert_eq!(b.sort, Sort::default(), "another listing's sort");
        b.on_reply(Reply::Sorted {
            dir: 4,
            sort: size_desc,
        });
        assert_eq!(b.sort, size_desc);
        // The same key again flips direction; a name key starts A to Z.
        let Effect::SetSort(_, s) = b.sort_by(SortKey::Size) else {
            panic!("no SetSort")
        };
        assert!(!s.reverse);
        let Effect::SetSort(_, s) = b.sort_by(SortKey::Name) else {
            panic!("no SetSort")
        };
        assert_eq!((s.key, s.reverse), (SortKey::Name, false));
        let Effect::SetSort(_, s) = b.sort_dirs_first() else {
            panic!("no SetSort")
        };
        assert!(!s.dirs_first && s.key == SortKey::Size);
    }

    #[test]
    fn fs_info_is_kept_for_the_shown_folder_only() {
        let mut b = listed(&[("a", Kind::File)]);
        b.on_reply(Reply::FsInfo {
            path: b"/elsewhere".to_vec(),
            free: 1,
            total: 2,
        });
        assert_eq!(b.space, None);
        b.on_reply(Reply::FsInfo {
            path: b"/w".to_vec(),
            free: 3,
            total: 4,
        });
        assert_eq!(b.space, Some((3, 4)));
    }

    #[test]
    fn tabs_share_listings_and_unsubscribe_only_the_last_holder() {
        let mut t = Tabs::new(Browser::new(b"/a".to_vec()));
        t.on_reply(snap("/a", 1, 0, &[("s", Kind::Dir)], &[0]));
        assert_eq!(t.open(), Effect::List(b"/a".to_vec()));
        assert_eq!((t.len(), t.index()), (2, 1));
        // fogd answers the second Subscribe from its cache: both tabs take
        // it, one as its first listing, one as a refresh.
        let fx = t.on_reply(snap("/a", 1, 0, &[("s", Kind::Dir)], &[0]));
        assert_eq!(fx, [(1, Effect::Entered(0))]);
        assert!(t.iter().all(|b| b.dir == Some(1)));

        // Tab 1 goes into /a/s: tab 0 still shows /a, so no Unsubscribe.
        assert_eq!(t.active_mut().open(), Effect::List(b"/a/s".to_vec()));
        let fx = t.on_reply(snap("/a/s", 2, 0, &[("x", Kind::File)], &[0]));
        assert_eq!(fx, [(1, Effect::Entered(0))]);

        // Back to tab 0 and into /a/s too.
        t.cycle(1);
        assert_eq!(t.index(), 0);
        t.active_mut().open();
        let fx = t.on_reply(snap("/a/s", 2, 0, &[("x", Kind::File)], &[0]));
        // Nobody shows /a any more.
        assert_eq!(fx, [(0, Effect::Entered(0)), (0, Effect::Unsubscribe(1))]);
        t.select(1);
        assert_eq!(t.close(), Some(Effect::None), "tab 0 still holds dir 2");
        assert_eq!(t.index(), 0);
        assert_eq!(t.close(), None, "the last tab closes the window");

        // A listing no tab wants (the path bar's completion) is ended.
        let fx = t.on_reply(snap("/z", 7, 0, &[], &[]));
        assert_eq!(fx, [(0, Effect::Unsubscribe(7))]);
    }

    #[test]
    fn tabs_route_diffs_by_listing() {
        let mut t = Tabs::new(Browser::new(b"/a".to_vec()));
        t.on_reply(snap("/a", 1, 0, &[("x", Kind::File)], &[0]));
        t.open();
        t.on_reply(snap("/a", 1, 0, &[("x", Kind::File)], &[0]));
        t.active_mut().parent();
        t.on_reply(snap("/", 2, 0, &[("a", Kind::Dir)], &[0]));
        t.on_reply(Reply::DirDiff {
            dir: 1,
            generation: 1,
            removed: vec![],
            added: vec![e("y", Kind::File)],
            changed: vec![],
            order: vec![0, 1],
            complete: true,
        });
        let lens: Vec<usize> = t.iter().map(Browser::len).collect();
        assert_eq!(lens, [2, 1]);
        t.cycle(1);
        assert_eq!(t.index(), 0);
        t.cycle(-1);
        assert_eq!(t.active().path, b"/");
    }

    #[test]
    fn fuzzy_is_an_ordered_subsequence() {
        assert!(fuzzy("", b"x"));
        assert!(fuzzy("crt", b"Cargo.toml") && fuzzy("CT", b"crates"));
        assert!(!fuzzy("tc", b"crates"));
        assert!(!fuzzy("aa", b"a"));
    }
}
