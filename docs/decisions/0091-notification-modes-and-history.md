<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0091 — Notifications have modes, a history and one widget
Status: accepted
Date: 2026-11-06
Deciders: chase (owner), Claude (advisory)

## Context
Toasts were stateless: a dismissed or expired notification was gone, there was no
do-not-disturb, and no way to quiet one app. `ec-center` is deprecated and was never a
notification list. Other daemons (dunst, mako, swaync, GNOME, KDE, macOS) all keep a
history, have a DND switch and honour more of the freedesktop hints.

## Options
1. Extend `ec-center` into a notification centre. It is slated for deletion.
2. A new pane crate. More packaging for one popover.
3. Keep the state in `ec-services`, and rework the bar's existing bell popover into the
   management widget. Chosen.

## Decision
**Modes.** `normal` shows and stores; `quiet` stores unread and shows nothing; `dnd`
shows nothing and stores nothing (it still returns an id). Modes apply to critical
notifications too. Only the compositor config-error notice (id `u32::MAX`, ADR 0064)
bypasses them, and it is never stored. Mode always beats per-app rules.

**History.** A ring of 200 in `$XDG_STATE_HOME/eclipse/notifications.json` (0600,
atomic write), with the mode. Transient notifications are skipped. Every stored entry
starts unread; opening the widget marks all read, moving them from quick view to the
history drawer. Restored entries are not live, so they carry no action buttons.

**Rules.** `~/.config/eclipse/notifications.json`, re-read on mtime change: `mute`,
`quiet`, `urgency`, `timeout-ms`, matched on `desktop-entry` then app name, case
insensitive. JSON because `ec-services` has no KDL parser.

**Hints.** `image-data`, `category`, `desktop-entry`, `resident`, `value`, `sound-*` are
parsed. Capabilities are advertised only once honoured end to end; `body-hyperlinks`
and `action-icons` are not.

**Sound.** Played in-house: a WAV parser and tones synthesised in code, sent over the
existing PulseAudio native-protocol client. No new dependency. One sound per 250 ms,
dropped on any failure. OGG/OPUS and theme lookup are not supported.

**Widget.** The bar bell shows a mode mark and `(N)`; it opens a reveal drawer like the
network and battery ones (not a popup): mode switch, the newest unread as a hero, quick
rows, and a folded, scrollable history. Its height is capped at the bar's reserved height
for either bar position. While it is open the bar tells the service (`SetViewing`), so
Normal arrivals are stored and merged into the drawer without a popup or sound; Critical
and the config-error notice still pop. `live` means "can still be invoked this session",
so an entry that never had a popup keeps its action buttons until the sender closes it.
The contract is `docs/internal/notifications-contract.md`.

## Consequences
- History holds notification content on disk (0600, bounded). DND stores nothing.
- Sound playback has not been run against a real pipewire-pulse server at the time of
  writing.
- Per-output toast placement and frost-mode background blur (BLUR-03) remain open.
