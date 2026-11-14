// SPDX-License-Identifier: AGPL-3.0-only
//! A tab: the tiled split tree, the floating layer, the focus model and zoom (T-13 §3, §4).
//!
//! Choices the spec leaves open, pinned here:
//! - Focus is per tab. `focus.last` is the previous entry of the tab's MRU list. A closed
//!   pane leaves the list; closing the focused pane focuses the MRU head.
//! - Directional focus picks the adjacent pane with the largest shared edge, ties broken
//!   by MRU rank, then lowest id. [`Tab::neighbours`] exposes the whole candidate set,
//!   which is symmetric (`B` in `neighbours(A, d)` iff `A` in `neighbours(B, d.opposite())`).
//! - Zoom shows only the zoomed pane. Focusing another pane, any structural change, or
//!   closing the zoomed pane ends zoom. Floats stay available while zoomed.
//! - `swap` exchanges positions; focus stays with the pane id.
//! - Structural operations that would make the tree need more cells than the tab has (and
//!   more than it needed before) fail with [`TabError::TooSmall`] and change nothing.

use crate::geom::{Dir, MIN_COLS, MIN_ROWS, Orient, Rect, Size};
use crate::preset::{self, Preset};
use crate::tree::{Node, PaneId, Placed, RATIO_ONE, StackStyle, ratio_of};

/// A tab id.
pub type TabId = u64;

/// Errors from tab operations. None of them change the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabError {
    /// The pane is not in this tab.
    NoSuchPane,
    /// The pane id is already used in this tab (or the two ids are the same).
    SamePane,
    /// The operation needs a tiled pane and this one floats.
    Floating,
    /// The operation needs a floating pane and this one is tiled.
    NotFloating,
    /// The operation needs the pane to be in a stack.
    NotStacked,
    /// The tab has no room for the result.
    TooSmall,
    /// There is nothing to operate on.
    Empty,
}

impl std::fmt::Display for TabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TabError::NoSuchPane => "no such pane in tab",
            TabError::SamePane => "pane already present",
            TabError::Floating => "pane is floating",
            TabError::NotFloating => "pane is not floating",
            TabError::NotStacked => "pane is not in a stack",
            TabError::TooSmall => "not enough room",
            TabError::Empty => "nothing to do",
        })
    }
}

impl std::error::Error for TabError {}

/// Size requested for the new pane of a split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitSize {
    /// Fraction of the target, parts per [`RATIO_ONE`].
    Fraction(u32),
    /// Fixed cell count along the split axis.
    Cells(u32),
}

impl SplitSize {
    /// Half.
    pub const HALF: SplitSize = SplitSize::Fraction(RATIO_ONE / 2);
}

/// Target of a relative focus request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusMove {
    /// Spatial neighbour.
    Dir(Dir),
    /// Next in tree order, wrapping.
    Next,
    /// Previous in tree order, wrapping.
    Prev,
    /// Previously focused pane.
    Last,
}

/// A floating pane (MUX-18). The model holds data only; placement search is the caller's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float {
    /// The pane.
    pub pane: PaneId,
    /// Cells it covers, in tab coordinates.
    pub rect: Rect,
    /// Shown or hidden.
    pub visible: bool,
    /// Pinned floats render above unpinned ones.
    pub pinned: bool,
}

/// A tab.
#[derive(Clone, Debug)]
pub struct Tab {
    pub(crate) id: TabId,
    pub(crate) name: String,
    pub(crate) size: Size,
    pub(crate) root: Option<Node>,
    pub(crate) floats: Vec<Float>,
    pub(crate) focused: Option<PaneId>,
    pub(crate) mru: Vec<PaneId>,
    pub(crate) zoomed: Option<PaneId>,
    pub(crate) preset: Option<Preset>,
    pub(crate) raise_on_focus: bool,
}

impl Tab {
    /// A tab holding one tiled pane.
    pub fn new(id: TabId, name: impl Into<String>, size: Size, first: PaneId) -> Self {
        Tab {
            id,
            name: name.into(),
            size,
            root: Some(Node::Pane(first)),
            floats: Vec::new(),
            focused: Some(first),
            mru: vec![first],
            zoomed: None,
            preset: None,
            raise_on_focus: true,
        }
    }

