# 0100 — The portal file chooser is ec-portal driving Fog's picker
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
Apps on EclipseOS that ask xdg-desktop-portal for a file dialog (Firefox,
Electron, GTK and Qt apps, Flatpaks) get none of ours: `abyss.portal` lists
only `org.freedesktop.impl.portal.Settings` (ADR 0084), so FileChooser falls
to whatever other backend is installed, or to nothing.

FOG-SPEC §File picker plans a separate `fog-portal` crate and bus name
(`fog.portal`, `FileChooser=fog`). The owner chose instead to grow
`ec-portal`, which already owns `org.freedesktop.impl.portal.desktop.abyss`:
one backend process, one `.portal` file, one portals.conf.

The interface (`/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml`)
has `OpenFile`, `SaveFile` and `SaveFiles`. Each call carries a Request
handle; the backend exports `org.freedesktop.impl.portal.Request` there and
must close its dialog on `Close`. The reply is `(u, a{sv})`: 0 success with
`uris` (`file://` only), 1 cancelled, 2 other.

## Options
1. **`ec-portal` spawns `ec-fog-ui --pick …` per request** and reads the
   answer from its stdout. No IPC to design, no resident picker; the picker
   is the same UI as the file manager. A cold start per dialog.
2. **A `fog-portal` crate (FOG-SPEC).** A second backend process and bus
   name for one interface, and Settings and FileChooser configured apart.
3. **A request over `fogd.sock` to a resident UI.** Faster opens, but a new
   protocol, and the UI process would have to stay running.

## Decision
Option 1. `ec-portal` serves FileChooser on the object it already serves
Settings on (`shell/crates/ec-services/src/filechooser.rs`). Each call:

- parses the `a{sv}` into a `Pick` (malformed or unknown options ignored),
  sanitizing everything the app sent: control and bidi-control characters
  stripped, lengths clamped, `current_folder`/`current_file` (`ay`,
  NUL-terminated) accepted only as absolute paths, `current_name` only as a
  bare file name, mime types only as `type/subtype`, `parent_window` only as
  `wayland:<handle>` or `x11:<hex>`;
- exports a `Request` object at the handle, then spawns `ec-fog-ui` with an
  **argv array** (never a shell): `--pick open|open-multiple|directory|directory-multiple|save|save-files`
  plus `--title`, `--accept-label`, `--current-folder`, `--current-name`,
  `--filter NAME (--glob P | --mime T)…`, `--current-filter N`,
  `--choice LABEL (--choice-option LABEL)… --choice-default V`, `--file NAME`,
  `--parent HANDLE`;
- runs the child on its own thread, which reads stdout to EOF, waits with
  `waitid(WNOWAIT)`, marks the pid reaped under the lock `Close` takes, then
  reaps. `Close` sends SIGTERM only to a pid not yet marked reaped, so it can
  never hit a reused pid. The bus side awaits a oneshot, so a dialog never
  blocks another call;
- maps the answer: exit 0 with well-formed records for the mode is 0, exit 1
  is 1, anything else (Fog missing, killed, malformed or oversized output,
  `Close`) is 2.

Fog's contract (`fog/crates/ec-fog-ui/src/pick.rs`): on confirm, stdout
carries NUL-terminated records (`P<abs path>` per chosen path, `F<n>` the
filter in use, `C<i>=<n|true|false>` per shown choice) and the process
exits 0; cancel or closing the window exits 1 with nothing written; a bad
command line exits 2. stdout is moved to a private close-on-exec descriptor
before the UI starts and fd 1 becomes /dev/null, so stray prints and spawned
custom actions cannot reach the answer.

`SaveFiles` picks a folder; `ec-portal` joins each app-given name to it,
using `name (2).ext`… where a name is taken on disk or earlier in the same
reply, as the interface allows. URIs percent-encode every byte outside
RFC 3986's unreserved set and `/`.

Only what Fog reports is returned. A choice Fog did not show is not echoed
with its default; a filter it did not report is not returned as
`current_filter`.

**Not supported yet** (ignored, never faked):
- `modal` and `parent_window`: no xdg-foreign or xdg-dialog parenting; the
  handle reaches Fog (`--parent`) but is unused. The picker is a top-level
  window that the Radiant rule can float.
- `writable` (OpenFile result): never set, so `false`.
- `app_id`: not passed to Fog.
- Filter kinds other than glob (0) and mime (1), and malformed mime types:
  dropped. A filter with no usable pattern is dropped.
- Fog's view (accept bar, name field, filter dropdown, choices, save-files
  name list) is not built. Until it is, only `open` and `open-multiple` can
  confirm, by opening a file (Enter or double-click); the folder and save
  modes can only be cancelled.
- Overwrite confirmation in `save` is the view's job; until it exists `save`
  cannot confirm at all.
- FOG-SPEC's "if `ec-fogd` is unreachable, exit with an error" is not
  implemented: the picker behaves as the file manager does without
  `ec-fogd`. xdg-desktop-portal picks its backend from portals.conf, not per
  call, so an early error would give the app no dialog rather than GTK's.

## Consequences
- `abyss.portal` lists FileChooser and `abyss-portals.conf` routes
  `org.freedesktop.impl.portal.FileChooser=abyss`. `eclipseos-desktop`
  depends on `eclipseos-fog`; without `ec-fog-ui` on PATH a request answers
  2 at once.
- Any session-bus client can call the backend directly, as with every
  portal backend. It returns only what the human picked in a dialog they
  saw; it grants nothing.
- FOG-SPEC §File picker and its component table still name `fog-portal`;
  they need updating to point here.
- We owe: the picker view (eclipse-frontend), then the M3 exit check
  (Firefox, a GTK4 app, a Qt6 app and a Flatpak open and save through Fog).

## Revisit when
Cold-start latency of the picker is noticeable (move to option 3), or
xdg-foreign/xdg-dialog parenting lands in the iced/winit stack Fog uses.
