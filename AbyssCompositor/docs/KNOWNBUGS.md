# Known bugs

Bugs that are understood, reproducible, and not yet fixed. A bug leaves this
file when the fix lands, not when a cause is identified — if it is diagnosed
but unfixed it stays here with the diagnosis attached.

Each entry carries the file and line where the fault lives, how to reproduce
it, and the proposed fix. "Proposed" means exactly that: nobody has committed
to it yet, and an entry with no proposal is more honest than one with a guess.

Found 2026-09-11 by `eclipse-ui-prober` against the recomposed launcher.
LAUNCH-01 through LAUNCH-04 were fixed on 2026-09-13 and removed from this file.
RAISE-01 was fixed on 2026-09-18 and removed.
BLUR-01, LAUNCH-05, and TERM-01 were fixed on 2026-09-22 and removed.
HW-01 through HW-07 (the first Framework install, 2026-09-19) and PKG-01/PKG-02
(updating a live install, 2026-09-22), PKG-03, PKG-04 and CFG-01 were fixed and removed on 2026-09-23; the
rules they left behind are kept below. The full write-ups are in git history
(this file at `20d8e3a`) and `docs/handoff/2026-09-19-greeter-to-abyss.md`.
BLUR-02 and TILE-01 were fixed on 2026-09-23 and removed.

---

# First real-hardware install — Framework 13, 2026-09-19

Every bug found taking the 2026.09.19 ISO to a working session on the owner's
Framework 13 (HW-01..HW-07) is fixed. What they leave behind:

- **A shipped bind must name a binary in `eclipseos-meta`'s dependency
  closure** (was HW-04). `default_binds()` spawns `foot` (Super+Q,
  Super+Return), `eclipse-launcher` (Super+E, Super+R) and `eclipse-center`
  (Super+N). Nothing enforces the rule yet — a test that walks
  `default_binds()` against the package list would.
- **User units are enabled by `abyss-session.target.wants/` symlinks the
  packages ship** (was HW-02), not by a user-preset, which only takes effect
  on `systemctl --user preset-all`.
- **A dependency only the dev box has is not a dependency the image has**
  (was HW-01, `xorg-xwayland`).

## Still open from this install

- **`eclipseos-postinstall.sh` mounts the root partition plainly**, so an
  archinstall btrfs subvolume layout lands in the top-level subvolume and the
  install goes to the wrong place. Only ext4 has been walked through.
- **The installed system has no way in without a screen.** `tailscale` is in no
  EclipseOS package set; it was installed by hand on the Framework, and it is
  the only reason any of the above could be diagnosed remotely rather than read
  off a photographed screen. Worth deciding whether a headless-debuggable image
  is the default.

---

# Touchscreen — CSW1322 panel on the dev box, 2026-09-24

## TOUCH-01: taskbar and tray context menus cannot be opened by touch

`hyperion/src/view.rs:777`, `:1152`, `:1567` open the window and tray menus
with `mouse_area::on_right_press`, and a finger has no right button. iced 0.14's
`mouse_area` has no long-press. **Repro:** on a touchscreen, hold a finger on a
taskbar window button or tray icon. No menu opens. **Proposed:** a long-press
(about 500 ms with no movement past a small slop) in hyperion that sends the same
`Menu`/`TrayMenu` message.

---

## Probing notes for the launcher

The Hyprland-host probing recipe (`hyprctl`, `ydotool` scale) that used to sit
here is retired with the Hyprland dev host. One note still holds: the journal
is silent under both `-t eclipse-launcher` and `-t abyss` for the
whole of a launcher probe. That is correct, not a fault: the launcher holds no
capability, and logging the query would violate the never-log-human-input
invariant. Visual state is the only oracle here, so silence is never evidence
of a dead control in this crate.
