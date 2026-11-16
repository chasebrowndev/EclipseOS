// SPDX-License-Identifier: AGPL-3.0-only
//! Session > windows > tabs > panes (T-13 §1, §4). Plain data, `u64` handles, one owner.
//!
//! The spec lists `Workspace` as a level; a workspace is a *name* carried by windows
//! (§4: "Windows carry a workspace name"), so there is no `Workspace` struct to keep in
//! sync. `switch_workspace` maps and unmaps windows (MUX-12).
//!
//! `P` is the per-pane payload (the `apoth-term` attachment, a test token, `()`); the model
//! never looks inside it. Removing a pane hands its payload back so the caller can drop the
//! PTY and terminal.

use std::collections::BTreeMap;

use crate::geom::{Dir, Rect, Size};
use crate::tab::{SplitSize, Tab, TabError, TabId};
use crate::tree::{PaneId, RATIO_ONE};

/// A window id.
pub type WindowId = u64;

/// What a pane runs: cwd, command argv, profile (T-13 §6). Command lines are data here and
/// are never logged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneSpec {
    /// Working directory.
    pub cwd: Option<String>,
    /// Command argv; empty means the profile's shell.
    pub argv: Vec<String>,
    /// Profile name.
    pub profile: Option<String>,
}

/// A pane's model-side record.
#[derive(Debug)]
pub struct PaneMeta<P> {
    /// What it runs.
    pub spec: PaneSpec,
    /// The attachment.
    pub payload: P,
}

/// A window: an ordered tab list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// Id.
    pub id: WindowId,
    /// Workspace name.
    pub workspace: String,
    /// Tabs left to right.
    pub tabs: Vec<TabId>,
    /// The shown tab.
    pub active: Option<TabId>,
    /// Tabs most recently shown first; a permutation of `tabs`.
    pub tab_mru: Vec<TabId>,
    /// Size in cells.
    pub size: Size,
    /// Whether the window is on the active workspace.
    pub mapped: bool,
}

/// Errors from session operations. Operations that return one change nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxError {
    /// Unknown pane.
    NoSuchPane,
    /// Unknown tab.
    NoSuchTab,
    /// Unknown window.
    NoSuchWindow,
    /// Index or relative target has no tab.
    NoSuchTarget,
    /// The tab (or window) is the last one; the operation would leave it empty.
    Last,
    /// A layout failed validation.
    BadLayout,
    /// A tab-level failure.
    Tab(TabError),
}

impl From<TabError> for MuxError {
    fn from(e: TabError) -> Self {
        MuxError::Tab(e)
    }
}

impl std::fmt::Display for MuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MuxError::NoSuchPane => f.write_str("no such pane"),
            MuxError::NoSuchTab => f.write_str("no such tab"),
            MuxError::NoSuchWindow => f.write_str("no such window"),
            MuxError::NoSuchTarget => f.write_str("no tab at that position"),
            MuxError::Last => f.write_str("it is the last one"),
            MuxError::BadLayout => f.write_str("layout failed validation"),
            MuxError::Tab(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for MuxError {}

/// Relative or absolute tab target (`tab.focus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabTarget {
    /// Zero-based index.
    Index(usize),
    /// Next tab, wrapping.
    Next,
    /// Previous tab, wrapping.
    Prev,
    /// The previously shown tab (MRU).
    Last,
}

/// What a close removed.
#[derive(Debug)]
pub struct CloseReport<P> {
    /// Removed panes with their payloads.
    pub removed: Vec<(PaneId, P)>,
    /// The tab went away because it became empty.
    pub tab_closed: bool,
    /// The window went away because its last tab went.
    pub window_closed: bool,
}

/// Result of `workspace.switch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceSwitch {
    /// Windows that were mapped and are now unmapped.
    pub unmapped: Vec<WindowId>,
    /// Windows that were unmapped and are now mapped.
    pub mapped: Vec<WindowId>,
}

