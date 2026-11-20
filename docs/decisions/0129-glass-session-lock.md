# 0129 — Glass under the session lock
Status: proposed
Date: 2026-11-19
Deciders: chase (owner), Claude (advisory)

## Context
The screen locker drew as an opaque sheet. `lock_elements` returned only the
lock surface's elements (or the solid fallback): no wallpaper, so a glass pane
would have blurred nothing, and `eclipse_glass_v1` panes were spliced only for
windows and layer surfaces (`collect_elements`). The owner wants the locker to
be real compositor glass. The lock is the one screen that must never show
the desktop, so the glass must be unable to sample it.

## Options
1. Run `collect_elements` under lock and hide windows by rule. Rejected: one
   missed element class (a new layer, a popup, a drag icon) leaks the desktop.
2. Keep the locker opaque and let it fake glass. Rejected: owner decision.
3. A separate lock-only element list built from three inputs, with the
   locker's panes spliced into it. Chosen.

## Decision
While locked, an output draws, back to front: the compositor's own ground (the
output's wallpaper element, else the solid fallback ground), then, above it,
the locker's panes' glass, then the lock surface tree. This is
`ec_abyss_render::lock_glass_elements`, called by `session_lock::lock_elements`
from all three backends. Panes use the same `insert_panes` path as layer
panes, with a `BlurKey::LockPanes(surface)` of their own, and their backdrop
is `elements[index..]`: the ground and nothing else.

`ec-lock` is added to the `eclipse_glass_v1` allow-list (`DEFAULT_ALLOW`, now 9
names). The global is not gated on the lock state, so it is offered while
locked. Panes draw only in `glass` blur mode, as everywhere.

## The security invariant
While locked, no window, layer surface (bar, launcher, toasts), popup, drag
icon or other client content is drawn or sampled by the lock's glass. Only the
wallpaper (or the solid ground) and the lock surface itself are. The cursor
is composited above the whole locked frame by the backends, as it was before,
and is never part of a pane's backdrop.

It holds by construction, not by filtering: `lock_glass_elements` takes no
`Space`, layer map, focus, popup or cursor state. Its inputs are the lock
`WlSurface`, the wallpaper store and a ground quad. A scan test
(`the_lock_glass_sees_only_the_wallpaper_and_the_lock_surface`) fails if the
source of that function, or of `lock_elements`, names any of those, and checks
that each backend's locked branch calls `lock_elements` and not
`collect_elements`.

## Why wallpaper-only is safe
The wallpaper is the compositor's own picture, drawn under every output
already and visible on the login greeter. It carries no client content and
nothing of the session's state. Blurring it reveals nothing the wallpaper
does not.

## Failure modes
All fail soft to today's behaviour (the lock surface alone), never to the
desktop and never to skipping the lock:
- not `glass` mode, a rotated output, the glass program failing to compile, or
  a locker with no panes: `lock_glass_elements` returns `None` and the plain
  lock surface draws (`lock_glass_possible`);
- the backdrop failing to build for a frame: the panes are not drawn; the
  surface still is, over the wallpaper;
- no wallpaper configured: the solid fallback ground is the backdrop;
- no lock surface: the compositor fallback, unchanged.
The lock state machine, the input grab and unlock are untouched. Capture is
denied while locked (`capture.rs`, unchanged), so the glass is not a capture
path.

## Consequences
A locker may be translucent now, but only when it pushes panes in `glass`
mode: a translucent locker without panes still draws as before (no wallpaper
behind it). Only the root surface's panes are honoured; subsurface panes are
not. Locked frames cost one persistent backdrop chain over the panes' union,
kept by the existing damage-limited refresh.

## Revisit when
A locker needs subsurface panes, or the Lens/Regular presets and blur tiers
(branch `adaptive-glass`) land: pane looks and the tier-0 wallpaper-only
pyramid should then be checked against this path.
