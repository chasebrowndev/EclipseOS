# 0085 — Update and power: the taskbar's "Update & reboot" and "Update & power off"
Status: proposed
Date: 2026-11-03
Deciders: chase (owner), Claude (advisory)

## Context
The power widget should offer a one-click way to finish an EclipseOS update and
restart. The owner's rules:

- The item is offered **only** while an Eclipse package update is pending.
- Clicking it asks for a scope: **(a)** Eclipse and its dependencies only, or
  **(b)** all packages, showing the count N of pending packages.
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
  `packaging/pkg/eclipseos/PKGBUILD`. The owner also named `ec-*`.

## Options
1. **Full upgrade only (`pacman -Syu`).** No partial upgrade, always the
   supported path. Fails the owner's scope (a): every update is pulled in, and
   the Eclipse-only choice disappears.
2. **Eclipse-only partial upgrade, unguarded.** `pacman -S` of the pending
   Eclipse packages. Cheap and small, but it can leave a library the Eclipse
   packages need out of step with the rest of the system.
3. **Eclipse-only partial upgrade with its dependency closure.** The pending
   Eclipse packages plus every pending package in their closure (`pactree -s -u`
   on the sync database). The closure is what limits the partial upgrade to
   what Eclipse needs. Accepted, with the risk recorded below.
4. **Run the upgrade in the session with `sudo`.** Needs a password prompt
   from a process that is not ours to trust and gives the widget a path to
   root that `pkexec` and polkit do not mediate. Rejected.
5. **A polkit-gated root helper with a fixed argv.** `pkexec` runs
   `/usr/lib/eclipse/ec-update-apply`, which accepts only `eclipse-deps PKG...`
   and `all`, validates every name, and execs pacman with fixed flags. Accepted.

## Decision
- **Offer only when pending.** The service reports `Update::Pending` with the
  Eclipse list and the Eclipse-plus-closure list, or `Update::Unavailable`. The
  widget offers the item only on `Pending` with a non-empty Eclipse list. The
  service enforces the same rule on apply: with nothing Eclipse pending, apply is
  `Refused` and no process runs.
- **Scopes.** `EclipseAndDeps` is option 3, run as
  `ec-update-apply eclipse-deps <pkgs>`, which execs
  `pacman -Sy --needed --noconfirm <pkgs>`. `All` is `ec-update-apply all`,
  which execs `pacman -Syu --noconfirm`.
- **Partial-upgrade mitigation.** The Eclipse-only path is a deliberate partial
  upgrade. The closure limits it to packages Eclipse needs. `-Sy` makes the sync
  database current first, so the install does not take a stale version. The
  residual risk (a package outside the closure that links against a library
  version the closure moved) is the same one `pacman -S` always carries; the
  widget says so in the scope prompt, and the owner can choose scope (b).
- **Polkit helper.** The privileged step is `pkexec` under the polkit action
  `org.eclipse.update.apply` (auth_admin_keep, exec path
  `/usr/lib/eclipse/ec-update-apply`). The helper accepts only the two shapes
  above, with names matching `[a-z0-9@._+-]+` and no leading `-`, and refuses
  anything else before pacman starts. Its root check comes after validation so
  the validation can be tested without root. Changing the helper or the policy
  is the privilege change to review.
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
- `eclipseos-desktop` installs `/usr/lib/eclipse/ec-update-apply` and
  `org.eclipse.update.apply.policy`, and depends on `polkit`.
  `eclipseos-hyperion` lists `pacman-contrib` as an optdepend: without it the
  item is never offered.
- Discovery needs the Eclipse packages to exist in the **sync** database
  (`pactree -s`). If they do not, the closure cannot be read and the service
  reports `Unavailable`. That is deliberate (fail closed), and it is the state on
  any host that does not consume the EclipseOS repo.
- Scope (a) can leave the system partially upgraded. The owner accepts that
  explicitly by choosing it; the prompt says what it does.
- The feed job expires after 60 s without a pacman transaction line. Long
  download or scriptlet phases may let the job drop off the widget while the
  upgrade still runs. Not fixed here.
- Owed: an owner review of `packaging/bin/ec-update-apply` and
  `packaging/polkit/org.eclipse.update.apply.policy` (F-07 §4), and a manual run
  of the apply path on a disposable VM before this ships.

## Revisit when
- Arch or pacman gains a supported way to install a subset with a consistent
  dependency view, or the EclipseOS repo starts shipping its own consistent
  snapshot.
- The helper needs a third shape. Widening it is a privilege change: write a new
  ADR, do not add an option.
- `auth_admin_keep` proves too lax for a session that is inactive or remote; the
  `allow_any`/`allow_inactive` defaults are the first things to tighten.