/// The whole model.
#[derive(Debug)]
pub struct Session<P> {
    next_id: u64,
    pub(crate) panes: BTreeMap<PaneId, PaneMeta<P>>,
    pub(crate) tabs: BTreeMap<TabId, Tab>,
    pub(crate) windows: BTreeMap<WindowId, Window>,
    active_workspace: String,
}

impl<P> Session<P> {
    /// An empty session whose active workspace is `workspace`.
    pub fn new(workspace: &str) -> Self {
        Session {
            next_id: 0,
            panes: BTreeMap::new(),
            tabs: BTreeMap::new(),
            windows: BTreeMap::new(),
            active_workspace: workspace.to_owned(),
        }
    }

    pub(crate) fn alloc(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The active workspace name.
    pub fn active_workspace(&self) -> &str {
        &self.active_workspace
    }

    /// A pane record.
    pub fn pane(&self, id: PaneId) -> Option<&PaneMeta<P>> {
        self.panes.get(&id)
    }

    /// A pane record, mutable.
    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut PaneMeta<P>> {
        self.panes.get_mut(&id)
    }

    /// All pane ids, ascending.
    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.panes.keys().copied().collect()
    }

    /// All tab ids, ascending.
    pub fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.keys().copied().collect()
    }

    /// Number of panes.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// A tab.
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.get(&id)
    }

    /// A tab, mutable. Pane-level operations (focus, resize, zoom, presets, floats, stacks,
    /// swap, move, rotate) are methods of [`Tab`].
    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.get_mut(&id)
    }

    /// A window.
    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(&id)
    }

    /// All windows by id.
    pub fn windows(&self) -> impl Iterator<Item = &Window> {
        self.windows.values()
    }

    /// The tab holding a pane.
    pub fn tab_of(&self, pane: PaneId) -> Option<TabId> {
        self.tabs.values().find(|t| t.contains(pane)).map(Tab::id)
    }

    /// The window holding a tab.
    pub fn window_of(&self, tab: TabId) -> Option<WindowId> {
        self.windows
            .values()
            .find(|w| w.tabs.contains(&tab))
            .map(|w| w.id)
    }

    /// Workspace names in use, sorted, plus the active one.
    pub fn workspaces(&self) -> Vec<String> {
        let mut v: Vec<String> = self.windows.values().map(|w| w.workspace.clone()).collect();
        v.push(self.active_workspace.clone());
        v.sort();
        v.dedup();
        v
    }

    fn insert_pane(&mut self, spec: PaneSpec, payload: P) -> PaneId {
        let id = self.alloc();
        self.panes.insert(id, PaneMeta { spec, payload });
        id
    }

    fn make_tab(&mut self, size: Size, spec: PaneSpec, payload: P) -> (TabId, PaneId) {
        let pane = self.insert_pane(spec, payload);
        let tid = self.alloc();
        self.tabs.insert(tid, Tab::new(tid, "", size, pane));
        (tid, pane)
    }

    fn show_tab(w: &mut Window, tid: TabId) {
        w.active = Some(tid);
        w.tab_mru.retain(|&t| t != tid);
        w.tab_mru.insert(0, tid);
    }

    /// New window with one tab and one pane. `workspace` defaults to the active one.
    pub fn new_window(
        &mut self,
        size: Size,
        workspace: Option<&str>,
        spec: PaneSpec,
        payload: P,
    ) -> (WindowId, TabId, PaneId) {
        let ws = workspace.map_or_else(|| self.active_workspace.clone(), str::to_owned);
        let (tid, pane) = self.make_tab(size, spec, payload);
        let wid = self.alloc();
        let mapped = ws == self.active_workspace;
        self.windows.insert(
            wid,
            Window {
                id: wid,
                workspace: ws,
                tabs: vec![tid],
                active: Some(tid),
                tab_mru: vec![tid],
                size,
                mapped,
            },
        );
        (wid, tid, pane)
    }

    /// Resize a window and all its tabs.
    pub fn resize_window(&mut self, id: WindowId, size: Size) -> Result<(), MuxError> {
        let w = self.windows.get_mut(&id).ok_or(MuxError::NoSuchWindow)?;
        w.size = size;
        for t in &w.tabs {
            if let Some(tab) = self.tabs.get_mut(t) {
                tab.set_size(size);
            }
        }
        Ok(())
    }

    /// `tab.new`: a tab after the shown one, which it then shows.
    pub fn new_tab(
        &mut self,
        window: WindowId,
        spec: PaneSpec,
        payload: P,
    ) -> Result<(TabId, PaneId), MuxError> {
        let size = self
            .windows
            .get(&window)
            .ok_or(MuxError::NoSuchWindow)?
            .size;
        let (tid, pane) = self.make_tab(size, spec, payload);
        let w = self
            .windows
            .get_mut(&window)
            .ok_or(MuxError::NoSuchWindow)?;
        let at = w
            .active
            .and_then(|a| w.tabs.iter().position(|&t| t == a))
            .map_or(w.tabs.len(), |i| i + 1);
        w.tabs.insert(at, tid);
        Self::show_tab(w, tid);
        Ok((tid, pane))
    }

    /// Detach a tab entry from its window; the caller disposes of the tab's panes.
    fn drop_tab(&mut self, tid: TabId) -> Result<(Tab, bool), MuxError> {
        let wid = self.window_of(tid).ok_or(MuxError::NoSuchTab)?;
        let tab = self.tabs.remove(&tid).ok_or(MuxError::NoSuchTab)?;
        let Some(w) = self.windows.get_mut(&wid) else {
            return Ok((tab, false));
        };
        w.tabs.retain(|&t| t != tid);
        w.tab_mru.retain(|&t| t != tid);
        if w.active == Some(tid) {
            w.active = w.tab_mru.first().or_else(|| w.tabs.first()).copied();
        }
        let gone = w.tabs.is_empty();
        if gone {
            self.windows.remove(&wid);
        }
        Ok((tab, gone))
    }

    /// `tab.close`: drops every pane of the tab; the window goes with its last tab.
    pub fn close_tab(&mut self, tid: TabId) -> Result<CloseReport<P>, MuxError> {
        let (tab, window_closed) = self.drop_tab(tid)?;
        let removed = tab
            .panes()
            .into_iter()
            .filter_map(|p| self.panes.remove(&p).map(|m| (p, m.payload)))
            .collect();
        Ok(CloseReport {
            removed,
            tab_closed: true,
            window_closed,
        })
    }

    /// Close a window and everything in it.
    pub fn close_window(&mut self, id: WindowId) -> Result<Vec<(PaneId, P)>, MuxError> {
        let w = self.windows.remove(&id).ok_or(MuxError::NoSuchWindow)?;
        let mut out = Vec::new();
        for t in w.tabs {
            if let Some(tab) = self.tabs.remove(&t) {
                out.extend(
                    tab.panes()
                        .into_iter()
                        .filter_map(|p| self.panes.remove(&p).map(|m| (p, m.payload))),
                );
            }
        }
        Ok(out)
    }

    /// `tab.move`: place the tab at `index` (clamped) within its window.
    pub fn move_tab(&mut self, tid: TabId, index: usize) -> Result<(), MuxError> {
        let wid = self.window_of(tid).ok_or(MuxError::NoSuchTab)?;
        let w = self.windows.get_mut(&wid).ok_or(MuxError::NoSuchWindow)?;
        w.tabs.retain(|&t| t != tid);
        let at = index.min(w.tabs.len());
        w.tabs.insert(at, tid);
        Ok(())
    }

    /// `tab.rename`.
    pub fn rename_tab(&mut self, tid: TabId, name: &str) -> Result<(), MuxError> {
        self.tabs
            .get_mut(&tid)
            .ok_or(MuxError::NoSuchTab)?
            .set_name(name);
        Ok(())
    }

    /// `tab.focus`: index, relative (wrapping) or MRU. Returns the shown tab.
    pub fn focus_tab(&mut self, window: WindowId, target: TabTarget) -> Result<TabId, MuxError> {
        let w = self
            .windows
            .get_mut(&window)
            .ok_or(MuxError::NoSuchWindow)?;
        let n = w.tabs.len();
        let cur = w
            .active
            .and_then(|a| w.tabs.iter().position(|&t| t == a))
            .unwrap_or(0);
        let tid = match target {
            TabTarget::Index(i) => w.tabs.get(i).copied(),
            TabTarget::Next => w.tabs.get((cur + 1) % n.max(1)).copied(),
            TabTarget::Prev => w.tabs.get((cur + n.max(1) - 1) % n.max(1)).copied(),
            TabTarget::Last => w.tab_mru.get(1).copied(),
        }
        .ok_or(MuxError::NoSuchTarget)?;
        Self::show_tab(w, tid);
        Ok(tid)
    }

    /// `tab.detach`: move a tab into a new window on the same workspace.
    pub fn detach_tab_to_window(&mut self, tid: TabId) -> Result<WindowId, MuxError> {
        let src = self.window_of(tid).ok_or(MuxError::NoSuchTab)?;
        let w = self.windows.get(&src).ok_or(MuxError::NoSuchWindow)?;
        if w.tabs.len() < 2 {
            return Err(MuxError::Last);
        }
        let (ws, size, mapped) = (w.workspace.clone(), w.size, w.mapped);
        let (tab, _) = self.drop_tab(tid)?;
        self.tabs.insert(tid, tab);
        if let Some(t) = self.tabs.get_mut(&tid) {
            t.set_size(size);
        }
        Ok(self.new_window_with_tab(tid, ws, size, mapped))
    }

    fn new_window_with_tab(
        &mut self,
        tid: TabId,
        ws: String,
        size: Size,
        mapped: bool,
    ) -> WindowId {
        let wid = self.alloc();
        self.windows.insert(
            wid,
            Window {
                id: wid,
                workspace: ws,
                tabs: vec![tid],
                active: Some(tid),
                tab_mru: vec![tid],
                size,
                mapped,
            },
        );
        wid
    }

    /// `pane.split`: returns the new pane, which takes focus.
    pub fn split_pane(
        &mut self,
        target: PaneId,
        dir: Dir,
        size: SplitSize,
        spec: PaneSpec,
        payload: P,
    ) -> Result<PaneId, MuxError> {
        let tid = self.tab_of(target).ok_or(MuxError::NoSuchPane)?;
        let id = self.alloc();
        self.tabs
            .get_mut(&tid)
            .ok_or(MuxError::NoSuchTab)?
            .split(target, id, dir, size)?;
        self.panes.insert(id, PaneMeta { spec, payload });
        Ok(id)
    }

    /// A new floating pane in the tab of `target` at `rect` (see [`Tab::place_float`]); it
    /// takes focus (MUX-18).
    pub fn float_pane(
        &mut self,
        target: PaneId,
        rect: Rect,
        spec: PaneSpec,
        payload: P,
    ) -> Result<PaneId, MuxError> {
        let tid = self.tab_of(target).ok_or(MuxError::NoSuchPane)?;
        let id = self.alloc();
        self.tabs
            .get_mut(&tid)
            .ok_or(MuxError::NoSuchTab)?
            .add_float(id, rect)?;
        self.panes.insert(id, PaneMeta { spec, payload });
        Ok(id)
    }

    /// Make room for a new pane by adding it to the stack holding `onto` (a plain pane
    /// becomes a stack). The new pane is expanded and focused.
    pub fn stack_add_pane(
        &mut self,
        onto: PaneId,
        spec: PaneSpec,
        payload: P,
    ) -> Result<PaneId, MuxError> {
        let tid = self.tab_of(onto).ok_or(MuxError::NoSuchPane)?;
        let id = self.alloc();
        self.tabs
            .get_mut(&tid)
            .ok_or(MuxError::NoSuchTab)?
            .stack_add(onto, id)?;
        self.panes.insert(id, PaneMeta { spec, payload });
        Ok(id)
    }

    /// `pane.close`. Closing the last pane of a tab closes the tab, and the last tab of a
    /// window closes the window.
    pub fn close_pane(&mut self, pane: PaneId) -> Result<CloseReport<P>, MuxError> {
        let tid = self.tab_of(pane).ok_or(MuxError::NoSuchPane)?;
        let empty = self
            .tabs
            .get_mut(&tid)
            .ok_or(MuxError::NoSuchTab)?
            .close(pane)?;
        let meta = self.panes.remove(&pane);
        let mut report = if empty {
            self.close_tab(tid)?
        } else {
            CloseReport {
                removed: Vec::new(),
                tab_closed: false,
                window_closed: false,
            }
        };
        if let Some(m) = meta {
            report.removed.insert(0, (pane, m.payload));
        }
        Ok(report)
    }

    /// `pane.break-to-tab`: the pane becomes its own tab after its old tab.
    pub fn break_to_tab(&mut self, pane: PaneId) -> Result<TabId, MuxError> {
        let (tid, wid) = self.break_out(pane)?;
        let size = self.windows.get(&wid).ok_or(MuxError::NoSuchWindow)?.size;
        let ntid = self.alloc();
        let w = self.windows.get_mut(&wid).ok_or(MuxError::NoSuchWindow)?;
        let at = w
            .tabs
            .iter()
            .position(|&t| t == tid)
            .map_or(w.tabs.len(), |i| i + 1);
        w.tabs.insert(at, ntid);
        Self::show_tab(w, ntid);
        self.tabs.insert(ntid, Tab::new(ntid, "", size, pane));
        Ok(ntid)
    }

    /// `pane.break-to-window`: the pane becomes a new window on the same workspace.
    pub fn break_to_window(&mut self, pane: PaneId) -> Result<WindowId, MuxError> {
        let (_, wid) = self.break_out(pane)?;
        let w = self.windows.get(&wid).ok_or(MuxError::NoSuchWindow)?;
        let (ws, size, mapped) = (w.workspace.clone(), w.size, w.mapped);
        let ntid = self.alloc();
        self.tabs.insert(ntid, Tab::new(ntid, "", size, pane));
        Ok(self.new_window_with_tab(ntid, ws, size, mapped))
    }

    /// Detach a pane from its tab, refusing when it is the tab's only pane.
    fn break_out(&mut self, pane: PaneId) -> Result<(TabId, WindowId), MuxError> {
        let tid = self.tab_of(pane).ok_or(MuxError::NoSuchPane)?;
        let wid = self.window_of(tid).ok_or(MuxError::NoSuchWindow)?;
        let tab = self.tabs.get_mut(&tid).ok_or(MuxError::NoSuchTab)?;
        if tab.panes().len() < 2 {
            return Err(MuxError::Last);
        }
        tab.close(pane)?;
        Ok((tid, wid))
    }

    /// `pane.join`: move `pane` next to `target` (another tab's pane, or the same tab's).
    /// Nothing changes on failure. A source tab left empty is removed.
    pub fn join(&mut self, pane: PaneId, target: PaneId, dir: Dir) -> Result<(), MuxError> {
        let src = self.tab_of(pane).ok_or(MuxError::NoSuchPane)?;
        let dst = self.tab_of(target).ok_or(MuxError::NoSuchPane)?;
        if src == dst {
            self.tabs
                .get_mut(&src)
                .ok_or(MuxError::NoSuchTab)?
                .move_pane(pane, target, dir)?;
            return Ok(());
        }
        let mut s = self.tabs.get(&src).ok_or(MuxError::NoSuchTab)?.clone();
        let mut d = self.tabs.get(&dst).ok_or(MuxError::NoSuchTab)?.clone();
        let emptied = s.close(pane)?;
        d.split(target, pane, dir, SplitSize::Fraction(RATIO_ONE / 2))?;
        self.tabs.insert(dst, d);
        if emptied {
            self.drop_tab(src)?;
        } else {
            self.tabs.insert(src, s);
        }
        Ok(())
    }

    /// `workspace.switch`: map the windows of `name`, unmap all others.
    pub fn switch_workspace(&mut self, name: &str) -> WorkspaceSwitch {
        self.active_workspace = name.to_owned();
        let mut sw = WorkspaceSwitch::default();
        for w in self.windows.values_mut() {
            let want = w.workspace == name;
            if want && !w.mapped {
                sw.mapped.push(w.id);
            } else if !want && w.mapped {
                sw.unmapped.push(w.id);
            }
            w.mapped = want;
        }
        sw
    }

    /// Whole-model invariants for tests: every pane record is in exactly one tab, every
    /// tab in exactly one window, window tab lists and MRU lists consistent, each tab valid.
    pub fn check_invariants(&self) -> Result<(), String> {
        let mut seen: Vec<PaneId> = Vec::new();
        for t in self.tabs.values() {
            t.check_invariants()?;
            seen.extend(t.panes());
        }
        seen.sort_unstable();
        let keys: Vec<PaneId> = self.panes.keys().copied().collect();
        if seen != keys {
            return Err("pane records and tab panes differ".into());
        }
        let mut in_windows: Vec<TabId> = Vec::new();
        for w in self.windows.values() {
            if w.tabs.is_empty() {
                return Err("empty window".into());
            }
            let mut a = w.tabs.clone();
            let mut b = w.tab_mru.clone();
            a.sort_unstable();
            b.sort_unstable();
            if a != b {
                return Err("tab_mru is not a permutation of tabs".into());
            }
            if w.active != w.tab_mru.first().copied() {
                return Err("active tab is not the mru head".into());
            }
            for t in &w.tabs {
                let tab = self.tabs.get(t).ok_or("window lists a missing tab")?;
                if tab.size() != w.size {
                    return Err("tab size differs from window size".into());
                }
            }
            in_windows.extend(&w.tabs);
        }
        in_windows.sort_unstable();
        let tab_keys: Vec<TabId> = self.tabs.keys().copied().collect();
        if in_windows != tab_keys {
            return Err("tabs and window tab lists differ".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SZ: Size = Size { cols: 80, rows: 24 };

    fn sess() -> (Session<u32>, WindowId, TabId, PaneId) {
        let mut s = Session::new("main");
        let (w, t, p) = s.new_window(SZ, None, PaneSpec::default(), 7);
        (s, w, t, p)
    }

    #[test]
    fn tabs_lifecycle() {
        let (mut s, w, t1, _) = sess();
        let (t2, _) = s.new_tab(w, PaneSpec::default(), 1).unwrap();
        let (t3, _) = s.new_tab(w, PaneSpec::default(), 2).unwrap();
        assert_eq!(s.window(w).unwrap().tabs, [t1, t2, t3]);
        assert_eq!(s.window(w).unwrap().active, Some(t3));
        assert_eq!(s.focus_tab(w, TabTarget::Last), Ok(t2));
        assert_eq!(s.focus_tab(w, TabTarget::Next), Ok(t3));
        assert_eq!(s.focus_tab(w, TabTarget::Next), Ok(t1));
        assert_eq!(s.focus_tab(w, TabTarget::Prev), Ok(t3));
        assert_eq!(s.focus_tab(w, TabTarget::Index(1)), Ok(t2));
        assert_eq!(
            s.focus_tab(w, TabTarget::Index(9)),
            Err(MuxError::NoSuchTarget)
        );
        s.move_tab(t3, 0).unwrap();
        assert_eq!(s.window(w).unwrap().tabs, [t3, t1, t2]);
        s.rename_tab(t1, "x").unwrap();
        assert_eq!(s.tab(t1).unwrap().name(), "x");
        s.check_invariants().unwrap();
        let rep = s.close_tab(t2).unwrap();
        assert_eq!(rep.removed.len(), 1);
        assert!(!rep.window_closed);
        assert!(s.window(w).unwrap().active.is_some());
        s.check_invariants().unwrap();
    }

    #[test]
    fn last_pane_closes_tab_then_window() {
        let (mut s, w, t, p) = sess();
        let q = s
            .split_pane(p, Dir::Right, SplitSize::HALF, PaneSpec::default(), 8)
            .unwrap();
        let r = s.close_pane(q).unwrap();
        assert_eq!(r.removed, vec![(q, 8)]);
        assert!(!r.tab_closed);
        let r = s.close_pane(p).unwrap();
        assert!(r.tab_closed && r.window_closed);
        assert_eq!(r.removed, vec![(p, 7)]);
        assert!(s.window(w).is_none() && s.tab(t).is_none());
        assert_eq!(s.pane_count(), 0);
        s.check_invariants().unwrap();
        assert_eq!(s.close_pane(p).unwrap_err(), MuxError::NoSuchPane);
    }

    #[test]
    fn detach_break_join() {
        let (mut s, w, t1, p1) = sess();
        let (t2, p2) = s.new_tab(w, PaneSpec::default(), 2).unwrap();
        let p3 = s
            .split_pane(p2, Dir::Down, SplitSize::HALF, PaneSpec::default(), 3)
            .unwrap();
        let w2 = s.detach_tab_to_window(t2).unwrap();
        assert_eq!(s.window_of(t2), Some(w2));
        assert_eq!(s.window(w).unwrap().tabs, [t1]);
        assert_eq!(s.detach_tab_to_window(t1), Err(MuxError::Last));
        s.check_invariants().unwrap();
        let t3 = s.break_to_tab(p3).unwrap();
        assert_eq!(s.window(w2).unwrap().tabs, [t2, t3]);
        assert_eq!(s.break_to_tab(p3), Err(MuxError::Last));
        s.join(p3, p1, Dir::Right).unwrap();
        assert!(s.tab(t3).is_none());
        assert_eq!(s.tab_of(p3), Some(t1));
        s.check_invariants().unwrap();
        let w3 = s.break_to_window(p3).unwrap();
        assert_ne!(w3, w);
        s.check_invariants().unwrap();
        // join within a tab
        let p4 = s
            .split_pane(p1, Dir::Right, SplitSize::HALF, PaneSpec::default(), 4)
            .unwrap();
        s.join(p4, p1, Dir::Down).unwrap();
        s.check_invariants().unwrap();
    }

    #[test]
    fn join_failure_changes_nothing() {
        let (mut s, w, t1, p1) = sess();
        s.resize_window(w, Size { cols: 3, rows: 24 }).unwrap();
        let (_, p2) = s.new_tab(w, PaneSpec::default(), 2).unwrap();
        assert_eq!(
            s.join(p2, p1, Dir::Right),
            Err(MuxError::Tab(TabError::TooSmall))
        );
        assert!(s.tab_of(p2).is_some());
        assert_eq!(s.tab(t1).unwrap().panes(), [p1]);
        s.check_invariants().unwrap();
    }

    #[test]
    fn workspace_switch_maps_windows() {
        let mut s: Session<()> = Session::new("a");
        let (w1, ..) = s.new_window(SZ, None, PaneSpec::default(), ());
        let (w2, ..) = s.new_window(SZ, Some("b"), PaneSpec::default(), ());
        assert!(s.window(w1).unwrap().mapped && !s.window(w2).unwrap().mapped);
        let sw = s.switch_workspace("b");
        assert_eq!((sw.unmapped, sw.mapped), (vec![w1], vec![w2]));
        assert_eq!(s.switch_workspace("b"), WorkspaceSwitch::default());
        assert_eq!(s.workspaces(), ["a", "b"]);
    }

    #[test]
    fn resize_window_resizes_tabs() {
        let (mut s, w, t, _) = sess();
        s.resize_window(
            w,
            Size {
                cols: 100,
                rows: 30,
            },
        )
        .unwrap();
        assert_eq!(
            s.tab(t).unwrap().size(),
            Size {
                cols: 100,
                rows: 30
            }
        );
        assert_eq!(s.resize_window(999, SZ), Err(MuxError::NoSuchWindow));
    }

    #[test]
    fn close_window_returns_payloads() {
        let (mut s, w, _, p) = sess();
        s.split_pane(p, Dir::Right, SplitSize::HALF, PaneSpec::default(), 8)
            .unwrap();
        let mut got: Vec<u32> = s
            .close_window(w)
            .unwrap()
            .into_iter()
            .map(|(_, v)| v)
            .collect();
        got.sort();
        assert_eq!(got, [7, 8]);
        assert_eq!(s.pane_count(), 0);
    }
}
