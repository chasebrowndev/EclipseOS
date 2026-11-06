# Notifications backend contract (ec-services)

Code: `shell/crates/ec-services/src/notifications/`. Everything is re-exported from
`ec_services::notifications`.

## 1. Toasts process (in-process handle)

`ec_toasts` owns the server (`notifications::spawn()` -> `Notifications`), as before.
`try_recv()` yields `Event::Posted(Box<Notification>)` / `Event::Closed{id, reason}`
(no new variants). **Only notifications that should pop up arrive**: DND, muted and
quiet arrivals never produce `Posted`. A quiet arrival that replaces a visible toast
sends `Closed{id, ByCall}` (ignore unknown ids). The compositor config-error notice
(id `u32::MAX`, critical, no expiry) bypasses mode and rules and is not stored.

`Notification` new fields (it now derives `Default`; build literals with
`..Notification::default()`):

| field | type | meaning |
|---|---|---|
| `category` | `String` | `category` hint, `""` if none |
| `desktop_entry` | `String` | `desktop-entry` hint without `.desktop` |
| `resident` | `bool` | invoking an action must not close the toast (server already skips `NotificationClosed`; UI should keep the card) |
| `progress` | `Option<u8>` | `value` hint clamped 0..=100 -> progress bar |
| `image` | `Option<Image>` | `image-data`: `Image{width,height,rgba}` straight RGBA8, tightly packed, max 512x512; wins over `icon` for the avatar |
| `sound` | `Sound{name,file,suppress}` | played by the server (`notifications/sound.rs`) when the notification is shown; see §3 |
| `group` | `String` | grouping key: lowercased desktop entry, else lowercased app name |

`urgency` and `expires_in` are **final** (per-app rules already applied). Image/body are never logged.

## 2. Control surface: `org.eclipse.NotificationHistory`

Session bus, name `org.eclipse.NotificationHistory`, path `/org/eclipse/NotificationHistory`,
same-named interface (the old `List/Remove/Clear/InvokeAction/Changed` are unchanged).
Rust client: `HistoryClient` (blocking; connects lazily; every call is a no-op/empty without a service).

| D-Bus | `HistoryClient` | notes |
|---|---|---|
| `GetMode() -> s` | `mode() -> Mode` | `"normal"` / `"quiet"` / `"dnd"` |
| `SetMode(s)` | `set_mode(Mode) -> bool` | invalid string = `InvalidArgs`; persisted |
| `ListPage(s filter, u offset, u limit) -> s` | `page(PageFilter, u32, u32) -> Page` | newest first; filter `all`/`unread`/`read` (unknown = all); limit capped at 200 |
| `UnreadCount() -> u` | `unread_count() -> u32` | |
| `MarkAllRead()` | `mark_all_read()` | call when the widget opens; entries stay in history as read |
| `MarkRead(u)` | `mark_read(id)` | |
| `Remove(u)` / `Clear()` | `remove(id)` / `clear()` | delete |
| `InvokeAction(u, s) -> b` | `invoke_action(id, key)` | only while the toast is live |
| signal `Changed()` | `changes() -> Receiver<()>` | list/flags changed; re-`page` |
| signal `UnreadChanged(u)` | `unread_changes() -> Receiver<u32>` | emitted with every `Changed` and on mark-read; carries the new count |
| signal `ModeChanged(s)` | `mode_changes() -> Receiver<Mode>` | |

Quick view = `page(Unread, 0, n)`. History drawer = `page(Read, offset, limit)`.
Badge = `UnreadChanged` / `unread_count()` (Quiet arrivals + toasts shown but never opened in the widget).

### `ListPage` JSON

```json
{"total": 12, "unread": 3, "mode": "normal",
 "entries": [{
   "id": 7, "app": "Mail", "icon": "mail-unread", "summary": "..", "body": "..",
   "urgency": "low|normal|critical", "timestamp": 1730000000,
   "actions": [{"key": "ok", "label": "OK"}],
   "live": false, "read": false, "category": "email.arrived",
   "desktop_entry": "thunderbird", "group": "thunderbird", "progress": null }]}
```

`total` = matches of the filter over all pages; `unread` = overall. `Page::from_json`/`to_json` are public.
History does **not** carry `image-data` (not persisted); use `icon`.

## 3. Semantics

- **Normal**: popup + stored unread. **Quiet**: stored unread, no popup, no sound. **DND**: ignored entirely (not stored, no popup, no sound); the client still gets an id. DND and Quiet apply to critical too; only the compositor config notice bypasses.
- `transient` is shown but never stored (not counted unread).
- Store: ring of 200, replace-by-id moves the entry to newest and resets it to unread.
- Persistence: `$XDG_STATE_HOME/eclipse/notifications.json` (`~/.local/state` fallback), written atomically (0600) after every change; mode is saved too. Restored entries are never `live`. Fresh ids continue above the highest restored id.
- **Sound**: played in the service (no UI involvement), only for a notification that is shown (Normal mode, not `mute`d, not `suppress-sound`; never the compositor config notice). `sound-file` (absolute path or `file://`; WAV PCM 8/16-bit or float32, mono/stereo, ≤2 MiB and ≤10 s) wins; `sound-name` maps a few theme names (`message-new-instant`, `message`, `dialog-information` → blip; `dialog-warning` → rising pair; `dialog-error` → low falling pair) to tones synthesised in code; an unknown name gets the default blip (Critical: the low pair) except at Low urgency, which stays silent. No hint, no sound. At most one per 250 ms; played over the PulseAudio native protocol on a worker thread, dropped if the server is unreachable (`EC_NOTIFY_DEBUG=1` prints why).
- Capabilities advertised: `body, body-markup, actions, icon-static, persistence, sound`. **Not** advertised yet: `body-hyperlinks`, `action-icons` (flip once toasts/widget render them: `CAPABILITIES` in `server.rs`).

## 4. Per-app rules

`$XDG_CONFIG_HOME/eclipse/notifications.json` (`~/.config/eclipse/`), re-read when its mtime changes:

```json
{"rules": [
  {"desktop-entry": "slack", "quiet": true},
  {"app": "Updater", "mute": true},
  {"app": "Mail", "urgency": "low", "timeout-ms": 3000}
]}
```

Match on `desktop-entry` first, then `app` (case-insensitive; a rule naming both needs both).
`mute` = like DND for that app; `quiet` = stored without popup; `urgency` = low/normal/critical;
`timeout-ms` (0 = server maximum, still clamped for non-critical). Mode always wins over rules.
