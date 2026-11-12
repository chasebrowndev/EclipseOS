<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0099 — One locker holds the session, a dead one is relaunched, and idle may suspend
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
The lock is a client locker over `ext_session_lock_v1` (ADR 0095); the compositor holds the
session shut and draws the fallback when no locker surface is up (ADR 0024). Four gaps:

1. **A second lock while locked.** `SessionLockHandler::lock` confirmed every request. A
   second client got `locked` too, and its `unlock_and_destroy` unlocked the session under
   the real locker. Smithay 0.7.0 makes this worse: for a lock that never locked it posts
   `invalid_unlock` and then calls `SessionLockHandler::unlock` anyway
   (`wayland/session_lock/lock.rs`, `Request::UnlockAndDestroy`), and `unlock` takes no
   argument, so the handler cannot tell who asked.
2. **Locker death.** The session stayed locked (ADR 0024), but nothing brought a locker
   back. The human had a fallback screen and no way to type a password.
3. **Lock state was not observable** over the control socket, and there was no way to ask
   for a lock from a script.
4. **Idle had no suspend stage.** The owner wants it configurable alongside display-off and
   lock, never on by default (an always-on dev box), and no default idle-lock timeout.

## Decision
**One holder.** `LockState::owner` is the lock object that holds the session. While it is
alive, a new lock is sent `finished` (the dropped `SessionLocker`) and the held lock is
untouched. `ExtSessionLockV1` is dispatched by hand instead of through
`delegate_session_lock!`: `unlock_and_destroy` from any lock but the owner is answered with
`invalid_unlock` and never reaches smithay or `unlock`. `new_surface` keeps only surfaces from
the owner's client, so a refused locker cannot paint the lock screen.

**Death and takeover.** The hand-written `Dispatch::destroyed` sees the owner go (client
death, or `destroy` while locked, which smithay answers with `invalid_destroy`). The session
stays locked and `owner` becomes `None`. With `idle.lock-command` set (not in kiosk mode),
the compositor relaunches it through `shell::spawn`, the same path the idle timeout uses,
after 1 s, then 2, 4, 8 and 8 s if no locker has taken over. It stops after five relaunches
per locked session, and unlocking resets the count. The check runs on the existing 1 s idle
timer, before idle inhibitors are consulted. There is no new timer or thread.

**Who may take over.** A lock request while the session is locked and ownerless is a
takeover only if the client runs the lock command's program. That program is the first word
of `idle.lock-command`: an absolute path as itself, a bare name as `/usr/bin/<name>` (never a
`PATH` search), symlinks resolved. Anything else (a relative path, quotes, shell syntax, an
env assignment, a path that does not resolve) names no program. The program is compared with
the kernel's `/proc/<pid>/exe` for the client's `SO_PEERCRED` pid. Any error is a refusal.
With no lock-command set, nothing takes over and the session stays on the ADR 0024 fallback.
A refused client is sent `finished`. This is path identity with the same stated limits as
ADR 0094: anything that can execute that binary as this user passes, and pid reuse between
connect and check is not defended against. On takeover `owner` is replaced and the request
is confirmed. `locked` never goes false, so no unlocked frame is drawn. Smithay allows this:
`locked_outputs` stores per-client `wl_output` resources, so the new client's
`get_lock_surface` does not hit `duplicate_output`.

**Control socket.** `get_lock_state` (Query) returns `{locked, pending, locker_alive}`.
`pending` means the lock command ran or a relaunch is scheduled, less than 10 s ago, and no
locker holds the session yet. A `lock` event carries the same payload on every change.
`lock_session` (Command) runs `idle.lock-command`. It answers `spawned: false` if a live locker
holds the session, `-32602` if no command is set, and `-32000` in kiosk mode. On an ownerless
locked session it brings up a replacement. `ec-ctl lock` and `ec-ctl lock --status` wrap
the two. There is no unlock method: only the locker's `unlock_and_destroy` unlocks.

**Idle suspend.** `idle.suspend-timeout-seconds` (unset or 0 = never, the default) and
`idle.lock-before-suspend` (default `#true`). The compositor has no D-Bus. Suspend goes to
logind through `systemctl suspend`, as the lid switch already does
(`outputs/power.rs`). With lock-before-suspend, the stage runs the lock command, suspends once
the session reports locked (the ownerless fallback counts), and does **not** suspend a
session that did not lock within 10 s, or one that has no lock command. It runs once per idle
period. Idle inhibitors freeze the clock, so they hold off suspend exactly as they hold off
DPMS and lock. `idle.lock-timeout-seconds` still has no default and no shipped value.

## Consequences
- Only the configured locker's binary can take over an ownerless session, and then unlock it
  (ADR 0024: "recovery is another locker", narrowed). While a locker is alive, nobody can.
  A lock-command written as shell (`env X=1 swaylock`, `exec ...`, quotes) cannot take over;
  give the program as its first word.
- A locker that crashes on every start costs five spawns and then gives up. `ec-ctl lock`
  from another VT or ssh tries again by hand.
- With lock-before-suspend on and no lock command, idle never suspends. That is deliberate:
  waking into an unlocked session is the failure mode.
- `pending` is time-bounded and not announced when it lapses. A subscriber that cares
  re-reads `get_lock_state`.

## Revisit when
Smithay passes the lock object to `SessionLockHandler::unlock` (the hand-written dispatch
can go), the compositor gains a logind connection of its own, or a spec decides lock
takeover needs an allow-list.
