<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0092 — Adaptive glass: the tint follows the backdrop
Status: accepted
Date: 2026-11-08
Deciders: chase (owner), Claude (advisory)

## Context
Glass tint is fixed (COMP-02 §9; ADR 0071's `.50` floor is for the HUD only). Over a
bright wallpaper or window, popups such as the power menu and the notifications drawer,
the bar and the titlebars lose contrast with their text. Nothing in the specs governs a
backdrop-driven tint. `PROPOSEDFEATURES.md` has a deferred client-side "luminance event",
and no rule says whether the compositor may read backdrop pixels back for a measure
(capture is gated by `policy.kdl`, ADR 0027).

## Options
1. A luminance event to clients (the `PROPOSEDFEATURES.md` entry). Every client must
   handle it, and it tells a client something about what is behind it: an information
   leak that needs a trust review.
2. Per-pixel luminance compression in the blur shader. Evens out a half-bright backdrop,
   but flattens the glass and changes how every pane looks.
3. Per-surface adaptive tint in the compositor: measure the mean luminance of the
   blurred backdrop under each glass surface and pull the tint toward the scheme's
   surface colour until the scheme's text keeps its contrast.

## Decision
Option 3, applied to the tinted modes (`glass`, `frost`; plain `blur` has no tint) on all compositor glass (layer-surface glass, `eclipse_glass_v1`
panes, window glass, titlebars). The measure and the shift both happen on the GPU,
in the glass/frost composite, from taps of the blurred backdrop texture it already
samples. No pixel is read back to
the CPU, nothing is sent to a client, and no new protocol exists, so the capture policy
is not touched. In a dark scheme a bright backdrop darkens the glass; in a light scheme a
dark backdrop lightens it. A backdrop that already contrasts leaves the tint as
configured. It is on by default:
`decoration { blur { adaptive { enabled #true; strength 0.5 } } }`, live on hot reload
and in Settings. `strength` (0.0..=1.0) scales how far the tint may move; 0 is the fixed
tint.

## Consequences
- The glass a client sees behind it is no longer a fixed tint; a client must not assume
  a constant backdrop colour (it never could, the backdrop already varies).
- The shift is a function of the blurred backdrop only, so it changes when the backdrop
  changes and costs no extra damage beyond what the blur already invalidates.
- Owed: config key + validation + `docs/CONFIG.md`, COMP-02 §9 amendment line, a
  Settings control, tests for the tint function in both schemes.

## Revisit when
A client needs to choose its own text colour from the backdrop (then the luminance event
of option 1 comes back with a trust review), or the per-surface mean proves wrong for
split backdrops (then option 2).
