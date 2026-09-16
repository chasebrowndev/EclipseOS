# Known bugs

Bugs that are understood, reproducible, and not yet fixed. A bug leaves this
file when the fix lands, not when a cause is identified — if it is diagnosed
but unfixed it stays here with the diagnosis attached.

Each entry carries the file and line where the fault lives, how to reproduce
it, and the proposed fix. "Proposed" means exactly that: nobody has committed
to it yet, and an entry with no proposal is more honest than one with a guess.

Found 2026-09-11 by `eclipse-ui-prober` against the recomposed launcher.
LAUNCH-01 through LAUNCH-04 were fixed on 2026-09-13 and removed from this file.

---

## LAUNCH-05 — a clipped note butts against the selected row's id

**Severity: cosmetic. Uncertain whether in scope.**

At 285 matches the note column's clip edge meets the selected row's mono
`.desktop` id with no visible gap: `Information about the Xfce Desktop Ex`
followed by `xfce4-about` reads as one string.
`EclipseDE/crates/eclipse-bar/src/launcher/view.rs:221` already adds `space::CARD` of
right padding for exactly this reason, and at width 560 it is not enough for
the longest comments. May simply be what clipping looks like.

---

## TERM-01 — `Terminal=true` entries are indexed, shown, and then refused

**Severity: design.** Not a crash; a category of result that cannot ever
succeed, occupying a fixed and scarce number of rows.

`apps::entry` records `Terminal` off the `.desktop` file
(`EclipseDE/crates/eclipse-services/src/apps.rs:159`) because the indexer parses every
key, and `apps::launch` refuses at the bottom
(`EclipseDE/crates/eclipse-services/src/apps.rs:275`) with `ErrorKind::Unsupported` and
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
`EclipseDE/crates/eclipse-bar` reads no configuration today — there is no `Config` in
`lib.rs` or `launcher/app.rs`. Putting the key in the abyss KDL config
(COMP-13) means the launcher must fetch it over `eclipse-ipc`, which is new
plumbing for the crate; the payoff is that `eclipse-settings/src/schema.rs`
builds its controls from `get_config {schema: true}` with no hand-written key
list, so the settings control appears on its own and `tests/coverage.rs`
enforces that it can be rendered. Keeping it local to the launcher skips the
IPC work and does not appear in settings at all.

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
