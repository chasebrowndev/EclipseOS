# D-08 — Desktop portal (`ecl-desktop-portal`)
Status: P1 partly built in `portal/` (not packaged, not started by the session;
ScreenCast answers from a stub backend; Wallpaper not yet served). Until this
ships, ADR 0100's `ec-portal` FileChooser backend under upstream
`xdg-desktop-portal` is what installs use. Governing ADRs: 0096 (architecture), 0097 (scoped
capture grants + trusted confirm). Both are `proposed`.

## 1. What it is
One Rust session daemon that replaces `xdg-desktop-portal` and every backend
(`-gtk`, `-kde`, `-wlr`). It serves the unchanged public API
(`org.freedesktop.portal.Desktop`, `/org/freedesktop/portal/desktop`) and the
backend name `org.freedesktop.impl.portal.desktop.eclipse`, so browsers,
Flatpaks, OBS and GTK/Qt apps need no change. All dialogs are drawn by
Eclipse panes in the STYLE.md language (dark, gold accent, glass, no gradients,
glassy-not-glowy).

## 2. Workspace `portal/`
| Crate | Role |
|---|---|
| `ecl-portal-proto` | daemon<->pane IPC types (postcard, no I/O, Apache-2.0) |
| `ecl-portal-core` | Request/Session objects, handle tokens, `Response(u, a{sv})`, option parsing, app-identity resolution (pure, tested) |
| `ecl-portal-store` | permission store (postcard files in `$XDG_DATA_HOME/ecl-portal`, atomic) |
| `ecl-portal-screencast` | PipeWire producer fed by abyss capture protocols |
| `ecl-desktop-portal` | the daemon (tokio + zbus 6) |
| `ecl-portal-ui` | iced panes: source picker, permission sheets, account, print, colour |
| `ecl-portal-ctl` | list/revoke grants, inspect requests |

## 3. Request lifecycle
1. App calls an interface method with an `options` dict (`handle_token`).
2. Daemon resolves caller identity (§4), creates
   `/org/freedesktop/portal/desktop/request/<sender>/<token>`, returns the handle.
3. Permission store decides: allow / deny / ask. Ask launches a pane over a
   private socketpair, one pane per request, which closes itself.
4. Authority-bearing interfaces (ScreenCast, later RemoteDesktop/InputCapture)
   additionally send a request over `portal.sock` to abyss (ADR 0097) and only
   proceed on a minted grant.
5. Daemon emits `Request::Response(code, results)`; `Close` and peer exit cancel.

## 4. App identity (advisory, never the enforcement key)
D-Bus `GetConnectionCredentials` (pid, unit) -> cgroup/systemd scope -> matching
`.desktop` id; `/.flatpak-info` when present; `wp_security_context` tag where
set; otherwise `host`. A claimed identity can only narrow a decision.

## 5. Interfaces
- **P1:** FileChooser (Fog picker), Settings (`org.freedesktop.appearance`
  color-scheme/accent/contrast from Eclipse tokens, live `SettingChanged`),
  OpenURI, Notification (delegates to `ec-services`), Inhibit (delegates to
  idle-inhibit / ScreenSaver), Screenshot, ScreenCast, Trash (via fogd), Email,
  Wallpaper, Account, PermissionStore.
- **P2:** Background, GlobalShortcuts, Clipboard, Print, Documents, DynamicLauncher,
  Camera, Location, Usb, Realtime, Power/Network/Memory monitors, ProxyResolver.
- **Excluded pending ADR:** Secret (VOL2 :1112), RemoteDesktop, InputCapture.

## 6. ScreenCast data path
Chooser pane -> daemon -> `portal.sock` -> abyss confirm modal -> policyd mints
`{stream, app-id claim, target, expiry}` -> abyss admits one capture session ->
`ecl-portal-screencast` pulls frames (wl_shm first, dmabuf later), publishes a
PipeWire node with cursor metadata. Redaction is applied by abyss before frames
leave the compositor. Prerequisites in abyss: cursor-session metadata
(`ext_image_copy_capture`, still refused) and a toplevel capture source
(`ext_foreign_toplevel_image_capture_source_manager_v1`, absent) for
per-window capture; `ext-foreign-toplevel-list` itself has since landed.

## 7. Packaging
New split `eclipseos-portal`: `/usr/bin/ecl-desktop-portal`, `ecl-portal-ui`,
`ecl-portal-ctl`; user unit `ecl-desktop-portal.service` (`PartOf=graphical-session.target`,
`WantedBy=abyss-session.target`); D-Bus activation file; 
`/usr/share/xdg-desktop-portal/portals.conf` (`default=eclipse`) and
`portals/eclipse.portal`. `Conflicts`/`Replaces` the upstream packages.

## 8. Tests
Per-interface D-Bus introspection and request/response harness with a fake
caller; compat smoke (Firefox/Chromium share, OBS, flatpak open, xdg-open,
colour-scheme following); abyss: no token = no frame, denied confirm = no frame,
redacted surface = black, frame rate vs the xdpw baseline in STATUS row 8.
