<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# D-05 — Default userland (as built)

Status: **written 2026-09-23**, describing what is built, not a plan. Partial:
the terminal (`cataclysm`, P-04) and the portal rows of the planning index are
not covered, because neither exists. Depends on D-01 (written).

Governing specs: Tier 6 D-05. Constrained by COMP-17 (desktop profiles),
COMP-13 §1.4/§1.5 (write API, parity), ADR 0038 (native Rust), ADR 0051
(screensaver), ADR 0052 (one crate per component), ADR 0053 (actions and the
secret prompt). Where this document and the code disagree, the code wins and
this document is the bug (F-07 §7).

**Written after the fact.** The DE was built in B5/B6 against ADRs, with no
D-05 text. The index still says D-05 is blocked on `cataclysm`; that is true of
the terminal row only.

## 1. Shape

Ten crates: seven programs, two libraries, and `eclipse-services`, which is both.

| Crate | Binary | Surface | Package |
|---|---|---|---|
| `hyperion` | `hyperion` | layer-shell, `Top`, one per output | `eclipseos-hyperion` |
| `eclipse-toasts` | `eclipse-toasts` | layer-shell, `Overlay`, top-right | `eclipseos-toasts` |
| `eclipse-center` | `eclipse-center` | layer-shell, `Overlay` | `eclipseos-center` |
| `eclipse-launcher` | `eclipse-launcher` | layer-shell, `Overlay`, exclusive keyboard | `eclipseos-launcher` |
| `eclipse-settings` | `eclipse-settings` | xdg_toplevel | `eclipseos-desktop` |
| `eclipse-policy-viewer` | `eclipse-policy-viewer` | xdg_toplevel | `eclipseos-desktop` |
| `eclipse-secret-prompt` | `eclipse-secret-prompt` | xdg_toplevel, fixed size | **none** (§5) |
| `eclipse-services` | `eclipse-screensaver` (`src/bin/`) | none; also a library | `eclipseos-desktop` (bin) |
| `eclipse-ui` | — | library: tokens, theme, widgets, `ipc::fetch_config_radius` | inside consumers |
| `eclipse-ipc` | — | library: control-socket client, `serde_json` + `libc` only | inside consumers |

**One crate per swappable component** (ADR 0052). A component never depends on
another; shared code goes into `eclipse-ui` (visual) or `eclipse-services`
(data and D-Bus). Cross-component launches go by binary name on `PATH`, so a
replacement binary of the same name slots in. Toolkit: iced 0.14 plus
iced_layershell 0.19.1 (ADR 0038).

## 2. Components

**hyperion** — the taskbar. With no arguments it is a surface-less
**supervisor** that spawns one `hyperion --output <NAME>` child per output
(`HYPERION_OUTPUT` carries the name into the app), reconciles the set on
`output` events, and ties children to itself with `PR_SET_PDEATHSIG`. Each bar
shows its output's workspaces, window chips (focus, close, minimize, new
instance via the matching `.desktop` entry), a clock, widgets (ADR 0065:
Now Playing, System Usage, Volume, network, bluetooth, battery, tray, clock
and user `widget` blocks; non-important ones compress to a
drag bar as chips need the room), network/bluetooth/battery drawers, and the SNI tray with an
overflow drawer. It folds per `bar.*` (ADR 0042). Chips and widgets are
placed by one layout solver and move under `bar.motion.*` (ADR 0065). The event thread `poll`s the
socket with a 500 ms ceiling and coalesces a burst into one refetch;
`window {change: "title"}` follows renames. The supervisor also holds the one
BlueZ pairing agent (ADR 0053).

**eclipse-toasts** — the notification stack. Owns
`org.freedesktop.Notifications` in-process via `eclipse_services::notifications`.
Bodies are never logged; only `Critical` may pin itself on screen.

**eclipse-center** — the control center: status readout plus lock, log out,
suspend, hibernate, reboot, power off, each greyed out when logind's `Can*`
says no, and a refusal shown rather than worked around. A menu: it exits after
one action.

**eclipse-launcher** — filters `.desktop` entries (`eclipse_services::apps`)
and spawns one, detached. `Terminal=true` entries run as `$term -e <argv>`
when `misc.terminal-command` is set and are **hidden**, not refused, when it is
not. The shipped `abyss.kdl` is empty, so by default they are hidden.

