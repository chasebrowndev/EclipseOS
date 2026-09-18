# 0050 — Oracle-Eyes may call `get_outputs`
Status: accepted
Date: 2026-09-18
Deciders: chase (owner), Claude (advisory)

## Context

Automatic mode (`Oracle-Eyes/spec.md` §2.2) reads one output per tick. It has no
idea which output the human is facing, so it walks all of them round-robin. On a
three-monitor desk that means two out of every three answers are about a screen
nobody is looking at — and because there is a single display slot, the one you
*are* looking at waits its turn behind them. From the seat it reads as the
feature being broken: answers appear late, in the wrong place, about the wrong
thing.

The fix is to read only the focused output. Oracle-Eyes cannot work that out on
its own. It holds exactly two capabilities (ADR 0041): capture, and the COMP-13
control socket scoped by `Oracle-Eyes/CLAUDE.md` to `annotation_create` /
`annotation_update` / `annotation_destroy` / `annotation_clear` plus the
`keybind` event kind. Focus is in neither, and that document says anything else
"is a new capability and needs an ADR". Hence this one.

## Options

1. **A new `output_focused` method** returning the focused output's logical
   rect. A `TABLE` entry in `ipc/gate.rs` and a handler in `ipc/methods.rs`.
   Purpose-built, minimal reply — and a new method on the control surface that
   every other socket client also gains, to answer a question an existing method
   already answers.
2. **Call the existing `get_outputs` query.** `ipc/methods.rs::get_outputs`
   already reports, per output, `focused` (bool), `position` (logical), `mode`
   (physical) and `scale` (fractional) — enough to rebuild the focused logical
   rect as `position` plus `mode / scale`, with a quarter-turn transform swapping
   the axes. `e("get_outputs", Kind::Query, true)` is already in the gate's
   `TABLE`.
3. **Follow the last manual selection** instead, using the region the user last
   dragged. No new surface at all, but stale until the user selects once, and
   wrong the moment they switch screens without selecting.

## Decision

**Option 2.** Oracle-Eyes' documented control-socket usage now includes the
read-only `get_outputs` query, alongside the four `annotation_*` commands.

No compositor change of any kind: no new method, no `ipc/methods.rs` edit, no
`ipc/gate.rs` `TABLE` edit. This ADR records a widening of what the *daemon*
asks for, not of what the compositor offers.

The consumer is `Oracle-Eyes/crates/oracle-eyes/src/focus.rs`, which returns
`None` — not a guess — when the call fails, nothing is focused, or an output has
no mode yet. `main.rs` then falls back to the old round-robin, so a compositor
that declines the query degrades to today's behaviour rather than fixing on one
screen forever.

## Consequences

- Automatic mode annotates the screen the user is on, and its answers stop
  queueing behind two screens they are not.
- `get_outputs` is `Kind::Query` and read-only. **The "never load-bearing for
  security" invariant is untouched**: the compositor computes, decides and draws
  exactly the same whether or not anyone calls it, and there is no code path
  that knows Oracle-Eyes is the caller.
- The reply carries connector names, identities and geometry — desk layout, not
  window content, and nothing about what is on any screen. Option 1 would have
  leaked strictly less, at the cost of a permanent new method on a surface whose
  design point is that an absent method does not exist. Trading one unnecessary
  method for slightly wider read on a query the owner's own tooling already uses
  is the cheaper side.
- Oracle-Eyes still cannot affect anything but the glyphs (COMP-18 §1.3). It
  learns *where* a screen is; placement, styling and eviction stay the
  compositor's.

Implements COMP-18 §3.
