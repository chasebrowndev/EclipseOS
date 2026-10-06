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
TRAY-01 was fixed on 2026-10-06 and removed: the Settings pane remembers
the tray ids it has listed this session.
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

- **`eclipseos-postinstall.sh` on an archinstall btrfs layout is untested.**
  It used to mount the root partition plainly, landing in the top-level
  subvolume. It now mounts the subvolume holding `/etc` (`@` in archinstall's
  layout), then everything the installed `/etc/fstab` names, under `/mnt`
  (2026-10-06). Nobody has walked a btrfs install through it yet; only ext4
  has been. This entry leaves when one has.
- **Remote access is opt-in at install time.** Both installers now ask
  whether to enable sshd, taking one public key and refusing password logins
  (2026-10-06). `tailscale` is still in no EclipseOS package set; it was what
  made the Framework install debuggable, but sshd on the LAN covers the same
  need without a third-party account.

---

# Touchscreen — CSW1322 panel on the dev box, 2026-09-24

## TOUCH-01: taskbar and tray context menus cannot be opened by touch

`shell/crates/ec-hyperion-bar/src/view.rs:1030`, `:1040` and
`shell/crates/ec-hyperion-bar/src/widgets/tray.rs:86` open the window and tray menus
with `mouse_area::on_right_press`, and a finger has no right button. iced 0.14's
`mouse_area` has no long-press. **Repro:** on a touchscreen, hold a finger on a
taskbar window button or tray icon. No menu opens. **Proposed:** a long-press
(about 500 ms with no movement past a small slop) in hyperion that sends the same
`Menu`/`TrayMenu` message.

---

# Screen capture — found building blur modes, 2026-09-27

## CAP-01: screenshots and screencasts drop every compositor effect

**Fix landed 2026-10-06, not yet seen on screen.** `capture_elements`
(`abyss/crates/ec-abyss/src/render/capture.rs`, TCB) still takes every
redaction decision itself, then hands each surviving window and layer to
`abyss/crates/ec-abyss-render/src/still.rs`, which adds opacity, dim-inactive,
rounding, shadow, glow and blurred backdrops from its own store. A backdrop
samples only the redacted capture list, placeholders included, so a blurred
secret is a blurred placeholder. Borders and trusted UI stay out, as before.
`still.rs` never reads animation state (`scan_tests.rs` pins it), so a capture
shows the settled frame. There is no GL pixel test; this entry leaves after a
nested-winit check: `decoration.blur.mode "glass"`, a translucent window over a
`capture.redact-app-id` window, `grim` the output, and see rounded corners and
a backdrop with only placeholder grey under the redacted rect.

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
(`abyss/crates/ec-abyss-render/src/lib.rs`, the layer pass of `collect_elements`); the toasts surface is larger than what it draws.
**Repro:** translucent window top-right, `notify-send` twice; see the slab
around both cards. **Compositor half fixed 2026-10-06:** abyss implements
`ext_background_effect_v1`
(`abyss/crates/ec-abyss/src/protocols/standard/background_effect.rs`), and a
committed blur region replaces the whole-surface backdrop, live and in
captures. **Still open:** ec-toasts must set a region per card. That is
frontend (`eclipse-frontend`), and the entry leaves when it lands.

---

## Probing notes for the launcher

The Hyprland-host probing recipe (`hyprctl`, `ydotool` scale) that used to sit
here is retired with the Hyprland dev host. One note still holds: the journal
is silent under both `-t ec-launcher` and `-t abyss` for the
whole of a launcher probe. That is correct, not a fault: the launcher holds no
capability, and logging the query would violate the never-log-human-input
invariant. Visual state is the only oracle here, so silence is never evidence
of a dead control in this crate.