    /// Tab id.
    pub fn id(&self) -> TabId {
        self.id
    }
    /// Tab name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Rename.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }
    /// Size in cells.
    pub fn size(&self) -> Size {
        self.size
    }
    /// Resize the tab. Never fails; the tree degrades per the tree sizing rule. Floats are
    /// moved (then shrunk) to stay inside it.
    pub fn set_size(&mut self, size: Size) {
        self.size = size;
        for f in &mut self.floats {
            f.rect = clamp_rect(f.rect, size);
        }
    }
    /// The tiled tree.
    pub fn root(&self) -> Option<&Node> {
        self.root.as_ref()
    }
    /// The focused pane.
    pub fn focused(&self) -> Option<PaneId> {
        self.focused
    }
    /// Panes most recently focused first.
    pub fn mru(&self) -> &[PaneId] {
        &self.mru
    }
    /// The zoomed pane.
    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }
    /// The preset last applied and not since disturbed.
    pub fn preset(&self) -> Option<Preset> {
        self.preset
    }
    /// Floats, back to front before pinning is considered.
    pub fn floats(&self) -> &[Float] {
        &self.floats
    }
    /// Raise a float when it takes focus (default true).
    pub fn set_raise_on_focus(&mut self, on: bool) {
        self.raise_on_focus = on;
    }

    /// Floats in render order: pinned above unpinned, otherwise list order.
    pub fn floats_render_order(&self) -> Vec<Float> {
        let mut v: Vec<Float> = self.floats.iter().filter(|f| !f.pinned).copied().collect();
        v.extend(self.floats.iter().filter(|f| f.pinned).copied());
        v
    }

    /// Tiled panes in tree order.
    pub fn tiled_order(&self) -> Vec<PaneId> {
        let mut v = Vec::new();
        if let Some(r) = &self.root {
            r.leaves(&mut v);
        }
        v
    }

    /// Every pane: tiled in tree order, then floats in list order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut v = self.tiled_order();
        v.extend(self.floats.iter().map(|f| f.pane));
        v
    }

    /// True when the pane is in this tab.
    pub fn contains(&self, pane: PaneId) -> bool {
        self.is_tiled(pane) || self.is_float(pane)
    }

    /// True when the pane is tiled.
    pub fn is_tiled(&self, pane: PaneId) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(pane))
    }

    /// True when the pane floats.
    pub fn is_float(&self, pane: PaneId) -> bool {
        self.floats.iter().any(|f| f.pane == pane)
    }

    /// Resolve the tiled layer (zoom applied). Floats are separate, see [`Tab::floats`].
    pub fn layout(&self) -> Vec<Placed> {
        if let Some(z) = self.zoomed {
            return vec![Placed {
                pane: z,
                rect: Rect::from_size(self.size),
                header: false,
            }];
        }
        self.tiled_layout()
    }

    fn tiled_layout(&self) -> Vec<Placed> {
        let mut v = Vec::new();
        if let Some(r) = &self.root {
            r.layout(Rect::from_size(self.size), &mut v);
        }
        v
    }

    fn touch(&mut self, p: PaneId) {
        self.mru.retain(|&x| x != p);
        self.mru.insert(0, p);
        self.focused = Some(p);
    }

    fn require_root(&self, pane: PaneId) -> Result<&Node, TabError> {
        match &self.root {
            Some(r) if r.contains(pane) => Ok(r),
            _ if self.is_float(pane) => Err(TabError::Floating),
            _ => Err(TabError::NoSuchPane),
        }
    }

    /// Install a new tree if it fits (or is no bigger than the old one).
    fn try_set(&mut self, new: Node) -> Result<(), TabError> {
        let old = self.root.as_ref().map(Node::min_size).unwrap_or_default();
        let m = new.min_size();
        if m.cols > self.size.cols.max(old.cols) || m.rows > self.size.rows.max(old.rows) {
            return Err(TabError::TooSmall);
        }
        self.root = Some(new);
        self.zoomed = None;
        self.preset = None;
        Ok(())
    }

    /// `pane.split`: split `target`, putting `new` on the `dir` side. The new pane takes
    /// focus.
    pub fn split(
        &mut self,
        target: PaneId,
        new: PaneId,
        dir: Dir,
        size: SplitSize,
    ) -> Result<(), TabError> {
        if self.contains(new) {
            return Err(TabError::SamePane);
        }
        let root = self.require_root(target)?;
        let leaf = root
            .leaf_rect(Rect::from_size(self.size), target)
            .ok_or(TabError::NoSuchPane)?;
        let total = match dir.axis() {
            Orient::Horizontal => leaf.w,
            Orient::Vertical => leaf.h,
        };
        let frac = match size {
            SplitSize::Fraction(f) => f.clamp(1, RATIO_ONE - 1),
            SplitSize::Cells(n) => ratio_of(n.min(total), total),
        };
        let mut nr = root.clone();
        nr.split_at(target, Node::Pane(new), dir.axis(), frac, !dir.is_forward());
        self.try_set(nr)?;
        self.touch(new);
        Ok(())
    }

    /// Put a pane into an empty or non-empty tab without a chosen target: the first tiled
    /// pane is split to the right at half, or it becomes the root.
    pub fn adopt(&mut self, pane: PaneId) -> Result<(), TabError> {
        if self.contains(pane) {
            return Err(TabError::SamePane);
        }
        match self.tiled_order().first().copied() {
            None => {
                self.root = Some(Node::Pane(pane));
                self.preset = None;
                self.touch(pane);
                Ok(())
            }
            Some(t) => self.split(t, pane, Dir::Right, SplitSize::HALF),
        }
    }

    /// `pane.close` (also detaches a pane that moves elsewhere). Returns true when the tab
    /// has no panes left.
    pub fn close(&mut self, pane: PaneId) -> Result<bool, TabError> {
        if let Some(i) = self.floats.iter().position(|f| f.pane == pane) {
            self.floats.remove(i);
        } else if self.is_tiled(pane) {
            self.root = self.root.take().and_then(|r| r.remove(pane));
            self.preset = None;
        } else {
            return Err(TabError::NoSuchPane);
        }
        let was_focused = self.focused == Some(pane);
        self.mru.retain(|&p| p != pane);
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        if was_focused {
            self.focused = None;
            if let Some(n) = self.mru.first().copied() {
                // cannot fail: mru holds only panes of this tab
                let _ = self.focus(n);
            }
        }
        Ok(self.mru.is_empty())
    }

    /// Focus `pane`: expands it in its stack, raises a float (when configured), ends zoom
    /// of another pane, and moves it to the MRU head.
    pub fn focus(&mut self, pane: PaneId) -> Result<(), TabError> {
        if !self.contains(pane) {
            return Err(TabError::NoSuchPane);
        }
        if self.zoomed.is_some_and(|z| z != pane) {
            self.zoomed = None;
        }
        if self.is_tiled(pane) {
            if let Some(r) = &mut self.root {
                r.activate(pane);
            }
        } else if let Some(i) = self.floats.iter().position(|f| f.pane == pane) {
            let mut f = self.floats.remove(i);
            f.visible = true;
            if self.raise_on_focus {
                self.floats.push(f);
            } else {
                self.floats.insert(i, f);
            }
        }
        self.touch(pane);
        Ok(())
    }

    /// Tiled panes adjacent to `pane` on its `dir` edge, best first (largest shared edge,
    /// then MRU rank, then id). Stack members that are not expanded are not candidates.
    pub fn neighbours(&self, pane: PaneId, dir: Dir) -> Vec<PaneId> {
        let pl = self.tiled_layout();
        let Some(me) = pl
            .iter()
            .find(|p| p.pane == pane && !p.header)
            .map(|p| p.rect)
        else {
            return Vec::new();
        };
        if me.is_empty() {
            return Vec::new();
        }
        let mut c: Vec<(u32, usize, PaneId)> = Vec::new();
        for p in pl
            .iter()
            .filter(|p| !p.header && p.pane != pane && !p.rect.is_empty())
        {
            let r = p.rect;
            let (touch, a0, a1, b0, b1) = match dir {
                Dir::Right => (r.x == me.right(), me.y, me.bottom(), r.y, r.bottom()),
                Dir::Left => (r.right() == me.x, me.y, me.bottom(), r.y, r.bottom()),
                Dir::Down => (r.y == me.bottom(), me.x, me.right(), r.x, r.right()),
                Dir::Up => (r.bottom() == me.y, me.x, me.right(), r.x, r.right()),
            };
            if !touch {
                continue;
            }
            let ov = a1.min(b1).saturating_sub(a0.max(b0));
            if ov > 0 {
                let rank = self
                    .mru
                    .iter()
                    .position(|&m| m == p.pane)
                    .unwrap_or(usize::MAX);
                c.push((ov, rank, p.pane));
            }
        }
        c.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        c.into_iter().map(|(_, _, p)| p).collect()
    }

    /// `pane.focus`. Returns the newly focused pane.
    pub fn focus_move(&mut self, mv: FocusMove) -> Result<PaneId, TabError> {
        let cur = self.focused.ok_or(TabError::Empty)?;
        let target = match mv {
            FocusMove::Dir(d) => self
                .neighbours(cur, d)
                .first()
                .copied()
                .ok_or(TabError::NoSuchPane)?,
            FocusMove::Last => self.mru.get(1).copied().ok_or(TabError::Empty)?,
            FocusMove::Next | FocusMove::Prev => {
                let mut order = self.tiled_order();
                order.extend(self.floats.iter().filter(|f| f.visible).map(|f| f.pane));
                if order.is_empty() {
                    return Err(TabError::Empty);
                }
                let i = order.iter().position(|&p| p == cur).unwrap_or(0);
                let n = order.len();
                let j = if mv == FocusMove::Next {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                };
                order[j]
            }
        };
        self.focus(target)?;
        Ok(target)
    }

    /// `pane.swap`: exchange two tiled panes' positions.
    pub fn swap(&mut self, a: PaneId, b: PaneId) -> Result<(), TabError> {
        if a == b {
            return Err(TabError::SamePane);
        }
        self.require_root(a)?;
        let root = self.require_root(b)?;
        let mut nr = root.clone();
        nr.swap_ids(a, b);
        // Focus stays with the pane id: keep it the expanded member of its stack.
        if let Some(f) = self.focused {
            nr.activate(f);
        }
        self.try_set(nr)
    }

    /// `pane.swap` with the best neighbour on `dir`.
    pub fn swap_dir(&mut self, pane: PaneId, dir: Dir) -> Result<(), TabError> {
        let other = self
            .neighbours(pane, dir)
            .first()
            .copied()
            .ok_or(TabError::NoSuchPane)?;
        self.swap(pane, other)
    }

    /// `pane.move`: take `pane` out and split `target` with it on the `dir` side, half/half.
    pub fn move_pane(&mut self, pane: PaneId, target: PaneId, dir: Dir) -> Result<(), TabError> {
        if pane == target {
            return Err(TabError::SamePane);
        }
        self.require_root(pane)?;
        let root = self.require_root(target)?;
        let mut nr = root.clone().remove(pane).ok_or(TabError::Empty)?;
        nr.split_at(
            target,
            Node::Pane(pane),
            dir.axis(),
            RATIO_ONE / 2,
            !dir.is_forward(),
        );
        self.try_set(nr)?;
        self.touch(pane);
        Ok(())
    }

    /// `pane.rotate`: rotate the leaves through the fixed tree shape by one position.
    pub fn rotate(&mut self, forward: bool) -> Result<(), TabError> {
        let mut nr = self.root.clone().ok_or(TabError::Empty)?;
        nr.rotate_leaves(forward);
        self.try_set(nr)
    }

    /// Flip the orientation of the split directly above `pane`.
    pub fn toggle_orient(&mut self, pane: PaneId) -> Result<(), TabError> {
        let mut nr = self.require_root(pane)?.clone();
        if !nr.toggle_parent_orient(pane) {
            return Err(TabError::Empty);
        }
        self.try_set(nr)
    }

    /// `pane.resize`: move the border on the `dir` side of `pane` outward by `amount` cells
    /// (negative shrinks), clamped to the minimum sizes. `Ok(false)` when the pane has no
    /// border on that side.
    pub fn resize(&mut self, pane: PaneId, dir: Dir, amount: i32) -> Result<bool, TabError> {
        self.require_root(pane)?;
        let rect = Rect::from_size(self.size);
        let Some(root) = &mut self.root else {
            return Err(TabError::Empty);
        };
        let moved = root.resize(rect, pane, dir, amount);
        if moved {
            self.preset = None;
        }
        Ok(moved)
    }

    /// `pane.equalize`.
    pub fn equalize(&mut self) {
        if let Some(r) = &mut self.root {
            r.equalize();
            self.preset = None;
        }
    }

    /// `pane.zoom` (toggle) on the focused pane. Returns whether zoom is now on.
    pub fn zoom_toggle(&mut self) -> Result<bool, TabError> {
        if self.zoomed.take().is_some() {
            return Ok(false);
        }
        let f = self.focused.ok_or(TabError::Empty)?;
        if !self.is_tiled(f) {
            return Err(TabError::Floating);
        }
        self.zoomed = Some(f);
        Ok(true)
    }

    /// Apply a layout preset over the tiled panes.
    pub fn apply_preset(&mut self, p: Preset) -> Result<(), TabError> {
        let order = self.tiled_order();
        if order.is_empty() {
            return Err(TabError::Empty);
        }
        let nr = preset::build(p, &order, self.focused);
        self.try_set(nr)?;
        self.preset = Some(p);
        Ok(())
    }

    /// `layout.next`: the next preset that fits, after the current one.
    pub fn next_preset(&mut self) -> Result<Preset, TabError> {
        let n = Preset::ALL.len();
        let start = self.preset.map_or(0, |p| p.index() + 1);
        for k in 0..n {
            let p = Preset::ALL[(start + k) % n];
            if self.apply_preset(p).is_ok() {
                return Ok(p);
            }
        }
        Err(TabError::TooSmall)
    }

    /// Stack `pane` onto the leaf holding `onto`; it becomes the expanded member.
    pub fn stack_combine(&mut self, pane: PaneId, onto: PaneId) -> Result<(), TabError> {
        if pane == onto {
            return Err(TabError::SamePane);
        }
        self.require_root(pane)?;
        let root = self.require_root(onto)?;
        if root.stack_of(pane).is_some_and(|s| s.panes.contains(&onto)) {
            return Err(TabError::SamePane);
        }
        let mut nr = root.clone().remove(pane).ok_or(TabError::Empty)?;
        nr.add_to_leaf(onto, pane);
        self.try_set(nr)?;
        self.touch(pane);
        Ok(())
    }

    /// Make room for a new pane by adding it to the stack (or pane) `onto` as a member.
    pub fn stack_add(&mut self, onto: PaneId, new: PaneId) -> Result<(), TabError> {
        if self.contains(new) {
            return Err(TabError::SamePane);
        }
        let mut nr = self.require_root(onto)?.clone();
        nr.add_to_leaf(onto, new);
        self.try_set(nr)?;
        self.touch(new);
        Ok(())
    }

    /// Break `pane` out of its stack into a split on the `dir` side of the stack.
    pub fn stack_break_out(&mut self, pane: PaneId, dir: Dir) -> Result<(), TabError> {
        let root = self.require_root(pane)?;
        let stack = root.stack_of(pane).ok_or(TabError::NotStacked)?;
        let anchor = stack
            .panes
            .iter()
            .copied()
            .find(|&p| p != pane)
            .ok_or(TabError::NotStacked)?;
        let mut nr = root.clone().remove(pane).ok_or(TabError::Empty)?;
        nr.split_at(
            anchor,
            Node::Pane(pane),
            dir.axis(),
            RATIO_ONE / 2,
            !dir.is_forward(),
        );
        self.try_set(nr)?;
        self.touch(pane);
        Ok(())
    }

    /// Change how the stack holding `pane` draws its headers.
    pub fn set_stack_style(&mut self, pane: PaneId, style: StackStyle) -> Result<(), TabError> {
        let mut nr = self.require_root(pane)?.clone();
        nr.stack_of_mut(pane).ok_or(TabError::NotStacked)?.style = style;
        self.try_set(nr)
    }

    // ---- floating layer (MUX-18) ----

    /// MUX-19 placement search for a new `w`x`h` float (clamped to the tab): centred first,
    /// then from the top-left, then from the top-right corner, stepping by (2, 1) cells, until
    /// the rect avoids every shown float. When nothing avoids them all, the first position
    /// whose top-left no float shares (a stagger); else the centre.
    pub fn place_float(&self, w: u32, h: u32) -> Rect {
        let (vw, vh) = (self.size.cols.max(MIN_COLS), self.size.rows.max(MIN_ROWS));
        let (w, h) = (w.clamp(MIN_COLS, vw), h.clamp(MIN_ROWS, vh));
        let centre = Rect::new((vw - w) / 2, (vh - h) / 2, w, h);
        let shown: Vec<Rect> = self
            .floats
            .iter()
            .filter(|f| f.visible)
            .map(|f| f.rect)
            .collect();
        let mut cands = vec![centre];
        let steps = (vw - w) / 2 + (vh - h) + 1;
        for k in 0..=steps.min(256) {
            let (dx, dy) = (2 * k, k);
            if dx <= vw - w && dy <= vh - h {
                cands.push(Rect::new(dx, dy, w, h));
                cands.push(Rect::new(vw - w - dx, dy, w, h));
            }
        }
        if let Some(r) = cands.iter().find(|r| !shown.iter().any(|s| s.overlaps(r))) {
            return *r;
        }
        cands
            .iter()
            .find(|r| !shown.iter().any(|s| s.x == r.x && s.y == r.y))
            .copied()
            .unwrap_or(centre)
    }

    /// Show or hide the floating layer: hides every unpinned float when any is shown
    /// (pinned floats stay), else shows them all and focuses the top one. Returns true when
    /// the layer is now shown.
    pub fn toggle_floats(&mut self) -> bool {
        if self.floats.is_empty() {
            return false;
        }
        let show = !self.floats.iter().any(|f| f.visible && !f.pinned);
        let panes: Vec<PaneId> = self
            .floats
            .iter()
            .filter(|f| show || !f.pinned)
            .map(|f| f.pane)
            .collect();
        for p in panes {
            let _ = self.set_float_visible(p, show);
        }
        if show && let Some(top) = self.floats_render_order().last().map(|f| f.pane) {
            let _ = self.focus(top);
        }
        show
    }

    /// Add a floating pane on top; it takes focus.
    pub fn add_float(&mut self, pane: PaneId, rect: Rect) -> Result<(), TabError> {
        if self.contains(pane) {
            return Err(TabError::SamePane);
        }
        self.floats.push(Float {
            pane,
            rect,
            visible: true,
            pinned: false,
        });
        self.touch(pane);
        Ok(())
    }

    fn float_mut(&mut self, pane: PaneId) -> Result<&mut Float, TabError> {
        if self.is_tiled(pane) {
            return Err(TabError::NotFloating);
        }
        self.floats
            .iter_mut()
            .find(|f| f.pane == pane)
            .ok_or(TabError::NoSuchPane)
    }

    /// Move or resize a float.
    pub fn set_float_rect(&mut self, pane: PaneId, rect: Rect) -> Result<(), TabError> {
        self.float_mut(pane)?.rect = rect;
        Ok(())
    }

    /// Pin or unpin a float.
    pub fn set_float_pinned(&mut self, pane: PaneId, pinned: bool) -> Result<(), TabError> {
        self.float_mut(pane)?.pinned = pinned;
        Ok(())
    }

    /// Show or hide a float. Hiding the focused float moves focus to the next MRU pane
    /// that is shown.
    pub fn set_float_visible(&mut self, pane: PaneId, visible: bool) -> Result<(), TabError> {
        self.float_mut(pane)?.visible = visible;
        if !visible && self.focused == Some(pane) {
            let next = self.mru.iter().copied().find(|&p| {
                p != pane
                    && (self.is_tiled(p) || self.floats.iter().any(|f| f.pane == p && f.visible))
            });
            if let Some(n) = next {
                self.focus(n)?;
            }
        }
        Ok(())
    }

    /// Raise a float to the top of the list.
    pub fn raise_float(&mut self, pane: PaneId) -> Result<(), TabError> {
        self.float_mut(pane)?;
        let i = self
            .floats
            .iter()
            .position(|f| f.pane == pane)
            .ok_or(TabError::NoSuchPane)?;
        let f = self.floats.remove(i);
        self.floats.push(f);
        Ok(())
    }

    /// Lower a float to the bottom of the list.
    pub fn lower_float(&mut self, pane: PaneId) -> Result<(), TabError> {
        self.float_mut(pane)?;
        let i = self
            .floats
            .iter()
            .position(|f| f.pane == pane)
            .ok_or(TabError::NoSuchPane)?;
        let f = self.floats.remove(i);
        self.floats.insert(0, f);
        Ok(())
    }

    /// Toggle: tiled pane becomes a float at `rect`.
    pub fn to_float(&mut self, pane: PaneId, rect: Rect) -> Result<(), TabError> {
        self.require_root(pane)?;
        if self.tiled_order().len() == 1 && self.floats.is_empty() {
            // the only pane stays tiled: a tab always keeps something to show
            return Err(TabError::Empty);
        }
        self.root = self.root.take().and_then(|r| r.remove(pane));
        self.preset = None;
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        self.floats.push(Float {
            pane,
            rect,
            visible: true,
            pinned: false,
        });
        self.touch(pane);
        Ok(())
    }

    /// Toggle: float becomes tiled, split off the best tiled target (`adopt` rule, or `onto`).
    pub fn embed(&mut self, pane: PaneId, onto: Option<PaneId>, dir: Dir) -> Result<(), TabError> {
        let i = self
            .floats
            .iter()
            .position(|f| f.pane == pane)
            .ok_or(TabError::NotFloating)?;
        let f = self.floats.remove(i);
        let r = match (
            self.root.is_some(),
            onto.or_else(|| self.tiled_order().first().copied()),
        ) {
            (true, Some(t)) => self.split(t, pane, dir, SplitSize::HALF),
            _ => self.adopt(pane),
        };
        match r {
            Ok(()) => Ok(()),
            Err(e) => {
                self.floats.insert(i, f);
                Err(e)
            }
        }
    }

    /// Structural and focus invariants; used by tests and debug assertions.
    ///
    /// - the tree is well-formed and its rectangles tile the tab exactly,
    /// - pane ids are unique across tiled and floating layers,
    /// - `mru` is a permutation of the panes with `focused` at its head,
    /// - `zoomed` is a tiled pane.
    pub fn check_invariants(&self) -> Result<(), String> {
        if let Some(r) = &self.root {
            r.check()?;
        }
        let mut all = self.panes();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        if all.len() != n {
            return Err("pane id used twice".into());
        }
        let mut m = self.mru.clone();
        m.sort_unstable();
        if m != all {
            return Err("mru is not a permutation of the panes".into());
        }
        if self.focused != self.mru.first().copied() {
            return Err("focused is not the mru head".into());
        }
        if let Some(z) = self.zoomed
            && !self.is_tiled(z)
        {
            return Err("zoomed pane is not tiled".into());
        }
        let win = Rect::from_size(self.size);
        let pl = self.tiled_layout();
        let mut area = 0u64;
        for (i, a) in pl.iter().enumerate() {
            if !win.contains_rect(&a.rect) {
                return Err("rect outside the tab".into());
            }
            area += a.rect.area();
            if pl[i + 1..].iter().any(|b| a.rect.overlaps(&b.rect)) {
                return Err("rects overlap".into());
            }
        }
        if self.root.is_some() && area != win.area() {
            return Err("rects do not tile the tab".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab() -> Tab {
        Tab::new(100, "t", Size { cols: 80, rows: 24 }, 1)
    }

    #[test]
    fn split_close_focus() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        assert_eq!(t.focused(), Some(2));
        assert_eq!(t.mru(), &[2, 1]);
        t.split(2, 3, Dir::Down, SplitSize::Cells(5)).unwrap();
        let l = t.layout();
        assert_eq!(l[2].rect.h, 5);
        t.check_invariants().unwrap();
        assert_eq!(t.close(3), Ok(false));
        assert_eq!(t.focused(), Some(2));
        assert_eq!(t.close(2), Ok(false));
        assert_eq!(t.close(1), Ok(true));
        assert_eq!(t.focused(), None);
    }

    #[test]
    fn split_left_puts_new_first() {
        let mut t = tab();
        t.split(1, 2, Dir::Left, SplitSize::Fraction(250_000))
            .unwrap();
        assert_eq!(t.tiled_order(), [2, 1]);
        assert_eq!(t.layout()[0].rect.w, 20);
    }

    #[test]
    fn split_refuses_when_too_small() {
        let mut t = Tab::new(1, "t", Size { cols: 3, rows: 1 }, 1);
        assert_eq!(
            t.split(1, 2, Dir::Right, SplitSize::HALF),
            Err(TabError::TooSmall)
        );
        assert_eq!(t.tiled_order(), [1]);
        assert_eq!(
            t.split(1, 1, Dir::Right, SplitSize::HALF),
            Err(TabError::SamePane)
        );
        assert_eq!(
            t.split(9, 2, Dir::Right, SplitSize::HALF),
            Err(TabError::NoSuchPane)
        );
        let mut t = Tab::new(1, "t", Size { cols: 80, rows: 2 }, 1);
        t.split(1, 2, Dir::Down, SplitSize::HALF).unwrap();
        assert_eq!(
            t.split(1, 3, Dir::Down, SplitSize::HALF),
            Err(TabError::TooSmall)
        );
    }

    #[test]
    fn directional_focus_and_symmetry() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        t.split(2, 3, Dir::Down, SplitSize::HALF).unwrap();
        // 1 | (2 over 3)
        assert_eq!(t.neighbours(1, Dir::Right).len(), 2);
        assert_eq!(t.neighbours(3, Dir::Left), [1]);
        assert_eq!(t.neighbours(2, Dir::Down), [3]);
        assert_eq!(t.neighbours(3, Dir::Up), [2]);
        assert!(t.neighbours(1, Dir::Left).is_empty());
        // tie: both share the same edge length? 2 is on top half (12) and 3 bottom (12);
        // MRU decides: 3 was focused before... focus 3 then 1 and go right.
        t.focus(3).unwrap();
        t.focus(1).unwrap();
        assert_eq!(t.focus_move(FocusMove::Dir(Dir::Right)), Ok(3));
        assert_eq!(t.focus_move(FocusMove::Last), Ok(1));
        assert_eq!(t.focus_move(FocusMove::Next), Ok(2));
        assert_eq!(t.focus_move(FocusMove::Prev), Ok(1));
        assert_eq!(t.focus_move(FocusMove::Prev), Ok(3));
    }

    #[test]
    fn swap_rotate_move() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        t.split(2, 3, Dir::Right, SplitSize::HALF).unwrap();
        t.swap(1, 3).unwrap();
        assert_eq!(t.tiled_order(), [3, 2, 1]);
        t.rotate(true).unwrap();
        assert_eq!(t.tiled_order(), [1, 3, 2]);
        t.move_pane(1, 2, Dir::Down).unwrap();
        t.check_invariants().unwrap();
        assert_eq!(t.neighbours(1, Dir::Up), [2]);
        assert_eq!(t.swap(1, 1), Err(TabError::SamePane));
        t.swap_dir(1, Dir::Up).unwrap();
        t.check_invariants().unwrap();
    }

    #[test]
    fn swap_keeps_focused_pane_expanded_in_stack() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        t.stack_combine(2, 1).unwrap();
        t.focus(2).unwrap();
        t.swap(1, 2).unwrap();
        t.check_invariants().unwrap();
        let s = t.root().unwrap().stack_of(2).unwrap();
        assert_eq!(s.panes[s.active], 2);
        assert_eq!(t.focused(), Some(2));
    }

    #[test]
    fn zoom_lifecycle() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        assert_eq!(t.zoom_toggle(), Ok(true));
        assert_eq!(t.layout().len(), 1);
        assert_eq!(t.layout()[0].rect, Rect::new(0, 0, 80, 24));
        t.check_invariants().unwrap();
        assert_eq!(t.zoom_toggle(), Ok(false));
        assert_eq!(t.layout().len(), 2);
        t.zoom_toggle().unwrap();
        t.focus(1).unwrap();
        assert_eq!(t.zoomed(), None);
        t.zoom_toggle().unwrap();
        t.close(1).unwrap();
        assert_eq!(t.zoomed(), None);
    }

    #[test]
    fn resize_and_equalize() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        assert_eq!(t.resize(1, Dir::Right, 10), Ok(true));
        assert_eq!(t.layout()[0].rect.w, 50);
        assert_eq!(t.resize(1, Dir::Right, 1000), Ok(true));
        assert_eq!(t.layout()[1].rect.w, 2);
        assert_eq!(t.resize(1, Dir::Up, 1), Ok(false));
        t.equalize();
        assert_eq!(t.layout()[0].rect.w, 40);
    }

    #[test]
    fn presets_cycle() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        t.split(2, 3, Dir::Right, SplitSize::HALF).unwrap();
        let mut seen = vec![];
        for _ in 0..Preset::ALL.len() {
            seen.push(t.next_preset().unwrap());
            t.check_invariants().unwrap();
        }
        assert_eq!(seen, Preset::ALL);
        assert_eq!(t.next_preset(), Ok(Preset::EvenH));
        t.split(3, 4, Dir::Down, SplitSize::HALF).unwrap();
        assert_eq!(t.preset(), None);
    }

    #[test]
    fn stacks() {
        let mut t = tab();
        t.split(1, 2, Dir::Right, SplitSize::HALF).unwrap();
        t.split(2, 3, Dir::Down, SplitSize::HALF).unwrap();
        t.stack_combine(3, 2).unwrap();
        assert_eq!(t.tiled_order(), [1, 2, 3]);
        let s = t.root().unwrap().stack_of(2).unwrap();
        assert_eq!((s.panes.as_slice(), s.active), (&[2, 3][..], 1));
        t.check_invariants().unwrap();
        // hidden member 2 is not a neighbour candidate; focusing it expands it
        assert_eq!(t.neighbours(1, Dir::Right), [3]);
        t.focus(2).unwrap();
        assert_eq!(t.neighbours(1, Dir::Right), [2]);
        t.stack_add(2, 4).unwrap();
        assert_eq!(t.root().unwrap().stack_of(4).unwrap().panes, [2, 3, 4]);
        t.stack_break_out(4, Dir::Down).unwrap();
        assert!(t.root().unwrap().stack_of(4).is_none());
        t.set_stack_style(2, StackStyle::Horizontal).unwrap();
        t.check_invariants().unwrap();
        assert_eq!(t.stack_break_out(1, Dir::Down), Err(TabError::NotStacked));
        assert_eq!(t.stack_combine(2, 3), Err(TabError::SamePane));
    }

    #[test]
    fn floats() {
        let mut t = tab();
        t.add_float(10, Rect::new(5, 5, 20, 8)).unwrap();
        t.add_float(11, Rect::new(6, 6, 20, 8)).unwrap();
        assert_eq!(t.focused(), Some(11));
        t.focus(10).unwrap();
        assert_eq!(
            t.floats().iter().map(|f| f.pane).collect::<Vec<_>>(),
            [11, 10]
        );
        t.set_float_pinned(11, true).unwrap();
        assert_eq!(
            t.floats_render_order()
                .iter()
                .map(|f| f.pane)
                .collect::<Vec<_>>(),
            [10, 11]
        );
        t.set_float_visible(10, false).unwrap();
        assert_eq!(t.focused(), Some(11));
        t.focus(10).unwrap();
        assert!(t.floats().iter().find(|f| f.pane == 10).unwrap().visible);
        assert_eq!(
            t.split(10, 12, Dir::Right, SplitSize::HALF),
            Err(TabError::Floating)
        );
        t.embed(10, None, Dir::Right).unwrap();
        assert_eq!(t.tiled_order(), [1, 10]);
        t.to_float(10, Rect::new(1, 1, 10, 5)).unwrap();
        assert!(t.is_float(10));
        assert_eq!(tab().to_float(1, Rect::default()), Err(TabError::Empty));
        t.check_invariants().unwrap();
        assert_eq!(t.close(11), Ok(false));
        t.raise_float(10).unwrap();
        t.lower_float(10).unwrap();
        assert_eq!(t.raise_float(1), Err(TabError::NotFloating));
    }
}

