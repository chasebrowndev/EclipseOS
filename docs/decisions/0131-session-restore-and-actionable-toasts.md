<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0131 — Session restore after an unclean end, and actionable toasts
Status: proposed
Date: 2026-11-24
Deciders: chase (owner), Claude (advisory)

## Context
When the machine loses power, the kernel panics or abyss dies, every client dies with
it. Nothing can be "recovered"; what can be offered is reopening the same apps in the
same places. Today nothing detects an unclean end. VOL1 §1.4 and §7 specify a layout
snapshot under `$XDG_RUNTIME_DIR`, which is tmpfs and does not survive a reboot, and
state that clients are never restored; relaunched apps are placed by the rules engine.
No snapshot code exists.

Toasts (ADR 0091) have modes, history, per-source rules and actions that work only while
an entry is live. `ec-toasts` is an ordinary layer-shell client, not TCB. Nothing
specifies fullscreen behaviour. Screen-share redaction already exists
(`ui.capture-toasts`, ADR 0106), so toasts need no new share handling.

## Options
1. Auto-restore silently. Can loop on the app that caused the crash. Rejected.
2. Restore only the compositor layout snapshot (VOL1 §1.4). Does not bring apps back.
3. Offer a one-shot, user-consented relaunch from a persisted session manifest. Chosen.

## Decision
**Detection.** `ec-services` writes `$XDG_STATE_HOME/eclipse/session.running` at login
and removes it when abyss emits a `session-end` event on a clean exit (an explicit compositor signal, not a socket-unlink heuristic). If it is present at the next login, the
previous end was unclean. A counter in the same file records consecutive unclean ends.

**Manifest.** `$XDG_STATE_HOME/eclipse/session.json` (0600, atomic write, rewritten on
change, debounced). It holds, per window: desktop-entry id, workspace, tile slot and
output name. It holds no titles, argv, environment, cwd, grants or handles. Window
titles are human-influenced text and stay out.

**Offer.** On the first login after an unclean end, a system toast, not an app toast,
asks "Eclipse ended unexpectedly. Reopen N apps?" with `Reopen` and `Start fresh`. It is
pinned until answered and, if never answered, falls to history as a non-live entry.
In Quiet or DND the offer follows `ui.restore-offer-in-quiet` (default `true`: shown
once, then the mode applies again; `false`: dropped).
After two consecutive unclean ends no offer is made and the manifest is set aside.

**Relaunch.** Accepting launches each desktop entry through the normal launcher and
policy path, placed by the rules engine; this keeps VOL1's "clients are never restored".
Each app gets fresh grants. Nothing is replayed, so there is no ambient authority and a
missing policy entry or an unavailable `policyd` means deny. Document recovery stays the
app's own business.

**Toast classes.**
- *App*: untrusted. Open, close, and app-declared actions that return an action id.
  Rendered so they cannot pass for a system toast.
- *System*: originates in the compositor or shell. May carry yes/no, for non-security
  questions only (session restore is the first).
- *Security*: permissions, secrets, capture and agent approvals remain compositor-drawn
  trusted UI and are never toasts.
A toast carrying a question never auto-expires. The timeout pauses while the pointer is
over the toast.

**Silence.** A per-card `Silence <source>` control opens an inline drawer: mute for 1 h,
until tomorrow, or forever, and "banners only" (keep in history). It writes the existing
rules file (ADR 0091). Silencing only tightens. An app cannot lift it or declare itself
critical to bypass it; only system sources may bypass, as today.

**Fullscreen.** Toasts are suppressed over a fullscreen surface by default and stored as
unread. A setting (`ui.toasts-over-fullscreen`, default `off`) turns this off. System
toasts that pose a question are held and shown when fullscreen ends.

**Placement.** Four corners. Stacks grow away from the anchor edge; the drawer expands
toward screen centre; slide in from the nearest horizontal edge; button order is the same
in every corner; offset by the bar; usable at any output or tile width.

## Consequences
- New: sentinel, manifest writer and restore flow in `ec-services`, and a system-toast
  path with yes/no. Settings gains the fullscreen switch (live apply, no Apply button).
- The drawer is frontend work. It waits for the organic-shape support landing in `ec-ui`.
- Specs to update in the same PR: VOL1 §1.4 (state dir vs runtime dir, relaunch on
  consent), `docs/CONFIG.md`, ADR 0091 (system toasts, fullscreen), D-05.
- Tests: unclean-end detection, two-strikes skip, manifest holds no titles or grants,
  question toasts never expire, silence only tightens.
- Open: VOL1 §9 vs VOL2 §9 on whether grants survive a reboot. This ADR does not settle
  it and persists none.

## Revisit when
ADR 0102 (containment on panic) is accepted, grants become durable across reboot, or a
compositor crash can keep clients alive.
