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

- **Push the glass further.** `bar_ground` (`crates/eclipse-ui/src/theme.rs:107`)
  is already meant to read as frosted glass over the compositor's dual-Kawase
  blur (COMP-02 §9), but today it's a flat translucent `GLASS_DEEP` fill with
  no blur-aware treatment of its own — no adjustable frost/tint balance, no
  distinct look from the other `surface`-derived panels. Once `BLUR-01` lands
  (blur-off fallback) this is the next layer: give the bar its own glass
  identity — tint strength, edge highlight intensity, maybe a settings-exposed
  frost amount — rather than reusing the same translucent constant every
  `surface` uses.
- **Clock → calendar drawer.** Clicking the time/date opens a full calendar
  panel, the way Windows does. The clock's own format (12-hour time,
  `m/d/y` date, both configurable per the rule above) is in flight; the drawer
  is not.
- **Sustained hover shows the full detail.** Resting the pointer on a chip pops
  the detail the ladder dropped — full title and path, whatever the chip itself
  had to shed. It goes away as soon as the pointer moves again. This is the
  escape hatch that lets the ladder be aggressive: no rung has to preserve
  legibility that a hover can recover.
- **Popup anchor is a setting.** A chip's context menu can appear either at the
  click point or below the chip, and which one is the user's choice, not ours.
  Below-the-chip is the default; the click-point behaviour stays implemented and
  becomes the other value of the setting. Same rule for every popup the bar owns
  — the drawers included — so the anchor is one setting, not one per surface.
- **Bluetooth drawer is a general tray drawer.** The BT applet collapses into
  an arrow that opens a drawer, and that drawer is the home for other applets
  that don't earn permanent bar space — same pattern as the Windows overflow
  tray. BT is just its first tenant.
- **Chip condensation is a ladder, not a mode.** As chips multiply they shed
  detail in stages: full detail → process name only → icon only → (extreme,
  last resort) no icon. The bar never spills past its bounds.
- **A Launcher settings pane.** The launcher
  (`crates/eclipse-launcher/src/main.rs`, spawned by `Super+R` —
  `crates/abyss/src/config/mod.rs`, `default_binds`) has no config keys at all
  today, so it has no tab in `eclipse-settings` either. It wants a `launcher.*`
  section in the schema table and its own pane alongside Taskbar once there is
  something to put in it: result count, whether it searches paths as well as
  desktop entries, where it anchors. Deferred deliberately — the Taskbar pane
  shipped first because `bar.*` had real keys to expose.

- **Wifi picker — blocked on an architecture decision.** The wifi applet is
  clickable and opens the Network drawer, but the drawer cannot yet scan, choose
  a network, or take a passphrase: `eclipse-services::status` is a read-only
  feed with no action path. Building it means two things the invariants govern.
  First, a new process-spawning path (`nmcli`, or iwd directly) — that belongs
  in a service, not on a view's draw path. Second, the passphrase field: a
  `password`-role value may never be delivered, logged or stored, and trusted UI
  is compositor-drawn, never a layer-shell client. So the secret cannot simply
  be typed into the taskbar (`hyperion`). The two honest shapes are a compositor-drawn
  trusted prompt for the secret alone, or the bar handing off to an external
  picker entirely. Pick one before anyone writes the drawer's second half.

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

- **A boot splash of our own.** Between firmware and the greeter the machine
  currently shows the stock Arch boot — kernel messages, Arch branding, no sign
  it is EclipseOS. That is the last place another distribution's identity shows
  through, and it should be an EclipseOS splash instead: quiet boot, our mark,
  handed off cleanly to the greeter with no flicker or VT flash between them.
  The greeter and the boot menu are already branded (D-03), so this is the
  remaining gap in the same story.

---

## Known bugs that are really missing features

See `docs/KNOWNBUGS.md` for defects. One entry overlaps this file: the taskbar
chip width cap is defeated by iced's `Row` handing `Fill` children exact
`min == max` limits, so chips run past `TASK_MAX`. The fix and the condensation
ladder above are the same piece of work — chip widths have to be computed from
the known bar width either way.
