# 0065 — Taskbar widgets, fluid bar motion, and the audio monitor tap
Status: accepted
Date: 2026-09-25
Deciders: chase (owner), Claude (advisory)

## Context
The taskbar (D-05 §2) draws a launcher, the pager, window chips, the tray and a
clock. The owner wants modular **widgets** on it: shipped presets (Now Playing,
System Usage, Volume) and widgets users define themselves, every one of them
configurable from Settings (CHARTER §4, COMP-17 §3, D-05 §4).

Forces:
- The strip is shared. Chips already step down a ladder (Full → Name → Icon →
  Bare → `+N`) as windows multiply; widgets compete for the same room.
- The bar has no motion vocabulary beyond the fold slide (ADR 0042) and the eye.
  Chips jump when the ladder changes a rung. The owner wants every movement
  fluid, including interrupted ones.
- Nothing in the userland collects media, volume, or system usage. Volume and
  per-window mute shell out to `pactl` on a UI path (`hyperion/src/audio.rs`),
  against D-05 §3 and ADR 0053.
- No spec admits third-party code into the taskbar. ADR 0041 rejected an
  in-process plugin ABI. Waybar-style `exec` modules are the established
  user expectation for custom widgets.
- A real audio visualizer has to read the output mix. No policy covers audio;
  the capture policy (ADRs 0027/0029/0030) is screen-only.

## Options
1. **Presets only.** Least surface; fails the owner's "people add custom ones".
2. **Declarative custom widgets only** (compose shipped data sources). No foreign
   code; too little to be worth the feature.
3. **Exec + declarative custom widgets**, exec running out-of-process on a
   service thread, output treated as untrusted text. Matches user expectation;
   the commands are the user's own, authored in their own `abyss.kdl`, at the
   user's own trust level.
4. Visualizer driven by play state only (synthetic). No new privacy surface; does
   not follow the music.
5. Visualizer from the PipeWire sink monitor, reduced in-process to band levels.

## Decision
We take **3** and **5**.

**Widgets.** `bar.widgets.order` (ordered string list) names the widgets drawn
after the task strip. The bar's existing right-hand elements become widgets
too, so every one of them is ordered, toggled and compressed the same way.
Built-in ids: `now-playing`, `system-usage`, `volume`, `network`, `bluetooth`,
`battery`, `tray` (the SNI items and their overflow drawer), `clock`. A custom
widget is `custom:<name>`, naming a `bar { widget "<name>" { … } }` block.
Default order: `now-playing`, `volume`, `network`, `bluetooth`, `battery`,
`tray`, `clock`. The launcher button and the pager stay fixed and are not
widgets. `bar.tray.pinned`/`hidden` keep governing SNI items only; the built-in
ids they used to carry (`network`, `bluetooth`, `battery`, `volume`) move to
`bar.widgets.order`, and `ec-ctl config migrate` rewrites old files.

`bar.widgets.important` (string list, default `clock`, `battery`) names widgets
that **never compress**: the solver keeps them at their core size and takes
the room from everything else.

Every widget is `[drag bar][core]` with an optional **revealed section**
shown by dragging the bar left (Now Playing: back / play-pause / skip; System
Usage: memory, GPU, disk). Only a widget with a revealed section carries a
drag bar at rest; one with nothing to extend into (Network, Bluetooth) is a
plain cell. A widget **compresses** to its drag bar alone; one without a drag
bar gains one once it is squeezed past its icon, and that bar opens it again.
Now Playing with nothing playing compresses to **zero width**, animating out.

**One layout solver** assigns every chip and widget its target x and width.
Important widgets never compress, and a widget the human opened (a tap or a
drag) keeps its room while the chips can stay at their bare width. Everything
else shrinks **together**: unpinned widgets and chips share one squeeze
factor, so no single element looks out of scale beside its neighbours. A
widget squeezed past its icon drops to its drag bar and gives the room back to
the same share. Only past that do opened widgets fold, then chips overflow
into `+N`.
Dragging a compressed bar left takes room from the chips; release snaps to the
nearest state, but a drag must end a clear allowance below the widget's core
width before it collapses, and a fling never carries past open into collapsed.
A tap steps down one state (revealed → open → collapsed) and opens a collapsed
widget.
Pointer and touch share one gesture path. Popup anchors read the same solver
output.

