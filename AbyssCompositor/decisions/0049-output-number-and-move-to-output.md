# 0049 — Outputs get a stable display number; move-window-to-output-workspace action
Status: accepted
Date: 2026-09-17
Deciders: chase (owner), Claude (advisory)

## Context
Owner wants Ctrl+Super+[N] to move the focused window to display N's currently
active workspace. Neither COMP-03 nor COMP-05 defines a numbering scheme for
outputs (confirmed via spec-oracle) — ADR 0023 only covers *identity*
(EDID/connector, for layout persistence across reboots), not a small stable
integer for human-facing keybinds. COMP-05 §5.1 already lists "set output" as
a spec'd per-window operation; composing it with the target output's current
active workspace into one action is new ground.

ADR 0023 also flags an open TODO: two outputs with identical EDID (same model
monitor x2) collide on identity. A number scheme must not inherit that collision.

## Options
1. Number by `Outputs::identity()` sort order (ADR 0023's existing sort-key)
   — stable across reconnects but silently renumbers if a new monitor is
   plugged in earlier in sort order than an existing one.
2. Number by connection order (first output seen this boot = 1, next = 2, ...),
   reassigned fresh each compositor start — simplest, matches "the display I
   plugged in first is 1" mental model, doesn't try to solve ADR 0023's
   identity-collision TODO.
3. Make numbering user-configurable in KDL (`output "DP-1" { number 2; }`),
   falling back to connection order when unset.

## Decision
Option 3, connection-order default with optional explicit config override.
Numbers are assigned when an output is added to `Outputs` (compaction on
removal is out of scope — a numbered output that unplugs just leaves a gap
until next restart, not renumbered live, so an in-flight Ctrl+Super+N binding
doesn't jump to a different monitor mid-session). This sidesteps ADR 0023's
identity-collision case entirely since numbering is positional/config-driven,
not identity-derived.

The new action (`Action::MoveToOutputWorkspace(u8)` or similar) composes the
already-spec'd "set output" primitive (COMP-05 §5.1) with a read of the
target `OutputEntry`'s own `active` workspace index to land on its current
workspace, rather than introducing a new persisted-focus concept.

## Consequences
Easier: a human keybind target for multi-monitor window shuffling that doesn't
require remembering workspace indices per output.
Harder / owed: docs update to COMP-03/COMP-05 noting the numbering field exists
(display-facing, not identity); config schema doc for the optional `number`
KDL field.
Forbidden: numbering must never be derived from or persisted via EDID/identity
— that's ADR 0023's job and mixing the two reintroduces its collision case.

## Revisit when
wlr-output-management output-management-v1 support lands (ADR 0023's own
open TODO) — at that point live renumbering/compaction on hotplug may become
desirable and should be reconsidered together.
