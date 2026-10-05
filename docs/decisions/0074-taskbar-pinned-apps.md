# 0074 — Taskbar pinned apps
Status: accepted
Date: 2026-10-05
Deciders: chase (owner), Claude (advisory)

## Context
D-05 §2 gives hyperion chips for live windows only. A closed app leaves no
trace on the bar, so the bar cannot be a launcher for the apps the user
reaches for most. KDE's task manager solves this with pinned launchers that
persist, launch on click, and merge with the running window. No spec covers
this; it is a new feature.

Existing machinery already fits: `ec_services::apps::launch` spawns a
`.desktop` entry (D-05 §2 already launches "new instance" this way),
`bar.tray.pinned` / `bar.widgets.order` are ordered string lists edited
through `set_config_value`, and ADR 0065's single layout solver places every
chip. "Pins" in D-05 §2 means popup/drag pins and `bar.tray.pinned` means
tray ids; neither is app pinning.

## Options
1. **`bar.pinned-apps` list in `ec-abyss-config`** — same pattern as the tray
   list; live reload; ec-settings and the chip menu write through
   `set_config_value`. Global, one list for every output.
2. Per-output lists — more schema and a control per output, no asked-for need.
3. A separate state file under `$XDG_STATE_HOME` — only precedent is owner
   approvals (ADR 0067), not preferences; splits config in two.

## Decision
Option 1. `bar.pinned-apps` is an ordered list of desktop-entry ids (the
`.desktop` basename, no suffix). Order is bar order.

- **Chip model.** Each pinned id yields one chip. A running window whose app
  id matches the entry (same matching the "new instance" action uses) merges
  into that chip instead of making a second one; extra windows of the same
  app follow as ordinary window chips. Unpinned running windows are unchanged.
- **Idle look.** A pinned app with no window renders icon-only, no fill,
  dimmed (the minimized-chip treatment); hover is brighter fill + crisp rim.
  Running pinned chips look like any window chip. No gold fill, outline or glow.
- **Click.** Idle: `ec_services::apps::launch`. Running: focus, as a window
  chip does. Launch is the user's own click; no capability check or approval
  gate applies (not a `widget { exec }`, so ADR 0067 is not involved).
- **Pin / unpin.** Chip context menu: "Pin to taskbar" on a window chip,
  "Unpin" on a pinned chip. Reorder by drag among pinned chips. All writes go
  through `set_config_value`; the GUI never keeps a parallel store.
- **Layout.** Pinned chips go through the ADR 0065 solver and the `+N`
  overflow, in list order ahead of unpinned window chips. Pinned-and-idle
  chips fold to icon-only before running ones lose their titles.
- **Unknown id.** An id with no `.desktop` entry is skipped by the bar and
  kept in the list (the app may be reinstalled); settings shows it as missing.
- **Fail-soft.** With abyss absent, pinned chips still render from empty live
  data and can still launch.
- **Gate.** Core bar behaviour; not behind the `taskbar-widgets` hook.
- **Scope.** Global. Per-output pins are not provided.

## Consequences
- Schema key, `docs/CONFIG.md` row, D-05 §2/§4 text, and a ec-settings Taskbar
  control (list editor with add-from-app-list, remove, reorder) are owed;
  `tests/coverage.rs` requires the control.
- Chip identity becomes entry-id keyed; the solver and motion code must treat
  an idle pinned chip as a first-class cell.
- Human input is not logged by content: log ids at most.

## Revisit when
Users run different bars per output and ask for different pins, or a pinned
chip needs more than launch/focus (jump lists, badges).
