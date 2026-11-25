<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0130 — Joined glass: pane groups drawn as one shape
Status: proposed
Date: 2026-11-24
Deciders: chase (owner), Claude (advisory)

## Context
`eclipse_glass_v1` panes (ADR 0081) are rounded rectangles, each drawn on its
own: its own bend, hairline and bezel band. Two things follow on the bar, both
measured in the hidden headless session on the merged-main build.

- **A drawer hangs off the bar with a dark wedge at the inside corner.** Where
  the bar's bottom edge meets the drawer's side the join is concave, and a
  rounded rectangle cannot express a concave corner. The client paints it as a
  tint over the wallpaper (`ShapedSheet`, `sheet_outline`), so the joint is the
  one place that is not glass.
- **Edges double up.** The pill behind the cells carries its own pane rim a few
  px above each cell's rim. Two nested lensed edges read as a doubled border.

Layer-level glass already has what is needed for the first case: a shape of up
to four unioned boxes with a fillet (`Shape` in `ec-abyss-render/src/blur.rs`,
`shape_sd` and `shape_grad` in `effects.rs`). Panes do not use it, because a
surface with panes replaces its whole-layer glass (ADR 0081). So the machinery
exists on one path and the client cannot reach it from the other.

## Options
1. **Accept the tinted join.** Do nothing. The wedge stays; edges still double.
2. **Draw the join client-side.** A glass-coloured fill drawn by iced over the
   corner. It cannot sample the backdrop, so it never matches the lens beside
   it. This is the situation ADR 0081 set out to leave.
3. **Pane groups.** The client names which panes belong to one joined shape and
   how softly they join. The compositor draws a group as one signed-distance
   shape (a smooth union of its panes' rounded boxes), so bend, bezel, rim and
   hairline all come from the same distance field. Concave joins and a single
   outline follow from the maths.
4. **Arbitrary client-sent outlines.** A path or SDF per pane. Most flexible,
   but it puts a shape language in the protocol and the cost per fragment is
   unbounded.

## Decision
Option 3. A pane gains an optional group id and the group a join radius.

- **Contract.** `eclipse-glass-v1` bumps to the next version. The pane record
  gains `group` (u32; 0 means none) and the surface gains a `set_group_join`
  request (group id, join radius in logical px). Panes with the same non-zero
  group form one shape. Group limits: at most 8 groups per surface, at most 16
  panes per group, join radius 0..=64. Over any limit the compositor draws the
  excess panes alone, never fails the commit.
- **Render.** A group's panes are unioned with a polynomial smooth minimum of
  their rounded-box distances, so adjacent boxes blend with a concave fillet of
  the join radius. The glass program reads that one distance and its gradient
  for the bend band, the hairline and the seam. A pane outside any group draws
  as today, bit for bit. Radius, bezel and tone come from each pane's own
  record; where two panes overlap the blend interpolates them by distance
  weight.
- **Backdrop and tint.** The backdrop is still blurred once per surface over the
  union of all panes. Adaptive tint reads its taps from the fragment's own
  pane, not the group's bounding box, so a fat group does not change the
  measure for a thin member.
- **Bar client.** `ec-ui` gets a `Group` builder. The bar puts its pill and any
  open drawer in one group, so the pill and drawer are a single lensed shape and
  the drawer's pane is a member of it, not a second surface to match. Cells stay
  alone and keep their own rims, so there is one edge per object.
- **Fallback.** Without the new version, or with glass off, clients draw what
  they draw today. Group ids are ignored by compositors that do not know them.
- **Cost.** One extra distance evaluation per member per fragment, only inside a
  group's bounding box and only on the bezel band. To be measured with
  `abyss/bench` and the glass GPU timers before landing; the occlusion cull
  applies to the group's bounding box.
- **Not TCB.** No trusted UI changes (ADR 0009, COMP-10). Groups give a client
  no new way to impersonate a prompt.

## Consequences
- The drawer joins the bar as real glass; the dark wedge goes away.
- One edge per object: the pill and cells stop competing for the same pixels.
- Owed: protocol XML and `ec-protocols` bindings (Apache-2.0), the shader and
  cache-key change in `ec-abyss-render`, the `ec-ui` builder, the bar's use of
  it, a COMP-02 §9 and COMP-06 §1 Appendix A row, tests for the distance
  blend (symmetry, radius limits, group limits), a headless before/after
  screenshot set, and a bench row.
- Clients cannot send arbitrary shapes. A shape that is not a union of rounded
  boxes needs its own decision.
- The start menu and other sheets that have the same missing pane can adopt
  groups the same way once this lands.

## Revisit when
A design needs a shape that is not a union of rounded boxes (a notch, a tail), or
the per-fragment cost of group evaluation shows in the glass timers on the
reference hardware.
