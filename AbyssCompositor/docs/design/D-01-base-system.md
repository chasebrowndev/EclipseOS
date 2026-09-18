<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# D-01 — Base system: packages, kernel, init, filesystem, users, defaults

Status: **written 2026-09-18**, first Tier 6 document. Supersedes nothing.

Governing specs: Tier 6 D-01. Constrained by F-01 (charter), F-04 (reference
hardware), F-05 (licensing), F-07 (repository and process), Appendix B B-01/B-02.
Where this document and a spec disagree, the spec wins and this document is the
bug.

**This document is written out of phase.** F-01 §8 makes the phases sequential
and puts distribution in Phase 6; the charter explicitly removed the early
"minimal ISO" phase. D-01 is pulled forward anyway because three of its six
answers are decisions abyss code is accreting assumptions about *today* — where
`policy.kdl` lives, what the unit layout is, and what a user needs to get a seat.
The rest of Tier 6 (D-02, D-04, D-06, D-08) stays in Phase 6 and is not
unblocked by this document. D-03 is, and is scoped in §7.

**Audience of one.** The first EclipseOS image installs on the owner's hardware
and nowhere else (F-01 charter decision, 2026-09-18). That removes public
mirrors, the COMP-15 §2 security suites and S-12 supply chain from D-01's
critical path. Every place where this document takes the personal-scale answer
says so explicitly, so the general-audience version is a diff and not a rewrite.

---

## 1. Base package set

EclipseOS is Arch Linux plus the Eclipse stack. There is no independent base:
the package set below is expressed as Arch package names, and anything not
listed is "available, not installed".

### 1.1 Eclipse packages

