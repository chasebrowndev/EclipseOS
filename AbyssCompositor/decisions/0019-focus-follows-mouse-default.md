# 0019 — Default human focus model is focus-follows-mouse
Status: accepted
Date: 2026-09-05
Deciders: chase (owner), Claude (advisory)

## Context
COMP-04 §7 left the default focus model open. The human UX is Hyprland-shaped
and the owner is the first full-time user; agent seats mean agent activity
never moves human focus either way, so the choice is purely human ergonomics.

## Options
1. Click-to-focus — conventional, one extra click per window switch.
2. Focus-follows-mouse — matches the Hyprland/dwm lineage the UX borrows from.

## Decision
Focus-follows-mouse is the default for the human seat; click-to-focus is
configurable. Human focus follows the human seat only — agents cannot change it
without the `seat.focus.human` capability, which is prompt-class.

## Consequences
- Must be correct across outputs and during drags (COMP-05 tests).
- Pointer motion during a trusted-UI prompt must not move focus out from under
  the prompt.
- Config key: `focus-follows-mouse true` (COMP-13).

## Revisit when
Multi-output focus-follows-mouse proves annoying in daily use, or a client
class breaks under it.
