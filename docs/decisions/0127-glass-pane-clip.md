# 0127 — Glass pane clip (eclipse_glass_v1 version 2)
Status: proposed
Date: 2026-11-19
Deciders: chase (owner), Claude (advisory)

## Context
`ec_ui::glass::scroll_region` places a pane by the scroll offset and cut it to
the viewport on the client. The compositor then saw a smaller box and drew
glass with all four corner radii, so a card cut by the window edge ended in a
rounded top or bottom (KNOWNBUGS GLASS-03) where the content runs straight
through. A cut card should end in a straight cut that implies more beyond.

## Options
1. **Per-corner radius in the pane record.** The client squares the cut side.
   Still draws a rim and bezel shading along the cut, and changes the record
   layout (36 bytes, parsed by v1 compositors).
2. **Client-side hacks:** over-extend the pane past the viewport and let iced
   clip the content. The glass is compositor-drawn and iced cannot clip it.
3. **A clip request.** The client sends the whole box and a clip rectangle;
   the compositor draws the part inside the clip from the whole box's shape.

## Decision
We take **3**. Version 2 of `eclipse_glass_v1` adds
`eclipse_glass_surface_v1.set_clip(x, y, width, height)` and a pane state bit
`clipped` (4).

- Surface-local logical px, double-buffered with `set_panes`, latched at the
  same commit, one clip per surface. The clip persists across `set_panes`.
- Only `clipped` panes are cut, so panes outside a scroll region (a sidebar)
  that sit beyond the clip rectangle are unaffected.
- Pane boxes are the full outline: radius, bezel and rim are computed from the
  whole box, so a cut is straight, with no corner rounding, rim or edge shading.
  The compositor draws the part inside the clip and the surface; the backdrop
  region and element box are that visible part (`PaneRequest::rect`), the shape
  uniforms the whole box (`PaneRequest::shape`), and a contact shadow is cut to
  the clip (`PaneRequest::cut`).
- A pane wholly outside the clip is not drawn. A zero width or height clears
  the clip. A negative one is `bad_pane`.
- It reuses the crop mechanism (`CropRenderElement`) used for output edges.
  The layer, window and subsurface paths all go through `PaneRequest`.

## Consequences
- Fail-soft: a v1 compositor never sees `set_clip` (the client binds 1..=2 and
  checks the version), and the client falls back to cutting the pane box
  itself, as before: a rounded cut. A v2 compositor with a v1 client is
  unchanged: no clip, no `clipped` bit.
- Panes and clip grant nothing; the client still gets no backdrop pixels.
- Tests: clip math (shape kept whole, cut box, zero/negative, outside) in
  `ec-abyss-render`, request parsing in the handler, the v1/v2 client path in
  `ec-ui`.
- A shape not cut to the surface is allowed for `clipped` panes; its box is
  still bounded by the clip, which is cut to the surface.

## Revisit when
A surface needs two clips at once (nested scroll regions) or a rounded clip.