**eclipse-settings** — §6. Takes an optional pane name as `argv[1]`
(hyperion's drawers use this). Panes: Appearance, Taskbar, Display, Network,
Input, Session, System, Privacy.

**eclipse-policy-viewer** — reads `/etc/eclipse/policy.kdl` then
`$XDG_CONFIG_HOME/eclipse/policy.kdl` off disk (`src/read.rs`) and renders
it. No write path exists and none may be added. An absent file is normal; a
malformed one names the file and the reason; unreadable reads as granting
nothing.

**eclipse-secret-prompt** — one password field, then exit (ADR 0053).
`wifi <ssid>` hands the passphrase to NetworkManager through
`eclipse_services::status::Actions`; `bt <addr> pin|passkey|authorize|confirm
<n>|show <code>` answers the supervisor's agent over the session-bus door
`org.eclipse.Services.Pairing`. Every owned copy of the secret is wiped; no
`Debug` on anything that holds it. App-id `eclipse-secret-prompt` is
load-bearing (§5).

**eclipse-screensaver** — owns `org.freedesktop.ScreenSaver` on
`/org/freedesktop/ScreenSaver` and `/ScreenSaver`, one cookie per `Inhibit`,
cookies dropped when their owner leaves the bus, and calls
`set_idle_inhibit {inhibit}` whenever "any cookie held" flips (ADR 0051). If
the name is taken it exits non-zero rather than stealing it.

## 3. Surfaces

The control socket is `$XDG_RUNTIME_DIR/eclipse/abyss.sock`, owner-only
(COMP-13 §2); `eclipse-ipc` is the client. Every socket read is fail-soft:
nothing listening renders empty, never crashes.

| Component | Socket methods | Events | D-Bus |
|---|---|---|---|
| hyperion (bar) | `get_workspaces`, `get_windows`, `get_focused`, `get_outputs`, `get_config`, `focus_window`, `close_window`, `set_minimized`, `switch_workspace` | `window`, `workspace`, `focus`, `output`, `config_error`, `config` | system: NetworkManager, BlueZ, UPower. session: SNI watcher/host, MPRIS players (`org.mpris.MediaPlayer2.*`). PipeWire: default sink volume/mute, monitor tap (ADR 0065) |
| hyperion (supervisor) | `get_outputs` | `output` | system: BlueZ `org.bluez.Agent1`. session: serves `org.eclipse.Services.Pairing` |
| eclipse-toasts | `get_config` (`decoration.rounding`, startup) | — | session: serves `org.freedesktop.Notifications` |
| eclipse-center | `get_config` (`decoration.rounding`, startup) | — | system: NetworkManager, BlueZ, UPower (read), logind `login1.Manager` / `login1.Session` |
| eclipse-launcher | `get_config` (`misc.terminal-command`, `decoration.rounding`, startup) | — | — |
| eclipse-settings | `get_config {schema: true}`, `set_config_value`, `get_outputs`, `set_output`, `calibrate_output` | `output`, `config_error`, `config` | system: NetworkManager, BlueZ (Network pane only). session: SNI host via `tray::observe` (Taskbar pane only; never serves the watcher, cannot click) |
| eclipse-policy-viewer | `get_config` (`decoration.rounding`, startup) | — | — |
| eclipse-secret-prompt | — | — | system: NetworkManager. session: calls `org.eclipse.Services.Pairing` |
| eclipse-screensaver | `set_idle_inhibit` | — | session: serves `org.freedesktop.ScreenSaver` |

The tray watcher (`org.kde.StatusNotifierWatcher`) is served by hyperion if
the name is free and queued for if not, so ours takes over when another
watcher leaves; the host reads from whoever owns it (`tray/mod.rs`). NetworkManager and BlueZ are
spoken to over zbus directly; nothing shells out to `nmcli` or `bluetoothctl`,
because a subprocess argv is where a secret would leak. Actions run on their
own threads and report back as updates; none runs on a draw path.

## 4. Configuration read

All keys are `abyss.kdl` keys in `crates/abyss/src/config/schema.rs`. No DE
component keeps a config file of its own.

| Key | Reader | Reload |
|---|---|---|
| `bar.fold-when-inactive`, `bar.fold-height`, `bar.fold-when-idle`, `bar.idle-seconds`, `bar.fold-duration-ms`, `bar.fold-curve` | hyperion, re-read on `config` | live |
| `bar.position` | hyperion, once, before the surface exists | restart |
| `bar.tray.pinned`, `bar.tray.hidden` | hyperion (read); eclipse-settings Taskbar pane (read/write) | live |
| `bar.rounding` | hyperion, re-read on `config` | live |
| `decoration.rounding` | hyperion and eclipse-settings, re-read on `config` | live |
| `decoration.rounding` | toasts, center, launcher, policy viewer, once at startup | next start |
| `misc.terminal-command` | eclipse-launcher, at startup | next launch |

Policy keys are never asked for over the socket: `get_config {file: "policy"}`
stays closed in `ipc/gate.rs`, which is why the viewer reads the file itself.

## 5. Launch, and trust

| Component | Started by | Trust |
|---|---|---|
| hyperion | `hyperion.service` | ordinary client |
| eclipse-toasts | `eclipse-toasts.service` | ordinary client |
| eclipse-screensaver | `eclipse-screensaver.service` | ordinary client |
| eclipse-launcher | `Super+E` / `Super+R` default binds; hyperion's launcher button | ordinary client |
| eclipse-center | `Super+N` default bind; `eclipse-center.desktop` | ordinary client |
| eclipse-settings | `eclipse-settings.desktop`; hyperion drawer links | ordinary client |
| eclipse-policy-viewer | `eclipse-policy-viewer.desktop` | ordinary client |
| eclipse-secret-prompt | hyperion (bar for wifi, supervisor for bluetooth) | ordinary client, surface classified `secret` |

The three units in `dist/` install to `/usr/lib/systemd/user/`, are
`PartOf=graphical-session.target`, `Requisite=`/`After=abyss-session.target`,
`Restart=on-failure`, and the PKGBUILD links each into
`abyss-session.target.wants/` (a preset alone never fires for an existing
account). Everything else is spawned by name, so `PATH` must carry it:
`/usr/bin` packaged, `target/debug` under `dist/abyss-dev-session`. Default
binds: `crates/abyss/src/config/mod.rs`. `.desktop` files: `dist/applications/`.

**None of these is TCB.** They hold no capability, enforce no policy, and are
refused or obliged by the compositor like any client (ADR 0038). Trusted UI is
compositor-drawn and never a layer-shell client; a DE binary that would need
to be trusted means ADR 0038 was applied too widely. The one elevated
treatment is a *restriction*: `dist/etc/policy.kdl` ships
`windowrule "sensitivity secret" { app-id "^eclipse-secret-prompt$"; }`, which
buys capture redaction and no direct scanout. Session actions are authorised
by logind/polkit, not by us.

**`eclipse-secret-prompt` is not packaged.** It is absent from the PKGBUILD,
`dist/install.sh` and `dist/install-session.sh`, so on a packaged system the
wifi join and bluetooth PIN paths fail to spawn (hyperion logs it; the pairing
agent refuses). This is a gap, not a decision.

## 6. Settings (COMP-17 §3)

`eclipse-settings` is a client of the COMP-13 §1.4 write API and nothing else:
every write is `set_config_value`, `set_output` or `calibrate_output`. As built
it holds to the four §3 limits:

- **Scoped to `abyss.kdl`.** Policy-owned keys are shown read-only with the
  "edit requires the policy editor" affordance, never hidden and never
  writable. The Network pane is the one pane that is not schema keys: it
  forgets saved networks and paired devices through NetworkManager and BlueZ
  and touches no config file.
- **Controls generated from the schema** (`get_config {schema: true}`); no
  hand-written key list. `tests/coverage.rs` fails if the compositor grows a
  key the app cannot render, unless it is on `ci/gui-coverage-exceptions.txt`,
  which only shrinks. Today it lists four collections: `bind`, `windowrule`,
  `output`, `workspace`. Bespoke controls: the tray lanes and the Display
  pane's calibration (the COMP-03 §1.1 screen-edges selector, whose overlay is
  compositor-drawn).