/// `r` moved, then shrunk, to lie inside `size` (at least the 2x1 minimum).
fn clamp_rect(r: Rect, size: Size) -> Rect {
    let (vw, vh) = (size.cols.max(MIN_COLS), size.rows.max(MIN_ROWS));
    let w = r.w.clamp(MIN_COLS, vw);
    let h = r.h.clamp(MIN_ROWS, vh);
    Rect::new(r.x.min(vw - w), r.y.min(vh - h), w, h)
}

#[cfg(test)]
mod float_tests {
    use super::*;

    fn tab() -> Tab {
        Tab::new(1, "t", Size { cols: 80, rows: 24 }, 1)
    }

    #[test]
    fn placement_centres_then_staggers_from_the_corners() {
        let mut t = tab();
        let a = t.place_float(40, 12);
        assert_eq!(a, Rect::new(20, 6, 40, 12), "centred half size");
        t.add_float(2, a).unwrap();
        // No free 40x12 spot beside a centred one: the next takes the top-left corner.
        let b = t.place_float(40, 12);
        assert_eq!(b, Rect::new(0, 0, 40, 12));
        t.add_float(3, b).unwrap();
        // Room for two side by side only: the third staggers off both origins.
        let c = t.place_float(40, 12);
        assert!(Rect::from_size(t.size()).contains_rect(&c));
        t.add_float(4, c).unwrap();
        let origins: Vec<(u32, u32)> = t.floats().iter().map(|f| (f.rect.x, f.rect.y)).collect();
        let mut d = origins.clone();
        d.dedup();
        assert_eq!(d.len(), origins.len(), "{origins:?}");
        // Where a free spot exists it is taken: a small float clears the others.
        let mut u = tab();
        let first = u.place_float(20, 6);
        u.add_float(2, first).unwrap();
        let next = u.place_float(20, 6);
        assert!(!next.overlaps(&first), "{next:?} over {first:?}");
        assert_eq!(next, Rect::new(0, 0, 20, 6));
        // Oversized requests clamp to the tab.
        assert_eq!(t.place_float(500, 500), Rect::new(0, 0, 80, 24));
    }

