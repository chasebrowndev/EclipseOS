<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0095 — ec-lock: the screen locker is a client, with PAM in it
Status: accepted
Date: 2026-11-10
Deciders: chase (owner), Claude (advisory)

## Context
EclipseOS shipped no screen locker. Abyss implements `ext_session_lock_v1`
(`protocols/standard/session_lock.rs`) with ADR 0024's fallback: while locked
it draws only lock surfaces, or black with an amber bar where it has none, and
a locker that dies leaves the session locked. `idle.lock-command` and the
`Super+Shift+L` bind (`loginctl lock-session`) exist, but nothing on the image
answers them.

Two parts of the spec pull apart here. Vol 1's protocol table lists
`ext_session_lock_v1` for "Screen lock" and `wlr_layer_shell` for "lockers":
the locker is a client. ADR 0024's context, and COMP-10 §2's "lock screen
handoff" (where the personal secret is shown), treat the lock screen as trusted
UI, which is compositor-drawn and never a client.

## Options
1. **Compositor-drawn lock screen.** Satisfies the trusted-UI reading and can
   show the personal secret. The compositor would collect the password and
   need its own route to PAM (as ADR 0088 does through polkit's helper, which
   only verifies polkit cookies), and the lock screen becomes TCB code.
2. **A client locker, `ec-lock`, PAM in the locker process.** What every
   Wayland locker does; the compositor already enforces the lock, so the
   client decides only when to send `unlock_and_destroy`. Chosen.
3. **Ship a third-party locker (swaylock, hyprlock).** No in-house code, but
   no design-system look and nothing EclipseOS controls.

## Decision
Option 2, by owner decision on 2026-11-10. The Vol 1 / ADR 0024 tension is
resolved in favour of the client: the lock is enforced by the compositor (ADR
0024 is unchanged and is what makes a client safe), and the lock screen's
*drawing* is not trusted UI.

`ec-lock` (`shell/crates/ec-lock`) is an iced client on `iced_sessionlock`
0.19.1, the session-lock sibling of the `iced_layershell` the panes use. It
takes one lock surface per output and a new one for each output that appears
while locked. It authenticates the process's own uid (never argv or `$USER`)
against PAM service `ec-lock` (`/etc/pam.d/ec-lock`, `auth include
system-auth`), on a worker thread, and its state machine (Locking, Idle,
Authenticating, Failed with a doubling backoff capped at 30 s, Unlocked) sends
`unlock_and_destroy` only for a PAM success on the attempt in flight. The proof
of success is a type only the PAM module can construct. libpam is linked
directly: the repository had no PAM binding and three calls do not justify one.

Every other exit (an error, a panic, a signal, `kill -9`) leaves the session
locked behind the ADR 0024 fallback, and that is correct. Anything that would
make unlocking impossible (an unknown uid, an unexpected argument, a missing
PAM service file) is refused before the lock is taken: `ec-lock` exits
non-zero, with a message on stderr, when neither `/etc/pam.d/ec-lock` nor
`/usr/lib/pam.d/ec-lock` exists, because libpam would then use `other`, which
on Arch is `pam_deny` and no password would ever open the lock. One locker at a time: a second `ec-lock` finds the
first's `flock` held and exits without locking.

The password exists in one fixed-size buffer that never reallocates and is
overwritten on every exit path, and in one key's worth of fixed array on its
way through the iced runtime. It is never logged, printed or `Debug`-formatted.
It is delivered only to PAM, the verifying authority (ADR 0088's reading of the
root invariant). There is no text widget: keys come from the keyboard
subscription, so no widget holds the value.

The shipped `/etc/eclipse/abyss.kdl` sets `idle { lock-command "ec-lock" }`.
No lock timeout ships (owner decision).

`ec-lock-trigger` (`shell/crates/ec-services`, user unit
`ec-lock-trigger.service`) answers logind. On `Session.Lock` for the user's
display session (`loginctl lock-session`: the `Super+Shift+L` bind, the bar,
the center, all unchanged) it starts `ec-lock`. On `PrepareForSleep(true)` it
starts `ec-lock` while holding a logind sleep `delay` inhibitor, and lets go
when that locker exits or after 3 s, whichever is first. That is under logind's
default 5 s `InhibitDelayMaxSec`, and suspend is never blocked longer than
that. It takes a fresh inhibitor on resume. Each locker runs in its own
`app-*.scope`, so stopping the trigger never kills a locker. The trigger never
unlocks: logind's `Unlock` is ignored.

## Consequences
- The lock screen cannot show the personal secret, so it can be spoofed by a
  same-uid client drawing a fake one. A fake lock screen is a fullscreen
  window: it is not a lock surface and holds no lock. Recorded as a limit.
- A dead locker is a hard lockout until another locker binds: the fallback
  takes no input. Getting a new `ec-lock` up today means a VT switch. The
  compositor relaunching `lock-command` when the locker dies while locked is
  owed (compositor change, not in this ADR).
- Before sleep the lock is best effort within the 3 s grace: a locker that
  takes longer to get its surfaces up may lock just after resume, not before
  suspend. Without the PAM service file it refuses, and the suspend goes ahead
  unlocked (logged by the trigger).
- Accounts with no password cannot unlock (`PAM_DISALLOW_NULL_AUTHTOK`).
- PAM stacks that ask a second question (an OTP) cannot unlock from here, as
  with ADR 0088. pam_faillock in `system-auth` adds its own lockout on top of
  the backoff, as it does for swaylock.
- `view.rs` is a functional placeholder; eclipse-frontend owns the real view.
- `iced_sessionlock` maps a window-close action to unlock, so the crate must
  never issue one (crate `CLAUDE.md`).

## Revisit when
The personal secret needs to appear on the lock screen, a supported PAM stack
needs more than a password, or abyss draws the lock screen itself.
