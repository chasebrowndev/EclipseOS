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

---

## BLUR-01 — the launcher and notification panels are unreadable with blur off

**Severity: design.** Not a crash; a visual dependency between two independent
config toggles that only one side knows about.

The launcher and notification popups in `eclipse-bar` paint their panel
background as a translucent dark fill (roughly `rgba(0,0,0,0.6)`), assuming
the compositor's dual-Kawase backdrop (`crates/abyss/src/render/blur.rs`,
COMP-02 §9) fills in behind them per `shows_through`
(`crates/abyss/src/render/mod.rs:154`). With `decoration.blur.enabled` false —
previously the shipped default in `crates/abyss/src/config/mod.rs:288`, now
flipped to `true` — nothing renders behind the translucent fill, so the panel
alpha-blends over whatever is underneath and reads as a flat dark rectangle,
not the intended frosted-glass panel. Reproduce with blur off
(`decoration { blur { enabled #false } }` in config): open the launcher or
trigger a notification.

Flipping the shipped default masks this for anyone who never touches the
setting, but a user who turns blur off (perf, preference, unsupported GPU
path) still gets a broken-looking panel with no visual cause pointing back to
that toggle. **Proposed fix:** either give the launcher/notification panels a
solid, non-blur-dependent fallback background when blur is off (frontend,
`eclipse-frontend` territory — `crates/eclipse-bar/src/launcher/view.rs` and
the notification view), or have the compositor refuse `shows_through` in a
way that degrades to a flat but *intentionally styled* fill rather than
leaving the client's own translucent color exposed unblurred. No one has
committed to either yet.

---

## LAUNCH-05 — a clipped note butts against the selected row's id

**Severity: cosmetic. Uncertain whether in scope.**

At 285 matches the note column's clip edge meets the selected row's mono
`.desktop` id with no visible gap: `Information about the Xfce Desktop Ex`
followed by `xfce4-about` reads as one string.
`crates/eclipse-bar/src/launcher/view.rs:221` already adds `space::CARD` of
right padding for exactly this reason, and at width 560 it is not enough for
the longest comments. May simply be what clipping looks like.

---

## TERM-01 — `Terminal=true` entries are indexed, shown, and then refused

**Severity: design.** Not a crash; a category of result that cannot ever
succeed, occupying a fixed and scarce number of rows.

`apps::entry` records `Terminal` off the `.desktop` file
(`crates/eclipse-services/src/apps.rs:159`) because the indexer parses every
key, and `apps::launch` refuses at the bottom
(`crates/eclipse-services/src/apps.rs:275`) with `ErrorKind::Unsupported` and
the message `entry wants a terminal`. The refusal is defensible on its own —
its test, `a_terminal_entry_refuses_rather_than_disappearing`, argues correctly
that spawning into no terminal leaves a process the human cannot see or reach —
but it is a runtime apology for something that should never have been indexed.

The launcher draws 8 rows and the index holds 285 entries, so every `htop` /
`btop` / `ncspot` row is a slot a launchable application did not get — the
scroll fix made the rows reachable, not useful.

**Proposed fix — one config key naming a terminal emulator command, unset by
default:**

- **Unset (the default):** `Terminal=true` entries are dropped during indexing.
  They never reach `matched`, never occupy a row, and need no note and no tint.
- **Set:** they launch through it (`$term -e <argv>`) and render as ordinary
  rows with no special casing at all.

A command rather than a `show_terminal_apps` bool: a bool would put the rows
back and still refuse to launch them, making the lie configurable. Naming the
emulator is what makes them work, and "is the key set" is then the visibility
rule for free. The cost is that it is a string key, so `eclipse-settings`
renders it as a text field rather than a toggle.

What this removes: the greyed row state, the "needs a terminal" note, the
`ErrorKind::Unsupported` branch, and the terminal-row tint rules the
LAUNCH-04 fix added. `apps::launch` keeps its error
return for genuinely broken entries (`entry has no command`), which is what it
is actually good for.

**Open, and the reason this is still a proposal:** the key placement.
`crates/eclipse-bar` reads no configuration today — there is no `Config` in
`lib.rs` or `launcher/app.rs`. Putting the key in the abyss KDL config
(COMP-13) means the launcher must fetch it over `eclipse-ipc`, which is new
plumbing for the crate; the payoff is that `eclipse-settings/src/schema.rs`
builds its controls from `get_config {schema: true}` with no hand-written key
list, so the settings control appears on its own and `tests/coverage.rs`
enforces that it can be rendered. Keeping it local to the launcher skips the
IPC work and does not appear in settings at all.

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