Four packages, split so the repo can move pieces independently (D-02, and
§3.4's restart policy depends on the split):

| Package | Contents | License |
|---|---|---|
| `eclipseos-abyss` | `abyss`, `eclipse-ctl`, the session entry, `abyss-session`, the user units | AGPL-3.0-only |
| `eclipseos-desktop` | `eclipse-bar`, `eclipse-settings`, `eclipse-center`, `eclipse-launcher`, `eclipse-policy-viewer`, `eclipse-toasts`, their `.desktop` files and user units | AGPL-3.0-only |
| `eclipseos-policyd` | `policyd` and its system/user unit | AGPL-3.0-only |
| `eclipseos-meta` | Depends on the three above plus §1.2; ships the pacman drop-in and `/etc/eclipse` defaults | AGPL-3.0-only |

**Four of the six desktop binaries come out of one crate.** `eclipse-bar`
declares `eclipse-toasts`, `eclipse-center` and `eclipse-launcher` as extra
`[[bin]]` targets (`crates/eclipse-bar/Cargo.toml`) because they share its
tokens and its service layer; there is no `crates/eclipse-center` and there
never will be. A PKGBUILD that iterates crate directories will silently ship
three fewer binaries than the DE needs, so `eclipseos-desktop` enumerates
binaries, not crates.

`eclipse-ipc`, `eclipse-ui`, `eclipse-services` and `policy-eval` are libraries
and ship inside their consumers; they are not separately packaged. `Oracle-Eyes`
is **not** in the first image: it path-depends on `eclipse-ipc` in a sibling
workspace, so it cannot be built from a single source tarball, and it targets
Phase 2 trusted UI that does not exist. The hand-placed `oracle-eyes.service`
on `mainframe` is a development artifact and must not be packaged (§3.3).

### 1.2 Runtime dependencies

Hard — abyss does not start without them:

`systemd` (logind, §5), `seatd` (fallback only, §5.3), `libinput`, `libudev`
(via `systemd`), `mesa`, `libdrm`, `libgbm`, `libdisplay-info`, `libxkbcommon`,
`wayland`, `pixman`.

Hard for the desktop:

`pipewire`, `pipewire-pulse`, `wireplumber` — audio, and the screencast portal
path later. `xdg-desktop-portal` plus `xdg-desktop-portal-wlr` as the interim
backend until an Eclipse portal exists (D-05). `xorg-xwayland` for COMP-07.

Hard for the bar, and easy to forget: **`networkmanager`, `bluez`, `bluez-utils`
and `upower`.** The DE is a read-only D-Bus consumer of all three. If they are
not in the base set the bar comes up with dead cells and looks broken for
reasons that are not the bar's fault.

Build-time only, and therefore *not* in the image: `rustup`/`rust`, `pkgconf`,
`git`. `dist/install.sh` installs these because it builds from source; the
packaged path does not.

### 1.3 The Appendix B floor

B-01/B-02 commit to a user reaching a working configuration without editing a
file. That makes `eclipse-settings` part of the base set, not an option, and it
makes a terminal part of the base set too. `cataclysm` (P-04) does not exist
(defect 12), so the first image ships `foot` as the default terminal and
`eclipseos-desktop` declares it an optional-but-installed dependency that
`cataclysm` will later replace. Say the substitution out loud in D-05 rather
than letting `foot` become the default by silence.

---

## 2. Kernel

**Stock Arch `linux`.** No custom build, no patches. EclipseOS has no kernel
requirement that Arch does not already meet: abyss needs KMS, DRM atomic,
render nodes and `evdev`, all of which are stock. Carrying a kernel would add a
build, a signing story and a rollback surface for zero function.

`linux-lts` is shipped as a second installed kernel with its own boot entry.
This is the only rollback that exists before the atomic scheme (D-04), and it is
worth an extra 150 MB.

### 2.1 NVIDIA

F-04 makes NVIDIA the reference hardware: `nvidia-open-dkms` plus
`nvidia-utils`, and `linux-headers`/`linux-lts-headers` for DKMS.

`nvidia-drm.modeset=1` is required. On current `nvidia-open-dkms` it defaults on,
but the image sets it explicitly in the bootloader entry rather than inheriting a
default that upstream may flip.

**The sharp edge:** abyss's guard at `crates/abyss/src/backend/drm.rs:141` tries
to read the parameter and cannot — `/sys/module/nvidia_drm/parameters/modeset`
is readable, but the guard's failure mode as a normal user is that it cannot
distinguish "off" from "unreadable" and proceeds. That is correct behaviour for
a compositor (refusing to start because it could not read a sysfs file would be
worse) but it means **the image, not the compositor, is responsible for the
parameter being right.** D-03's bootloader configuration owns it, and the ISO
must fail loudly at install time if `nvidia-drm.modeset` is not set, because the
symptom otherwise is a black screen with no explanation.

Intel and AMD need no packages beyond `mesa`; F-04 tests Intel and validates AMD
before any release beyond the owner.

### 2.2 Firmware and early KMS

`linux-firmware`. `mkinitcpio` with the stock `kms` hook for Intel/AMD; for the
NVIDIA path the modules go in `MODULES=(nvidia nvidia_modeset nvidia_uvm
nvidia_drm)` and the `kms` hook comes **out**, which is the standard Arch NVIDIA
arrangement and the one place D-03's `mkinitcpio.conf` is not stock.

---

## 3. Init and systemd layout

### 3.1 The shape

```
greetd (system) ──► abyss-session (user login shell of the session)
                      └─ abyss --backend drm --session
                           └─ session::import() → systemd user manager
                                └─ graphical-session.target
                                     └─ abyss-session.target
                                          ├─ eclipse-bar.service
                                          ├─ eclipse-toasts.service
                                          └─ (Phase 2: agentd, brokerd)
```

`abyss-session.target` already exists (`dist/abyss-session.target`):
`BindsTo=graphical-session.target`, `Wants=`/`After=graphical-session-pre.target`.
That stays. The rule it encodes — **the target dies with the session, so every
per-session service is bound to it and nothing leaks across a logout** — is the
layout's one invariant.

### 3.2 System units vs user units

The line is the TCB boundary, not convenience.

- **User units**: everything that renders or talks to the user's session —
  `abyss-session.target` and every `eclipse-*` service. They live in
  `/usr/lib/systemd/user/` (shipped by the package; the development
  `install-session.sh` writes `~/.config/systemd/user/` instead, which is why
  the packaged path and the dev path differ and the package must not fight the
  dev symlinks — see §4.4).
- **System units**: nothing today. `policyd` is the first candidate and is
  decided in §3.4.

`eclipse-bar.service` and `eclipse-toasts.service` are `WantedBy=abyss-session.target`
and enabled by the package via a `systemd-user.preset`, not by a post-install
`systemctl --user enable` (which cannot run for a user who does not exist yet at
install time).

### 3.3 What is not packaged

`oracle-eyes.service` currently sits in `~/.config/systemd/user/abyss-session.target.wants/`
on `mainframe`. It was placed by hand and `install-session.sh` does not install
it. **It is not part of EclipseOS** and `eclipseos-desktop` must not ship it.
Recorded here so a later packaging pass does not "fix" the omission.

### 3.4 policyd: a user service, and the restart policy

`policyd` holds the audit journal and the issuing key. Both are per-user state
(`$HOME/.local/state/eclipse/policyd`, `0700`, key `0600` — see
`crates/policyd/src/main.rs`), and a grant is scoped to a human. **Therefore
`policyd` is a user service, not a system service.** A system `policyd` would
have to multiplex users across one journal and one key, which is a larger
security surface for no gain on a single-seat machine.

```
# /usr/lib/systemd/user/policyd.service
[Unit]
Description=Eclipse policy daemon
Before=abyss-session.target
[Service]
Type=notify
ExecStart=/usr/bin/policyd
Restart=on-failure
RestartSec=1
```

Three decisions inside that, each of which is the interesting part:

1. **`Restart=on-failure`, not `always`.** A clean exit of a TCB daemon is a
   decision it made; restarting over it is how a fail-closed component becomes
   fail-open by accident.
2. **No `StartLimitBurst` escape.** If `policyd` crash-loops, the session must
   become unusable, not degraded. Policy is fail-closed (CLAUDE.md invariant):
   an unavailable `policyd` denies. A user staring at a session where everything
   is denied is the correct symptom, and it is the one that gets reported.
3. **`Before=abyss-session.target`, not `BindsTo`.** abyss must be able to start
   with `policyd` down and deny everything, rather than refuse to paint. A
   display server that will not come up because a policy daemon is sick leaves
   the user with no way to fix the policy daemon.

Socket activation is **rejected** for `policyd`. Activation-on-connect means the
first `check()` pays a process start, and COMP-11's check is on a hot path with
no allocation allowed. The daemon starts eagerly and stays up.

`agentd` and `brokerd` (Phase 2) attach as user services `WantedBy=abyss-session.target`,
`After=policyd.service`. They are named here only so the ordering is not
reinvented; their units are S-tier documents' business.

### 3.5 greetd

greetd stays the display manager, running `cage -s -m last -- regreet`. This is
already the arrangement on `mainframe` and there is no reason to own a greeter.
The session entry is `/usr/share/wayland-sessions/abyss.desktop` with
`Exec=/usr/bin/abyss-session`.

`abyss-session` passes `--backend drm` **explicitly**. `default_backend()` picks
DRM only by the *absence* of `WAYLAND_DISPLAY`/`DISPLAY`, and a greeter that
leaks either would silently start a nested winit compositor inside itself. Do
not "simplify" that flag away.

---

## 4. Filesystem layout

### 4.1 The ownership split

Decision 2 of the roadmap says updates are a signed pacman repo now and an
atomic image later. **The binding constraint on this section: nothing the
machine owns may live in a path an image swap would replace.** Every path below
is labelled.

| Path | Owner | Atomic-safe |
|---|---|---|
| `/usr` | **image** | replaced wholesale |
| `/etc/eclipse/abyss.kdl` | **image** (shipped defaults, §6) | replaced; must be pristine |
| `/etc/eclipse/policy.kdl` | **image** | replaced; must be pristine |
| `/etc/eclipse/*.d/` | machine | reserved, not yet read by abyss |
| `~/.config/eclipse/` | **machine** | untouched |
| `~/.local/state/eclipse/` | **machine** | untouched |
| `/var/lib/eclipse/` | machine | reserved; nothing writes it today |

The rule that follows: **the shipped configuration is read-only and lives under
`/etc/eclipse`, and no component ever writes there.** `eclipse-settings` writes
`~/.config/eclipse/abyss.kdl` and nothing else. An image swap replacing
`/etc/eclipse` therefore loses nothing the user did.

This is a real constraint on Arch semantics: pacman treats `/etc` as
`backup=()`-protected and would leave a modified `/etc/eclipse/abyss.kdl` in
place as `.pacnew`. **The Eclipse packages deliberately do not list the
`/etc/eclipse` files in `backup=()`**, so pacman overwrites them on upgrade.
That is the correct behaviour precisely because nothing is supposed to edit
them, and it is what makes the later atomic scheme a no-op migration rather than
a merge problem.

### 4.2 The config search path, as built

From `crates/abyss/src/config/mod.rs`, in apply order, later overriding earlier:

1. `/etc/eclipse/abyss.kdl` *(image)*
2. `/etc/eclipse/policy.kdl` *(image)*
3. `$XDG_CONFIG_HOME/eclipse/abyss.kdl` *(machine)*
4. `$XDG_CONFIG_HOME/eclipse/abyss.d/*.kdl`, sorted *(machine)*
5. `$XDG_CONFIG_HOME/eclipse/policy.kdl` *(machine)*
6. `$XDG_CONFIG_HOME/abyss/config.kdl` — compatibility fallback, **deprecated**

`--config <path>` replaces the whole path with one file.

Two facts about this that D-01 ratifies rather than invents:

- **Each directory contributes its `policy.kdl` after its `abyss.kdl`.** A
  policy-owned key written into `abyss.kdl` is *refused*, not shadowed (ADR 0037,
  COMP-13 §1.3). abyss will not start if it finds one.
- **There are deliberately no `policy.d/` drop-ins.** One policy file per
  directory, so "what is the policy here" has one answer a human can read. D-01
  keeps that and extends it: `/etc/eclipse/policy.d/` must never be added.

Item 6 is scheduled for deletion. It exists for the M2 task brief and nothing
else; D-01 marks it removable at the first Eclipse release.

**The user's `policy.kdl` overriding the shipped one is intentional and is a
personal-scale answer.** For an audience of one, the human and the
administrator are the same person. A multi-user or managed EclipseOS must invert
this — `/etc/eclipse/policy.kdl` last, and root-owned — and that inversion is a
one-line change to `search_path()`. Flagged here so the general-audience version
is a known diff and not a discovery.

### 4.3 State

- **Audit journal and issuing key**: `~/.local/state/eclipse/policyd/`, directory
  `0700`, `issuer.key` `0600`. Already implemented. `$ECLIPSE_STATE_DIR`
  overrides it for tests and nested dev instances; nothing else in the daemon
  reads the environment.
- **Runtime sockets**: `$XDG_RUNTIME_DIR/eclipse/` — `abyss.sock` is the human
  JSON-RPC control socket (COMP-13). Per-session, tmpfs, never persisted.
- **Per-agent sandbox images** (milestone 20, does not exist): reserved at
  `/var/lib/eclipse/images/`, machine-owned. Named now so milestone 20 does not
  put them in `/usr` and break the atomic constraint.

### 4.4 The development install coexists

`dist/install-session.sh` symlinks `/usr/local/bin/*` into `target/release` and
writes user units into `~/.config/systemd/user/`. The packages install to
`/usr/bin` and `/usr/lib/systemd/user/`. Those do not collide — `/usr/local/bin`
precedes `/usr/bin` on PATH and user units shadow system ones — so a developer
box runs the build and a packaged box runs the package, with the same file
names. This is deliberate and D-03 must not "clean up" `/usr/local/bin`.

---

## 5. User and group model

### 5.1 The rule

> **A user needs a logind session on a seat, and nothing else.**

No root. No `seat` group. No seatd. No membership in `video` or `input`.

### 5.2 Why, and what the 2026-09-10 confusion was

libseat prefers its logind backend. logind puts an ACL on the DRM node for the
user who holds the active session on the seat, so the node is readable and
writable by exactly that user for exactly as long as they are logged in.
Verified on `mainframe`, 2026-09-18:

```
$ loginctl list-sessions
  4  1000 chase  seat0  tty1   user
$ getfacl /dev/dri/card1
  user::rw-
  user:chase:rw-      ← logind put this here
  group::rw-
```

`chase` is in `video` and `input`, and **that is not what makes this work** —
the ACL is. There is no `seat` group on Arch at all; `OS_WORK.md`'s reference to
one was a guess and this section retires it.

The 2026-09-10 KMS boot needed root plus `LIBSEAT_BACKEND=seatd` because it was
launched with `openvt`, which creates no logind session. That was an artifact of
the launch method, not a requirement of the compositor. **EclipseOS's installer
therefore does no group management at all** beyond the Arch default of adding
the first user to `wheel`.

### 5.3 seatd as a fallback

`seatd` stays in the base set (§1.2) and stays disabled. It is the escape hatch
for a system where logind is broken or absent, reached by setting
`LIBSEAT_BACKEND=seatd` and enabling the service. Documented, not default.
Shipping it disabled costs nothing and removes the one recovery path that
otherwise requires a second machine.

### 5.4 System users

`greeter` (already created by the `greetd` package). No Eclipse system user
exists, because §3.4 puts `policyd` in the user session. When `brokerd` arrives
and needs a system identity, it gets a dynamic one (`DynamicUser=yes`) rather
than a packaged static uid — decided here so the question is not reopened.

---

## 6. Shipped defaults

Two files, both under `/etc/eclipse`, both image-owned, both read-only.

### 6.1 `/etc/eclipse/abyss.kdl`

A convenience default. It should be **nearly empty**: every key it sets is a key
a user discovers by surprise rather than by choosing. The shipped file carries
only what a machine cannot infer and a user would otherwise hit as a defect:

- the default terminal binding (§1.3, `foot` until `cataclysm`),
- nothing else.

Layout, animation and decoration defaults belong in the schema's own defaults in
`crates/abyss/src/config/schema.rs`, not in a shipped file. A default that lives
in a config file is a default the user has to delete to get back to; a default
that lives in the schema is one they never see. **Prefer the schema every time.**

### 6.2 `/etc/eclipse/policy.kdl`

**A shipped `policy.kdl` is a security default, not a convenience default, and
gets the same scrutiny as code** — owner line-by-line review, same as any TCB
change (F-07 §4).

It ships **deny-by-default and essentially empty.** Policy is fail-closed: an
absent table entry denies. So the shipped file grants nothing, and the first
image's answer to "what may an agent do out of the box" is "nothing". That is
the only defensible default for a system whose entire premise is that agents
operate under capability checks, and it costs nothing today because the agent
stack does not exist.

The file is shipped rather than omitted so that `policy.kdl`'s existence, path
and ownership are established before there is anything at stake, and so the
first grant is a diff against a known file.

### 6.3 What is *not* shipped

No default wallpaper, theme or font configuration in `/etc/eclipse`. Appearance
is `eclipse-settings`' business and belongs to D-05, not here. D-01 shipping a
theme would put a machine-owned concern in an image-owned path, which §4.1
forbids.

---

## 7. What this unblocks, and what it does not

**Unblocked: D-03 (ISO and installer).** It inherits §1 as `packages.x86_64`,
§2 as `mkinitcpio.conf` plus the bootloader entry, §3 as the `airootfs` greetd
overlay, §4 as the `/etc/eclipse` overlay, and §5 as "the installer creates a
normal user and does nothing else". D-03 carries the §7 gate — *fresh install to
a working agent session in under 30 minutes, documentation only.* With no agent
stack, D-03 measures the honest half: **ISO to a working abyss session in under
30 minutes**, and records the gate as partially met.

**Still blocked, and not by this document:** D-02 (on S-12), D-04 (on D-02),
D-05 (on `cataclysm`, defect 12), D-06 (on F-04 validation runs), D-07 (on
D-03), D-08 (on F-02).

**Left open deliberately**, to be answered by the document that owns it, not by
D-01 guessing:

1. Whether `/etc/eclipse/policy.kdl` becomes last-wins for a multi-user image
   (§4.2). D-07 or the general-audience revision of this document.
2. The `cataclysm`-for-`foot` substitution (§1.3). D-05.
3. Reproducibility of the image itself. D-02, and it is the reason §4.1's
   ownership table exists now rather than later.
