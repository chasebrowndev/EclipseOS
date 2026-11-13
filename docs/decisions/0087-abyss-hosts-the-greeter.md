<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0087 — Abyss hosts the greeter
Status: accepted
Date: 2026-11-04
Deciders: chase (owner), Claude (advisory)

## Context
greetd ran `cage -s -m last -- regreet`, so the login screen was the one thing
in the image drawn by a compositor that wasn't ours. `-m last` also tore down
cage's only output as a dock came up and crashed it in wlroots (three coredumps
on 2026-11-04).

Abyss could not take over as it stood. It could not launch a client, and nothing
stopped it when one exited, so greetd would have waited on a live greeter after
login. Its default binds are compiled in and always merged (`merge_binds`), so
`Super+Return` would have opened a foot shell as `greeter` on the login screen,
before authentication. Add-on hooks would have applied too, dialling `policyd`
and `brokerd` with nobody logged in.

## Options
1. A greeter `abyss.kdl` plus a wrapper script that starts abyss, finds its
   socket and runs regreet. No compositor change, but config cannot remove a
   default bind, a wrapper still needs a way to stop the compositor, and under
   ADR 0064 a config error drops nodes and starts anyway. A login screen must
   not depend on a file parsing correctly to stay locked down.
2. `--kiosk -- <argv>`, enforced in code. Chosen.
3. Keep cage and drop `-m last`. Fixes the crash, keeps the foreign compositor.

## Decision
`ec-abyss --kiosk -- <argv>` spawns one client directly (no shell) and stops the
loop when that client exits, detected by pidfd because `SA_NOCLDWAIT` makes the
child unwaitable. Abyss then exits 0; greetd only asks whether a session started.

The restrictions are state, not config, so no file and no reload can lift them.
While `state.kiosk` is set:

- Actions pass an allowlist at the one place they run, so keys, pointer and
  gestures are all covered. Only the override chords (`Super+Escape`,
  `Super+Shift+Escape`), VT switching and keys for an open trusted prompt survive.
  `Super+space` is dropped too: it opens phrase entry, which means nothing with no
  user session. The match has no catch-all, so a new `Action` will not compile
  until someone decides.
- Add-on hooks stay empty and manifests are not read.
- Xwayland does not start, and neither the control socket nor the trusted socket
  is bound.

- `idle.lock-command` is not run: it is a shell line that does not pass through
  the allowlist.

Lid `suspend` stays. It runs a fixed `systemctl suspend`, and a laptop at the login
screen should still be able to sleep. *(Amended ADR 0108: the compositor is the only lid handler, so
the login screen sleeps through abyss's lid rules. With no compositor running, a
lid close does nothing.)*

greetd calls `/usr/bin/ec-abyss` by absolute path, and `eclipseos-abyss` provides
`wayland-compositor`, which `greetd-regreet` depends on. `cage` leaves every
package set.

## Consequences
- A compositor failure now takes the login screen with it, where cage might have
  survived. tty2 is the way back. The `policy.kdl` fail-closed refusal (ADR 0064)
  applies here as well.
- Dev installs symlink `/usr/local/bin/ec-abyss` into a home directory `greeter`
  cannot traverse, so `/usr/bin/ec-abyss` must be a real, current binary there.
- With several outputs, regreet goes on the focused one and the rest stay black.

## Revisit when
EclipseOS ships its own greeter, or a second kiosk client appears that needs more
than one window's worth of compositor.
