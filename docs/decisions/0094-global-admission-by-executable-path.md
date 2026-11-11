# 0094 — Security globals admit by executable path
Status: proposed
Date: 2026-11-10
Deciders: chase (owner), Claude (advisory)

Amends 0022 and 0027 (and 0030, which shares 0027's gate).

## Context
An audit of the human-path Wayland globals found four gaps:

- **SEC-04.** `data_control::client_name` returned the *basename* of
  `/proc/<pid>/exe`. The screencopy, image-copy-capture and data-control
  allowlists compared against it, so a copy of `grim` at `/tmp/grim` was
  admitted as `grim`. ADR 0022 and 0027 called the identity "containment, not
  authentication", but containment that any user-writable directory defeats
  contains nothing.
- **SEC-01.** `zwp_input_method_manager_v2` was created with `|_| true`: any
  client could become the input method, read the focused field's surrounding
  text and commit text into it.
- **SEC-02.** `zwlr_virtual_pointer_manager_v1` had no bind filter. Vol 1
  COMP-06 §2 says virtual pointer/keyboard are "enabled only for an explicit
  client allowlist", and §5 asks for a test that the globals are absent for
  non-allowlisted clients.
- **SEC-03.** `ext_foreign_toplevel_list_v1` published the title and app_id of
  every window, `secret` ones included. `registryd` reads that list and
  `secret` windows never reach it (Vol 2 S-05 §2).

Security allowlists are policy-owned. They live in `policy.kdl`, not
`abyss.kdl` (COMP-13 §1.3, ADR 0037). COMP-06 §4 says data control is
"allowlisted by config". COMP-05 app identity is still the planned
replacement for exe-based identity (ADR 0022, 0027).

Three layouts must keep working:

- **Installed.** Pacman binaries live in `/usr/bin`. `xdg-desktop-portal-wlr`
  lives in `/usr/lib`.
- **Dev.** `packaging/install-session.sh` symlinks the build tree's binaries
  into `/usr/local/bin`. `/proc/<pid>/exe` reports the build-tree target, not
  the link.
- **Tests.** In-process test clients are the test binary itself.

## Options
1. **Keep basenames and add a directory check** (only `/usr/bin`, `/usr/lib`,
   …). This brings in a second, implicit list of trusted directories, and it
   still cannot express the dev layout.
2. **Full executable path, with a bare-name shorthand.** An entry containing
   `/` is an exact absolute path. A bare name means `/usr/bin/<name>`. Entries
   are resolved through symlinks when the list is loaded. The client side is
   the kernel's already-canonical `/proc/<pid>/exe`.
3. **A DE identity mechanism** (signed binaries, a root-owned manifest). That
   is a new trust mechanism and belongs to COMP-05.

## Decision
**Option 2**, for `capture.allow`, `clipboard.data-control-allow` and two new
policy-owned keys, `input-method.allow` and `virtual-pointer.allow`.

- **Identity.** `data_control::client_exe` reads `/proc/<pid>/exe` and returns
  it only if it is absolute and does not end in `" (deleted)"`. It denies on
  `pid <= 0` and on a read error. The `comm` fallback is gone.
- **Matching.** `ExeAllowlist` (in `ec-abyss-config`) resolves each entry once,
  in `new` and in `set` (startup and every reload), with `canonicalize`. A
  path that does not exist is kept literal. Matching is then path equality.
- **Validation.** An entry that is empty, `.`, `..`, or relative with a `/` is
  a parse error. In `policy.kdl` that is fatal (ADR 0064).
- **Warnings.** An entry whose path does not exist is logged as a warning at
  startup and on reload, with the fix.
- **Dev layout.** It needs no new mechanism. The owner writes the
  `/usr/local/bin/<bin>` path in `~/.config/eclipse/policy.kdl`. The
  root-owned symlink decides which file that admits, as of the last reload.
- **Tests and WLCS.** They admit `std::env::current_exe()` through the same
  lists. No bypass flag exists.
- **Input method.** It gets the shared `allow_filter`.
- **Virtual pointer.** Its `can_view` uses the shared `admit`.
- **Foreign-toplevel list.** It stays open to every client. A window is
  withheld while it is `secret`:
  - **Rule.** The rule is the same as capture redaction
    (`render::capture::classify`): in the sensitive set, or app_id in
    `capture.redact-app-id`, or, once a policy table is live,
    `policy::classes::class_of == Secret`.
  - **At map.** A window that is `secret` when it maps is never published.
    `window_mapped` now runs after the window rules.
  - **On raise.** A window raised to `secret` is withdrawn with `closed`.
    This happens on a commit, after the rules re-run, so the raising title is
    never sent. It also happens on config reload (`refresh_all`).
  - **On lowering.** A window that drops back below `secret` is published
    again under a new identifier.

`capture.hide-layer` and `eclipse_glass_v1` keep basename matching. Hiding a
layer only tightens, and glass panes are cosmetic. Neither is an admission to
a security global.

### Shipped default
`/etc/eclipse/policy.kdl` ships `input-method { allow "fcitx5" "kime"
"squeekboard"; }`. These are the Arch input-method-v2 clients a user would
install: fcitx5 for CJK, kime for Korean, squeekboard for the on-screen
keyboard. Without them the global would be hidden from every IME on a stock
install, which would regress the Phase 1 milestone 5 "IME" row.

ibus is left out. Its Wayland frontend lives under `/usr/lib/ibus/`, and which
input-method protocol version it speaks was not verified.

The schema default stays empty. `schema.rs` pins every policy-owned default to
`EmptyList`, so the grant sits in the image's file, where it gets the owner
review that file states.

Nothing in the tree uses the virtual pointer except WLCS and the tests, so
`virtual-pointer.allow` ships empty.

## Consequences
- **SEC-01..04 narrowed, not closed.** A client that merely connects, or a
  copy of an admitted binary elsewhere, is now refused. A same-uid process
  that works at it is not (see "Still not authentication"). Each fix has a
  test:
  - `protocols/standard/admission_tests.rs`: the globals are absent by
    default and visible to the allowlisted executable. The same basename
    elsewhere is denied, a bind to a hidden global is a protocol error, and a
    reload revokes for new clients.
  - `screencopy` tests: `/tmp/<name>` is denied.
  - `data_control` tests: a deleted executable is denied, and so are
    `pid <= 0` and a relative link.
  - `foreign_toplevel` tests: a `secret` window is absent, a raise withdraws
    it before the raising title is sent, and `refresh_all` withdraws on a
    raise that comes without a commit.
- **Existing user policies need editing.** A bare name now means
  `/usr/bin/<name>` only. `xdg-desktop-portal-wlr` must be written as
  `/usr/lib/xdg-desktop-portal-wlr`, and a dev build as
  `/usr/local/bin/<bin>`. Abyss logs a warning for every entry that names no
  file.
- **Rebuilds revoke.** A rebuilt binary leaves the running process's exe as
  `(deleted)`. The next capture request or bind from it is denied until it is
  restarted, which is correct: the admitted file is gone.
- **A reload revokes new binds only** for data-control, input-method and
  virtual-pointer: the filter runs at bind, so a client already bound keeps
  its global until it disconnects. Screencopy re-checks on every request.
  Removing an entry therefore needs the client restarted to take effect.
- **Image-copy-capture's per-frame check is weaker than screencopy's.** It
  compares against the single `capture_consumer` slot (the last session
  created), not the frame's own client, and does not re-read `/proc`. A
  session whose entry was removed or whose binary was rebuilt keeps capturing
  while another admitted client holds a later session. Predates this change;
  the "Rebuilds revoke" line above holds for screencopy and for new binds
  only.
- **Still not authentication.** The executable path says which file was
  exec'd, not who holds the socket. Stated limits, all same-uid:
  - *Exec after connect.* A process connects, then execs (or forks a child
    that keeps the fd and execs) an admitted binary; `SO_PEERCRED` and
    `/proc/<pid>/exe` then read as admitted.
  - *Injection.* `LD_PRELOAD`, ptrace or `/proc/<pid>/mem` into an admitted
    binary runs arbitrary code under its path.
  - *Writing an admitted path*, and pid reuse between the credential read and
    the decision.
  - *Mount namespaces* may hide the real file behind an admitted path
    (untested).
  These are the same class ADR 0028 accepts for the control socket. The
  replacement is a principal the client cannot choose: a cgroup/unit identity
  resolved by the compositor (Threat Model §7: cgroup identity is the principal identity), not this lookup.
- **Policy table refresh (TCB, in this change).** `policy/table.rs::receive`
  calls `foreign_toplevel::refresh_all` once a verified table is live, so a
  window the new table makes `secret` is withdrawn without waiting for a
  commit.
- **Owed outside this change:**
  - **Settings.** `ec-settings` `place_for` needs arms for the two new keys,
    and `ec-policy-viewer` needs its mirrored policy-owned set updated.
  - **Oracle Eyes.** Its "add `capture { allow … }`" hint should name a path.
  - **Spec.** COMP-06 §2 asks for a trusted-UI indicator while a virtual
    pointer is active. That is not built.

## Upgrade note
A bare entry now means `/usr/bin/<name>` and nothing else. Every entry in an
existing `policy.kdl` for a binary outside `/usr/bin` must become its absolute
path, or it admits nothing:

```kdl
capture { allow "grim" "/usr/lib/xdg-desktop-portal-wlr" "/usr/local/bin/ec-oracle-eyes"; }
```

`grim` can stay bare because it is `/usr/bin/grim`. The portal backend lives
in `/usr/lib`. A dev build is admitted through its `/usr/local/bin` symlink.
Abyss logs a warning naming each entry that points at no file. A process
whose binary was rebuilt or deleted after it started is denied until it is
restarted.

## Open (spec silent; nothing implemented)
1. **Password fields.** May an input method receive text from inputs with
   `password` purpose? COMP-19 §10 item 2 is an open proposal. No withholding
   was implemented.
2. **A filter on foreign-toplevel itself.** Should ordinary clients be
   filtered from `ext_foreign_toplevel_list` at bind time? No: third-party
   bars need it, and only `secret` windows are withheld.
3. **The no-table case.** With no policy table, `class_of` calls every window
   `secret` (S-05 §8). The list follows capture's precedent and counts only
   the compositor's own set then. Whether the human-path list should go empty
   instead is an owner call.

## Revisit when
COMP-05 app identity lands, at which point admission keys on it and the
`/proc` lookup goes. Also revisit when `policyd` owns these allowlists as
table entries.
