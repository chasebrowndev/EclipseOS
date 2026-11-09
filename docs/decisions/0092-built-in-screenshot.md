<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0092 — The compositor takes its own screenshots
Status: accepted
Date: 2026-11-08
Deciders: chase (owner), Claude (advisory)

## Context
Screenshots were `slurp` then `grim`, bound to `spawn`. Each is a separate process that
connects, negotiates the capture protocol and encodes a PNG, so a shot is not instant. The
region selector also starts after the keypress, so the shot shows what the screen looks like
once the user has finished choosing, not when they asked.

`render/capture.rs` already builds one redacted pass list (ADR 0030) and `still.rs` draws the
screen's effects over it with redaction first. COMP-02 §7 and ADR 0056 govern what a capture
may contain. No spec covers a human-seat capture: COMP-10 §3.6's indicator names portal and
agent streams only, and `capture.allow` (ADR 0027) gates protocol clients. The region
selector in `ec-abyss-render/src/select.rs` is an untrusted pass behind the add-on
`region-select` hook (ADR 0040, 0066).

## Options
1. Keep `grim`/`slurp` and speed them up. Still two processes, still after the keypress.
2. Built-in capture through a trusted surface. The surface list is closed (ADR 0053), so
   it needs a new surface and more TCB review for no gain: the selector reveals nothing.
3. Built-in capture through the shared pass list, with the existing untrusted selector pass.
   Chosen.

## Decision
Two bind actions, `screenshot-full` and `screenshot-region`. Both render once through
`capture_elements` and `still.rs`, so the shot has the screen's blur, glass, rounding,
shadow, glow, panes and borders. It cannot be a second pass list, since that would break
ADR 0030's single implementation of redaction.

**Redaction.** Sensitive surfaces and secret nodes are redacted exactly as for any capture,
before any effect, so a blurred secret stays a blurred placeholder. A human shot gets no
exception to `capture.secret`.

**Freeze.** `screenshot-region` renders when the bind is pressed and keeps that texture as
the selector's backdrop. The shot is a crop of it, so it shows the moment of the keypress.
Esc, a miss-click under the selector's minimum edge, a lock, or the output going away drops
it. Nothing is written.

**Selector.** The existing untrusted compositor pass, enabled for this built-in use only.
The `region-select` add-on hook stays off unless a manifest names it.

**Authority.** The bind is the seat's own act. It does not go through `capture.allow` and
needs no `policyd`, so a shot works when `policyd` is down. It emits the COMP-12 `capture`
record (post-redaction hash and region list, no pixels). The capture indicator is not shown:
it is for streams that run unseen.

**Output.** PNG to `~/Pictures/Screenshots/<timestamp>.png`, encoded on the blocking pool and
returned by channel, and set as the clipboard selection by the compositor. Clipboard contents
are never logged.

## Consequences
- A shot is one render and a readback, not three processes. The region shot is exact to
  the keypress.
- `grim` and `screencopy` clients see the same effects, as before.
- Panes and borders now appear in every capture, not just screenshots.
- A human screenshot ignores `capture.allow` and the indicator. That is a deliberate
  carve-out from a literal reading of "every operation needs a capability check". The
  capability is holding the seat's input.
- We owe: tests for freeze-on-press and cancel, a scan test that the selector pass is
  absent from the capture list, a `docs/CONFIG.md` entry for the actions, and an
  update of KNOWNBUGS CAP-01 and GLASS-01.

## Revisit when
A second seat or a remote-input path can produce the bind, or a spec decides human
captures need the indicator.
