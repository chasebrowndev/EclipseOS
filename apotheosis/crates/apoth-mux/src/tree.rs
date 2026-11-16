// SPDX-License-Identifier: AGPL-3.0-only
//! The tiled split tree (T-13 §3): a binary tree, each split with an orientation and a
//! fraction; leaves are a pane or a stack of panes. Pure data; every operation here is a
//! function of the tree and a rectangle.
//!
//! Sizing rule (pinned here, the spec leaves it open): a split's first child gets
//! `round(total * ratio)` cells, clamped so both children keep their minimum size. No cells
//! are spent on gutters; borders are drawn inside pane rectangles by the renderer. When the
//! rectangle is smaller than the sum of the minimums the children share it in proportion
//! to their minimums and may be empty, but the rectangles still tile exactly.

use crate::geom::{Dir, MIN_COLS, MIN_ROWS, Orient, Rect, Size};

/// A pane id. Panes are plain `u64` handles (T-13 §1); `apoth-term` attaches later.
pub type PaneId = u64;

/// Fixed-point denominator for split ratios (parts per million).
pub const RATIO_ONE: u32 = 1_000_000;

/// How a stack shows its non-expanded panes (MUX-17).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StackStyle {
    /// One-line headers stacked above/below the expanded pane.
    Vertical,
    /// One title strip along the top, panes' titles side by side.
    Horizontal,
}

/// A tree leaf holding several panes, one expanded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    /// Members in display order; at least two.
    pub panes: Vec<PaneId>,
    /// Index of the expanded pane.
    pub active: usize,
    /// Header layout.
    pub style: StackStyle,
}

/// A split node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Split {
    /// Orientation.
    pub orient: Orient,
    /// Fraction given to `a`, in `1..RATIO_ONE`.
    pub ratio: u32,
    /// First child (left or top).
    pub a: Node,
    /// Second child (right or bottom).
    pub b: Node,
}

/// A node of the tiled tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A single pane.
    Pane(PaneId),
    /// A stack leaf.
    Stack(Stack),
    /// An inner split.
    Split(Box<Split>),
}

/// One rectangle produced by layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    /// The pane.
    pub pane: PaneId,
    /// Its cells.
    pub rect: Rect,
    /// True for a stack title line/strip segment rather than the pane body. The expanded
    /// pane of a horizontal stack appears twice: its title segment and its body.
    pub header: bool,
}

/// `n / d` as a ratio, clamped into `1..RATIO_ONE`.
pub fn ratio_of(n: u32, d: u32) -> u32 {
    if d == 0 {
        return RATIO_ONE / 2;
    }
    let r = (n as u64 * RATIO_ONE as u64 + d as u64 / 2) / d as u64;
    (r as u32).clamp(1, RATIO_ONE - 1)
}

/// Cells given to the first child of a split of `total` cells.
pub fn split_cells(total: u32, ratio: u32, min_a: u32, min_b: u32) -> u32 {
    if total >= min_a + min_b {
        let want = ((total as u64 * ratio as u64 + RATIO_ONE as u64 / 2) / RATIO_ONE as u64) as u32;
        want.clamp(min_a, total - min_b)
    } else {
        (total as u64 * min_a as u64 / (min_a + min_b).max(1) as u64) as u32
    }
}

impl Split {
    /// The rectangles of both children inside `r`.
    pub fn rects(&self, r: Rect) -> (Rect, Rect) {
        let ma = self.a.min_size();
        let mb = self.b.min_size();
        match self.orient {
            Orient::Horizontal => {
                let wa = split_cells(r.w, self.ratio, ma.cols, mb.cols);
                (
                    Rect::new(r.x, r.y, wa, r.h),
                    Rect::new(r.x + wa, r.y, r.w - wa, r.h),
                )
            }
            Orient::Vertical => {
                let ha = split_cells(r.h, self.ratio, ma.rows, mb.rows);
                (
                    Rect::new(r.x, r.y, r.w, ha),
                    Rect::new(r.x, r.y + ha, r.w, r.h - ha),
                )
            }
        }
    }
}

impl Node {
    /// Smallest size at which every pane keeps its minimum.
    pub fn min_size(&self) -> Size {
        match self {
            Node::Pane(_) => Size {
                cols: MIN_COLS,
                rows: MIN_ROWS,
            },
            Node::Stack(s) => match s.style {
                StackStyle::Vertical => Size {
                    cols: MIN_COLS,
                    rows: s.panes.len() as u32,
                },
                StackStyle::Horizontal => Size {
                    cols: MIN_COLS,
                    rows: 2,
                },
            },
            Node::Split(sp) => {
                let a = sp.a.min_size();
                let b = sp.b.min_size();
                match sp.orient {
                    Orient::Horizontal => Size {
                        cols: a.cols + b.cols,
                        rows: a.rows.max(b.rows),
                    },
                    Orient::Vertical => Size {
                        cols: a.cols.max(b.cols),
                        rows: a.rows + b.rows,
                    },
                }
            }
        }
    }

