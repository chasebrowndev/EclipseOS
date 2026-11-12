# 0101 — A bad Abyss update falls back to the previous build
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
Updates are a plain `pacman -Syu`, from the power widget through
`/usr/lib/eclipse/ec-update-apply` (ADR 0085) or typed by hand. Arch has no
atomic upgrade and nothing keeps the old files. One broken `eclipseos-abyss`
takes out **both** the desktop and the login screen: greetd runs regreet on
`/usr/bin/ec-abyss --kiosk` (ADR 0087), so the greeter has no menu left to
pick anything from. The way back today is a tty, a login, and a
`pacman -U /var/cache/pacman/pkg/eclipseos-abyss-<old>.pkg.tar.zst` from
memory.

What exists:
- **Filesystems.** `ec-setup` (D-07) and `install-eclipseos.sh` (D-03) make
  one GPT disk: a FAT ESP at `/boot` and **ext4** root
  (`installer/crates/ec-setup-helper/src/stages.rs`, `limine_conf`). No
  subvolumes, no LUKS, no swap. `eclipseos-postinstall.sh` bolts EclipseOS
  onto an archinstall install, which may be btrfs with archinstall's `@`
  layout and no snapper.
- **Boot.** Limine, three fixed entries: `linux`, its fallback initramfs, and
  `linux-lts`. D-01 §2 calls the LTS entry "the only rollback that exists before
  the atomic scheme (D-04)".
- **Sessions.** `eclipseos-abyss` ships `/usr/share/wayland-sessions/abyss.desktop`
  → `/usr/bin/abyss-session`, which execs `ec-abyss --backend drm --session`.
  regreet lists every entry in that directory.
