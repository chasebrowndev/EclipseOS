# 0025 — Direct scanout is off for any frame containing a sensitive surface
Status: accepted
Date: 2026-09-06
Deciders: chase (owner), Claude (advisory)

## Context
COMP-02 §2 wants client buffers handed straight to KMS planes: it removes a
full-screen composite pass, keeps the client's buffer format, and is the only
way to hit the COMP-14 frame budget on a 4K panel. COMP-02 §7 wants the
compositor to be able to redact regions of a surface before they reach the
screen (and, later, before they reach a screencopy consumer).

The two cannot both apply to the same buffer. A buffer on a plane is never read
or written by the compositor — the display engine scans it out of client memory
directly — so there is no point at which redaction could paint over it. Any
"redacted" plane would show the unredacted pixels.

At this milestone there is no policy engine, so "sensitive" is a per-window flag
(`HeliosState::sensitive`) with no producer yet.

## Options
1. Scan out unconditionally, redact only on the screencopy path. Fastest;
   silently defeats on-screen redaction — the thing being protected is what the
   human is looking at. Fail-open, so out.
2. Never scan out. Trivially correct, costs a composite pass on every frame
   forever, and makes video playback miss the budget.
3. Per-frame decision: pick the scanout candidate first, force composition for
   the whole frame if that candidate is flagged sensitive.

## Decision
Option 3. `render_output` resolves the direct-scanout candidate (a surface
covering the whole output), and passes `FrameFlags::DEFAULT` to
`DrmCompositor::render_frame` only when that candidate is not in the sensitive
set and the `render { direct-scanout }` config knob is on; otherwise
`FrameFlags::empty()`, which composites everything. The decision is per frame,
so flagging a window sensitive takes effect on the next frame with no
renegotiation, and clearing the flag restores scanout just as cheaply.

The same candidate drives adaptive sync (COMP-03 §8): VRR is enabled only while
a surface covers the output.

## Consequences
- Redaction stays fail-closed by construction: a frame is either composited (and
  therefore redactable) or contains nothing sensitive.
- A sensitive fullscreen video player loses direct scanout and pays a composite
  pass. Accepted; that is the cost of showing it at all.
- Marking a window sensitive costs a whole-output composite even if the
  redacted region is a few pixels. Plane-granular scanout (redact only the
  planes that intersect a redacted region) is possible later but needs the
  region tracking COMP-02 §7 has not specified yet.
- Owed: once the policy engine lands, it — not a manually set bool — populates
  the sensitive set, and this ADR's assumption of a per-window flag is revisited.
- `render { direct-scanout false }` exists as an escape hatch for driver bugs;
  it is not a security control (the redaction path does not depend on it).

## Revisit when
COMP-02 §7 specifies redaction regions precisely enough to redact per plane, or
a driver ships plane-level readback that would let a scanned-out buffer be
redacted after the fact.
