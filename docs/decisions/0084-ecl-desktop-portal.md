# 0084 — `ecl-desktop-portal`: an in-house, drop-in desktop portal
Status: proposed
Date: 2026-11-02
Deciders: chase (owner), Claude (advisory)

## Context
D-01 names `xdg-desktop-portal` plus `xdg-desktop-portal-wlr` as an interim
backend "until an Eclipse portal exists"; D-05 says there is no portal. Apps,
browsers, Flatpaks and OBS only know how to ask `org.freedesktop.portal.Desktop`.
The upstream frontend asks a backend (`org.freedesktop.impl.portal.*`) to draw
every dialog; those dialogs (GTK/KDE/wlr pickers) cannot match the Eclipse
aesthetic and are out-of-tree UI we neither control nor trust (ADR 0029).

What the specs fix: the capture gate is fail-closed and keyed on process name
(ADR 0027), which degrades to "anything that can reach the portal" once the
portal is allowlisted (ADR 0029). COMP-10 has no capture-consent prompt, and its
§3 list of prompts is closed (ADR 0053). Trusted UI is compositor-drawn, never a
client (ADR 0009). `org.freedesktop.secrets` is deliberately not provided
(VOL2 :1112).

## Options
1. Keep upstream xdg-desktop-portal and write only Eclipse backends. Least code,
   but the frontend still owns identity, permission storage and routing, and
   third-party dialogs remain the fallback for anything we do not implement.
2. Replace the frontend only and keep third-party backends. Same problem on the
   UI side.
3. **Replace both**: one Rust daemon serving the unchanged public D-Bus API
   (`org.freedesktop.portal.*`) and every backend role in-process, with all
   dialogs drawn by Eclipse panes.

## Decision
Option 3. A new `portal/` workspace ships `ecl-desktop-portal` (tokio + zbus,
session service). It owns `org.freedesktop.portal.Desktop` and the
`org.freedesktop.impl.portal.desktop.eclipse` backend name, implements the
Request/Session lifecycle, app-identity resolution and a permission store
itself, and launches themed `ecl-portal-ui` panes (iced, `ec-ui` tokens) for
dialogs. The wire API is bit-compatible, so no application changes.

Rules the daemon lives under:
- It is an **untrusted session process** (ADR 0028). It holds no ambient capture
  or input authority; every capture/input grant is minted by policyd and
  enforced by abyss (ADR 0085).
- Anything it forwards about the requester (app id, target) is a **claim to
  display**, never a grant, and may only narrow a decision (ratchet rule).
- App identity is best-effort and advisory: D-Bus connection credentials, the
  systemd scope/cgroup, `.desktop` matching, `/.flatpak-info` where present and
  `wp_security_context` tags. Unknown callers get a least-privilege `host`
  identity. Identity is never used as the enforcement key.
- Dialog panes are rich but carry no authority. No password-role value,
  keystroke, clipboard or IME content is logged or stored by the daemon.
- Fog's picker (`ec-fog-ui --picker`) is the FileChooser UI; `fog-portal` is
  folded into the daemon.
- Phased: P1 daily driver (FileChooser, Settings, OpenURI, Notification,
  Inhibit, Screenshot, ScreenCast, Trash, Email, Wallpaper, Account,
  PermissionStore), P2 the rest (Background, GlobalShortcuts, Clipboard, Print,
  Documents, DynamicLauncher, Camera, Location, Usb, Realtime, monitors),
  P3 RemoteDesktop/InputCapture under its own ADR.
- **Secret** is excluded from P2 until a separate ADR reconciles it with VOL2
  :1112 / open question 3 (the "no read-only shim in v1" position). Until then
  `org.freedesktop.portal.Secret` is not served.

## Consequences
- D-05 is amended: the portal is now part of the default userland; `xdg-desktop-portal*`
  stays out of `eclipseos-meta` and is declared `Conflicts`/`Replaces` in the new
  `eclipseos-portal` split package.
- `docs/STATUS.md` row 8 and a new D-08 design doc track it. The daemon and
  panes follow the frontend/backend agent split; nothing here touches TCB paths
  (those are ADR 0085).
- New dependencies (`pipewire`, later `fuser`) each need a `cargo deny` review.
- We owe: D-Bus conformance tests per interface, compat smoke tests against real
  clients, and a Settings page for grants.

## Revisit when
Upstream changes the frontend/backend split incompatibly; or the Secret ADR
lands; or a Flatpak/sandbox identity scheme is specified (S-03 `crates/sandbox`).
