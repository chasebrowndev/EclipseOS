# 0027 — Screen capture is denied unless the client is on a process-name allowlist
Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context
COMP-16 M8 puts screen sharing via xdg-desktop-portal on the roadmap. The
compositor half of that is `zwlr_screencopy_v1`: the portal backend
(xdg-desktop-portal-wlr, out of tree) binds it and hands the frames to PipeWire.
Smithay 0.7.0 ships no screencopy support, so the protocol is implemented here.

Screen capture is the single broadest read capability a Wayland compositor can
hand out: one bind reads every pixel of every window, including the lock screen
and the trusted UI, regardless of focus. The root invariants say there is no
ambient authority and that policy is fail-closed — and `policyd`, which would
normally answer "may this client capture?", does not exist yet.

## Options
1. Ship the protocol ungated until `policyd` lands, behind a config flag that
   defaults on. Matches what every wlroots compositor does. It is exactly the
   "just for now" ungated path the invariants forbid, and "for now" outlives the
   milestone that wrote it.
2. Ship it gated on an interactive trusted-UI prompt. Correct end state
   (COMP-10), but the prompt machinery is a later milestone, and building half of
   it here would be guessed against a spec that is not written.
3. Gate on the same process-name allowlist ADR 0022 already uses for
   `wlr_data_control`, defaulting to empty.

## Decision
Option 3. `capture { allow "<comm>" ... }` in the KDL config names the processes
that may capture; it is **empty by default**, so a stock helios denies every
capture request. The client's identity is its `/proc/<pid>/comm`, resolved from
the `SO_PEERCRED` pid of the connection, exactly as ADR 0022 does.

The gate is applied twice, and the same `decide()` function answers both:
- as `GlobalDispatch::can_view`, so a non-allowlisted client never sees the
  `zwlr_screencopy_manager_v1` global at all (`grim` reports "compositor doesn't
  support the screen capture protocol");
- again on every `capture_output`/`capture_output_region` request, because a
  global can be bound before an allowlist reload and the check must not be a
  one-shot at bind time.

A denied request is answered with `failed()` before anything is computed: no
`buffer` event, no offscreen texture, no queue entry — no state mutation before
the decision is `Allow`, per the invariant. A locked session denies
unconditionally and outranks the allowlist, and is re-checked a third time when
the capture is actually serviced.

Redaction rides on the same path. `capture_elements` builds its own pass list
rather than reusing `space_render_elements`: a surface that is sensitive (in
`HeliosState::sensitive`, or matching `capture { redact-app-id ... }`) is
**excluded** from that list and an opaque black quad is drawn in its place, so
the pixels are never rendered into the capture buffer rather than being painted
over afterwards. Both inputs may only raise the class — the ratchet rule holds
by construction, since the test is an `or`.

The capture indicator and the lock screen are drawn only by the backends, into
the on-screen frame, and never by `capture_elements`. Trusted UI is therefore
structurally uncapturable rather than uncapturable by policy.

## Consequences
- Out of the box, screen sharing does not work. That is the intended posture:
  the user opts a named process in, knowingly, in their config.
- The identity is a process name, which is weak — anything the user can rename or
  exec can borrow it. It is a coarse containment measure, not an authentication
  mechanism; ADR 0022 already accepted that trade for the clipboard, and the same
  reasoning (and the same replacement plan) applies here.
- A capture is serviced after submit, from inside the backend redraw where the
  `GlesRenderer` lives, so an authorised capture never delays the frame the user
  is looking at. Queued captures cost one extra full-output composite.
- `wl_shm` only. No dmabuf capture, so a screen-sharing consumer pays a readback
  per frame. Fine for the portal's still-frame and low-rate use; a video-rate
  consumer will want dmabuf later.
- No per-toplevel capture. `zwlr_screencopy_v1` cannot express it, and COMP-02 §8
  `capture_toplevel` needs `ext-image-copy-capture-v1`, which is owed.

## Revisit when
`policyd` exists — at which point the allowlist becomes a policy table entry and
`decide()` calls `check()`, with the empty-allowlist default replaced by an
unavailable-policyd deny that means the same thing — or when the COMP-10 capture
prompt lands and consent becomes interactive rather than declared in config.

## Amendment — 2026-09-07
The resolution order is inverted: the basename of `/proc/<pid>/exe` is tried
first, and `/proc/<pid>/comm` is now only the fallback. `comm` is capped at 15
bytes, so every `xdg-desktop-portal-*` backend reports `xdg-desktop-por` and a
single allowlist entry would have admitted all of them. `exe` is maintained by
the kernel, is not writable by the process, and is not truncated. The identity
is still containment rather than authentication: a binary copied under an
allowlisted name still passes.

## Amendment — 2026-09-07 (hot reload)
`capture.allow` is now a shared `config::Allowlist` handle rather than a
snapshot, so a config reload retunes the `zwlr_screencopy_v1` bind filter
without a restart; see the matching amendment on ADR 0022 for the mechanism and
why the lock is not a hot-path violation. `decide()` takes `&Allowlist` and
still consults the lock state first, so the ordering guarantee — a locked
session denies capture regardless of the allowlist — is unchanged.
