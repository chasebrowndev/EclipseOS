# 0085 — Update and power: the taskbar's "Update & reboot" and "Update & power off"
Status: proposed
Date: 2026-11-03
Deciders: chase (owner), Claude (advisory)

Owner decision, 2026-11-03: the update offers a **full upgrade only**. The
Eclipse-and-dependencies scope and its dependency-closure code are removed. This
record supersedes the scope text of the earlier draft of this ADR and the
scope-prompt wording in ADR 0065's amendment.

## Context
The power widget should offer a one-click way to finish an EclipseOS update and
restart. The owner's rules:

- The item is offered **only** while an Eclipse package update is pending.
- Running it upgrades **every** pending package. The count shown is N, the
  number of all pending packages.
- The upgrade runs, progress is reported, and **only on success** does the
  machine reboot or power off through logind. A failure never powers the
  machine off.

Forces:
- Arch has no atomic upgrades. `pacman -Syu` is the supported path; any subset
  upgrade is a partial upgrade, and the usual hazard is a shared library that
  one package now needs at a version the rest of the system no longer has.
- `pacman` needs root. The session user must not get root, and no shell may sit
  between the widget and pacman (COMP-13 authority is not involved here, but the
  same "no ambient authority" rule is).
- `checkupdates` (pacman-contrib) reads a temporary copy of the sync database and
  needs no root. The real sync database under `/var/lib/pacman/sync` is only
  refreshed by a `-Sy`, so an install that uses it as-is can take a stale version
  and report success on a no-op.
- Eclipse's packages are the `eclipseos-*` names built from
  `packaging/pkg/eclipseos/PKGBUILD`. The owner also named `ec-*`. These names
  decide only whether the item is offered; they do not select what is upgraded.

## Options
1. **Full upgrade only (`pacman -Syu`), offered while an Eclipse update is
   pending.** The supported path. Accepted.
2. **Eclipse-only partial upgrade, unguarded.** `pacman -S` of the pending
   Eclipse packages. Rejected: a partial upgrade.
3. **Eclipse-only partial upgrade with its dependency closure.** The pending
   Eclipse packages plus the pending packages in their closure (`pactree -s -u`).
   Rejected: still a partial upgrade, and the closure is a guess at what a
   package links against. Its residual risk is outside the closure.
4. **Run the upgrade in the session with `sudo`.** Needs a password prompt
   from a process that is not ours to trust and gives the widget a path to
   root that `pkexec` and polkit do not mediate. Rejected.
5. **A polkit-gated root helper with a fixed argv.** `pkexec` runs
   `/usr/lib/eclipse/ec-update-apply`, which accepts exactly `all`, and execs
   pacman with fixed flags. Accepted, as the carrier for option 1.

## Decision
- **Offer only when pending.** The service reports `Update::Pending` with the
  total N of all pending packages, `eclipse_pending` and the Eclipse list, or
  `Update::Unavailable`. The widget offers the item only when `eclipse_pending`
  is true. The service enforces the same rule on apply: with no Eclipse update
  pending, apply is `Refused` and no process runs.
- **One scope: the full upgrade.** `Handle::apply(after)` runs
  `ec-update-apply all`, which execs `pacman -Syu --noconfirm`. Running it
  installs every pending package, not only the Eclipse ones.
- **No partial upgrade.** There is no Eclipse-only path, no dependency closure,
  and no `-S` of a package list. The helper takes no package names.
- **Polkit helper.** The privileged step is `pkexec` under the polkit action
  `org.eclipse.update.apply` (auth_admin_keep, exec path
  `/usr/lib/eclipse/ec-update-apply`). The helper accepts exactly one argument,
  `all`, and refuses any other argv with status 2 before pacman starts. Its
  root check comes after validation so the validation can be tested without
  root. Changing the helper or the policy is the privilege change to review.
- **Power after success only.** The power action (`Reboot` or `PowerOff` on
  logind, non-interactive) is requested only when pacman exits 0. Every other
  outcome (non-zero exit, a signal, a missing `pkexec`, a timeout, a refused
  polkit prompt) ends with `Failed` and no power request. If logind refuses the
  power action after a good upgrade, the widget is told the update installed but
  the machine did not restart.
- **Single-flight.** One upgrade at a time. A second apply while one runs is
  `Refused`.
- **Progress.** The upgrade reports as progress job `eclipse-update` on the
  feed, from pacman's `(n/m) upgrading|installing|removing` lines.

## Consequences
- ec-services gains `updates` (the service, the parsers and the seams). The
  taskbar's widget reads `Update` and calls `Handle::apply`; it does not run
  pacman.
- No partial upgrade is possible through this path, so the partial-upgrade
  hazard in the Context does not apply to it. The cost is that a full upgrade
  can pull in packages that are not Eclipse's, and it needs the time and
  bandwidth a full `pacman -Syu` takes. The prompt shows N so the human sees
  what it installs.
- `eclipseos-desktop` installs `/usr/lib/eclipse/ec-update-apply` and
  `org.eclipse.update.apply.policy`, and depends on `polkit`.
  `eclipseos-hyperion` lists `pacman-contrib` as an optdepend: without it the
  item is never offered.
- Discovery is `checkupdates` only. A failed or missing `checkupdates` is
  `Unavailable`, and the item is never offered (fail closed).
- The feed job expires after 60 s without a pacman transaction line. Long
  download or scriptlet phases may let the job drop off the widget while the
  upgrade still runs. Not fixed here.
- Owed: an owner review of `packaging/bin/ec-update-apply` and
  `packaging/polkit/org.eclipse.update.apply.policy` (F-07 §4), and a manual run
  of the apply path on a disposable VM before this ships.

## Revisit when
- The helper needs a second shape. Widening it is a privilege change: write a
  new ADR, do not add an option.
- `auth_admin_keep` proves too lax for a session that is inactive or remote; the
  `allow_any`/`allow_inactive` defaults are the first things to tighten.