**Motion.** Every chip and widget animates x, width and opacity toward the
solver's targets. The default curve is a critically damped spring that
retargets from its current position and velocity, so an interrupted animation
never jumps. The frame clock runs only while something moves. Keys:
`bar.motion.enabled` (default on; off snaps), `bar.motion.duration-ms`
(default 220), `bar.motion.curve` (the `animations` curves plus `spring`,
default `spring`). The primitives live in `ec_ui::motion` so other
components may use them.

**Custom widgets.** A `widget` block is either:
- `exec "argv0" "arg"…` with `interval-ms`, or `stream #true` for a
  long-running command emitting one update per line; a line is plain text or
  JSON `{text, detail, tooltip, state}`;
- or `source "<shipped source>"` with a `format` string (declarative).

Optional `icon`, `on-click`, `on-scroll-up`, `on-scroll-down` (argv lists).
Commands are argv-exec, never through an implicit shell. They run on an
`ec-services` thread, never on a draw path, with a timeout, a 4 KiB line
cap, and are killed on reload or removal. Output is rendered as plain text,
never markup, and never logged (it may carry anything the user's command
prints). Exec widgets have exactly the authority of the user who wrote them;
they are not a plugin mechanism and gain nothing from the taskbar.

**Data services** go in `ec-services`, on the `status` pattern (one
watcher thread per source, an `mpsc` feed, a separate actions handle):
`media` (MPRIS over zbus), `audio` (PipeWire default-sink volume and mute, the
per-window mute that replaces `pactl`, and the monitor tap), `usage`
(`/proc`, `statvfs`, GPU sysfs, and NVIDIA's NVML loaded at runtime when the
driver ships it; a runtime-suspended GPU is read as idle, never woken; an
absent source is hidden, not zero),
`custom` (the exec runner).

**Remote album art.** Players such as Spotify publish cover art only as an
`https` URL. `media` fetches it by spawning `curl` (argv-exec, `https` only,
5 s timeout, 4 MiB cap, no cookies or credentials) on its art thread; we link
no HTTP or TLS client. The fetch tells the image host what is playing, which
the player's own fetch already did, but the request now comes from the
desktop, so `bar.widgets.now-playing.remote-art` (default true) turns it off.
Art bytes stay in memory; the URL is never logged. No `curl`, a failed fetch,
or the key off all show the fallback glyph.

**The monitor tap** reads the default sink's monitor only while media is
playing, the Now Playing widget is on screen, and
`bar.widgets.now-playing.visualizer` is true. Samples are reduced to band
levels in memory and dropped; they are never stored, logged, or sent anywhere.
Turning the key off closes the stream.

## Consequences
- New `bar.widgets.*` (including `order` and `important`), `bar.motion.*` keys and a `widget` collection, all with
  Settings controls. A new socket method, `set_config_collection` (gate kind
  `Command`, owner uid), writes `widget` entries and the widget lists with
  dry-run validation (COLLECTIONS were not settable before). It lets a socket
  peer add an exec widget, so the socket can now cause a command to run; how
  that is confirmed is an open question for the owner (see Revisit when).
- `hyperion/src/audio.rs`'s `pactl` path goes away; volume keybinds are
  unaffected.
- New crates on the supply chain: an audio-server client (native PipeWire, or
  the PulseAudio protocol that PipeWire serves; whichever builds without
  libclang and passes `cargo deny`), `realfft` (MIT/Apache), `libc`, the
  `nvml-wrapper`/`nvml-wrapper-sys` binding that `dlopen`s `libnvidia-ml.so`
  (no build-time NVIDIA dependency; NVML is released while a runtime-PM dGPU
  idles so it can suspend), and iced's `image` feature, whose decoders now read
  untrusted remote art bytes.
