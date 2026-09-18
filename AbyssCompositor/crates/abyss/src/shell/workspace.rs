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
    /// Floating windows, bottom of the stack first. `note_focused` moves the
    /// focused window to the end; `shell::arrange_output` replays the order
    /// into `Space::raise_element`, so the last entry is the topmost window.
    pub floating: Vec<Floating>,
    /// Set by `toggle-layout`; otherwise the config decides.
    pub layout: Option<LayoutKind>,
    /// Windows adopted from a departed output, not yet placed in the layout.
    /// `shell::arrange` drains this.
    pub pending: Vec<Window>,
    /// Minimized windows, oldest first. Minimizing is per workspace, so a
    /// window sent away on workspace 3 comes back on workspace 3.
    pub minimized: Vec<Minimized>,
    /// The tiling area `shell::arrange_output` last laid this workspace out
    /// against. Floating rectangles are absolute and were computed against
    /// this area, so they are only re-clamped when it actually changes (a
    /// mode/scale change, a bar fold, an adopted window from a departed
    /// output). `None` means "never arranged yet", which is also not a
    /// change: a rectangle set before the first arrange was already computed
    /// against the current area. Clamping unconditionally would drag a
    /// window pinned by `place_at` off its exact position.
    pub(crate) last_area: Option<Rectangle<i32, Logical>>,
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
    ///
    /// Also moves `w` to the end of `floating`, which is the floating stacking
    /// order bottom-to-top: `shell::arrange_output` replays it into
    /// `Space::raise_element`, so without this the click's raise is undone by
    /// the next relayout and the window takes focus while staying behind
    /// (RAISE-01). A tiled window is not in the vec and nothing moves.
    pub(crate) fn note_focused(&mut self, w: &Window) {
        self.focus_history.note(w);
        self.raise_floating(w);
    }

    /// Move `w` to the top of the floating stack. No-op for a window that is
    /// not floating here.
    fn raise_floating(&mut self, w: &Window) {
        raise_to_top(&mut self.floating, |f| &f.window == w);
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

/// Move the first matching element to the end of `v`. Generic only so the
/// ordering can be unit-tested without a live `Window`; `Workspace` uses
/// exactly one instantiation.
fn raise_to_top<T>(v: &mut Vec<T>, matches: impl Fn(&T) -> bool) {
    if let Some(i) = v.iter().position(matches) {
        let e = v.remove(i);
        v.push(e);
    }
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

    #[test]
    fn focusing_a_floating_window_moves_it_to_the_top_of_the_stack() {
        // `Workspace::raise_floating` is the `&f.window == w` instantiation.
        // The vec is bottom-to-top: `shell::arrange_output` replays it into
        // `Space::raise_element`, so the last entry ends up frontmost (RAISE-01).
        let mut v = vec![1, 2, 3];
        raise_to_top(&mut v, |x| *x == 1);
        assert_eq!(v, vec![2, 3, 1]);

        // Already on top: order is unchanged, not rotated.
        raise_to_top(&mut v, |x| *x == 1);
        assert_eq!(v, vec![2, 3, 1]);

        // A window that is not floating here moves nothing.
        raise_to_top(&mut v, |x| *x == 9);
        assert_eq!(v, vec![2, 3, 1]);
    }
}
