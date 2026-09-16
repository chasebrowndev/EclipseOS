# 0024 — A locker crash leaves the session locked behind a compositor fallback
Status: accepted
Date: 2026-09-06
Deciders: chase (owner), Claude (advisory)

## Context
`ext_session_lock_v1` makes the compositor, not the locker, the thing that
holds the session shut. Two moments are underspecified in practice and both
decide whether a crash exposes the human's screen:

1. **No lock surface yet for an output.** The protocol requires the compositor
   to stop showing normal content the instant `lock` is handled — before the
   client has drawn anything, and again for every output that hotplugs in
   while locked. Something has to be on screen in that gap.
2. **The locker dies while locked.** `hyprlock` segfaults, is OOM-killed, or
   is `kill -9`'d by whoever is sitting at the machine.

The root invariant is that trusted UI is compositor-drawn and never a
layer-shell client, and COMP-10 puts the lock screen in that category.

## Options
1. **Unlock when the locker dies.** Every locker crash — and every `kill` an
   attacker can reach — becomes an unlock. Rejected outright.
2. **Stay locked, show the last frame.** Keeps a client's pixels on screen
   with no client alive to redraw them; the "last frame" is exactly the
   content the lock exists to hide.
3. **Stay locked, compositor-drawn fallback.** The compositor paints the
   screen itself and keeps painting it for as long as the session is locked.

## Decision
Option 3, for both moments, using the same code path.

`session_lock::lock_elements()` is the only element source consulted while
`state.lock.locked`. Per output it returns either that output's `LockSurface`
subtree, or — when there is none — a compositor-drawn solid fill: black ground
with a centred amber bar. Nothing else is ever collected, so no client surface
can appear behind, beside, or through the lock, and an output that appears
mid-lock is covered by the fallback from its first frame.

On locker death nothing unlocks. Smithay 0.7.0 enforces the protocol side of
this: `ext_session_lock_v1::destroy` posts `invalid_destroy` while locked, and
only `unlock_and_destroy` reaches `SessionLockHandler::unlock`. So a dead
locker simply stops producing surfaces and every output falls back — the
session stays shut. Recovery is another locker binding
`ext_session_lock_v1`; the fallback deliberately offers no way in.

Input follows the pixels. While locked, keyboard focus is handed to the first
lock surface and no compositor binding is allowed to act on the session
behind the lock (the VT-switch intercept stays, so the human is never trapped
on a wedged GPU). Pointer motion still moves the compositor-drawn cursor but
reports no surface under it.

The idle lock timeout refuses to self-lock when no `lock-command` is
configured, and logs why: locking with no locker able to start would strand
the human, since only an `ext_session_lock_v1` client can unlock.

## Consequences
- A locker crash is a hard lockout, not a bypass. The human's route back in
  is a VT switch or another locker, which is the correct trade.
- The fallback must stay dependency-free and allocation-light; it is drawn on
  the path that runs when the rest of the session is unavailable.
- The fallback is intentionally featureless — no password field, no hints. It
  is a refusal to display, not a second locker.
