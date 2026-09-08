# 0031 — Resizing a window over the control socket

Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context

COMP-13 §2.1 lists `resize` as a Phase-1 command and gives it an absolute
logical size, but the shell had no resize primitive at all. A floating window
owns a rectangle and can simply be told a new one. A tiled window owns nothing:
its size is a consequence of the split ratios above it in the layout tree, and
under the master layout it is not even that — `master()` arranges from the
window *order* and ignores the tree's shape entirely, so a ratio written into
the tree would have nowhere to land.

`set_output` raised a smaller version of the same question for `vrr`: the
persisted output record (`SavedOutput`) has fields for position, scale, mode,
transform and enabled, and none for adaptive sync.

## Options

1. Floating only — refuse `resize` for tiled windows.
2. Resize tiled windows by converting them to floating first.
3. Re-solve split ratios for tiled windows, and refuse only where the layout
   genuinely has no size to change.

For `vrr`: persist it (new field, new schema revision) or treat it as runtime
state that the config owns.

## Decision

Option 3. `Tree::resize` inverts the ratio arithmetic of `split_rect` at the
nearest ancestor split along each requested axis and clamps the result to
[0.05, 0.95]; the request names the *window's* size and the shell adds
`2 * border_size` to reach the tile size. A window with no ancestor split along
an axis (a lone leaf, or a request for width in a stacked column) is not an
error — the reply is `changed: false`. Under `LayoutKind::Master` a tiled
resize is refused with `invalid_params` rather than accepted as a silent no-op,
because there the tree is not consulted at all and no ratio could ever have an
effect.

`vrr` is applied at runtime through the backend (`backend::set_output_vrr` →
`drm::set_vrr`) and is **not** persisted. `vrr: true` on an output whose
connector does not advertise adaptive sync — including every output under the
winit backend — is `invalid_params`; `vrr: false` is always accepted.

## Consequences

- One shell entry point, `shell::resize_window`, covers both window kinds and
  validates before mutating, so a rejected request leaves no half-applied state.
- Callers cannot tell "resized by zero" from "this axis has no split" except by
  reading `changed`; that is deliberate, and cheaper than inventing a second
  error class for a legal request.
- Changing the master layout to honour tree geometry later would remove the
  refusal without changing the wire contract.
- VRR does not survive a restart; a human who wants it permanently sets it in
  `abyss.kdl`. If that proves wrong, `SavedOutput` gains a field and this ADR
  is revisited.

## Revisit when

The master layout starts using the tree, an interactive (pointer-drag) resize
path lands, or adaptive sync needs to persist across restarts.