- **No persistent state.** Window size and selected pane are not remembered.
- **Not TCB**, freely sandboxable, because it cannot write `policy.kdl`.

## 7. What this does not do

- No `mode "wm" | "de"` key (COMP-17 §2). The DE described here is the only
  profile; the tiling defaults are always on.
- No terminal of our own. `foot` is the default (`eclipseos-meta` depends on
  it, and the default `Super+Return` bind spawns it) until `cataclysm` exists.
  This is the D-01 §1.3 substitution, said out loud.
- No portal. `xdg-desktop-portal*` is not in `eclipseos-meta`.
- No policy editor; that is COMP-10 §3.9, compositor-drawn, milestone 15.
- No clipboard manager, OSD, wallpaper setter or desktop icons.
- No in-process widget plugins. Custom taskbar widgets are argv commands run
  off the draw path, or declarative (ADR 0065).

## 8. Open questions

1. **Desktop icons** (COMP-17 §6.2): in v1 scope or not. Nothing is built.
2. **The `mode` key** (COMP-17 §2): whether it lands, and what the WM profile
   drops from the autostart set above.
3. **Packaging `eclipse-secret-prompt`** (§5): which package carries it. It
   belongs with hyperion's actions but is reusable by any bar (ADR 0053).
4. **Center from the taskbar.** ADR 0052 says hyperion spawns `eclipse-center`
   by name; as built it spawns only the launcher, settings and the secret
   prompt. Either the ADR is stale or a button is missing.
