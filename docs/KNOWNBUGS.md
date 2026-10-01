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
(this file at `20d8e3a`) and `docs/internal/handoff/2026-09-19-greeter-to-abyss.md`.
BLUR-02 and TILE-01 were fixed on 2026-09-23 and removed. BLUR-02 came back and
was fixed for real on 2026-09-28: the line was ec-toasts' idle 1 px
transparent layer, which the compositor blurred; the stack now has no surface
while empty.
BLUR-01's fix (opaque sheets, because clients could not tell whether blur was on)
is superseded on 2026-09-29 (C-14): panes read `decoration.blur.mode` and only tint.

---

# First real-hardware install — Framework 13, 2026-09-19

Every bug found taking the 2026.09.19 ISO to a working session on the owner's
Framework 13 (HW-01..HW-07) is fixed. What they leave behind:

- **A shipped bind must name a binary in `eclipseos-meta`'s dependency
  closure** (was HW-04). `default_binds()` spawns `foot` (Super+Q,
  Super+Return), `ec-launcher` (Super+E, Super+R) and `ec-center`
  (Super+N). `default_bind_spawns_name_shipped_binaries` enforces it against
  `packaging/pkg/eclipseos/PKGBUILD`.
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

# Screen capture — found building blur modes, 2026-09-27

## CAP-01: screenshots and screencasts drop every compositor effect

`render/capture.rs:315` (`capture_elements`, TCB) builds its own pass list
from surface elements only; it deliberately leaves out borders and trusted UI
(the shell's content, not the compositor's chrome), and in doing so also
loses blur and its frost/glass modes, rounding, shadow, glow and
dim-inactive. A capture is therefore not what the user sees: translucent
windows show the unblurred desktop through square corners. **Repro:** set
`decoration.blur.mode "glass"`, make a window translucent, `grim` the output;
the backdrop is sharp and the corners square, identical in every mode.
**Proposed:** none committed. Redaction must stay authoritative, so the effect
elements would have to be rebuilt over the *redacted* list (a blurred secret
is still a secret). The owner's direction is a first-party capture tool — see
`PROPOSEDFEATURES.md` "Screen capture" — which would own this. Visual checks
meanwhile use a nested winit abyss captured from the outer compositor.

**Cursor (fixed 2026-09-29):** `wlr_screencopy`'s `overlay_cursor` request
flag was silently discarded, so no client — with or without the flag set —
ever saw the pointer in a capture (COMP-02 §8 requires cursor-in-capture to be
opt-in, not ambiently absent). `copy_one` now bakes the cursor in via
`render::cursor::elements` when the client asked and no trusted prompt holds
the seat. `ext_image_copy_capture_v1`'s separate pointer-cursor session
(`image_copy_capture.rs`) remains a stub — see `PROPOSEDFEATURES.md` Phase 1
M8 — since it needs a metadata cursor stream, not baked-in pixels.

## BLUR-03: a blurred rectangle frames the toast stack

Found 2026-09-28 fixing BLUR-02. While cards show, the compositor blurs the
whole ec-toasts layer (404 x stack height), including the transparent
gaps around and between the rounded cards, so a square blurred slab shows
behind them. Layers are blurred wherever they leave the surface uncovered
(`render/mod.rs:192`); the toasts surface is larger than what it draws.
**Repro:** translucent window top-right, `notify-send` twice; see the slab
around both cards. **Proposed:** none committed. Either the client tells the
compositor where its glass is (`ext-background-effect-v1`, deferred from the
blur-modes work) or each card becomes its own surface.

---

# Settings Taskbar pane — found probing drag and drop, 2026-09-28

## TRAY-01: a tray entry that is not running vanishes when moved to the drawer

`ec-settings/src/tray.rs:81` (`Tray::ids`) lists live items plus those
named in `pinned` or `hidden`. The drawer is simply "in neither list", so an
entry that isn't running, once dragged (or keyed) from the bar or hidden row
into the drawer, is named nowhere and drops out of the pane. The config write
is correct; the pane just can't show it until the app runs again.
**Repro:** in `~/.config/eclipse/abyss.kdl` pin a tray id whose app isn't
running (e.g. `steam`), open Settings, Taskbar, drag it into the drawer row;
it disappears. **Proposed:** none committed. Either keep ids moved this
session in the pane's own list, or give the drawer its own config key.

---

## Probing notes for the launcher

The Hyprland-host probing recipe (`hyprctl`, `ydotool` scale) that used to sit
here is retired with the Hyprland dev host. One note still holds: the journal
is silent under both `-t ec-launcher` and `-t abyss` for the
whole of a launcher probe. That is correct, not a fault: the launcher holds no
capability, and logging the query would violate the never-log-human-input
invariant. Visual state is the only oracle here, so silence is never evidence
of a dead control in this crate.
