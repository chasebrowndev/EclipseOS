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
3. **Pane joins.** The client names which pairs of panes join and how softly. The compositor draws joined panes as one signed-distance
   shape (a smooth union of the joined panes' rounded boxes), so bend, bezel, rim and
   hairline all come from the same distance field. Concave joins and a single
   outline follow from the maths.
4. **Arbitrary client-sent outlines.** A path or SDF per pane. Most flexible,
   but it puts a shape language in the protocol and the cost per fragment is
   unbounded.

## Decision
Option 3, expressed as joins between panes rather than a group id on each pane.

- **Contract.** `eclipse-glass-v1` goes to version 3 (manager and surface). A
  new request, `set_joins`, takes pairs of pane indices and a fillet radius, so
  the pane record keeps its nine fields and older clients and compositors are
  unaffected. Joins chain: joined panes form one shape. Joins are
  double-buffered with `set_panes`, which clears them (their indices refer to
  the list it replaces). Limits: at most 64 joins (`too_many_joins`), radius
  capped at 64 px, at most 16 panes in one shape (past that the extra panes
  draw alone, a limit not an error); a bad index or a pane joined to itself is
  `bad_join`. The wire text is in the XML.
- **Render.** A joined shape's panes are unioned with a polynomial smooth minimum of
  their rounded-box distances, so adjacent boxes blend with a concave fillet of
  the join radius. The glass program reads that one distance and its gradient
  for the bend band, the hairline and the seam. A pane with no join draws
  as today, bit for bit. Radius, bezel and tone come from each pane's own
  record; where two panes overlap the blend interpolates them by distance
  weight.
- **Backdrop and tint.** The backdrop is still blurred once per surface over the
  union of all panes. Adaptive tint reads its taps from the fragment's own
  pane, not the shape's bounding box, so a large shape does not change the
  measure for a thin member.
- **Bar client.** `ec-ui` gets a join builder. The bar joins its pill and any
  open drawer, so the pill and drawer are a single lensed shape and
  the drawer's pane is a member of it, not a second surface to match. Cells stay
  alone and keep their own rims, so there is one edge per object.
- **Fallback.** Without the new version, or with glass off, clients draw what
  they draw today. A compositor at a lower version never sees `set_joins`.
- **Cost.** One extra distance evaluation per member per fragment, only inside a
  shape's bounding box and only on the bezel band. To be measured with
  `abyss/bench` and the glass GPU timers before landing; the occlusion cull
  applies to the shape's bounding box.
- **Not TCB.** No trusted UI changes (ADR 0009, COMP-10). Groups give a client
  no new way to impersonate a prompt.

## Consequences
- The drawer joins the bar as real glass; the dark wedge goes away.
- One edge per object: the pill and cells stop competing for the same pixels.
- Owed: protocol XML and `ec-protocols` bindings (Apache-2.0), the shader and
  cache-key change in `ec-abyss-render`, the `ec-ui` builder, the bar's use of
  it, a COMP-02 §9 and COMP-06 §1 Appendix A row, tests for the distance
  blend (symmetry, radius limits, join limits), a headless before/after
  screenshot set, and a bench row.
- Clients cannot send arbitrary shapes. A shape that is not a union of rounded
  boxes needs its own decision.
- The start menu and other sheets that have the same missing pane can adopt
  joins the same way once this lands.

## Revisit when
A design needs a shape that is not a union of rounded boxes (a notch, a tail), or
the per-fragment cost of joined-shape evaluation shows in the glass timers on the
reference hardware.
