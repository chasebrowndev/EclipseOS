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

---

## BLUR-02 — a 1px dark line appears across a translucent window on eDP-1

**Severity: visual.** Cosmetic, persistent until the window redraws.

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

Everything below was found taking the 2026.09.19 ISO from boot medium to a
working session on the owner's Framework 13 (AMD, eDP 2880x1920@120). All five
were fixed the same day; they are recorded because every one of them was
invisible to CI, to the nested winit backend, and to a dev session on chase-pc,
and the next new machine will find the next batch the same way.

## HW-01 — abyss died at startup when Xwayland was not installed — FIXED

Selecting Abyss at the greeter bounced straight back to the greeter.
`journalctl -b -t abyss` ended at `spawning XWayland instance` with no error:
smithay's `XWayland::spawn` failure path panics in the child reaper
(`wait() should either return Ok or panic`) rather than returning `Err`, so the
`Err` arm in `crates/abyss/src/xwayland/mod.rs` that is written to degrade
gracefully never ran. `xorg-xwayland` was in no package list — not the PKGBUILD
depends, not `packages.x86_64` — so no installed system had it.

Fixed: `xorg-xwayland` is a hard dependency of `eclipseos-abyss` and is listed
on the medium; `xwayland::start` checks `PATH` before spawning and takes the
degrade path itself. **The lesson is the general one:** a dependency that only
the dev box happens to have is not a dependency the image has.

## HW-02 — the bar, toasts and policyd never started — FIXED

An installed session ran abyss and nothing else. The units shipped "enabled"
via a systemd **user**-preset (`/usr/lib/systemd/user-preset/50-eclipseos.preset`),
and a user-preset only takes effect when somebody runs
`systemctl --user preset-all` — which nothing does for an account that already
exists. `systemctl --user is-enabled eclipse-bar.service` said `disabled` on a
fresh install.

Fixed: the packages ship `abyss-session.target.wants/` symlinks instead, which
enable the units image-side for every user; the preset file is gone.

## HW-03 — policyd crash-looped into the start limit — FIXED

`policyd.service` is `Type=notify`, but policyd never sends `READY=1` — it has
no socket to be ready on until milestone 11 — so systemd declared every start a
protocol failure, restarted it, and hit `start-limit-hit`. Fixed by
`Type=simple` until the agent socket lands. Note that policyd still exits
immediately by design; that is the stub, not a fault.

## HW-04 — every shipped app keybind was dead — FIXED

Super+Q, Super+E and Super+F spawned `kitty`, `dolphin` and `firefox`. EclipseOS
installs none of the three, so the spawn succeeded, the child died instantly,
and the binds read as broken input handling. They were not: the journal showed
`spawning` for each press, and Super+1..n logged `workspace switched` every
time — invisible only because there were no windows and the bar was not running
(HW-02).

Fixed: `default_binds()` now names only binaries the image installs (foot,
eclipse-launcher). **Rule going forward: a shipped bind must name a binary in
`eclipseos-meta`'s dependency closure.** Nothing enforces that yet — a test
that walks `default_binds()` against the package list would.

## HW-05 — the boot menu said "Arch Linux" — FIXED