    #[test]
    fn the_layer_hides_and_shows_as_one_and_focus_follows() {
        let mut t = tab();
        t.add_float(2, Rect::new(1, 1, 10, 5)).unwrap();
        t.add_float(3, Rect::new(5, 5, 10, 5)).unwrap();
        assert_eq!(t.focused(), Some(3));
        assert!(!t.toggle_floats());
        assert!(t.floats().iter().all(|f| !f.visible));
        assert_eq!(t.focused(), Some(1), "back to the tiled pane");
        assert!(t.toggle_floats());
        assert!(t.floats().iter().all(|f| f.visible));
        assert_eq!(t.focused(), Some(3), "the top float");
        // A pinned float stays when the layer hides.
        t.set_float_pinned(2, true).unwrap();
        assert!(!t.toggle_floats());
        let shown: Vec<PaneId> = t
            .floats()
            .iter()
            .filter(|f| f.visible)
            .map(|f| f.pane)
            .collect();
        assert_eq!(shown, [2]);
        assert!(t.toggle_floats());
        assert!(t.floats().iter().all(|f| f.visible));
        t.check_invariants().unwrap();
    }

    #[test]
    fn floats_stay_inside_a_shrinking_tab() {
        let mut t = tab();
        t.add_float(2, Rect::new(60, 20, 20, 4)).unwrap();
        t.set_size(Size { cols: 30, rows: 10 });
        let r = t.floats()[0].rect;
        assert!(Rect::from_size(t.size()).contains_rect(&r), "{r:?}");
        assert_eq!((r.w, r.h), (20, 4));
        t.set_size(Size { cols: 10, rows: 3 });
        assert_eq!(t.floats()[0].rect, Rect::new(0, 0, 10, 3));
    }
}
