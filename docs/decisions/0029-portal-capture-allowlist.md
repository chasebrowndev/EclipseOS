# 0029 — `xdg-desktop-portal-wlr` is allowlisted by the user, never by default
Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0027 gates `zwlr_screencopy_v1` on a fail-closed process-name allowlist that
is empty out of the box. COMP-16 M8 asks for screen sharing through
`xdg-desktop-portal`, and the only backend that speaks our protocol is
`xdg-desktop-portal-wlr` (xdpw). To make a screen share work at all, a user has
to write `capture { allow "xdg-desktop-portal-wlr" }` into their config.

That single line is not the same shape of grant as allowlisting `grim`. xdpw is
a **proxy**: it holds the screencopy bind on behalf of whatever asked it over
D-Bus. Once it is allowlisted, our per-client gate can no longer tell a video
call from a random Flatpak — it sees one process name, always the same one, for
every consumer. The gate degrades from "these programs may capture" to "anything
that can reach the portal may capture", and the compositor has no way to
re-narrow it, because the requesting app's identity never crosses the D-Bus
boundary into a form we can check.

What the portal path buys in exchange is the ecosystem: browsers and conferencing
apps only know how to ask the portal. What it does not buy is consent — xdpw's
own picker is an out-of-tree UI we do not control and do not trust.

Measured this milestone, on the winit backend: our screencopy loop sustains 851
frames in ~11 s to a direct client (`wf-recorder`), and delivers correct redacted
frames. Through xdpw 0.8.3, exactly one frame reaches the PipeWire consumer and
the stream then stalls; the wire trace shows abyss answering `capture_output`
with `buffer`/`buffer_done` and then `damage`/`ready` in 5 ms, after which xdpw
never issues another capture. In that version the wlr backend restarts the loop
only from `pwr_handle_stream_on_process`, and the stream is connected
`PW_STREAM_FLAG_DRIVER` without `PW_STREAM_FLAG_TRIGGER`; the same xdpw against
Hyprland uses its `ext_image_copy_capture` path instead and does not stall. So
the limit is xdpw's wlr path and its PipeWire scheduling, not our protocol.

## Options
1. Ship `allow "xdg-desktop-portal-wlr"` in a default or example config so screen
   sharing works after install. Convenient, and exactly the ambient grant the
   root invariants forbid — a stock install would let any portal client read
   every pixel.
2. Refuse the portal entirely until the COMP-10 capture prompt exists. Keeps the
   grant honest, but leaves M8 with no path at all and no way to shake out the
   protocol against a real consumer.
3. Support the entry, document it as an opt-in the user types themselves, ship no
   default or sample config containing it, and record that the real consent gate
   is the trusted-UI prompt.

## Decision
Option 3. `capture { allow "xdg-desktop-portal-wlr" }` is a supported
configuration and is what a user writes to screen-share today. It is **not** in
any config we ship, not in a default, not in an example, and not created by the
first run; `capture.allow` stays empty, so a stock abyss still denies every
capture. The entry is described in the docs together with what it widens: it
grants capture to the portal, and therefore to every client the portal will serve,
for as long as it is in the file.

We also record that this is a stopgap. The per-process allowlist was already
containment rather than authentication (ADR 0027); against a proxy it is barely
even containment. The gate that makes portal capture defensible is the COMP-10
trusted-UI capture prompt (M14) — compositor-drawn, per-session, naming what is
about to be shared — after which the allowlist entry admits the portal to *ask*
and the human answers.

## Consequences
- Screen sharing is reachable, one config line away, and off until the user
  writes that line. Nothing about the fail-closed default changes.
- While that line is present the capture allowlist is effectively "any portal
  client". Redaction (`capture { redact-app-id ... }`), the lock check, and the
  structural uncapturability of the trusted UI and the lock screen are what still
  hold — they are enforced when the capture is serviced and are indifferent to
  who the consumer is. Verified this milestone: a `redact-app-id` window is
  entirely black in captured pixels while the same scene without the entry shows
  it in full.
- We owe the COMP-10 prompt before this configuration can be recommended rather
  than merely documented, and this ADR is the thing M14 has to discharge.
- End-to-end video-rate sharing through xdpw 0.8.3 does not work, for reasons on
  xdpw's side. We do not carry a workaround for it: `ext-image-copy-capture-v1`
  is already owed for `capture_toplevel` (COMP-02 §8), and implementing it is
  also what puts us on xdpw's maintained path.

## Revisit when
The COMP-10 capture prompt lands and consent becomes interactive — the allowlist
entry then stops being the grant and becomes a permission to prompt — or when
`ext-image-copy-capture-v1` is implemented, at which point the portal path should
be re-measured end to end and this ADR's stall finding retired.