On the archinstall route, archinstall writes `limine.conf` and titles every
entry `Arch Linux`; `eclipseos-postinstall.sh` never touched it, so a finished
EclipseOS install booted through a menu naming a different distribution. Our
own `install-eclipseos.sh` was always correct. Fixed: the postinstall script
rewrites the entry titles (titles only — paths and cmdline stay archinstall's).

## HW-06 — eclipse-settings could not save: "permission denied (os error 13)" — FIXED

Changing anything in eclipse-settings (rounding, taskbar behaviour, …) failed
with EACCES. `config_rpc::target_path` picked the *last loaded source* with the
key's owner as the write target. A fresh install has no
`~/.config/eclipse/abyss.kdl`, and a file that does not exist (or fails to
parse) is never pushed onto `Config::sources` — so the only Abyss source was
root-owned `/etc/eclipse/abyss.kdl`, and a write from a normal session was
refused by the kernel.

Fixed: writes target the user tier — the last user-owned source if there is
one, otherwise `$XDG_CONFIG_HOME/eclipse/abyss.kdl`, created on first write.
`--config` still names the file to edit. This is also the explanation for the
earlier, separately confusing observation that `eclipse-ctl reload` listed only
the two `/etc` files in `sources` while a user config existed: at that moment
the user file had a syntax error, and a source that fails to parse is skipped.

## HW-07 — no wifi after boot; NetworkManager never autoconnects — FIXED

The laptop came up with wlan0 `disconnected` and stayed there until someone
connected by hand. NetworkManager is configured with `wifi.backend=iwd` (the
live medium's saved networks are iwd's, so the installed system reads them
with the same backend), but nothing orders NM behind iwd: both start at once,
NM loses the race, logs

    iwd-manager: IWD device named wlan0 is not a Wifi device

parks the device as unmanaged, and once it settles back to `disconnected` it
never retries autoconnect. On the archinstall route `iwd.service` was also left
disabled outright -- it was only alive because NM D-Bus-activated it, which is
exactly the timing that loses the race.

Fixed in both install routes: write a `NetworkManager.service.d/10-iwd-first.conf`
drop-in (`Wants=`/`After=iwd.service`) whenever the iwd backend is configured,
and enable `iwd.service` rather than relying on D-Bus activation.

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

# Updating a live install to a new build — 2026-09-22

Two connected incidents from taking a running desktop from a stale
independently-`pacman`-installed build to a fresh `cargo build --release` +
`install-session.sh` checkout. Both are process/packaging traps, not crate
bugs, but the second one cost real diagnostic time and the mechanism is worth
keeping on record.

## PKG-01 — a stale `eclipseos-desktop`/`eclipseos-meta` pacman install fights the source build — FIXED

**Severity: correctness.** Two taskbars rendered at once after a rebuild.

The machine had `eclipseos-desktop` and `eclipseos-meta` installed via pacman
from before commit `0eddb2b` (the hyperion rename/split). Their payload
included the old-named `eclipse-bar.service` / `/usr/local/bin/eclipse-bar`,
enabled independently of the git checkout. Building and installing the
current source (`hyperion.service` / `/usr/local/bin/hyperion`) added the new
bar alongside the old one instead of replacing it — nothing in
`install-session.sh` knows to disable units it didn't itself enable, and
pacman-owned files aren't touched by a source install at all. Both bars ran
and painted, hence "two bars."

Fixed by `sudo pacman -R eclipseos-desktop eclipseos-meta` to remove the
pacman-owned copy entirely, leaving only the source-installed
`hyperion.service`. `pacman -R eclipseos-desktop` alone refused
(`eclipseos-meta` depends on it), which is what forced both packages out
together — see PKG-02 for what that dependency pairing actually cost.

**Lesson:** a machine that has ever had the pacman packages installed is not
the same machine as one built purely from source, and `install-session.sh`
does not reconcile the two. Worth a check in the installer (or a note in
`docs/BUILDING.md`) that flags pacman-owned `eclipseos-*` packages before a
from-source install proceeds.

## PKG-02 — removing `eclipseos-desktop` cascade-removed `eclipseos-meta`, which silently deleted the graphical greeter — FIXED (pending reboot verification)

**Severity: correctness, high blast radius.** No graphical login after the
next reboot; degrades to a plain-text `agreety` shell prompt with no error
pointing at the cause.

`eclipseos-meta` is not just metadata about the desktop packages — it also
owns the entire greeter chain: a systemd drop-in
(`/usr/lib/systemd/system/greetd.service.d/10-eclipseos.conf`) that points
`greetd` at an EclipseOS-specific config via `--config`, plus the config tree
itself (`/etc/eclipse/greetd/{config.toml,regreet.toml,regreet.css}`) and the
greeter background. `pacman -R eclipseos-desktop` alone fails because
`eclipseos-meta` depends on it (PKG-01), so removing the stale desktop
package forces `eclipseos-meta` out too — deleting the drop-in and the config
tree as an unrelated side effect of fixing the two-bars bug. `greetd` then
fell back to its own package-default `/etc/greetd/config.toml`
(`agreety --cmd /bin/sh`), which was never touched and is still on disk,
so nothing about *that* file looked wrong.

**What made this hard to diagnose:** no pacman hook or scriptlet announced
the change, and the obvious suspect file
(`/etc/greetd/config.toml`) never changed — its mtime predates the whole
session. The actual mechanism (`greetd -c/--config`, set via a systemd
drop-in, both owned by `eclipseos-meta` rather than `eclipseos-desktop`) only
surfaced by reading `greetd --help` and cross-referencing
`pacman -Qlp /var/cache/pacman/pkg/eclipseos-{desktop,meta}-*.pkg.tar.zst`
against the still-present package cache archives — the packages were gone
from the system but their manifests and contents were still recoverable from
`/var/cache/pacman/pkg/`.

**Fix applied:** restored only `eclipseos-meta`'s files
(`etc/eclipse/greetd/`, the systemd drop-in, and the greeter background) from
the cached `.pkg.tar.zst` via `bsdtar -xpf ... -C /`, followed by
`systemctl daemon-reload` — deliberately *not* a `pacman -S` reinstall, which
would have pulled `eclipseos-desktop` back in as a hard dependency and risked
resurrecting the old `eclipse-bar.service` from PKG-01.

**Lesson:** `eclipseos-meta` bundling the greeter (login-critical, needed by
everyone) with desktop package metadata (replaced wholesale by the hyperion
split) means removing either one for an unrelated reason can silently take
the other down. Worth considering whether the greeter drop-in/config belongs
in its own package with no dependency edge to `eclipseos-desktop`, so a
desktop package swap can never touch login.

---

## Probing notes for this pane

Two traps that produced a clean-looking false negative before they were caught:

- **`ydotool mousemove -a` is half-scale on chase-pc.** Asking for
  `-x 1000 -y 500` puts the cursor at `2000,1000`. Halve the absolute target,
  and read `hyprctl cursorpos` back before trusting any click result.
- **These are layer surfaces, so they never appear in `hyprctl clients`.**
  Geometry comes from `hyprctl layers -j`, whose `.x` is already absolute
  across outputs:

  ```
  hyprctl layers -j | jq -r 'to_entries[]|.key as $m|.value.levels|to_entries[]|.value[]|select(.namespace|test("launcher"))|"\($m) \(.x),\(.y) \(.w)x\(.h)"'
  ```

The journal is silent under both `-t eclipse-launcher` and `-t abyss` for the
whole of a launcher probe. That is correct, not a fault: the launcher holds no
capability, and logging the query would violate the never-log-human-input
invariant. Visual state is the only oracle here, so silence is never evidence
of a dead control in this crate.