- **Specs.** D-04 ("Update strategy: rolling vs snapshots, atomic updates,
  rollback") and S-13 ("Update & rollback security") are both `planned` in the
  Vol 1/Vol 2 index and have no document. D-02 §6 and D-03 §7 defer rollback to
  D-04. This ADR is **not** D-04: it is insurance until D-04 exists, and is
  written so D-04 can supersede it without a migration.
- **Config.** An old binary reading a newer config drops the nodes it does not
  know and starts (ADR 0064).

Forces: it must work on every install the installers can produce (ext4 first),
need no new privileged entry point, never make an update less likely to finish
cleanly, and never delete the only good copy of anything.

## Options
1. **Pre-update btrfs snapshot + a Limine entry for it.** Real rollback of all
   of `/` (kernel modules excepted — `/boot` is FAT). Rejected for now: the
   default installs are ext4, so it protects the minority archinstall-btrfs
   path only; Limine has no snapshot integration in the official repos
   (limine-snapper-sync is AUR); and it needs a subvolume layout D-03/D-07
   explicitly deferred to D-04. It belongs in D-04 with LUKS.
2. **Filesystem-independent "previous Abyss".** Before pacman replaces
   `/usr/bin/ec-abyss`, copy the running build to `/usr/lib/eclipse/previous/`
   and ship a second session entry, "Abyss (previous)", that runs it. Pacman's
   cache pin variant was weighed: a cached `.pkg.tar.zst` is not runnable
   without extracting it, and `paccache` may delete it. A copied tree is
   runnable as-is. ~tens of MB. **Accepted.**
3. **Automatic health check.** If the new session fails to reach a ready
   marker N times, fall back by itself. A persistent counter needs a ready
   marker abyss does not emit and a state file the `greeter` user cannot
   write. **Accepted for the greeter only**, in its degenerate form: N = 1, no
   state. A greeter compositor that exits non-zero within 10 s of starting is
   replaced once by the previous build, because a broken compositor cannot draw
   the login screen that would offer the choice. User sessions get no automatic
   retry: a crashed session lands at the login screen (greetd restarts the
   greeter when the session exits) and the user picks "Abyss (previous)".
4. **Full A/B images.** Out of scope: needs an image build, two root
   partitions (or an immutable `/usr`), a boot counter and a different package
   model than pacman's. That is the D-04 decision itself, not insurance.

## Decision
Option 2, plus Option 3 for the greeter only, on every install, with no filesystem dependency:

- **Keep.** A pacman `PreTransaction` hook, `eclipseos-abyss-keep.hook`, fires
  on any upgrade touching `usr/bin/ec-abyss` and runs
  `/usr/lib/eclipse/ec-abyss-keep`. It copies `ec-abyss` and `ec-ctl` into
  `/usr/lib/eclipse/previous/` with a `VERSION` file (from pacman's local db),
  staged in `previous.new` and swapped by rename. A hook, not a step in
  `ec-update-apply`, so a hand-typed `pacman -Syu` is covered and ADR 0085's
  helper does not widen.
- **Only a proven build replaces the fallback.** The copy is refreshed only
  while an `ec-abyss` from `/usr/bin` runs with `--session` (the kiosk greeter
  does not count). Updating from a tty, or from inside the previous session,
  keeps the older fallback. With no fallback yet, the current build is kept.
  So a second update on top of a bad one never overwrites the last good
  compositor.
- **Fail closed.** The hook is `AbortOnFail`: if the copy cannot be made, pacman
  aborts before changing a file. Owner override:
  `sudo EC_ABYSS_KEEP=skip pacman -Syu`. The widget path has no override.
- **Choose (the one user-facing mechanism).** The login screen.
  `/usr/share/wayland-sessions/abyss-previous.desktop` ("Abyss (previous)")
  runs `abyss-session --previous`, which skips the widget-order migration and
  execs the kept build directly (the current one if none is kept). regreet lists
  it by itself; no greeter code changes. `abyss-session` otherwise execs
  `ec-abyss` directly, as before this ADR: no wrapper, no re-exec.
- **Greeter fallback (the only automatic one).**
  `/usr/lib/eclipse/ec-abyss-launch BINARY ARGS…` runs the greeter compositor
  and is used only by the greetd command. If BINARY exits non-zero within 10 s
  and was not stopped by a signal to the wrapper, the previous build is exec'd
  once with the same arguments. No loop.
- **Preselecting "Abyss (previous)" after a crash: not done.** regreet's cache
  (`/var/lib/regreet`, key `user_to_last_session`, seen in the binary's
  strings; no offline docs) is owned by `greeter`, which the session user cannot
  write, and a marker the greeter could read would need a shared writable
  location plus a greeter-side reader. Not ~10 lines and not robust, so skipped;
  the entry is one click away.
- **Recover.** The previous session is a recovery session: get a terminal,
  `pacman -U` the cached older package or wait for a fixed one. `VERSION` names
  the cached file to look for.

## Consequences
What this does **not** protect against, stated so nobody assumes otherwise:
- **Kernel, firmware, mesa, NVIDIA/DKMS breakage.** Only the compositor binary
  is kept. The `linux-lts` Limine entry is still the only kernel fallback.
- **Library soname bumps.** The kept build is dynamically linked against
  today's libraries; a `libinput`/`libdisplay-info`/`libwayland` soname change
  in the same upgrade leaves it unrunnable (it then fails exactly like the new
  one, and the tty path remains).
- **Config written in a newer schema.** The old binary drops nodes it does not
  know (ADR 0064) — it starts, but settings the new build added are gone in
  that session, and a node whose meaning changed is dropped.
- **The rest of the desktop.** The bar, toasts, launcher, settings, policyd
  and agentd run today's builds against the old compositor; a control-socket
  method the old build lacks fails in that client. Bugs in those components are
  not covered: they are not the compositor and do not take the login with them.
- **A crash after 10 s**, a hang, or a black screen without an exit, in the
  greeter. Only a startup death is detected, and only for the greeter.
- **A crashing session** returns to the login screen with the current build
  still selected by default; the user must pick "Abyss (previous)" themselves.
- **The first update after this ships**: the hook arrives with that update, so
  protection starts from the update after it.
- **Non-pacman installs** (`install.sh`, `install-session.sh`): no hook, no
  fallback, unchanged behaviour (`abyss-session` execs `ec-abyss`
  directly regardless).
- `/usr/lib/eclipse/previous/` is not owned by any package. Removing
  `eclipseos-abyss` leaves it behind (`rm -r` it by hand).

Also:
- `greetd/config.toml`'s command changes from `ec-abyss …` to
  `ec-abyss-launch /usr/bin/ec-abyss …`. Same argv to the compositor, same
  user.
- The keep script runs as root from a hook. It takes no input but the root it
  is given, writes only under `/usr/lib/eclipse/previous*`, and reads `/proc`.
  Owner review like any root code.
- Owed: `packaging/test/update-rollback.sh` (rootless, in the gate); one manual
  run on a disposable VM — install, update to a deliberately crashing build,
  confirm the greeter comes up on the previous one and "Abyss (previous)" works
  from the login screen.
- D-01 §2 and §3.5 updated in the same change.

## Revisit when
- D-04 is written: it decides snapshots/A-B and supersedes this, or keeps this
  as its per-package layer.
- The installer gains btrfs (with LUKS, per D-03 §7): Option 1 becomes
  available on the default install.
- A soname bump strands the kept build in practice: then keep the libraries it
  needs too, or move to Option 1.
- abyss gains a ready signal (e.g. `sd_notify`): the 10 s window becomes "did
  not reach ready".

## Owner decisions
- Greeter wrap through `ec-abyss-launch`: accepted by the owner 2026-11-11 in
  this simplified form (greeter only; sessions are chosen at the login screen).
