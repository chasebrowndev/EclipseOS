<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0126 — An iced greeter, a tile layout and Glances
Status: proposed
Date: 2026-11-18
Deciders: chase (owner), Claude (advisory)
Amends: 0089 (the regreet and `@define-color` parts)

## Context
0089 customises regreet by rewriting its config and CSS. GTK4 has no backdrop filter,
so the login screen cannot have real glass panes, and nothing in regreet can move.
The owner wants a phone-lock-screen style editor (move and resize, nothing overlaps),
real glass panes, no CSS, and one Apply at the end. The idle lock (ADR 0095, 0099) is
to get the same treatment, then the desktop.

## Options
1. Keep regreet and add more config keys. No glass, no layout.
2. A greeter on `ec-ui`. Pulls the desktop widget crate into the login TCB, and 0089's
   package has no edge to desktop packages on purpose.
3. A new iced greeter on a small pure layout crate, with its own glass drawing. Chosen.

## Decision
**Layout.** `ec-layout` is a pure crate (no UI toolkit, no I/O): a 24x14 grid that scales
to any output, tiles that never overlap or leave the grid, per-kind minimum sizes, and
`validate`, `place`, `resize`. `Users` and `Password` are required, so no layout can
hide the way in. A bad layout is rejected whole and the greeter draws the built-in one.

**Settings.** Same file, same strict hand parser (0089). A tile is
`tile.<kind> = "x y w h"`, four plain decimal cells. No `tile.*` line means the default
layout. `ec-greeter-apply` re-validates through the same parser before writing.

**Greeter.** A new iced client of the kiosk compositor (0087) replaces regreet. It does
not depend on `ec-ui`. Panes use `eclipse_glass_v1`, which kiosk mode does not block
(it follows `decoration.blur.mode`) and which fails soft to opaque panes. `ec-greeter-run`
keeps its panic fallback, now to the built-in layout.

**Apply.** The settings editor previews live and writes nothing until Apply. Apply runs
`pkexec ec-greeter-apply`; the polkit prompt Abyss already draws itself (0088) is the
"are you sure". No new trusted prompt kind, so COMP-10 §3 (closed by 0053) is unchanged.

**Glances.** A *Glance* is a small live-info pane (battery with connected devices, wifi,
power, now playing...) that fills a tile slot. Not "widget": the taskbar owns that word.
On the login screen only Glances that need no session may appear. Now playing needs the
user's session bus and is private on a locked screen, so it is opt-in there.

**Lock screen.** `ec-lock` moves to the same engine later, with its own layout and Glance
choices stored per user in the user's config dir and applied live (the owner's own
screen, so no root helper or prompt). Its surfaces stay opaque (glass-styled panes over a
blurred wallpaper the lock draws itself), the password field is required, layout code
must not panic, and nothing from the layout reaches `update` or the unlock gate (0095).
Glance reads run off the UI loop with a time limit.

## Consequences
- Settings stay world-readable machine state; layout is public like the greeting.
- Login and lock share one layout engine, and the desktop can later.
- The editor is an exception to live-apply settings, by decision: the login screen is
  the only way in, so changes are batched and confirmed.
- A second greeter binary stands between greetd and login, so it is TCB and
  line-reviewed, with the same all-or-nothing fallback.

## Revisit when
Per-user login screens are wanted, or a Glance needs more than sysfs, NetworkManager or
UPower pre-auth.

## Amendment 2026-11-19: content-sized Glances
A Glance whose content decides its size (the battery gauge row) keeps only a position in
the layout. `ec-layout` marks such kinds (`Kind::is_intrinsic`; today `Battery`) and derives
the footprint from the content: one gauge is 76 px plus an 8 px gap, four to a row, a second
row after that, and the footprint in cells is the content px plus the tile gap, over the
reference cell (1920x1080 / 24x14), rounded up. The file format is unchanged: the
`tile.battery = "x y w h"` line keeps four numbers so every reader still parses it, but `w`
and `h` are ignored (old files keep their position and lose their size). `validate` holds the
tile to the footprint of one gauge, so a stored layout fits whatever devices appear; at run
time `ec_layout::fit` shows as many devices as fit at the place, giving one up at a time
when the footprint would overlap a neighbour or leave the grid, and nothing when there are
none. Resizing such a tile is refused. The settings editor previews three devices.
