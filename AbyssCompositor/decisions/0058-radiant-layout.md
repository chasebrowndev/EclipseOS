# 0058 — Radiant: weighted n-ary tiling with drop zones
Status: accepted
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
The binary tree of ADR 0021 cannot express several arrangements the owner wants to reach directly. Three equal columns are impossible, because a binary split gives 1/2, 1/4, 1/4. A thin | thick | thin sandwich is impossible for the same reason. A 2x2 grid can only be had by luck of insertion order.

The only weight is a per-split `ratio`, set only by the `resize` method. Dragging a tiled window floats it (ADR 0057), and nothing ever tiles it back.

The owner asked for "dwindle, but with the ability to do more", and ruled on the rest:
- Per-window priority, bound to Super+Shift+Up/Down.
- Drag-to-tile by drop position, with no aiming for edges.
- Guides drawn while dragging.
- Classic dwindle and master kept as alternatives.

## Options
1. **Keep the binary tree and derive ratios from subtree weight sums along same-axis chains.** The spec stays binary. But every traversal must know which ancestors share an axis, and a drop has to rebalance the chain.
2. **A flat row with weights.** Trivial to build. It cannot stack windows vertically, which the owner rejected.
3. **An n-ary weighted arena tree.** Each split holds any number of children along one axis, and each node carries a weight. Same arena, ids and free list as 0021.

## Decision
Option 3, as the new default layout, **radiant**. `shell::layout::Tree` becomes n-ary: a split holds a `Vec<NodeId>` of children and an axis, and every node has a weight (default 1.0).

**Geometry.** A child's share of its split's axis is its weight over the sum of its siblings' weights. The last child takes the rounding remainder.

**Normalization.** It runs after every mutation:
- A split with one child collapses into that child.
- A child split that runs on its parent's axis is flattened into the parent.
- The result: three columns are three siblings.

**New windows.** They still split the target leaf along its longer side, and the pointer picks the half. This matches dwindle's feel.

**Priority.** A window's priority is its leaf weight: 1..9, changed by `priority-up` and `priority-down`. It follows the window through `swap`, and through float and unfloat.

**Dragging.** Dragging a tiled window leaves its leaf in place as a placeholder, so the other windows do not reflow mid-aim. The drop position then decides the result:
- Each other tile is cut by its diagonals into four sides, plus a centre third.
- Dropping on a side splits that tile on that side: a sibling insert if the parent split runs on that axis, a wrap otherwise.
- Dropping on the centre swaps the two windows.
- An edge band of the tiling area inserts at the root, giving a full-height column or a full-width row.
- Dropping on the window's own tile, or outside the tiling area, restores it.

A floating window tiles on drop only while Super is held.

**Guides.** While a drag is in flight, the compositor draws tile outlines, the edge bands and a ghost of the landing rect. Three config keys control them: `drop-guides`, `drop-guide-color` (default: the active-border accent) and `drop-edge-band`.

**Classic dwindle and master.** `dwindle` (labelled Dwindle Classic) and `master` stay available. Both are pure functions of the tree's in-order leaves and ignore weights. Toggling between all three layouts is lossless. Drop zones and priority apply only under radiant. Under dwindle and master, a drag keeps ADR 0057's float-on-first-motion.

## Consequences
- Easier: equal thirds, sandwiches and grids by drag, and resizing by weight instead of by ratio.
- Easier: `Tree` is generic over the leaf type, so the layout is unit-tested on integers without a live client.
- Harder: tree surgery now edits vectors of children, and normalization must hold after every insert and remove.
- Harder: the ghost preview simulates a drop on a clone of the tree. It is computed only when the (target, zone) pair changes, which keeps it off the per-motion path.
- Supersedes ADR 0021's "splits hold two child ids, an axis and a ratio". Everything else in 0021 stands: the arena, the handles and the ban on `user_data`.
- Amends ADR 0057: under radiant, a tiled window is not floated on first motion.
- The `resize` method on a tiled window works only under radiant, where it solves for a weight. It is refused under dwindle and master.
- Spec: COMP-05 §3 and §3.1 are amended in place (Appendix, C-10).

## Revisit when
Owner use shows that the diagonal zones are misread, for example when drops land on the wrong side of small tiles. Also revisit if a keyboard-only way to place a window, a `preselect`-style bind (COMP-05 §11 item 2), is asked for.
