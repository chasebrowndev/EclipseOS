// SPDX-License-Identifier: AGPL-3.0-only
//! Workspaces (COMP-05 §4). Ten numbered workspaces per output; the set is
//! owned by the output's entry in `outputs::Outputs`, so unplugging a monitor
//! takes its workspaces with it (COMP-03 §5).

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle},
};

use crate::config::LayoutKind;

use super::layout::Tree;

pub const COUNT: usize = 10;

#[derive(Debug)]
pub struct Floating {
    pub window: Window,
    pub rect: Rectangle<i32, Logical>,
}

/// A window the human sent away (COMP-05 §4 `minimized`). It keeps its place
/// in the workspace but leaves the layout entirely: not tiled, not floating,
/// not mapped into the space. `was_floating` is the rectangle it owned before,
/// so restoring puts it back where it was rather than dropping it into tiling.
#[derive(Debug)]
pub struct Minimized {
    pub window: Window,
    pub was_floating: Option<Rectangle<i32, Logical>>,
}

/// Most-recently-focused-first history. Generic only so the ordering rules can
/// be unit-tested without standing up a real `Window` (which needs a live
/// toplevel surface); `Workspace` uses exactly one instantiation.
#[derive(Debug)]
pub struct FocusHistory<T>(Vec<T>);

impl<T> Default for FocusHistory<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: PartialEq + Clone> FocusHistory<T> {
    /// Move `v` to the head, without duplicating it.
    fn note(&mut self, v: &T) {
        self.0.retain(|x| x != v);
        self.0.insert(0, v.clone());
    }

    /// The most recent entry still considered focusable. A read: it does not
    /// prune, so an entry that is temporarily not focusable (minimized) keeps
    /// its place and comes back when it is focusable again.
    fn head_where(&self, focusable: impl Fn(&T) -> bool) -> Option<T> {
        self.0.iter().find(|x| focusable(x)).cloned()
    }

    fn remove(&mut self, v: &T) {
        self.0.retain(|x| x != v);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Debug, Default)]
pub struct Workspace {
    pub tiled: Tree,
    pub floating: Vec<Floating>,
    /// Set by `toggle-layout`; otherwise the config decides.
    pub layout: Option<LayoutKind>,
    /// Windows adopted from a departed output, not yet placed in the layout.
    /// `shell::arrange` drains this.
    pub pending: Vec<Window>,
    /// Minimized windows, oldest first. Minimizing is per workspace, so a
    /// window sent away on workspace 3 comes back on workspace 3.
    pub minimized: Vec<Minimized>,
    /// Per-workspace focus history, most recent first (COMP-05 §5, ADR 0042).
    /// Written only by `note_focused`, whose single caller is
    /// `shell::focus::focus_window` — the one place focus actually moves.
    focus_history: FocusHistory<Window>,
}

impl Workspace {
    /// Every window on this workspace, tiled first then floating (stacking order).
    pub fn windows(&self) -> Vec<Window> {
        let mut v = self.tiled.windows();
        v.extend(self.floating.iter().map(|f| f.window.clone()));
        v
    }

    /// Every window this workspace owns, minimized ones included. `windows`
    /// answers "what is on screen"; this answers "what is here".
    pub fn all_windows(&self) -> Vec<Window> {
        let mut v = self.windows();
        v.extend(self.minimized.iter().map(|m| m.window.clone()));
        v
    }

    pub fn is_minimized(&self, w: &Window) -> bool {
        self.minimized.iter().any(|m| &m.window == w)
    }

    /// Record `w` as the most recently focused window here.
    pub(crate) fn note_focused(&mut self, w: &Window) {
        self.focus_history.note(w);
    }

    /// Is `w` on screen here — tiled or floating, but not minimized? Allocation
    /// free on purpose: `focus_head` runs on every pointer motion, and the
    /// input-delivery path does not allocate.
    pub(crate) fn holds_live(&self, w: &Window) -> bool {
        self.tiled.contains(w) || self.floating.iter().any(|f| &f.window == w)
    }

    /// Is `w` owned by this workspace at all — minimized and pending included?
    pub(crate) fn holds(&self, w: &Window) -> bool {
        self.holds_live(w) || self.minimized.iter().any(|m| &m.window == w) || self.pending.contains(w)
    }

    /// The most recent still-focusable window: present on this workspace and
    /// not minimized.
    pub(crate) fn focus_head(&self) -> Option<Window> {
        self.focus_history.head_where(|w| self.holds_live(w))
    }

    pub fn remove(&mut self, w: &Window) -> bool {
        self.focus_history.remove(w);
        if let Some(i) = self.pending.iter().position(|p| p == w) {
            self.pending.remove(i);
            return true;
        }
        if let Some(i) = self.minimized.iter().position(|m| &m.window == w) {
            self.minimized.remove(i);
            return true;
        }
        let was_floating = self.floating.iter().position(|f| &f.window == w);
        if let Some(i) = was_floating {
            self.floating.remove(i);
            return true;
        }
        self.tiled.remove(w)
    }
}

pub fn new_set() -> Vec<Workspace> {
    (0..COUNT).map(|_| Workspace::default()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Window` needs a live toplevel surface, so the history rules are tested
    /// on the generic helper `Workspace` stores. `Workspace::focus_head` is the
    /// `focusable = windows().contains(..)` instantiation of `head_where`, and
    /// `Workspace::remove` is `FocusHistory::remove`.
    fn hist(entries: &[u32]) -> FocusHistory<u32> {
        let mut h = FocusHistory::default();
        for e in entries {
            h.note(e);
        }
        h
    }

    const ALL: fn(&u32) -> bool = |_| true;

    #[test]
    fn most_recent_is_head() {
        let h = hist(&[1, 2, 3]);
        assert_eq!(h.head_where(ALL), Some(3));
    }

    #[test]
    fn refocus_moves_to_head_without_duplicating() {
        let h = hist(&[1, 2, 3, 1]);
        assert_eq!(h.head_where(ALL), Some(1));
        assert_eq!(h.len(), 3);
    }

    #[test]
    fn removing_the_head_falls_through() {
        let mut h = hist(&[1, 2, 3]);
        h.remove(&3);
        assert_eq!(h.head_where(ALL), Some(2));
        assert_eq!(h.len(), 2);
        h.remove(&2);
        h.remove(&1);
        assert_eq!(h.head_where(ALL), None);
    }

    #[test]
    fn unfocusable_head_is_skipped_and_returns_when_restored() {
        let h = hist(&[1, 2, 3]);
        // 3 is minimized: not in `windows()`, so not focusable.
        assert_eq!(h.head_where(|x| *x != 3), Some(2));
        // Restored, without having been re-noted: it keeps its place.
        assert_eq!(h.head_where(ALL), Some(3));
    }

    #[test]
    fn remove_purges_the_entry() {
        let mut h = hist(&[1, 2]);
        h.remove(&1);
        h.remove(&1);
        assert_eq!(h.len(), 1);
        assert_eq!(h.head_where(ALL), Some(2));
    }

    #[test]
    fn empty_workspace_has_no_focus_head() {
        assert_eq!(Workspace::default().focus_head(), None);
    }
}
