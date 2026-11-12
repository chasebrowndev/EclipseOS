# 0097 — Scoped capture grants and a compositor-drawn capture confirm
Status: accepted (amended 2026-11-12, see below)
Date: 2026-11-02
Deciders: chase (owner), Claude (advisory)

## Context
VOL1 §3 / Capture Gating says a capturing client triggers "the portal's own
picker". ADR 0029 says the opposite for consent: allowlisting a portal backend
by process name makes the gate "anything that can reach the portal", xdpw's
picker is untrusted, and the real consent gate must be a compositor-drawn
COMP-10 prompt (it calls that "M14"; COMP-16 M14 is in fact "atomic batches", so
no milestone owns it). COMP-10's prompt list is closed (ADR 0053), and the only
trusted-UI request socket, `trusted.sock`, is root-only and carries one action
(`erase-disk`). Nothing specifies a user-session request channel, a grant shaped
`{stream, app-id, target}`, or how abyss identifies the portal.

## Options
1. Allowlist the portal process name (ADR 0029 status quo). Rejected there.
2. Let the portal draw the confirm. abyss cannot tell a human click from the
   portal itself, and any client could cover or imitate the dialog.
3. Portal draws a themed **source chooser** (untrusted input); abyss draws a
   minimal confirm that is the consent, and policyd mints a grant bound to one
   stream.

## Decision
Option 3.
- **Chooser vs consent.** `ecl-portal-ui` may show a rich picker (outputs and
  windows with previews, audio toggle, remembered choice). Its result is a
  *request*. abyss then shows a compositor-drawn modal (new COMP-10 prompt kind,
  closed-list extension in the §3.10 precedent) naming the **actual** target as
  abyss sees it, the requesting app-id (marked as a claim) and the stream.
  Human seat only, injected input dropped, default focus Deny, Escape denies,
  Enter never allows, timeout = deny, fixed text.
- **Channel.** A new user-uid socket `$XDG_RUNTIME_DIR/eclipse/portal.sock`
  (0600), one request per connection, one pending at a time, fields sanitised.
  abyss authenticates the peer by `SO_PEERCRED` pid and `/proc/<pid>/exe`
  against the installed `ecl-desktop-portal` path (the §3.10 pattern). This
  authenticates *which daemon is asking*, not the application behind it.
- **Grant.** On Allow, abyss asks policyd to mint a grant bound to
  `{stream id, app-id (claim), target, expiry}`; the minted scope must be
  byte-identical to the scope shown (COMP-10 §7 test). abyss enforces it in
  `screencopy` / `image_copy_capture`. A portal-supplied scope can only narrow;
  abyss re-checks the target itself. policyd or the compositor unavailable =
  deny.
- **Allowlist.** The process-name entry for a portal backend is replaced by
  "may ask via `portal.sock`"; no capture without a live grant. Redaction stays
  compositor-side regardless of what was chosen.
- **Scope.** Screen/window capture first; remote input and input capture reuse
  the same modal and channel but each needs its own sub-ADR before enabling.
- **Indicator.** Active grants drive the existing COMP-10 §3.6 trusted
  indicator, naming the consumer.
- Amends VOL1 §3 (portal draws only the chooser, not the consent) and discharges
  ADR 0029. A COMP-16 milestone is assigned in the same PR (new, not M14).

## Consequences
- TCB work in `trusted_ui/`, `policy/`, `audit/` and the capture protocols;
  main thread only, owner line-by-line review, spoof tests mandatory (ADR 0009).
- Cursor metadata (`ext_image_copy_capture` cursor session, currently refused)
  and `ext-foreign-toplevel-list` for per-window capture become prerequisites
  for full ScreenCast.
- One extra click per new share compared with upstream; persistent "always
  allow" is a policyd grant with expiry and is revocable in Settings.
- Identity remains containment, not authentication (ADR 0027); this ADR does not
  claim otherwise.

## Revisit when
A sandbox/app-identity scheme gives abyss an authenticated app id; or the
modal needs previews drawn by the compositor.

## Amendment, 2026-11-12: what v1 builds
The owner accepted this design to make screen sharing work out of the box,
without the user editing policy. v1 (`ec-abyss/src/trusted_ui/screencast.rs`)
narrows it:
- **abyss holds the grant, not policyd.** policyd ships in the optional
  `eclipseos-agents` package, and a desktop without it must still be able to
  share a screen. The grant `{stream, output, app-id claim}` lives in abyss.
  Minting it through policyd, with the byte-identical scope test, comes back
  once policyd leaves the agent package (ADR 0098).
- **The grant lives exactly as long as the asking connection.** After
  `Granted` the portal keeps `portal.sock` open for the whole stream. Hanging
  up, any further byte, or the daemon dying revokes the grant, and the next
  frame is refused. Locking the session revokes every grant.
- **Screens only.** `Source::Window` is refused until a toplevel capture
  source exists (D-08 §6). `persistent` is ignored: every share asks again.
- **Admission.** Screencopy and image-copy admit `/usr/bin/ecl-desktop-portal`
  only for the output named in a live grant (`screencopy::admit`). The lock is
  checked first, and a grant never lifts it. The capture globals are visible to
  that executable so it can bind before asking.
- **Wire.** The postcard frames of `ecl-portal-proto` are decoded by hand in
  abyss, so the TCB carries no codec dependency.
