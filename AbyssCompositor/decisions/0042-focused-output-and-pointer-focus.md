# 0042 — Focused output is first-class; keyboard focus derives from it
Status: accepted
Date: 2026-09-16
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0019 made focus-follows-mouse the human default but said nothing about the
*focused output*, which VOL1:4383 still lists as an open question. The
implementation grew one anyway — `Outputs::focused: u64` — and moved it on every
pointer motion while keyboard focus moved on a separate, narrower path that
early-returned whenever the pointer was over empty space.

The two states drift. Hovering an empty desktop on monitor B makes B the focused
output while the keyboard stays on a window on monitor A, so `refocus_topmost`,
new-window placement and `focus_layer_if_wanted` all consult a focused output
that disagrees with `keyboard.current_focus()`. The reported symptom is a text
field on one monitor that keeps eating keystrokes while the pointer sits on an
empty region of another. Three separate hand-rolled focus paths
(`input::focus_window_under`, `shell::focus_window`, `input/inject.rs`) made the
divergence invisible: only one of them emitted the `focus` IPC event, so
eclipse-bar showed stale state on top of it.

COMP-05 §5 also requires per-workspace focus history, which does not exist in
the tree at all.

## Options
1. Delete the focused-output concept and derive everything from
   `keyboard.current_focus()`. Honest, but there is no answer for "which output
   does a new window open on when nothing is focused", and eclipse-bar's
   per-output fold needs it.
2. Keep both states and add reconciliation at each consumer. This is what exists;
   each consumer reconciles slightly differently and the bug class recurs.
3. Make the focused output authoritative and derive keyboard focus from it
   through a single decision function.

## Decision
Option 3. The focused output is a first-class piece of compositor state, moved
only by human pointer motion or an explicit human focus change — never by an
agent, which still needs `seat.focus.human` (ADR 0007, ADR 0019 unchanged).
Keyboard focus is a *function of* the focused output plus that output's
workspace focus history, computed by one pure function
(`shell::focus::decide_pointer_focus`) and applied by one applier
(`shell::focus::apply_focus`). Every human-seat focus change — pointer, click,
touch, keybind, map, unmap, workspace switch, gated IPC — goes through that
applier, which is therefore also the single write site for focus history.

Concretely: the pointer over empty space on the *same* output as the focused
window keeps focus (Hyprland's feel); over empty space on a *different* output
it takes that output's workspace focus-history head, and clears focus if that
workspace has nothing focusable.

## Consequences
- Per-workspace focus history becomes required state (`Workspace::focus_history`),
  satisfying the COMP-05 §5 requirement that was silently unimplemented. Alt-tab
  cycling becomes a small addition on top; it is not part of this change.
- The `focus` IPC event now fires on *every* human focus change, including
  pointer-driven ones, and gains a null-handle form meaning "nothing focused".
  The `output` event becomes load-bearing for eclipse-bar's per-output fold.
- `Outputs::focused()`'s silent fall-back to the first entry is replaced by
  explicit repair on output removal — a fallback would make the new
  same-output/different-output comparison lie.
- `refocus_topmost` no longer falls back to the last window on any output, so
  closing the last window on a monitor leaves focus off that monitor instead of
  yanking it across the desk.
- **Deliberate deviation from Hyprland:** Hyprland's cross-output FFM is
  unconditional. Ours freezes at the drag's origin output until button release,
  because COMP-05 §5 forbids crossing an output boundary mid-drag. This is not a
  bug; do not "fix" it.
- Because ADR 0026 scopes X11 clipboard access to focus, more responsive
  refocusing revokes X11 selection access more often. Intended.
- Owed: table-driven unit tests over the pure decision function, `Workspace`
  history tests, and a headless two-output integration test.

## Revisit when
Alt-tab cycling or a workspace-per-output model needs focus history to be
global rather than per-workspace, or a spec revision closes VOL1:4383
differently.
