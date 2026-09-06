// SPDX-License-Identifier: AGPL-3.0-only
//! Tiling layouts (COMP-05 §2-§3).
//!
//! One binary tree per workspace, stored as an arena and referenced by `u32`
//! handles — no `Rc<RefCell<_>>` graph (root invariant). Dwindle walks the
//! tree; master ignores its shape and uses only its in-order window sequence,
//! so toggling between the two is lossless. See `decisions/0021-layout-tree.md`.

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle, Size},
};

pub type NodeId = u32;
const NIL: NodeId = u32::MAX;

#[derive(Debug)]
enum Kind {
    Leaf(Window),
    Split {
        a: NodeId,
        b: NodeId,
        side_by_side: bool,
        ratio: f64,
    },
}

#[derive(Debug)]
struct Node {
    parent: NodeId,
    kind: Kind,
}

/// Binary layout tree. Empty slots are recycled through `free`.
#[derive(Debug)]
pub struct Tree {
    nodes: Vec<Option<Node>>,
    free: Vec<NodeId>,
    root: NodeId,
}

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
}

impl Tree {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NIL,
        }
    }

    fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id as usize).and_then(|n| n.as_ref())
    }

    fn alloc(&mut self, node: Node) -> NodeId {
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
        if let Some(Some(n)) = self.nodes.get_mut(id as usize) {
            n.parent = parent;
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
                Some(Kind::Split { a, b, .. }) => {
                    stack.push(*b);
                    stack.push(*a);
                }
                None => {}
            }
        }
        out
    }

    pub fn windows(&self) -> Vec<Window> {
        self.leaf_ids()
            .into_iter()
            .filter_map(|id| match &self.get(id)?.kind {
                Kind::Leaf(w) => Some(w.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn contains(&self, window: &Window) -> bool {
        self.leaf_of(window).is_some()
    }

    fn leaf_of(&self, window: &Window) -> Option<NodeId> {
        self.leaf_ids()
            .into_iter()
            .find(|id| matches!(self.get(*id).map(|n| &n.kind), Some(Kind::Leaf(w)) if w == window))
    }

    /// Insert `window` by splitting the leaf holding `near` (or the last leaf).
    /// The split runs along the longer dimension of that leaf's current
    /// rectangle — COMP-05 §11 open question 2, resolved as proposed.
    pub fn insert(&mut self, window: Window, near: Option<&Window>, area: Rectangle<i32, Logical>) {
        if self.root == NIL {
            self.root = self.alloc(Node {
                parent: NIL,
                kind: Kind::Leaf(window),
            });
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

        let parent = self.get(target).map(|n| n.parent).unwrap_or(NIL);
        let new_leaf = self.alloc(Node {
            parent: NIL,
            kind: Kind::Leaf(window),
        });
        let split = self.alloc(Node {
            parent,
            kind: Kind::Split {
                a: target,
                b: new_leaf,
                side_by_side,
                ratio: 0.5,
            },
        });
        self.set_parent(target, split);
        self.set_parent(new_leaf, split);
        if parent == NIL {
            self.root = split;
        } else if let Some(Some(p)) = self.nodes.get_mut(parent as usize) {
            if let Kind::Split { a, b, .. } = &mut p.kind {
                if *a == target {
                    *a = split;
                } else if *b == target {
                    *b = split;
                }
            }
        }
    }

    /// Remove a window; its sibling takes the parent's place.
    pub fn remove(&mut self, window: &Window) -> bool {
        let Some(leaf) = self.leaf_of(window) else {
            return false;
        };
        let parent = self.get(leaf).map(|n| n.parent).unwrap_or(NIL);
        self.release(leaf);
        if parent == NIL {
            self.root = NIL;
            return true;
        }
        let (a, b) = match self.get(parent).map(|n| &n.kind) {
            Some(Kind::Split { a, b, .. }) => (*a, *b),
            _ => return true,
        };
        let sibling = if a == leaf { b } else { a };
        let grandparent = self.get(parent).map(|n| n.parent).unwrap_or(NIL);
        self.release(parent);
        self.set_parent(sibling, grandparent);
        if grandparent == NIL {
            self.root = sibling;
        } else if let Some(Some(g)) = self.nodes.get_mut(grandparent as usize) {
            if let Kind::Split { a, b, .. } = &mut g.kind {
                if *a == parent {
                    *a = sibling;
                } else if *b == parent {
                    *b = sibling;
                }
            }
        }
        true
    }

    /// Exchange the positions of two windows in the tree.
    pub fn swap(&mut self, x: &Window, y: &Window) {
        let (Some(ix), Some(iy)) = (self.leaf_of(x), self.leaf_of(y)) else {
            return;
        };
        if ix == iy {
            return;
        }
        let (Some(a), Some(b)) = (self.nodes.get(ix as usize), self.nodes.get(iy as usize)) else {
            return;
        };
        let (wa, wb) = match (a.as_ref().map(|n| &n.kind), b.as_ref().map(|n| &n.kind)) {
            (Some(Kind::Leaf(wa)), Some(Kind::Leaf(wb))) => (wa.clone(), wb.clone()),
            _ => return,
        };
        if let Some(Some(n)) = self.nodes.get_mut(ix as usize) {
            n.kind = Kind::Leaf(wb);
        }
        if let Some(Some(n)) = self.nodes.get_mut(iy as usize) {
            n.kind = Kind::Leaf(wa);
        }
    }

    /// (leaf id, window, rectangle) for the dwindle arrangement.
    fn geometries(
        &self,
        area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<(NodeId, Window, Rectangle<i32, Logical>)> {
        let mut out = Vec::new();
        if self.root == NIL {
            return out;
        }
        let mut stack = vec![(self.root, area)];
        while let Some((id, rect)) = stack.pop() {
            match self.get(id).map(|n| &n.kind) {
                Some(Kind::Leaf(w)) => out.push((id, w.clone(), rect)),
                Some(Kind::Split {
                    a,
                    b,
                    side_by_side,
                    ratio,
                }) => {
                    let (ra, rb) = split_rect(rect, *side_by_side, *ratio, gap);
                    stack.push((*b, rb));
                    stack.push((*a, ra));
                }
                None => {}
            }
        }
        out
    }

    pub fn dwindle(&self, area: Rectangle<i32, Logical>, gap: i32) -> Vec<(Window, Rectangle<i32, Logical>)> {
        self.geometries(area, gap)
            .into_iter()
            .map(|(_, w, r)| (w, r))
            .collect()
    }

    /// Master: first window on the left half, the rest stacked on the right.
    /// With one window it fills the area.
    pub fn master(&self, area: Rectangle<i32, Logical>, gap: i32) -> Vec<(Window, Rectangle<i32, Logical>)> {
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