    /// True when `id` is a pane (or stack member) of this subtree.
    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Pane(p) => *p == id,
            Node::Stack(s) => s.panes.contains(&id),
            Node::Split(sp) => sp.a.contains(id) || sp.b.contains(id),
        }
    }

    /// Pane ids in tree order (stack members in stack order).
    pub fn leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane(p) => out.push(*p),
            Node::Stack(s) => out.extend_from_slice(&s.panes),
            Node::Split(sp) => {
                sp.a.leaves(out);
                sp.b.leaves(out);
            }
        }
    }

    /// Resolve the tree into rectangles inside `rect`.
    pub fn layout(&self, rect: Rect, out: &mut Vec<Placed>) {
        match self {
            Node::Pane(p) => out.push(Placed {
                pane: *p,
                rect,
                header: false,
            }),
            Node::Stack(s) => layout_stack(s, rect, out),
            Node::Split(sp) => {
                let (ra, rb) = sp.rects(rect);
                sp.a.layout(ra, out);
                sp.b.layout(rb, out);
            }
        }
    }

    /// The cells of the leaf (pane or whole stack) holding `id`.
    pub(crate) fn leaf_rect(&self, rect: Rect, id: PaneId) -> Option<Rect> {
        match self {
            Node::Pane(_) | Node::Stack(_) => self.contains(id).then_some(rect),
            Node::Split(sp) => {
                let (ra, rb) = sp.rects(rect);
                if sp.a.contains(id) {
                    sp.a.leaf_rect(ra, id)
                } else {
                    sp.b.leaf_rect(rb, id)
                }
            }
        }
    }

    /// True when this node is a leaf holding `id`.
    fn is_leaf_with(&self, id: PaneId) -> bool {
        !matches!(self, Node::Split(_)) && self.contains(id)
    }

    /// Remove `id`; a stack left with one member collapses to a pane, a split left with one
    /// child is replaced by it. `None` when nothing is left.
    pub(crate) fn remove(self, id: PaneId) -> Option<Node> {
        match self {
            Node::Pane(p) => (p != id).then_some(Node::Pane(p)),
            Node::Stack(mut s) => {
                if let Some(i) = s.panes.iter().position(|&p| p == id) {
                    s.panes.remove(i);
                    if i < s.active {
                        s.active -= 1;
                    }
                    s.active = s.active.min(s.panes.len().saturating_sub(1));
                }
                match s.panes.len() {
                    0 => None,
                    1 => Some(Node::Pane(s.panes[0])),
                    _ => Some(Node::Stack(s)),
                }
            }
            Node::Split(sp) => {
                let Split {
                    orient,
                    ratio,
                    a,
                    b,
                } = *sp;
                match (a.remove(id), b.remove(id)) {
                    (Some(a), Some(b)) => Some(Node::Split(Box::new(Split {
                        orient,
                        ratio,
                        a,
                        b,
                    }))),
                    (Some(x), None) | (None, Some(x)) => Some(x),
                    (None, None) => None,
                }
            }
        }
    }

    /// Replace the leaf holding `target` by a split of it and `new`. `new_ratio` is the
    /// fraction `new` receives. False when `target` is absent.
    pub(crate) fn split_at(
        &mut self,
        target: PaneId,
        new: Node,
        orient: Orient,
        new_ratio: u32,
        new_first: bool,
    ) -> bool {
        match self {
            Node::Split(sp) => {
                if sp.a.contains(target) {
                    sp.a.split_at(target, new, orient, new_ratio, new_first)
                } else {
                    sp.b.split_at(target, new, orient, new_ratio, new_first)
                }
            }
            _ => {
                if !self.contains(target) {
                    return false;
                }
                let old = std::mem::replace(self, Node::Pane(0));
                let (a, b, ratio) = if new_first {
                    (new, old, new_ratio)
                } else {
                    (old, new, RATIO_ONE - new_ratio)
                };
                *self = Node::Split(Box::new(Split {
                    orient,
                    ratio: ratio.clamp(1, RATIO_ONE - 1),
                    a,
                    b,
                }));
                true
            }
        }
    }

    /// Exchange the positions of two panes.
    pub(crate) fn swap_ids(&mut self, x: PaneId, y: PaneId) {
        let map = |p: &mut PaneId| {
            if *p == x {
                *p = y;
            } else if *p == y {
                *p = x;
            }
        };
        match self {
            Node::Pane(p) => map(p),
            Node::Stack(s) => s.panes.iter_mut().for_each(map),
            Node::Split(sp) => {
                sp.a.swap_ids(x, y);
                sp.b.swap_ids(x, y);
            }
        }
    }

    /// Move the border on the `dir` side of `id` outward by `amount` cells (negative
    /// shrinks). The nearest ancestor split whose border lies on that side is used.
    /// Returns true when such a border exists (even when clamped to no change).
    pub(crate) fn resize(&mut self, rect: Rect, id: PaneId, dir: Dir, amount: i32) -> bool {
        let Node::Split(sp) = self else { return false };
        let (ra, rb) = sp.rects(rect);
        let in_a = sp.a.contains(id);
        if !in_a && !sp.b.contains(id) {
            return false;
        }
        let done = if in_a {
            sp.a.resize(ra, id, dir, amount)
        } else {
            sp.b.resize(rb, id, dir, amount)
        };
        if done {
            return true;
        }
        if sp.orient != dir.axis() || in_a != dir.is_forward() {
            return false;
        }
        let (ma, mb) = (sp.a.min_size(), sp.b.min_size());
        let (total, cur_a, min_a, min_b) = match sp.orient {
            Orient::Horizontal => (rect.w, ra.w, ma.cols, mb.cols),
            Orient::Vertical => (rect.h, ra.h, ma.rows, mb.rows),
        };
        if total < min_a + min_b {
            return false;
        }
        let want = if in_a {
            cur_a as i64 + amount as i64
        } else {
            cur_a as i64 - amount as i64
        };
        let new_a = want.clamp(min_a as i64, (total - min_b) as i64) as u32;
        sp.ratio = ratio_of(new_a, total);
        true
    }

    fn weight(&self, o: Orient) -> u32 {
        match self {
            Node::Split(sp) if sp.orient == o => sp.a.weight(o) + sp.b.weight(o),
            _ => 1,
        }
    }

    /// Give every leaf of a same-orientation run an equal share, recursively.
    pub(crate) fn equalize(&mut self) {
        if let Node::Split(sp) = self {
            let wa = sp.a.weight(sp.orient);
            let wb = sp.b.weight(sp.orient);
            sp.ratio = ratio_of(wa, wa + wb);
            sp.a.equalize();
            sp.b.equalize();
        }
    }

    fn take_leaves(&self, out: &mut Vec<Node>) {
        match self {
            Node::Split(sp) => {
                sp.a.take_leaves(out);
                sp.b.take_leaves(out);
            }
            leaf => out.push(leaf.clone()),
        }
    }

    fn put_leaves(&mut self, it: &mut std::vec::IntoIter<Node>) {
        match self {
            Node::Split(sp) => {
                sp.a.put_leaves(it);
                sp.b.put_leaves(it);
            }
            leaf => {
                if let Some(n) = it.next() {
                    *leaf = n;
                }
            }
        }
    }

    /// Rotate the leaves through the fixed tree shape by one position.
    pub(crate) fn rotate_leaves(&mut self, forward: bool) {
        let mut v = Vec::new();
        self.take_leaves(&mut v);
        if forward {
            v.rotate_right(1);
        } else {
            v.rotate_left(1);
        }
        self.put_leaves(&mut v.into_iter());
    }

    /// Flip the orientation of the split directly above the leaf holding `id`.
    pub(crate) fn toggle_parent_orient(&mut self, id: PaneId) -> bool {
        let Node::Split(sp) = self else { return false };
        if sp.a.is_leaf_with(id) || sp.b.is_leaf_with(id) {
            sp.orient = sp.orient.flip();
            return true;
        }
        sp.a.toggle_parent_orient(id) || sp.b.toggle_parent_orient(id)
    }

    /// Add `pane` to the leaf holding `target`, making a stack when needed; the new member
    /// becomes the expanded one.
    pub(crate) fn add_to_leaf(&mut self, target: PaneId, pane: PaneId) -> bool {
        match self {
            Node::Split(sp) => sp.a.add_to_leaf(target, pane) || sp.b.add_to_leaf(target, pane),
            Node::Stack(s) => {
                if !s.panes.contains(&target) {
                    return false;
                }
                s.panes.push(pane);
                s.active = s.panes.len() - 1;
                true
            }
            Node::Pane(p) => {
                if *p != target {
                    return false;
                }
                *self = Node::Stack(Stack {
                    panes: vec![target, pane],
                    active: 1,
                    style: StackStyle::Vertical,
                });
                true
            }
        }
    }

    /// Expand `id` inside its stack (no-op for plain panes).
    pub(crate) fn activate(&mut self, id: PaneId) {
        match self {
            Node::Stack(s) => {
                if let Some(i) = s.panes.iter().position(|&p| p == id) {
                    s.active = i;
                }
            }
            Node::Split(sp) => {
                sp.a.activate(id);
                sp.b.activate(id);
            }
            Node::Pane(_) => {}
        }
    }

    /// The stack holding `id`, if any.
    pub fn stack_of(&self, id: PaneId) -> Option<&Stack> {
        match self {
            Node::Stack(s) => s.panes.contains(&id).then_some(s),
            Node::Split(sp) => sp.a.stack_of(id).or_else(|| sp.b.stack_of(id)),
            Node::Pane(_) => None,
        }
    }

    pub(crate) fn stack_of_mut(&mut self, id: PaneId) -> Option<&mut Stack> {
        match self {
            Node::Stack(s) => s.panes.contains(&id).then_some(s),
            Node::Split(sp) => {
                if sp.a.contains(id) {
                    sp.a.stack_of_mut(id)
                } else {
                    sp.b.stack_of_mut(id)
                }
            }
            Node::Pane(_) => None,
        }
    }

    /// Structural well-formedness: ratios in range, stacks of at least two with a valid
    /// active index, no repeated pane.
    pub fn check(&self) -> Result<(), String> {
        let mut ids = Vec::new();
        self.leaves(&mut ids);
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() != n {
            return Err("duplicate pane in tree".into());
        }
        self.check_shape()
    }

    fn check_shape(&self) -> Result<(), String> {
        match self {
            Node::Pane(_) => Ok(()),
            Node::Stack(s) => {
                if s.panes.len() < 2 || s.active >= s.panes.len() {
                    Err("malformed stack".into())
                } else {
                    Ok(())
                }
            }
            Node::Split(sp) => {
                if sp.ratio == 0 || sp.ratio >= RATIO_ONE {
                    return Err("ratio out of range".into());
                }
                sp.a.check_shape()?;
                sp.b.check_shape()
            }
        }
    }
}

