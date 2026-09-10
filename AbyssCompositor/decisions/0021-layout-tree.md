# 0021 — Per-workspace arena binary tree for tiling
Status: accepted
Date: 2026-09-06
Deciders: chase (owner), Claude (advisory)

## Context
Phase 1 M2 needs dwindle (Hyprland's default) and master tiling, per workspace,
with floating windows on top, on a compositor whose root invariants forbid
`Rc<RefCell<_>>` graphs and locks on the hot path (ADR 0018, root CLAUDE.md).

Dwindle is inherently a binary split tree: each new window splits the focused
leaf along its longer dimension. Master is a flat list with a ratio. Both must
coexist per workspace and be switchable at runtime (`Super+Space`), and both
must produce geometry for a rectangle that changes as layer-shell exclusive
zones appear (COMP-05, COMP-06).

## Options
1. **Pointer tree (`Rc<RefCell<Node>>`).** Natural to write; violates the
   handle-based-state invariant, makes the tree unmovable across workspaces and
   hard to reason about for the TCB review of the input/focus path.
2. **Flat `Vec<Window>` per workspace, geometry computed by the layout
   function.** Trivial; loses dwindle's split history entirely — insertion order
   is not enough to reproduce Hyprland-like dwindle after closes and swaps.
3. **Arena binary tree: `Vec<Option<Node>>` + free list, children as `u32`
   ids.** One owned struct per workspace, no interior mutability, cheap to
   clone/move, indices are stable handles. Master reads the same tree's leaf
   order and ignores the split structure.

## Decision
Option 3. `shell::layout::Tree` is an arena of `Option<Node>` with a free list;
`NodeId` is a `u32` index and `NIL = u32::MAX` marks "no node" (so `Default`
is written by hand — a derived `Default` would make node 0, a valid id, the
root). Leaves hold a `Window`, splits hold two child ids, an axis and a ratio.
`dwindle()` walks the tree assigning rectangles by splitting the longer
dimension; `master()` walks the same leaf order and ignores splits, so
switching layouts is a per-workspace enum flip with no data migration. Each
`Workspace` owns one `Tree` plus a `Vec<Floating>`; only the active workspace's
windows are mapped into the single `Space`.

## Consequences
- Easier: workspace switching (unmap/remap the active set), layout toggling,
  moving a window between workspaces, and reviewing the shell for the
  no-ambient-authority and single-threaded invariants — it is plain data.
- Harder: tree surgery reads less obviously than pointer code; every traversal
  is an explicit id walk, and removing a leaf must collapse its parent split.
- Forbidden: storing per-window layout state in `Window::user_data()` (that
  requires `Send + Sync` and would reintroduce shared mutability). Border
  buffers live in `AbyssState.borders` for the same reason.
- Owed: unit tests for insert/remove/geometry across both layouts beyond the
  current config tests, and a resize-by-ratio binding (M3).

## Revisit when
Multi-monitor layouts (COMP-03) need a tree per output rather than per
workspace, or when agent workspaces (ADR 0011, untiled) require a third layout
kind that the split tree cannot express.
