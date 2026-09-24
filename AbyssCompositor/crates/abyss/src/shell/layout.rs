// SPDX-License-Identifier: AGPL-3.0-only
//! Tiling layouts (COMP-05 §2-§3).
//!
//! One weighted n-ary tree per workspace, stored as an arena and referenced by
//! `u32` handles — no `Rc<RefCell<_>>` graph (root invariant). Radiant walks
//! the tree: every container splits its rectangle along one axis between its
//! children in proportion to their weights. Classic dwindle and master ignore
//! its shape and use only its in-order window sequence, so switching between
//! the three is lossless. See `decisions/0021-layout-tree.md`.
//!
//! The tree is kept normalized after every mutation: no container holds a
//! single child, and no container runs along the same axis as its parent. The
//! second rule is what makes three columns three siblings with exact thirds
//! rather than a half and two quarters.
//!
//! Generic over the leaf payload only so the tree can be unit-tested with
//! plain integers; the compositor uses exactly one instantiation, `Window`.

use smithay::{
    desktop::Window,
    utils::{Logical, Point, Rectangle, Size},
};

pub type NodeId = u32;
const NIL: NodeId = u32::MAX;

/// Weight bounds for [`Tree::resize`], which solves for arbitrary shares.
const RESIZE_WEIGHT: (f64, f64) = (0.1, 20.0);
/// Weight bounds for [`Tree::adjust_weight`], the priority keybind.
const PRIORITY_WEIGHT: (f64, f64) = (1.0, 9.0);

#[derive(Debug, Clone)]
enum Kind<W> {
    Leaf(W),
    Split {
        children: Vec<NodeId>,
        side_by_side: bool,
    },
}

#[derive(Debug, Clone)]
struct Node<W> {
    parent: NodeId,
    /// Share of the parent container, relative to the siblings' weights.
    weight: f64,
    kind: Kind<W>,
}

/// Which side of a target a new leaf goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// Does the split this side implies run left-to-right?
    fn side_by_side(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    /// Does the new leaf come before the target in reading order?
    fn before(self) -> bool {
        matches!(self, Side::Left | Side::Top)
    }
}

/// Where a pointer sits inside a tile: one of the four triangles the
/// diagonals cut it into, or the middle-third box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

impl Zone {
    pub fn side(self) -> Option<Side> {
        match self {
            Zone::Left => Some(Side::Left),
            Zone::Right => Some(Side::Right),
            Zone::Top => Some(Side::Top),
            Zone::Bottom => Some(Side::Bottom),
            Zone::Center => None,
        }
    }
}

/// What [`Tree::insert_at`] splits.
#[derive(Debug, Clone, Copy)]
pub enum Target<'a, W> {
    /// The leaf holding this window.
    Leaf(&'a W),
    /// The whole tree: a full-height column or full-width row.
    Root,
}

/// Weighted n-ary layout tree. Empty slots are recycled through `free`.
#[derive(Debug, Clone)]
pub struct Tree<W = Window> {
    nodes: Vec<Option<Node<W>>>,
    free: Vec<NodeId>,
    root: NodeId,
}

impl<W> Default for Tree<W> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NIL,
        }
    }
}