fn layout_stack(s: &Stack, r: Rect, out: &mut Vec<Placed>) {
    let n = s.panes.len() as u32;
    match s.style {
        StackStyle::Vertical => {
            let hdrs = n.saturating_sub(1).min(r.h.saturating_sub(1));
            let body_h = r.h - hdrs;
            let mut y = r.y;
            let mut left = hdrs;
            for (i, &p) in s.panes.iter().enumerate() {
                if i == s.active {
                    out.push(Placed {
                        pane: p,
                        rect: Rect::new(r.x, y, r.w, body_h),
                        header: false,
                    });
                    y += body_h;
                } else if left > 0 {
                    out.push(Placed {
                        pane: p,
                        rect: Rect::new(r.x, y, r.w, 1),
                        header: true,
                    });
                    y += 1;
                    left -= 1;
                }
            }
        }
        StackStyle::Horizontal => {
            let mut body = r;
            if r.h >= 2 && n > 0 {
                let (base, rem) = (r.w / n, r.w % n);
                let mut x = r.x;
                for (i, &p) in s.panes.iter().enumerate() {
                    let w = base + u32::from((i as u32) < rem);
                    out.push(Placed {
                        pane: p,
                        rect: Rect::new(x, r.y, w, 1),
                        header: true,
                    });
                    x += w;
                }
                body = Rect::new(r.x, r.y + 1, r.w, r.h - 1);
            }
            if let Some(&p) = s.panes.get(s.active) {
                out.push(Placed {
                    pane: p,
                    rect: body,
                    header: false,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u64) -> Node {
        Node::Pane(id)
    }

    fn hsplit(a: Node, b: Node, ratio: u32) -> Node {
        Node::Split(Box::new(Split {
            orient: Orient::Horizontal,
            ratio,
            a,
            b,
        }))
    }

    #[test]
    fn halves_sum() {
        let t = hsplit(pane(1), pane(2), RATIO_ONE / 2);
        let mut out = vec![];
        t.layout(Rect::new(0, 0, 81, 24), &mut out);
        assert_eq!(out[0].rect, Rect::new(0, 0, 41, 24));
        assert_eq!(out[1].rect, Rect::new(41, 0, 40, 24));
    }

    #[test]
    fn minimums_hold() {
        let t = hsplit(pane(1), pane(2), 1);
        let mut out = vec![];
        t.layout(Rect::new(0, 0, 10, 5), &mut out);
        assert_eq!(out[0].rect.w, MIN_COLS);
        let t = hsplit(pane(1), pane(2), RATIO_ONE - 1);
        let mut out = vec![];
        t.layout(Rect::new(0, 0, 10, 5), &mut out);
        assert_eq!(out[1].rect.w, MIN_COLS);
    }

    #[test]
    fn too_small_still_tiles() {
        let t = hsplit(pane(1), pane(2), RATIO_ONE / 2);
        let mut out = vec![];
        t.layout(Rect::new(0, 0, 3, 5), &mut out);
        assert_eq!(out[0].rect.w + out[1].rect.w, 3);
    }

    #[test]
    fn remove_collapses() {
        let t = hsplit(pane(1), hsplit(pane(2), pane(3), 500_000), 500_000);
        let t = t.remove(2).unwrap();
        let mut v = vec![];
        t.leaves(&mut v);
        assert_eq!(v, [1, 3]);
        assert!(t.remove(1).unwrap().remove(3).is_none());
    }

    #[test]
    fn stack_remove_active_and_collapse() {
        let s = Node::Stack(Stack {
            panes: vec![1, 2, 3],
            active: 2,
            style: StackStyle::Vertical,
        });
        let s = s.remove(3).unwrap();
        let Node::Stack(st) = &s else { panic!() };
        assert_eq!((st.panes.as_slice(), st.active), (&[1, 2][..], 1));
        assert_eq!(s.remove(1), Some(Node::Pane(2)));
    }

    #[test]
    fn stack_vertical_headers() {
        let s = Node::Stack(Stack {
            panes: vec![1, 2, 3],
            active: 1,
            style: StackStyle::Vertical,
        });
        let mut out = vec![];
        s.layout(Rect::new(0, 0, 10, 6), &mut out);
        assert_eq!(
            out[0],
            Placed {
                pane: 1,
                rect: Rect::new(0, 0, 10, 1),
                header: true
            }
        );
        assert_eq!(
            out[1],
            Placed {
                pane: 2,
                rect: Rect::new(0, 1, 10, 4),
                header: false
            }
        );
        assert_eq!(
            out[2],
            Placed {
                pane: 3,
                rect: Rect::new(0, 5, 10, 1),
                header: true
            }
        );
    }

    #[test]
    fn resize_moves_nearest_border() {
        let mut t = hsplit(pane(1), pane(2), RATIO_ONE / 2);
        let r = Rect::new(0, 0, 100, 10);
        assert!(t.resize(r, 1, Dir::Right, 10));
        let mut out = vec![];
        t.layout(r, &mut out);
        assert_eq!(out[0].rect.w, 60);
        // pane 2 growing left shrinks pane 1 back
        assert!(t.resize(r, 2, Dir::Left, 10));
        out.clear();
        t.layout(r, &mut out);
        assert_eq!(out[0].rect.w, 50);
        // pane 1 has no border on its left
        assert!(!t.resize(r, 1, Dir::Left, 3));
        assert!(!t.resize(r, 1, Dir::Down, 3));
    }

    #[test]
    fn equalize_chain() {
        let mut t = hsplit(pane(1), hsplit(pane(2), pane(3), 900_000), 100_000);
        t.equalize();
        let mut out = vec![];
        t.layout(Rect::new(0, 0, 90, 4), &mut out);
        assert_eq!(
            out.iter().map(|p| p.rect.w).collect::<Vec<_>>(),
            [30, 30, 30]
        );
    }

    #[test]
    fn rotate_leaves_cycles() {
        let mut t = hsplit(pane(1), hsplit(pane(2), pane(3), 500_000), 500_000);
        t.rotate_leaves(true);
        let mut v = vec![];
        t.leaves(&mut v);
        assert_eq!(v, [3, 1, 2]);
        t.rotate_leaves(false);
        v.clear();
        t.leaves(&mut v);
        assert_eq!(v, [1, 2, 3]);
    }

    #[test]
    fn split_at_orders() {
        let mut t = pane(1);
        assert!(t.split_at(1, pane(2), Orient::Horizontal, 250_000, true));
        let mut out = vec![];
        t.layout(Rect::new(0, 0, 80, 5), &mut out);
        assert_eq!((out[0].pane, out[0].rect.w), (2, 20));
        assert!(!t.split_at(9, pane(3), Orient::Horizontal, 1, false));
    }
}