- The tray's built-in status items become widgets; `bar.tray.*` shrinks to SNI
  items, with a config migration.
- The chip layout functions (`ladder`, `expand`, `chip_span`) fold into the
  solver; `chip_span`'s drift is fixed as a consequence.
- hyperion becomes one process drawing a bar on every output, replacing the
  per-output supervisor and children, so services, the monitor tap and exec
  widgets run once per session rather than once per monitor. The curl art URL
  goes to curl on stdin, never argv, so it is not visible in `/proc`.
- D-05 §2, §3 and §7 are updated to say what the bar now does.
- Owed tests: solver invariants, spring continuity under retarget, clock stops
  when settled, MPRIS mapping, `/proc` parsing, exec timeout and caps.

## Revisit when
- ~~The owner settles how an exec widget written over the socket is approved
  before it runs.~~ Settled by ADR 0067 (command approval, COMP-10 §3.11).
- A spec (COMP-10 or a privacy volume) defines audio capture policy: the monitor
  tap moves under it.
- Anyone proposes loading widget code in-process: that is ADR 0041's question
  and is not settled here.

## Amendment (2026-09-28)

Owner testing of the first build changed four behaviours, now in the body
above: the drag bar only where there is something to reveal (or once
squeezed), proportional crowding of unpinned widgets and chips instead of
widgets-first, a collapse allowance on release, and a one-step tap ladder.
The Settings Taskbar pane edits `order` and the tray lanes by drag and drop on
its bar picture (D-05 §2); the writes are unchanged.

## Amendment (2026-10-22): the Volume widget opens an audio picker

Clicking the Volume widget used to toggle mute. It now opens a drawer that
unrolls from the bar under the cell, the same reveal the wifi and bluetooth
drawers use. Mute moved into the drawer, and wheel scrolling still steps the
level. The drawer has two sections, output and input. Each has a level row
(mute, rail, percent), the default device as the top row, and the other
devices below it. Picking a device makes it the default and moves the
streams that are playing or recording to it. The only exception is
recordings of a monitor source, such as visualizers and loopbacks.
`ec-services::audio` gains the sink and source lists, with `.monitor`
sources left out, the default switch, and input volume and mute. It still
speaks the native protocol, so nothing shells out.

## Amendment (2026-11-03): the battery drawer, quick actions and Bluetooth battery

Three things the bar gained on the widgets branch.

**The battery drawer.** Clicking the battery cell opens a drawer: the reading
as a magnitude hero (percentage, state, and the time left when UPower has
one), then the power profile selector. The selector is `ec-services`'
`power_profiles` (ADR 0038's bus pattern): one watcher, `set_profile` on its
own thread, and the daemon's refusal shown in danger ink until the human
asks again. The selector is hidden when the daemon is unavailable. Performance
is greyed, with the reason as a line under the selector, when the machine does
not offer it or the daemon reports it limited. The reading takes the accent
only when the battery is full, as the cell does.

**The `quick-actions` widget.** A new built-in id, default-ordered before the
tray. Its cell is one understated glyph and is zero width while no `action`
block is configured. The drawer is a grid of tiles, one per action. An `app`
target launches its desktop entry as the launcher does. An `exec` target runs
its argv with no shell, and only when the owner's approval for exactly that
argv is recorded (ADR 0067); any other exec tile is disabled and reads "needs
approval", and the bar checks the approval again at the press. The socket does
not serve `bar.quick_actions`, so the bar reads the owner's config through
`ec-abyss-config` at start and on reload, the way the compositor reads it.

**Bluetooth.** A device's own battery (BlueZ `org.bluez.Battery1`
`Percentage`) is read with the device list and shown as a figure on the
device's row, and on the connected hero's state line when it is known. The
drawer's marks are 13px tertiary, as the context menu's are. Closing the
drawer stops discovery and clears the scanning state, so no scan runs unseen.

## Amendment (2026-11-03): the progress widget

The bar hosts `ec-services`' progress feed (ARCHITECTURE, "Progress feed") and
shows it as a built-in widget, `progress`, default-ordered between the tray and
the clock.