impl<W: Clone + PartialEq> Tree<W> {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, id: NodeId) -> Option<&Node<W>> {
        self.nodes.get(id as usize).and_then(|n| n.as_ref())
    }

    fn get_mut(&mut self, id: NodeId) -> Option<&mut Node<W>> {
        self.nodes.get_mut(id as usize).and_then(|n| n.as_mut())
    }

    fn parent(&self, id: NodeId) -> NodeId {
        self.get(id).map(|n| n.parent).unwrap_or(NIL)
    }

    fn alloc(&mut self, node: Node<W>) -> NodeId {
        match self.free.pop() {
            Some(id) => {
                self.nodes[id as usize] = Some(node);
                id
            }
            None => {
                self.nodes.push(Some(node));
                (self.nodes.len() - 1) as NodeId
            }
        }
    }

    fn release(&mut self, id: NodeId) {
        self.nodes[id as usize] = None;
        self.free.push(id);
    }

    fn set_parent(&mut self, id: NodeId, parent: NodeId) {
        if let Some(n) = self.get_mut(id) {
            n.parent = parent;
        }
    }

    fn children(&self, id: NodeId) -> Option<(&[NodeId], bool)> {
        match &self.get(id)?.kind {
            Kind::Split {
                children,
                side_by_side,
            } => Some((children, *side_by_side)),
            Kind::Leaf(_) => None,
        }
    }

    /// Put `new` where `old` hangs: in `old`'s parent's child list, or at the
    /// root. `new`'s parent pointer is updated; `old`'s is left alone.
    fn replace_in_parent(&mut self, old: NodeId, new: NodeId) {
        let parent = self.parent(old);
        self.set_parent(new, parent);
        if parent == NIL {
            self.root = new;
        } else if let Some(Node {
            kind: Kind::Split { children, .. },
            ..
        }) = self.get_mut(parent)
        {
            if let Some(slot) = children.iter_mut().find(|c| **c == old) {
                *slot = new;
            }
        }
    }

    /// In-order leaf ids: the visual left-to-right / top-to-bottom sequence.
    fn leaf_ids(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            if id == NIL {
                continue;
            }
            match self.get(id).map(|n| &n.kind) {
                Some(Kind::Leaf(_)) => out.push(id),
                Some(Kind::Split { children, .. }) => stack.extend(children.iter().rev()),
                None => {}
            }
        }
        out
    }

    pub fn windows(&self) -> Vec<W> {
        self.leaf_ids()
            .into_iter()
            .filter_map(|id| match &self.get(id)?.kind {
                Kind::Leaf(w) => Some(w.clone()),
                _ => None,
            })
            .collect()
    }

    /// Allocation free: `Workspace::holds_live` calls this on every pointer
    /// motion.
    pub fn contains(&self, window: &W) -> bool {
        self.leaf_of(window).is_some()
    }

    /// The arena slot holding `window`. A window is in the tree at most once,
    /// so arena order is as good as reading order, and scanning it allocates
    /// nothing.
    fn leaf_of(&self, window: &W) -> Option<NodeId> {
        self.nodes
            .iter()
            .position(|n| matches!(n, Some(Node { kind: Kind::Leaf(w), .. }) if w == window))
            .map(|i| i as NodeId)
    }

    /// `window`'s weight in its container, if it is tiled here.
    pub fn weight_of(&self, window: &W) -> Option<f64> {
        self.leaf_of(window).and_then(|id| self.get(id)).map(|n| n.weight)
    }

    /// Set `window`'s weight, clamped to the resize range. False when it is
    /// not tiled here or the weight did not change.
    pub fn set_weight(&mut self, window: &W, weight: f64) -> bool {
        let Some(n) = self.leaf_of(window).and_then(|id| self.get_mut(id)) else {
            return false;
        };
        let w = weight.clamp(RESIZE_WEIGHT.0, RESIZE_WEIGHT.1);
        let changed = n.weight != w;
        n.weight = w;
        changed
    }

    /// Insert `window` by splitting the leaf holding `near` (or the last leaf),
    /// dwindle-style. The split runs along the longer dimension of that leaf's
    /// current rectangle — COMP-05 §11 open question 2, resolved as proposed.
    /// Which side `window` lands on is decided by `pointer`'s position within
    /// that rectangle, so a window spawned in the top-right of a portrait leaf
    /// lands above the existing window rather than always below it.
    pub fn insert(
        &mut self,
        window: W,
        near: Option<&W>,
        area: Rectangle<i32, Logical>,
        pointer: Point<f64, Logical>,
    ) {
        if self.root == NIL {
            self.insert_at(window, Target::Root, Side::Right, 1.0);
            return;
        }
        let target = near
            .and_then(|w| self.leaf_of(w))
            .or_else(|| self.leaf_ids().last().copied())
            .unwrap_or(self.root);
        let rect = self
            .geometries(area, 0)
            .into_iter()
            .find(|(id, _, _)| *id == target)
            .map(|(_, _, r)| r)
            .unwrap_or(area);
        let side_by_side = rect.size.w >= rect.size.h;
        let side = match (side_by_side, new_leaf_first(rect, side_by_side, pointer)) {
            (true, true) => Side::Left,
            (true, false) => Side::Right,
            (false, true) => Side::Top,
            (false, false) => Side::Bottom,
        };
        let target_window = match self.get(target).map(|n| &n.kind) {
            Some(Kind::Leaf(w)) => w.clone(),
            _ => {
                self.insert_at(window, Target::Root, side, 1.0);
                return;
            }
        };
        self.insert_at(window, Target::Leaf(&target_window), side, 1.0);
    }

    /// Put `window` on `side` of `target` with `weight`.
    ///
    /// If the target's container already runs along that side's axis the new
    /// leaf becomes a sibling right before or after it; otherwise the target
    /// is wrapped in a new perpendicular container that takes over its weight.
    /// Nothing is mutated when the target is not in the tree (`false`); an
    /// empty tree takes the window as its root whatever the target.
    pub fn insert_at(&mut self, window: W, target: Target<'_, W>, side: Side, weight: f64) -> bool {
        if self.root == NIL {
            self.root = self.alloc(Node {
                parent: NIL,
                weight: 1.0,
                kind: Kind::Leaf(window),
            });
            return true;
        }
        let t = match target {
            Target::Leaf(w) => match self.leaf_of(w) {
                Some(id) => id,
                None => return false,
            },
            Target::Root => self.root,
        };
        let axis = side.side_by_side();
        let leaf = self.alloc(Node {
            parent: NIL,
            weight,
            kind: Kind::Leaf(window),
        });
        let parent = self.parent(t);
        if let Some(Node {
            kind: Kind::Split {
                children,
                side_by_side,
            },
            ..
        }) = self.get_mut(parent)
        {
            if *side_by_side == axis {
                let at = children.iter().position(|c| *c == t).unwrap_or(children.len());
                let at = if side.before() { at } else { at + 1 };
                children.insert(at, leaf);
                self.set_parent(leaf, parent);
                self.normalize();
                return true;
            }
        }
        let t_weight = self.get(t).map(|n| n.weight).unwrap_or(1.0);
        let children = if side.before() {
            vec![leaf, t]
        } else {
            vec![t, leaf]
        };
        let split = self.alloc(Node {
            parent: NIL,
            weight: t_weight,
            kind: Kind::Split {
                children,
                side_by_side: axis,
            },
        });
        self.replace_in_parent(t, split);
        self.set_parent(t, split);
        self.set_parent(leaf, split);
        if let Some(n) = self.get_mut(t) {
            n.weight = 1.0;
        }
        self.normalize();
        true
    }

    /// Remove a window; the tree is normalized after.
    pub fn remove(&mut self, window: &W) -> bool {
        let Some(leaf) = self.leaf_of(window) else {
            return false;
        };
        let parent = self.parent(leaf);
        self.release(leaf);
        if parent == NIL {
            self.root = NIL;
            return true;
        }
        if let Some(Node {
            kind: Kind::Split { children, .. },
            ..
        }) = self.get_mut(parent)
        {
            children.retain(|c| *c != leaf);
        }
        self.normalize();
        true
    }

    /// Collapse single-child containers into their child (which takes over
    /// the container's weight) and flatten a container into a parent running
    /// along the same axis (its children keep their own weights). Repeats
    /// until neither applies; trees are a handful of nodes.
    fn normalize(&mut self) {
        loop {
            let mut changed = false;
            for id in 0..self.nodes.len() as NodeId {
                let Some((children, side_by_side)) = self.children(id) else {
                    continue;
                };
                match children.len() {
                    0 => {
                        let parent = self.parent(id);
                        if parent == NIL {
                            self.root = NIL;
                        } else if let Some(Node {
                            kind: Kind::Split { children, .. },
                            ..
                        }) = self.get_mut(parent)
                        {
                            children.retain(|c| *c != id);
                        }
                        self.release(id);
                        changed = true;
                    }
                    1 => {
                        let only = children[0];
                        let weight = self.get(id).map(|n| n.weight).unwrap_or(1.0);
                        self.replace_in_parent(id, only);
                        if let Some(n) = self.get_mut(only) {
                            n.weight = if n.parent == NIL { 1.0 } else { weight };
                        }
                        self.release(id);
                        changed = true;
                    }
                    _ => {
                        let parent = self.parent(id);
                        if !matches!(self.children(parent), Some((_, axis)) if axis == side_by_side) {
                            continue;
                        }
                        let moved = children.to_vec();
                        for c in &moved {
                            self.set_parent(*c, parent);
                        }
                        if let Some(Node {
                            kind: Kind::Split { children, .. },
                            ..
                        }) = self.get_mut(parent)
                        {
                            if let Some(at) = children.iter().position(|c| *c == id) {
                                children.splice(at..=at, moved);
                            }
                        }
                        self.release(id);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Exchange the positions of two windows in the tree. Weights travel with
    /// their windows, so priority follows the window.
    pub fn swap(&mut self, x: &W, y: &W) {
        let (Some(ix), Some(iy)) = (self.leaf_of(x), self.leaf_of(y)) else {
            return;
        };
        if ix == iy {
            return;
        }
        let (Some(a), Some(b)) = (self.get(ix), self.get(iy)) else {
            return;
        };
        let (wa, wb) = match (&a.kind, &b.kind) {
            (Kind::Leaf(wa), Kind::Leaf(wb)) => (wa.clone(), wb.clone()),
            _ => return,
        };
        let (ka, kb) = (a.weight, b.weight);
        if let Some(n) = self.get_mut(ix) {
            n.kind = Kind::Leaf(wb);
            n.weight = kb;
        }
        if let Some(n) = self.get_mut(iy) {
            n.kind = Kind::Leaf(wa);
            n.weight = ka;
        }
    }

    /// Change `window`'s priority by `delta` steps: its weight is rounded,
    /// shifted and clamped to `1..=9`. False when nothing changed.
    pub fn adjust_weight(&mut self, window: &W, delta: i32) -> bool {
        let Some(n) = self.leaf_of(window).and_then(|id| self.get_mut(id)) else {
            return false;
        };
        let w = (n.weight.round() + delta as f64).clamp(PRIORITY_WEIGHT.0, PRIORITY_WEIGHT.1);
        let changed = n.weight != w;
        n.weight = w;
        changed
    }

    /// Give `window` a new tiled size by re-solving weights (ADR 0031).
    ///
    /// A tiled window has no size of its own: what it has is its share of the
    /// nearest ancestor container along each axis. For each requested axis we
    /// walk from the leaf to the first container running along that axis and
    /// solve [`split_weighted`]'s formula for the weight of the child on our
    /// path that yields the wanted extent, clamped to `0.1..=20`. A window with
    /// no such container (the only window on its workspace, or the only
    /// column) cannot be resized along that axis; `false` says nothing changed.
    pub fn resize(
        &mut self,
        window: &W,
        area: Rectangle<i32, Logical>,
        gap: i32,
        want: (Option<i32>, Option<i32>),
    ) -> bool {
        let Some(leaf) = self.leaf_of(window) else {
            return false;
        };
        let rects: Vec<(NodeId, Rectangle<i32, Logical>)> = self.node_rects(area, gap);
        let mut changed = false;
        for (horizontal, target) in [(true, want.0), (false, want.1)] {
            let Some(target) = target else { continue };
            let mut child = leaf;
            let mut cur = self.parent(leaf);
            while cur != NIL {
                let Some((children, side_by_side)) = self.children(cur) else {
                    break;
                };
                if side_by_side == horizontal {
                    let Some((_, rect)) = rects.iter().find(|(id, _)| *id == cur) else {
                        break;
                    };
                    let n = children.len() as i32;
                    let extent = if horizontal { rect.size.w } else { rect.size.h };
                    let span = extent - gap * (n - 1);
                    let others: f64 = children
                        .iter()
                        .filter(|c| **c != child)
                        .filter_map(|c| self.get(*c))
                        .map(|n| n.weight)
                        .sum();
                    if span > 0 && others > 0.0 {
                        let share = (target as f64 / span as f64).clamp(0.0, 0.999);
                        let w = (share * others / (1.0 - share)).clamp(RESIZE_WEIGHT.0, RESIZE_WEIGHT.1);
                        if let Some(n) = self.get_mut(child) {
                            n.weight = w;
                            changed = true;
                        }
                    }
                    break;
                }
                child = cur;
                cur = self.parent(cur);
            }
        }
        changed
    }

    /// The child rectangles of container `id` laid over `rect`.
    fn child_rects(
        &self,
        children: &[NodeId],
        side_by_side: bool,
        rect: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<Rectangle<i32, Logical>> {
        let weights: Vec<f64> = children
            .iter()
            .map(|c| self.get(*c).map(|n| n.weight).unwrap_or(1.0))
            .collect();
        split_weighted(rect, side_by_side, &weights, gap)
    }

    /// Rectangle of every node, containers included.
    fn node_rects(&self, area: Rectangle<i32, Logical>, gap: i32) -> Vec<(NodeId, Rectangle<i32, Logical>)> {
        let mut out = Vec::new();
        if self.root == NIL {
            return out;
        }
        let mut stack = vec![(self.root, area)];
        while let Some((id, rect)) = stack.pop() {
            out.push((id, rect));
            if let Some((children, side_by_side)) = self.children(id) {
                let rects = self.child_rects(children, side_by_side, rect, gap);
                stack.extend(children.iter().copied().zip(rects).rev());
            }
        }
        out
    }

    /// (leaf id, window, rectangle) for the Radiant arrangement.
    fn geometries(
        &self,
        area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<(NodeId, W, Rectangle<i32, Logical>)> {
        self.node_rects(area, gap)
            .into_iter()
            .filter_map(|(id, r)| match &self.get(id)?.kind {
                Kind::Leaf(w) => Some((id, w.clone(), r)),
                Kind::Split { .. } => None,
            })
            .collect()
    }

    /// Radiant: the tree itself, every container split by weight.
    pub fn radiant(&self, area: Rectangle<i32, Logical>, gap: i32) -> Vec<(W, Rectangle<i32, Logical>)> {
        self.geometries(area, gap)
            .into_iter()
            .map(|(_, w, r)| (w, r))
            .collect()
    }

    /// Classic dwindle over the in-order window sequence; see [`dwindle_rects`].
    pub fn dwindle(&self, area: Rectangle<i32, Logical>, gap: i32) -> Vec<(W, Rectangle<i32, Logical>)> {
        let windows = self.windows();
        let rects = dwindle_rects(windows.len(), area, gap);
        windows.into_iter().zip(rects).collect()
    }

    /// Master: first window on the left half, the rest stacked on the right.
    /// With one window it fills the area.
    pub fn master(&self, area: Rectangle<i32, Logical>, gap: i32) -> Vec<(W, Rectangle<i32, Logical>)> {
        let windows = self.windows();
        let mut out = Vec::with_capacity(windows.len());
        match windows.split_first() {
            None => {}
            Some((first, [])) => out.push((first.clone(), area)),
            Some((first, rest)) => {
                let (left, right) = split_rect(area, true, 0.5, gap);
                out.push((first.clone(), left));
                let n = rest.len() as i32;
                let total_gap = gap * (n - 1);
                let each = ((right.size.h - total_gap) / n).max(1);
                for (i, w) in rest.iter().enumerate() {
                    let y = right.loc.y + i as i32 * (each + gap);
                    let h = if i as i32 == n - 1 {
                        (right.loc.y + right.size.h - y).max(1)
                    } else {
                        each
                    };
                    out.push((
                        w.clone(),
                        Rectangle::new((right.loc.x, y).into(), Size::from((right.size.w, h))),
                    ));
                }
            }
        }
        out
    }
}

/// Classic dwindle for `n` windows: window `i` takes the first half of what
/// is left, split along the remaining rectangle's longer dimension, and the
/// last window takes the remainder. Pure, and blind to weights and to the
/// tree's shape.
pub fn dwindle_rects(n: usize, area: Rectangle<i32, Logical>, gap: i32) -> Vec<Rectangle<i32, Logical>> {
    let mut out = Vec::with_capacity(n);
    let mut rest = area;
    for i in 0..n {
        if i + 1 == n {
            out.push(rest);
            break;
        }
        let (a, b) = split_rect(rest, rest.size.w >= rest.size.h, 0.5, gap);
        out.push(a);
        rest = b;
    }
    out
}

/// Which zone of `rect` the pointer is in. The middle third on both axes is
/// the centre; outside it, the diagonals decide.
pub fn drop_zone(rect: Rectangle<i32, Logical>, pointer: Point<f64, Logical>) -> Zone {
    let w = rect.size.w.max(1) as f64;
    let h = rect.size.h.max(1) as f64;
    let u = (pointer.x - rect.loc.x as f64) / w;
    let v = (pointer.y - rect.loc.y as f64) / h;
    let third = 1.0 / 3.0;
    if (third..=2.0 * third).contains(&u) && (third..=2.0 * third).contains(&v) {
        return Zone::Center;
    }
    // Above the main diagonal (v < u) and above the anti-diagonal (v < 1 - u)
    // is the top triangle; the other three follow by symmetry.
    match (v < u, v < 1.0 - u) {
        (true, true) => Zone::Top,
        (false, false) => Zone::Bottom,
        (false, true) => Zone::Left,
        (true, false) => Zone::Right,
    }
}

/// Split `r` along one axis between children in proportion to `weights`,
/// with `gap` between neighbours. Each share is floored and the last child
/// takes the remainder, so the pieces always tile `r` exactly.
pub fn split_weighted(
    r: Rectangle<i32, Logical>,
    side_by_side: bool,
    weights: &[f64],
    gap: i32,
) -> Vec<Rectangle<i32, Logical>> {
    let n = weights.len();
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return out;
    }
    let extent = if side_by_side { r.size.w } else { r.size.h };
    let span = (extent - gap * (n as i32 - 1)).max(n as i32);
    let total: f64 = weights.iter().map(|w| w.max(0.0)).sum();
    let mut offset = 0;
    for (i, w) in weights.iter().enumerate() {
        let len = if i + 1 == n {
            (extent - offset).max(1)
        } else if total > 0.0 {
            // The epsilon keeps an exact share (a solved resize) from
            // flooring one pixel short on float error.
            ((span as f64 * w.max(0.0) / total + 1e-6) as i32).max(1)
        } else {
            (span / n as i32).max(1)
        };
        out.push(if side_by_side {
            Rectangle::new((r.loc.x + offset, r.loc.y).into(), Size::from((len, r.size.h)))
        } else {
            Rectangle::new((r.loc.x, r.loc.y + offset).into(), Size::from((r.size.w, len)))
        });
        offset += len + gap;
    }
    out
}

/// Whether the newly inserted leaf goes first (left or top) in a split of
/// `rect`, given where the pointer sits inside it. On the exact midpoint the
/// existing window keeps first place — pre-fix behavior, unchanged.
fn new_leaf_first(rect: Rectangle<i32, Logical>, side_by_side: bool, pointer: Point<f64, Logical>) -> bool {
    if side_by_side {
        pointer.x < rect.loc.x as f64 + rect.size.w as f64 / 2.0
    } else {
        pointer.y < rect.loc.y as f64 + rect.size.h as f64 / 2.0
    }
}

fn split_rect(
    r: Rectangle<i32, Logical>,
    side_by_side: bool,
    ratio: f64,
    gap: i32,
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    if side_by_side {
        let first = (((r.size.w - gap) as f64 * ratio) as i32).max(1);
        let second = (r.size.w - gap - first).max(1);
        (
            Rectangle::new(r.loc, Size::from((first, r.size.h))),
            Rectangle::new(
                (r.loc.x + first + gap, r.loc.y).into(),
                Size::from((second, r.size.h)),
            ),
        )
    } else {
        let first = (((r.size.h - gap) as f64 * ratio) as i32).max(1);
        let second = (r.size.h - gap - first).max(1);
        (
            Rectangle::new(r.loc, Size::from((r.size.w, first))),
            Rectangle::new(
                (r.loc.x, r.loc.y + first + gap).into(),
                Size::from((r.size.w, second)),
            ),
        )
    }
}

// A real `Window` only comes out of a live client's `xdg_toplevel` handshake
// (see the precedent and rationale in `shell::focus`'s `state_tests` module),
// so the tree is tested on its `u32` instantiation; `Window` is the only other.
#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), Size::from((w, h)))
    }

    const AREA: (i32, i32) = (1920, 1080);

    fn area() -> Rectangle<i32, Logical> {
        rect(0, 0, AREA.0, AREA.1)
    }

    /// Bottom-right corner: every auto insert lands after its target, which is
    /// the classic dwindle order.
    fn far() -> Point<f64, Logical> {
        (1e6, 1e6).into()
    }

    fn tree(n: u32) -> Tree<u32> {
        let mut t = Tree::new();
        for w in 1..=n {
            let near = w.checked_sub(1).filter(|p| *p > 0);
            t.insert(w, near.as_ref(), area(), far());
        }
        t
    }

    fn rect_of(t: &Tree<u32>, w: u32, gap: i32) -> Rectangle<i32, Logical> {
        t.radiant(area(), gap)
            .into_iter()
            .find(|(x, _)| *x == w)
            .map(|(_, r)| r)
            .unwrap()
    }

    /// Container count, for the normalization invariants.
    fn splits(t: &Tree<u32>) -> usize {
        t.nodes
            .iter()
            .flatten()
            .filter(|n| matches!(n.kind, Kind::Split { .. }))
            .count()
    }

    fn assert_normalized(t: &Tree<u32>) {
        for (id, n) in t.nodes.iter().enumerate() {
            let Some(Node {
                kind:
                    Kind::Split {
                        children,
                        side_by_side,
                    },
                parent,
                ..
            }) = n
            else {
                continue;
            };
            assert!(
                children.len() >= 2,
                "container {id} has {} children",
                children.len()
            );
            if let Some((_, axis)) = t.children(*parent) {
                assert_ne!(axis, *side_by_side, "container {id} shares its parent's axis");
            }
            for c in children {
                assert_eq!(t.parent(*c), id as NodeId);
            }
        }
    }

    #[test]
    fn portrait_split_top_half_puts_the_new_window_first() {
        let target = rect(0, 0, 200, 800);
        assert!(new_leaf_first(target, false, (50.0, 100.0).into()));
    }

    #[test]
    fn portrait_split_bottom_half_leaves_the_target_first() {
        let target = rect(0, 0, 200, 800);
        assert!(!new_leaf_first(target, false, (50.0, 700.0).into()));
    }

    #[test]
    fn landscape_split_left_half_puts_the_new_window_first() {
        let target = rect(0, 0, 800, 200);
        assert!(new_leaf_first(target, true, (100.0, 50.0).into()));
    }

    #[test]
    fn landscape_split_right_half_leaves_the_target_first() {
        let target = rect(0, 0, 800, 200);
        assert!(!new_leaf_first(target, true, (700.0, 50.0).into()));
    }

    #[test]
    fn exact_midpoint_keeps_the_target_first() {
        let target = rect(0, 0, 200, 800);
        assert!(!new_leaf_first(target, false, (100.0, 400.0).into()));
        let target = rect(0, 0, 800, 200);
        assert!(!new_leaf_first(target, true, (400.0, 100.0).into()));
    }

    #[test]
    fn weights_one_one_two_give_quarters_and_a_half() {
        let r = split_weighted(rect(0, 0, 1020, 100), true, &[1.0, 1.0, 2.0], 10);
        assert_eq!(
            r,
            vec![
                rect(0, 0, 250, 100),
                rect(260, 0, 250, 100),
                rect(520, 0, 500, 100)
            ]
        );
        // Uneven: the floor leaves a remainder, and the last child takes it.
        let r = split_weighted(rect(0, 0, 100, 1003), false, &[1.0, 1.0, 2.0], 0);
        assert_eq!(
            r,
            vec![
                rect(0, 0, 100, 250),
                rect(0, 250, 100, 250),
                rect(0, 500, 100, 503)
            ]
        );
    }

    #[test]
    fn radiant_honours_the_weights() {
        let mut t = Tree::new();
        t.insert_at(1, Target::Root, Side::Right, 1.0);
        t.insert_at(2, Target::Leaf(&1), Side::Right, 1.0);
        t.insert_at(3, Target::Leaf(&2), Side::Right, 2.0);
        let area = rect(0, 0, 1020, 500);
        let got = t.radiant(area, 10);
        assert_eq!(
            got,
            vec![
                (1, rect(0, 0, 250, 500)),
                (2, rect(260, 0, 250, 500)),
                (3, rect(520, 0, 500, 500)),
            ]
        );
    }

    #[test]
    fn insert_at_on_the_same_axis_is_a_sibling() {
        let mut t = tree(2); // [1 | 2]
        assert!(t.insert_at(3, Target::Leaf(&1), Side::Right, 1.0));
        assert_eq!(t.windows(), vec![1, 3, 2]);
        assert!(t.insert_at(4, Target::Leaf(&1), Side::Left, 1.0));
        assert_eq!(t.windows(), vec![4, 1, 3, 2]);
        assert_eq!(splits(&t), 1);
        assert_normalized(&t);
        assert_eq!(rect_of(&t, 4, 0), rect(0, 0, 480, 1080));
    }

    #[test]
    fn insert_at_on_the_other_axis_wraps_the_target() {
        let mut t = tree(2); // [1 | 2]
        assert!(t.insert_at(3, Target::Leaf(&2), Side::Top, 1.0));
        assert_eq!(t.windows(), vec![1, 3, 2]);
        assert_eq!(rect_of(&t, 3, 0), rect(960, 0, 960, 540));
        assert_eq!(rect_of(&t, 2, 0), rect(960, 540, 960, 540));
        assert!(t.insert_at(4, Target::Leaf(&1), Side::Bottom, 1.0));
        assert_eq!(rect_of(&t, 1, 0), rect(0, 0, 960, 540));
        assert_eq!(rect_of(&t, 4, 0), rect(0, 540, 960, 540));
        assert_eq!(splits(&t), 3);
        assert_normalized(&t);
    }

    #[test]
    fn insert_at_a_missing_target_changes_nothing() {
        let mut t = tree(2);
        assert!(!t.insert_at(3, Target::Leaf(&9), Side::Left, 1.0));
        assert_eq!(t.windows(), vec![1, 2]);
    }

    #[test]
    fn wrapping_hands_the_target_weight_to_the_new_container() {
        let mut t = tree(2);
        t.adjust_weight(&2, 2); // 1 : 3
        t.insert_at(3, Target::Leaf(&2), Side::Bottom, 1.0);
        assert_eq!(rect_of(&t, 1, 0).size.w, 480);
        assert_eq!(rect_of(&t, 2, 0), rect(480, 0, 1440, 540));
    }

    #[test]
    fn removing_collapses_single_child_containers() {
        let mut t = tree(3); // [1 | [2 / 3]]
        assert_eq!(splits(&t), 2);
        t.remove(&3);
        assert_eq!(splits(&t), 1);
        assert_normalized(&t);
        assert_eq!(rect_of(&t, 2, 0), rect(960, 0, 960, 1080));
        t.remove(&1);
        assert_eq!(splits(&t), 0);
        assert_eq!(rect_of(&t, 2, 0), area());
        t.remove(&2);
        assert!(t.windows().is_empty());
        assert!(t.radiant(area(), 0).is_empty());
    }

    #[test]
    fn collapse_then_same_axis_flatten_makes_exact_thirds() {
        let mut t = tree(4); // [1 | [2 / [3 | 4]]]
        t.remove(&2); // [1 | [3 | 4]] -> [1 | 3 | 4]
        assert_eq!(splits(&t), 1);
        assert_normalized(&t);
        let area = rect(0, 0, 1500, 900);
        let got: Vec<_> = t.radiant(area, 0).into_iter().map(|(_, r)| r.size.w).collect();
        assert_eq!(got, vec![500, 500, 500]);
    }

    #[test]
    fn root_bands_add_a_full_column_or_row() {
        let mut t = tree(3); // [1 | [2 / 3]]
        t.insert_at(4, Target::Root, Side::Right, 1.0);
        assert_normalized(&t);
        // Same axis as the root: flattened into a third column.
        assert_eq!(rect_of(&t, 4, 0), rect(1280, 0, 640, 1080));
        assert_eq!(rect_of(&t, 1, 0), rect(0, 0, 640, 1080));
        t.insert_at(5, Target::Root, Side::Top, 1.0);
        assert_normalized(&t);
        assert_eq!(rect_of(&t, 5, 0), rect(0, 0, 1920, 540));
        assert_eq!(rect_of(&t, 1, 0), rect(0, 540, 640, 540));
        assert_eq!(t.windows(), vec![5, 1, 2, 3, 4]);
    }

    #[test]
    fn root_band_on_a_single_window() {
        let mut t = tree(1);
        t.insert_at(2, Target::Root, Side::Left, 1.0);
        assert_eq!(rect_of(&t, 2, 0), rect(0, 0, 960, 1080));
        assert_eq!(rect_of(&t, 1, 0), rect(960, 0, 960, 1080));
    }

    #[test]
    fn swap_carries_the_weights() {
        let mut t = tree(2);
        t.adjust_weight(&2, 2); // 1 : 3
        assert_eq!(rect_of(&t, 2, 0).size.w, 1440);
        t.swap(&1, &2);
        assert_eq!(t.windows(), vec![2, 1]);
        assert_eq!(t.weight_of(&2), Some(3.0));
        assert_eq!(rect_of(&t, 2, 0), rect(0, 0, 1440, 1080));
        assert_eq!(rect_of(&t, 1, 0), rect(1440, 0, 480, 1080));
    }

    #[test]
    fn priority_is_clamped_to_one_through_nine() {
        let mut t = tree(2);
        assert!(!t.adjust_weight(&1, -1));
        assert_eq!(t.weight_of(&1), Some(1.0));
        assert!(t.adjust_weight(&1, 20));
        assert_eq!(t.weight_of(&1), Some(9.0));
        assert!(!t.adjust_weight(&1, 1));
        assert!(!t.adjust_weight(&7, 1));
        // A fractional weight left by a resize is rounded before stepping.
        t.set_weight(&2, 2.4);
        t.adjust_weight(&2, 1);
        assert_eq!(t.weight_of(&2), Some(3.0));
    }

    #[test]
    fn resize_solves_for_the_weight_and_clamps() {
        let mut t = tree(2);
        let area = rect(0, 0, 1010, 500);
        assert!(t.resize(&1, area, 10, (Some(250), None)));
        let got = t.radiant(area, 10);
        assert_eq!(got[0].1.size.w, 250);
        // Nothing stacks vertically: no container on that axis.
        assert!(!t.resize(&1, area, 10, (None, Some(100))));
        // A share past the limit is clamped rather than squeezing 2 away.
        t.resize(&1, area, 10, (Some(100_000), None));
        assert_eq!(t.weight_of(&1), Some(20.0));
    }

    #[test]
    fn drop_zone_diagonals_and_centre() {
        let r = rect(100, 100, 300, 300);
        let at = |x: f64, y: f64| drop_zone(r, (x, y).into());
        assert_eq!(at(250.0, 250.0), Zone::Center);
        assert_eq!(at(210.0, 210.0), Zone::Center);
        assert_eq!(at(250.0, 110.0), Zone::Top);
        assert_eq!(at(250.0, 390.0), Zone::Bottom);
        assert_eq!(at(110.0, 250.0), Zone::Left);
        assert_eq!(at(390.0, 250.0), Zone::Right);
        // Outside the centre box, near a corner: the diagonal splits it.
        assert_eq!(at(130.0, 120.0), Zone::Top);
        assert_eq!(at(120.0, 130.0), Zone::Left);
        assert_eq!(at(380.0, 370.0), Zone::Right);
        assert_eq!(at(370.0, 380.0), Zone::Bottom);
        // Just outside the middle third on one axis.
        assert_eq!(at(250.0, 195.0), Zone::Top);
        // Wide rectangles use proportional diagonals.
        let wide = rect(0, 0, 900, 90);
        assert_eq!(drop_zone(wide, (100.0, 45.0).into()), Zone::Left);
        assert_eq!(drop_zone(wide, (450.0, 5.0).into()), Zone::Top);
    }

    #[test]
    fn auto_insert_matches_classic_dwindle_for_one_to_four_windows() {
        // The binary dwindle this tree replaced, with the new window always
        // after its target: [1], [1|2], [1|[2/3]], [1|[2/[3|4]]].
        let want: [&[Rectangle<i32, Logical>]; 4] = [
            &[rect(0, 0, 1920, 1080)],
            &[rect(0, 0, 955, 1080), rect(965, 0, 955, 1080)],
            &[
                rect(0, 0, 955, 1080),
                rect(965, 0, 955, 535),
                rect(965, 545, 955, 535),
            ],
            &[
                rect(0, 0, 955, 1080),
                rect(965, 0, 955, 535),
                rect(965, 545, 472, 535),
                rect(1447, 545, 473, 535),
            ],
        ];
        for (n, want) in (1..=4).zip(want) {
            let t = tree(n);
            let radiant: Vec<_> = t.radiant(area(), 10).into_iter().map(|(_, r)| r).collect();
            assert_eq!(radiant, want, "radiant, {n} windows");
            let classic: Vec<_> = t.dwindle(area(), 10).into_iter().map(|(_, r)| r).collect();
            assert_eq!(classic, want, "dwindle, {n} windows");
            assert_eq!(dwindle_rects(n as usize, area(), 10), want);
        }
    }

    #[test]
    fn classic_dwindle_ignores_weights_and_shape() {
        let mut t = tree(3);
        t.adjust_weight(&1, 5);
        t.insert_at(4, Target::Root, Side::Top, 1.0);
        let got: Vec<_> = t.dwindle(area(), 0).into_iter().collect();
        let want = dwindle_rects(4, area(), 0);
        assert_eq!(got.iter().map(|(w, _)| *w).collect::<Vec<_>>(), vec![4, 1, 2, 3]);
        assert_eq!(got.into_iter().map(|(_, r)| r).collect::<Vec<_>>(), want);
    }

    #[test]
    fn a_pointer_on_the_first_half_puts_the_auto_insert_first() {
        let mut t = Tree::new();
        t.insert(1, None, area(), far());
        t.insert(2, Some(&1), area(), (10.0, 10.0).into());
        assert_eq!(t.windows(), vec![2, 1]);
    }
}
