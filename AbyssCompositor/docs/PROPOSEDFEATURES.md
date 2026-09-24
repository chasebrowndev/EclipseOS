<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Proposed features

Things the owner has asked for that are **not scheduled yet**. This file is the
backlog of intent: a note here is a decision already made about direction, not
an idea up for debate. Delete an entry when it ships (and say where it landed);
never silently drop one.

The governing goal behind most of this: **EclipseOS is a WM and DE hybrid.**
Tiling/keyboard-first behaviour underneath, a real desktop environment on top —
taskbar, context menus, drawers, applets. When a design choice splits the two,
that hybrid is the tiebreaker.

---

## Everything configurable

Nothing that is a matter of taste may be a constant in the source. The accent
colours are the live example — the taskbar's yellow "up" accent is currently
`eclipse_ui::tokens`, and it should be a settings value like any other. The
rule generalises: palette, geometry, clock format, chip behaviour, which
applets appear and in what order.

`eclipse_ui::tokens` stays the only *source* of colour/size/radius for the
views; the question is where the tokens themselves come from. Today they are
compiled in. They should be loaded from config, with the compiled values as
defaults.

## Taskbar

- **Push the glass further.** `bar_ground` (`crates/eclipse-ui/src/theme.rs:124`)
  is already meant to read as frosted glass over the compositor's dual-Kawase
  blur (COMP-02 §9), but today it's a flat translucent `GLASS_DEEP` fill with
  no blur-aware treatment of its own — no adjustable frost/tint balance, no
  distinct look from the other `surface`-derived panels. The blur-off fallback
  and the radius sync have landed (`c0545f3`, `6d85517`); the next layer is to
  give the bar its own glass
  identity — tint strength, edge highlight intensity, maybe a settings-exposed
  frost amount — rather than reusing the same translucent constant every
  `surface` uses.
- **Clock → calendar drawer.** Clicking the time/date opens a full calendar
  panel, the way Windows does. Not built: `crates/hyperion/src/view.rs` carries
  the `TODO`. The clock formats shipped as `bar.clock.hour-12` and
  `bar.clock.date-mdy` (2026-09-23).
- **Sustained hover shows the full detail.** Resting the pointer on a chip pops
  the detail the ladder dropped — full title and path, whatever the chip itself
  had to shed. It goes away as soon as the pointer moves again. This is the
  escape hatch that lets the ladder be aggressive: no rung has to preserve
  legibility that a hover can recover.
- **A Launcher settings pane.** The launcher
  (`crates/eclipse-launcher/src/main.rs`, spawned by `Super+E` and `Super+R` —
  `crates/abyss/src/config/mod.rs`, `default_binds`) has no `launcher.*` keys;
  the only config it reads is `misc.terminal-command`, once at startup
  (`crates/eclipse-launcher/src/conn.rs`), so it has no tab in
  `eclipse-settings` either. It wants a `launcher.*`
  section in the schema table and its own pane alongside Taskbar once there is
  something to put in it: result count, whether it searches paths as well as
  desktop entries, where it anchors. Deferred deliberately — the Taskbar pane
  shipped first because `bar.*` had real keys to expose.

- **Wifi and bluetooth pickers — decided and built (ADR 0053).** The drawers
  scan, join, disconnect, pair and connect through the `eclipse-services::status`
  action path. Passphrases and PINs go through `eclipse-secret-prompt`, a separate
  toplevel whose whole surface is `secret`. What is left:
  - **`Pairing.Answer` is not authenticated.** Any process running as the same
    user can answer a pairing request on the session bus while its prompt is
    open (answers sent early are refused). That is the same trust level as the
    user's own session, but it should be narrowed to the prompt's own connection.

## Window management

- **Per-window mute needs a real audio abstraction.** `hyperion/src/audio.rs`
  shells out to `pactl -f json list sink-inputs` and joins on pid (or any
  descendant pid). That works, but it puts a subprocess on a UI path and it
  hardcodes PulseAudio/PipeWire's CLI. A proper audio service belongs in the
  userland crates.