**The cell.** Zero width, and drawn by nothing, while no job is in flight. With
jobs, the shell's own presence and width animation brings it in. It shows the
oldest job: a ring (a determinate arc from the top, or a partial arc turning
slowly when the amount of work is unknown), the label cut to a width token, `N%`
when the amount is known, and `+N` when there are more. The ring and its fill
are the neutral ink, not the accent: the strip already carries the one live
yellow (the focused chip's label and a full battery), and a second would
compete with it.

**The drawer.** Clicking the cell opens `Drawer::Progress`, a reveal drawer like
the others. Its head is followed by a magnitude hero for the job the cell shows
(its percent, or its elapsed time when the amount is unknown), a rule, and then
one block per job, oldest first, up to the drawer's row cap with a note for the
rest. A job shows its ring, label and reading, its bar when the amount is known,
and a mono line with its source (`feed` or `scan`) and how long it has run.
When the last job ends the drawer closes with the cell it hangs from.

**Motion.** The frame clock runs only while an indeterminate job is in flight and
motion is on (`App::progress_turning`), like the battery's energy flow. A bar
with no job, or only determinate jobs, wakes nothing for progress.

**Scanning.** The bar keeps the `/proc` scan on for the life of the process
rather than turning it off while the cell is hidden. The scanner already
slow-polls at five seconds while nothing runs, and `ec-ctl progress` should find
a live feed whether or not a job is in flight.

## Amendment (2026-11-03): the power widget

The bar gains a built-in widget, `power`, default-ordered between `progress` and
`clock`, so it sits at the far right of the strip. Its cell is one understated
glyph (the system-shutdown mark, in the secondary ink), always present, and
clicking it opens `Drawer::Power`, a reveal drawer like the others.

**The rows.** Lock, Log out, Suspend, Reboot and Power off, each greyed by what
logind answers for it (`ec_services::session`'s `Availability`): an action it
offers is live, a Challenge is live with a note that polkit asks first, and a No
is greyed with "not permitted". Log out, Reboot and Power off are destructive and
need a second click: the row becomes "Confirm …" with a Cancel beside it. A
confirmation, and an open scope prompt, are forgotten when the drawer closes, so
a reopened drawer starts from its rows. Lock and Suspend are one press.

**The update rows.** Below a hairline, and only while `ec_services::updates`
reports a pending Eclipse package, sit "Update & reboot" and "Update & power
off" (ADR 0085). Each is offered only when its power action is offered too, since
the power action follows a successful upgrade. Choosing one opens a scope prompt
in the drawer: two radio options, "Eclipse and dependencies only (K packages)"
and "All packages (N)", with the counts from the pending update, then Confirm and
Cancel. Confirm hands the scope and the power action to `updates::apply`; the
widget never runs pacman, and it never calls a power action except for a machine
row the human confirmed.

**The apply.** While it runs the power rows wait, and the chosen update row shows
a spinner and "see progress", pointing at the progress cell, where the backend
reports the job `eclipse-update`. A refusal or a failure is shown inline in danger
ink and the rows come back. A success shows the restart the backend requested.

**The accent.** None. Availability is grey, the pending update is plain ink, and
the spinner is neutral. The strip's one yellow stays where it was.

**The services.** The update watcher is hosted with the other widget services
and started on the first configure. It runs `checkupdates` on start, every 30
minutes and never in a loop. The logind probe runs on a thread when the drawer
opens, and a session action runs on a thread when the human confirms it; a
refusal comes back as a line in the drawer. Nothing is polled from the draw. A
debug preview (`HYPERION_PREVIEW=power`, with `HYPERION_PREVIEW_POWER` and
`HYPERION_PREVIEW_SESSION`) starts none of these and its messages reach no
service, so no fixture can reach logind, pkexec or pacman.

**The config.** `power` is a built-in id of `bar.widgets.order` and is accepted
by the compositor's validator and by `ec-ctl config migrate`'s default order
(`docs/CONFIG.md` regenerated from the schema).
