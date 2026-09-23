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
(updating a live install, 2026-09-22) were fixed and removed on 2026-09-23; the
rules they left behind are kept below. The full write-ups are in git history
(this file at `20d8e3a`) and `docs/handoff/2026-09-19-greeter-to-abyss.md`.

---

## BLUR-02 — a 1px dark line appears across a translucent window on eDP-1

**Severity: visual.** Cosmetic, persistent until the window redraws.

**Needs re-verification (2026-09-23).** `107b5de` (#29, rounded borders, masks
and blur that match their windows) and `e906321` reworked the blur path after
this was written, including the final upright pass's orientation. Nobody has
looked at eDP-1 at scale 2.0 since. Check before working on it.

Found 2026-09-21. A thin dark horizontal line, one pixel high, starts
partway across a translucent kitty window and runs to its right edge. Not a
dead pixel (checked in the BIOS).

**Confirmed:**
- Only on eDP-1 (2880x1920, scale 2.0), not on DP-3 (scale 1.0).
- Gone with `decoration { blur { enabled #false } }` plus `eclipse-ctl reload`.
- Absent from `grim` screenshots and from region selection, so the fault is in
  what the live output frame shows, not in the composed scene.
- No `invalid damage clip` or `queueing frame` in the journal, so it is not a
  refused DRM commit.

**Root cause: not identified.** Suspects in `BlurStore::element`
(`crates/abyss/src/render/blur.rs`, the `src`/`size` block near the end of the
function), all unverified:
- `src` is `region / scale` (logical) but the texture is in output-local
  physical pixels with buffer scale 1, so at scale 2.0 the sampled rectangle
  differs from the region. This would explain why it only shows at 2.0.
- `size` is rounded on its own instead of derived from `region.size`, so the
  element geometry can differ from the blur region by a pixel.
- The result texture is static and refreshes only when `invalidates()` fires,
  so a missed invalidation leaves a stale strip.

**Proposed fix:** none. Blur is due for a major rework; the rework should
cover the three points above and add a test that element geometry equals the
region at scales 1.5 and 2.0. Workaround: disable blur.

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
  on `systemctl --user preset-all`. `dist/eclipseos.preset` is still in the
  tree but nothing installs it; it is dead.
- **A dependency only the dev box has is not a dependency the image has**
  (was HW-01, `xorg-xwayland`).

## Still open from this install

- **Boot is visually Arch, not EclipseOS.** Kernel messages and the Arch
  plymouth-less boot are what the user sees between firmware and the greeter.
  Tracked as a wanted feature, not a bug — see "Boot splash" in
  `PROPOSEDFEATURES.md`.
- **`eclipseos-postinstall.sh` mounts the root partition plainly**, so an
  archinstall btrfs subvolume layout lands in the top-level subvolume and the
  install goes to the wrong place. Only ext4 has been walked through.
- **The installed system has no way in without a screen.** `tailscale` is in no
  EclipseOS package set; it was installed by hand on the Framework, and it is
  the only reason any of the above could be diagnosed remotely rather than read
  off a photographed screen. Worth deciding whether a headless-debuggable image
  is the default.

---

# Packaging

## PKG-03 — `eclipse-secret-prompt` is never installed

**Severity: correctness.** On a packaged or `install-session.sh` system,
joining a wifi network that needs a passphrase, or pairing a Bluetooth device
that asks for a PIN, cannot start the prompt.

`hyperion` spawns the prompt by name from `PATH`
(`crates/hyperion/src/main.rs:262`, `app.rs:497` `SECRET_PROMPT`), per
ADR 0052/0053. The binary builds, but nothing puts it on `PATH`: it is in no
`package_*` function of `dist/pkg/eclipseos/PKGBUILD`, and neither
`dist/install.sh` nor `dist/install-session.sh` links it. Only a dev session
with `target/*/` on `PATH` has it. `dist/etc/policy.kdl` already carries the
`sensitivity secret` windowrule for `^eclipse-secret-prompt$`, so the policy
side is ready.

**Proposed fix:** install `/usr/bin/eclipse-secret-prompt` from the
`eclipseos-hyperion` package (the only spawner; ADR 0052's one-package-per-
component rule would also admit its own package), and add it to both
install scripts' binary lists.

---

## CFG-01 — `misc { xwayland … }` is accepted and ignored

**Severity: correctness (fail-open parse).** `apply_misc` has an empty
`"xwayland" => {}` arm (`crates/abyss/src/config/mod.rs:2064`). A config that
disables X11 the old way (`misc { xwayland #false }`) parses without error and
X11 stays on. The real key is the `xwayland { enable … }` node (COMP-13 §1.1,
amended C-05).

**Proposed fix:** delete the arm so the key is refused like any other unknown
`misc` child, with a hint pointing at `xwayland { enable }`.

---

## Probing notes for the launcher

The Hyprland-host probing recipe (`hyprctl`, `ydotool` scale) that used to sit
here is retired with the Hyprland dev host. One note still holds: the journal
is silent under both `-t eclipse-launcher` and `-t abyss` for the
whole of a launcher probe. That is correct, not a fault: the launcher holds no
capability, and logging the query would violate the never-log-human-input
invariant. Visual state is the only oracle here, so silence is never evidence
of a dead control in this crate.
