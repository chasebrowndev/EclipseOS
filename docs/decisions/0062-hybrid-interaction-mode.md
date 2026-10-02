# 0062 — Hybrid interaction mode
Status: proposed
Date: 2026-09-25
Deciders: chase (owner), Claude (advisory)

## Context
COMP-17 §1-§2 defines two interaction modes, `wm` and `de`. What the owner runs
today, and what `hyperion` ships, is neither: a tiling compositor with a
taskbar that shows workspaces **and window chips** (focus, close, minimize, the
condensation ladder; STATUS B3, D-05). Under `wm` the taskbar would show
workspaces only. "Hybrid" already appears as a design tiebreaker (ADR 0052,
PROPOSEDFEATURES) but is not a value of `mode`.

## Decision
`mode` takes three values: `wm | hybrid | de`.

| Mode | Taskbar | Extras |
|---|---|---|
| `wm` | workspaces only | tiling, keyboard-first, no chips |
| `hybrid` | workspaces, window chips, tray, clock | today's behaviour |
| `de` | as hybrid | desktop icons, pointer-first navigation (COMP-17 §1) |

`mode` still selects defaults that explicit configuration overrides (COMP-17
§2). Setup profiles seed it: Minimal `wm`; Standard, Full and Agentic
`hybrid`. Switching applies on hot reload. The default when the key is absent
is `hybrid`, because that is what the shipped bar does.

## Consequences
- COMP-17 §1/§2/§2.1 and CONFIG.md amended in the same change; D-07 step 7 offers
  the three values.
- Owed code: `mode` and `components {}` in `config/schema.rs`; hyperion hides
  chips under `wm`; desktop layer starts only under `de`. `eclipse-backend`
  and `eclipse-frontend` respectively.
- Installer seeds `mode`, `general.layout` (radiant default) and `components.*`
  as plain values; nothing reads the profile again (ADR 0060).
- Not changed: the trust boundary, COMP-02/04/08/09/11.

## Rejected
- **Keep two modes and treat chips as a bar option.** Chips are the defining
  difference between the tiling-only and the current session, and users flip it
  as a style, which is exactly what `mode` is for.
