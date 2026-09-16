# 0020 — Interim hardcoded quit binding is `Super+Shift+Q`
Status: accepted
Date: 2026-09-05
Deciders: chase (owner), Claude (advisory)

## Context
Milestone 1 runs `abyss` nested under Hyprland with no config parser yet
(KDL lands in milestone 2, ADR 0016). A nested compositor with no way out is a
lost session. `Super+Escape` is reserved by COMP-04 §6 as the human override
chord that pauses all agent seats, and must never be overloaded — muscle memory
for a safety chord cannot be shared with "quit".

## Options
1. `Super+Escape` — already reserved; overloading it is a safety hazard.
2. `Super+Q` — collides with the common close-window binding.
3. `Super+Shift+Q` — matches the Hyprland-lineage convention for exit.

## Decision
`Super+Shift+Q` quits the compositor, hardcoded in `input/` until the KDL
config binds it. `Super+Escape` stays reserved for the agent override chord and
is not bound to anything else, at any point.

## Consequences
- The hardcoded bind is deleted, not merely defaulted, when milestone 2 lands
  configurable keybindings; the default moves into the shipped config.
- Any future ADR proposing a binding must check it against the reserved chord
  list first.

## Revisit when
Configurable keybindings land (COMP-16 milestone 2).
