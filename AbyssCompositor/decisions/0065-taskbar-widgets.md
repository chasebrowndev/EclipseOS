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
`bar.widgets.order`, and `eclipse-ctl config migrate` rewrites old files.

`bar.widgets.important` (string list, default `clock`, `battery`) names widgets
that **never compress**: the solver keeps them at their core size and takes
the room from everything else.

Every widget is `[drag bar][core]` with an optional **revealed section**
shown by dragging the bar left (Now Playing: back / play-pause / skip; System
Usage: memory, GPU, disk). A widget **compresses** to its drag bar alone; one
without a drag bar gets one when compressed. Now Playing with nothing playing
compresses to **zero width**, animating out.

**One layout solver** assigns every chip and widget its target x and width. As
room shrinks: revealed sections fold first, then non-important widgets
compress to their bars (last in `order` first), and only then do chips step
down the ladder. Important widgets never compress.
Dragging a compressed bar left takes room from the chips, which re-ladder;
release snaps to the nearest state; dragging right collapses; a tap toggles.
Pointer and touch share one gesture path. Popup anchors read the same solver
output.

**Motion.** Every chip and widget animates x, width and opacity toward the
solver's targets. The default curve is a critically damped spring that
retargets from its current position and velocity, so an interrupted animation
never jumps. The frame clock runs only while something moves. Keys:
`bar.motion.enabled` (default on; off snaps), `bar.motion.duration-ms`
(default 220), `bar.motion.curve` (the `animations` curves plus `spring`,
default `spring`). The primitives live in `eclipse_ui::motion` so other
components may use them.

**Custom widgets.** A `widget` block is either:
- `exec "argv0" "arg"…` with `interval-ms`, or `stream #true` for a
  long-running command emitting one update per line; a line is plain text or
  JSON `{text, detail, tooltip, state}`;
- or `source "<shipped source>"` with a `format` string (declarative).

Optional `icon`, `on-click`, `on-scroll-up`, `on-scroll-down` (argv lists).
Commands are argv-exec, never through an implicit shell. They run on an
`eclipse-services` thread, never on a draw path, with a timeout, a 4 KiB line
cap, and are killed on reload or removal. Output is rendered as plain text,
never markup, and never logged (it may carry anything the user's command
prints). Exec widgets have exactly the authority of the user who wrote them;
they are not a plugin mechanism and gain nothing from the taskbar.

**Data services** go in `eclipse-services`, on the `status` pattern (one
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
  Settings controls. `set_config_value` gains a collection write for `widget`
  entries (COLLECTIONS were not settable before).
- `hyperion/src/audio.rs`'s `pactl` path goes away; volume keybinds are
  unaffected.
- New crates on the supply chain: an audio-server client (native PipeWire, or
  the PulseAudio protocol that PipeWire serves; whichever builds without
  libclang and passes `cargo deny`), `realfft` (MIT/Apache), and an NVML
  binding that `dlopen`s `libnvidia-ml.so` (no build-time NVIDIA dependency).
- The tray's built-in status items become widgets; `bar.tray.*` shrinks to SNI
  items, with a config migration.
- The chip layout functions (`ladder`, `expand`, `chip_span`) fold into the
  solver; `chip_span`'s drift is fixed as a consequence.
- D-05 §2, §3 and §7 are updated to say what the bar now does.
- Owed tests: solver invariants, spring continuity under retarget, clock stops
  when settled, MPRIS mapping, `/proc` parsing, exec timeout and caps.

## Revisit when
- A spec (COMP-10 or a privacy volume) defines audio capture policy: the monitor
  tap moves under it.
- Anyone proposes loading widget code in-process: that is ADR 0041's question
  and is not settled here.