## Install and boot

- **A first-class GUI installer — no command line, ever.** The CLI
  `install-eclipseos.sh` and the archinstall + `eclipseos-postinstall.sh`
  route are both explicitly temporary. The shipped installer is a graphical
  one, and a user must be able to go from boot medium to a working desktop
  without ever seeing a terminal. That is the bar: not "a TUI with nice
  colours", not "a GUI for the common case with a shell for the rest" — the
  full path, disks and all.

  What it has to cover before the CLI scripts can be deleted: disk selection
  and partitioning, including dual-boot and reusing an existing ESP, which the
  script refuses today (it wipes one whole disk); filesystem choice with btrfs
  first-class, since D-04's snapshot story wants it; LUKS; swap or zram;
  timezone, locale, `vconsole`, keymap; user and password creation; wifi,
  carried from the live medium; and the bootloader install. It should be an
  `iced` app in the DE's own visual language, run by the live medium's own
  compositor rather than by a second stack — the medium already boots abyss,
  so the installer is another pane, not another environment.

  Forking archinstall was considered and rejected: it inherits upstream churn,
  it is Python where the DE is Rust/iced, and GPL-3.0-only would permanently
  pin that component. It stays as an escape hatch until the GUI lands.

- **User-chosen boot and shutdown splash, set in Settings.** The EclipseOS
  mark now shows at boot (UKI `--splash`, `dist/boot/splash.bmp`) and at
  shutdown (`eclipse-shutdown-splash.service` blitting to fbdev), with quiet
  boot and no menu. Letting the user pick their own PNG was scoped on
  2026-09-23 and held:
  - Two settings, boot and shutdown; shutdown defaults to the boot image.
  - PNG only. GIF was considered: boot cannot animate (systemd-stub draws one
    BMP), shutdown could, but it was dropped for now.
  - Decode and convert as the user; root only validates fixed-format output
    (BMP for the UKI, raw XRGB for fbdev), installs it and runs
    `mkinitcpio -P`. Root never parses a user-supplied PNG.
  - Applying the boot image needs root, and the owner wants an admin prompt,
    not a passwordless wheel rule. **Blocked:** the session has no polkit
    authentication agent, and a password prompt is trusted UI that must be
    compositor-drawn (COMP-10). This lands after that, or with an interim
    `sudo eclipse-splash apply` step if one is wanted sooner.
  - Settings has no path/image control yet (`schema.rs` maps `string` to a
    text field), so the pane needs an image picker type.

- **The greeter gets its own package.** `eclipseos-meta` owns the greetd
  drop-in and `/etc/eclipse/greetd/`, and depends on every desktop package, so
  removing a desktop package for an unrelated reason cascades into `-meta` and
  silently drops the graphical login (was PKG-02, 2026-09-22). The greeter
  belongs in a package with no dependency edge to the desktop ones.

---

## Known bugs that are really missing features

See `docs/KNOWNBUGS.md` for defects.

Shipped and removed 2026-09-23: **popup anchor as a setting**
(`bar.popup-anchor`), the **tray drawer** (the SNI tray, `360b704`)
and the **chip condensation ladder** together with the `TASK_MAX` width-cap bug
it fixed (`ladder()` in `crates/hyperion/src/view.rs`, `c46ff92`).

**Needs re-verification against `8f76207`** (release tiles of windows that die
off-screen or unmap) and `f58585b` (remap a null-buffer-unmapped toplevel),
both of which touch this bookkeeping. Found 2026-09-22 while screenshot-verifying chip expansion, out of scope for
that change and not diagnosed: closing a window leaves a permanent ghost
entry in `get_windows` (`app_id: null, title: null, minimized: true`, handle
changes each query), and `get_windows`/`get_workspaces` can disagree — stale
nonzero per-workspace counts survive after windows are killed by pid instead
of via `close_window`. Likely bookkeeping in `crates/abyss/src/shell/workspace.rs`.
Needs `eclipse-backend` to reproduce and fix.

