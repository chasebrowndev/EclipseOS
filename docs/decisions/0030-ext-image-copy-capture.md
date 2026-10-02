# 0030 — Two capture protocols coexist, behind one shared gate
Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0027 shipped `zwlr_screencopy_v1` as the compositor half of COMP-16 M8
(screen sharing via xdg-desktop-portal). It works against a direct client, but
M8 cannot close on it: `xdg-desktop-portal-wlr` 0.8.3, on its wlr-screencopy
path, delivers exactly one frame and then stalls — that path re-arms only from
`pwr_handle_stream_on_process`, and it connects the PipeWire stream `DRIVER`
without `TRIGGER`, so nothing ever calls back. The same xdpw against a
compositor that offers `ext_image_copy_capture_v1` takes a different code path
and streams continuously. That protocol is also what COMP-02 §8
`capture_toplevel` needs, and it is the one upstream is standardising on;
`zwlr_screencopy_v1` is on its way out.

Smithay 0.7.0 ships neither protocol, so both are hand-written against the XML
on `wayland-server` `Dispatch`/`GlobalDispatch`. Two protocols that read every
pixel on the machine is two attack surfaces, and the invariant is that there is
no ambient authority — so the question is not only whether to add the second
one, but how to keep it from becoming a second, subtly weaker gate.

## Options
1. Replace `zwlr_screencopy_v1` with `ext_image_copy_capture_v1`. One surface,
   no duplication — but it breaks `grim`, `wf-recorder` and every other tool in
   the existing ecosystem, none of which speak the staging protocol yet, and
   drops working functionality to fix a different client's bug.
2. Keep `zwlr_screencopy_v1` only, and carry the xdpw stall as an upstream bug.
   Honest, but M8 stays open indefinitely on someone else's schedule, and
   `capture_toplevel` stays unimplementable.
3. Implement both, with the second one duplicating the gate and the redaction
   pass so the two are independent. Simple to write, and exactly how a
   fail-open divergence gets introduced: the next change to `decide()` lands in
   one copy.
4. Implement both, sharing one gate function, one pending-capture queue, one
   servicing path and one redaction pass.

## Decision
Option 4. `ext_image_capture_source_v1` (output source) and
`ext_image_copy_capture_v1` (manager/session/frame, `wl_shm` buffers) are added
alongside `zwlr_screencopy_v1`, and share its enforcement with it rather than
reimplementing it. The single `screencopy::decide()` from ADR 0027 answers for
both, in the same three places: as `GlobalDispatch::can_view` on both new
manager globals, again on `create_source`/`create_session`/`capture`, and a
third time in `capture::service`. A locked session denies unconditionally and
outranks the allowlist, on both protocols, because it is literally the same
branch of the same function. An authorised capture becomes a
`capture::Pending` on the one shared queue — the frame object it will answer on
is the only per-protocol part, behind a `capture::Sink` enum — and is serviced
by the one `capture::service`, which builds its pass list with the one
`capture::capture_elements`: sensitive surfaces excluded, black quad in their
place. There is no second redaction implementation to keep in step.

Continuous capture is driven by re-arming on a short calloop timer per
serviced frame rather than by damage. A nested abyss whose host window is
occluded receives no frame callbacks, and commit `dedc26e` already had to route
`backend::damage_all` around that for the wlr path; making the new path wait on
damage would reintroduce the same class of stall.

Cursor capture (`ext_image_copy_capture_cursor_session_v1`) is answered with a
session that is stopped from the start, rather than left unimplemented as a
silent no-op. The foreign-toplevel capture source is not implemented, because
`ext-foreign-toplevel-list` does not exist in this tree yet.

## Consequences
- M8's blocking client bug is routed around: xdpw takes its
  `ext_image_copy_capture` path against abyss and streams continuously —
  measured at 60 frames in 1.18 s through PipeWire, sustained, with no stall.
- Two capture protocols to maintain, and two globals to hide. The mitigation is
  that they are two front ends onto one enforcement path; a change to the gate
  or to redaction is still a change in exactly one place.
- `zwlr_screencopy_v1` stays for as long as the tools that need it do. It is
  deprecated upstream and this ADR is the place to record its removal when the
  ecosystem has moved; there is no deadline on it today.
- `wl_shm` only, again. `ext_image_copy_capture_v1` can advertise dmabuf
  constraints and abyss sends none, so a video-rate consumer still pays a
  readback per frame. Owed, as in ADR 0027.
- Per-toplevel capture becomes expressible for the first time, but is still not
  implemented: it needs `ext-foreign-toplevel-list`, which is now what COMP-02
  §8 `capture_toplevel` is blocked on.
- The re-arm timer means an authorised session costs a full-output composite at
  up to frame rate for as long as it is open, even over a still scene.

## Revisit when
`ext-foreign-toplevel-list` lands and toplevel capture can be built; or when
`policyd` lands and `decide()` becomes a `check()` call — at which point both
protocols change together, which is the point of sharing it; or when the tools
that still need `zwlr_screencopy_v1` have moved and it can be dropped.
