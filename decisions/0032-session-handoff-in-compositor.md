# 0032 — The systemd session handoff lives in the compositor, behind `--session`
Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context
Abyss had no way to be picked at a greeter: no `wayland-sessions` entry, no
launcher, and no handoff to `systemd --user`, so a user's
`PartOf=graphical-session.target` services never started.

The handoff is three calls — `dbus-update-activation-environment`,
`systemctl --user import-environment`, and starting a session target. All three
need `WAYLAND_DISPLAY`, and that value does not exist until the compositor has
bound its socket. A wrapper script cannot know it: it would have to poll
`$XDG_RUNTIME_DIR` for a socket that may be `wayland-0`, `wayland-1`, or
whatever the first free name is, and it would race the compositor's own
startup.

COMP-01 §5 specifies startup up to step 9, "create the public socket
`wayland-N`; export `$WAYLAND_DISPLAY`", and steps 10–14 for the privileged
socket, policyd, IPC and the loop. It says nothing about the user session
manager, greeters, or desktop entries — §2 only notes that the daemons are
systemd user units that abyss does not `exec`. So this is additive to the spec
rather than an implementation of a numbered step, and no spec citation is
claimed for it.

## Options
1. **Wrapper script does the handoff.** Simple, no compositor code. But it must
   guess or poll for `WAYLAND_DISPLAY`, which is exactly the race described
   above, and a wrong guess silently gives user services a dead socket.
2. **Compositor does it, always.** Removes the race and needs no flag, but a
   nested development run would then start and stop the user's real
   `graphical-session.target` — actively destructive on this machine.
3. **Compositor does it, behind `--session`.** The flag is set only by the
   installed launcher, which the greeter runs. Nested runs are unaffected
   unless the flag is passed deliberately.

## Decision
Option 3, the niri/sway model. `dist/abyss-session` sets only what is knowable
in advance (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, `XDG_SESSION_DESKTOP`)
and `exec`s `abyss --session`. `crate::session::import()` runs immediately
after each backend sets `WAYLAND_DISPLAY`, and `teardown()` on the normal exit
path. The helpers are spawned as children and never waited on — `SIGCHLD`
already carries `SA_NOCLDWAIT` — so nothing blocks the calloop. Failure to
spawn, or a unit that refuses to start, is logged at `warn` and ignored: a
missing systemd must never take the compositor down with it.

## Consequences
- Abyss appears at greetd/regreet once `dist/abyss.desktop` is installed, and
  ordinary user services start with the session.
- The flag is load-bearing for that behaviour and must stay threaded as a plain
  `bool` through `backend::*::run` — no global, no lock (ADR 0018).
- The handoff is unverifiable in CI and only partially verifiable nested: the
  nested smoke test proves it does not crash and that failures are tolerated,
  not that the target came up. Real verification needs a TTY login, and is
  batched with the other deferred hardware gates in `docs/STATUS.md`.
- We now owe the `dist/` files continued accuracy if the target name changes.

## Revisit when
`sd_notify(READY=1)` (COMP-01 §5 step 13) is implemented and abyss becomes a
systemd unit itself, or if the spec grows a section on session integration —
either could move this out of the compositor and into unit ordering.