---

## Code still to write — the Phase 1 exit and Phase 2 backlog

Owner-requested 2026-09-23: every piece of specified code the tree does not
have yet, in one list, so nothing lives only in `docs/STATUS.md` prose. Spec
first, then where the code goes, then the COMP-16 milestone.

### Phase 1 exit

- **`Toplevel.class_source` and `irreversible_capable`.** COMP-05 §1 (A-12);
  the rest of milestone 9e. Absent everywhere in `crates/`. Goes in
  `crates/abyss/src/shell/` (the toplevel record) fed from `shell/rules.rs`,
  with the unit tests the 9e gate names. *Phase 1, M9e.*
- **Cursor capture for `ext-image-copy-capture`.** COMP-06 §1, ADR
  0029/0030. `CreatePointerCursorSession` is answered with a session that
  never produces a frame (`protocols/standard/image_copy_capture.rs:329`,
  `:344`). Behind the same fail-closed gate and the `capture.cursor` policy.
  *Phase 1, M8.*
- **Per-device input settings.** COMP-04 §2: accel speed, tap-and-drag, click
  method (hardcoded `Clickfinger` in `input::configure_device`), scroll
  method, touchscreen/tablet calibration, per-device overrides. Today only
  global `accel-profile` and touchpad natural-scroll/tap/dwt exist.
  `crates/abyss/src/input/mod.rs`, `config/mod.rs` + `config/schema.rs`.
  *Phase 1 (COMP-04 is a Phase 1 document).*
- **The `mode` key.** COMP-17 §2: `mode "wm" | "de"` selects defaults
  (autostart set, default binds, panel) that explicit config overrides. Does
  not exist; was out of scope by DE plan B7. `config/mod.rs`, `config/schema.rs`.
  *Phase 1, DE userland.*
- **Taskbar clock: calendar drawer.** See Taskbar above;
  `crates/hyperion/src/view.rs`. *Phase 1, DE userland.*
- **Re-verify, don't write:** `BLUR-02` (`KNOWNBUGS.md`, against `107b5de`)
  and the ghost-window entry above (against `8f76207`).
- **Manual gates** (COMP-16 table; `docs/STATUS.md` "Deferred hardware
  verification"): M3 with three monitors on a real TTY, hotplug and
  dock/undock; M4 with Firefox and mpv on real KMS, measured against the M9f
  harness budgets; M6 with lock/idle/DPMS/lid across a real logind
  suspend/resume cycle; then **14 consecutive days as the only compositor**.

### Phase 2 — starts with M11 (privileged socket, `agentd`, `protocols/agent/`)

- **Override and attention chords do something.** COMP-04 §6. Super+Escape
  and Super+Space dispatch `Action::AgentOverride`/`AgentAttention`, which only
  log (`crates/abyss/src/input/mod.rs`). Stubs until agent seats exist.
  *M11 / M13; the attention queue at M15.*
- **Trusted-UI prompt grab.** COMP-10. `crates/abyss/src/shell/focus.rs:188`
  (`TODO(step 6: trusted UI)`): `prompt_grab_active` is always false. Lands in
  `crates/abyss/src/trusted_ui/`. *M15.*
- **Virtual outputs.** COMP-03 §6. `outputs/` handles physical outputs only
  plus the COMP-03 §5 fallback (`OutputKind::Virtual` is that fallback, not
  an agent-workspace output). *M24.*
- **COMP-15 §2 security suites** (redaction, seat isolation, trusted UI,
  enforcement, scope leakage, audit completeness, X11 posture, classification
  races, irreversible matching, provenance, secrets, egress). Each lands with
  the surface it tests, not before. *Across M11–M24.*
