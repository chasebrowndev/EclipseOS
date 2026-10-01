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

Ten crates: seven programs, two libraries, and `ec-services`, which is both.

| Crate | Binary | Surface | Package |
|---|---|---|---|
| `hyperion` | `hyperion` | layer-shell, `Top`, one per output | `eclipseos-hyperion` (add-on) |
| `ec-toasts` | `ec-toasts` | layer-shell, `Overlay`, top-right | `eclipseos-toasts` |
| `ec-center` | `ec-center` | layer-shell, `Overlay` | `eclipseos-center` |
| `ec-launcher` | `ec-launcher` | layer-shell, `Overlay`, exclusive keyboard | `eclipseos-launcher` |
| `ec-settings` | `ec-settings` | xdg_toplevel | `eclipseos-desktop` |
| `ec-policy-viewer` | `ec-policy-viewer` | xdg_toplevel | `eclipseos-desktop` |
| `ec-secret-prompt` | `ec-secret-prompt` | xdg_toplevel, fixed size | `eclipseos-desktop` |
| `ec-services` | `ec-screensaver`, `ec-pairing` (`src/bin/`) | none; also a library | `eclipseos-desktop` (bins) |
| `ec-ui` | — | library: tokens, theme, widgets, `ipc::fetch_config_radius` | inside consumers |
| `ec-ipc` | — | library: control-socket client, `serde_json` + `libc` only | inside consumers |

**One crate per swappable component** (ADR 0052). A component never depends on
another; shared code goes into `ec-ui` (visual) or `ec-services`
(data and D-Bus). Cross-component launches go by binary name on `PATH`, so a
replacement binary of the same name slots in. Toolkit: iced 0.14 plus
iced_layershell 0.19.1 (ADR 0038).

## 2. Components

**hyperion** — the taskbar. One process draws a bar on every output. It
reconciles the set on `output` events: a new output gets its bar about 250 ms
after it first appears, a vanished one's bar closes, the others are untouched,
and an empty output list (socket still connecting) closes nothing.
`--output <NAME>` pins one bar to one output for debugging. Services, the
control-socket connection and custom widget commands run once per session, not
per monitor; widget data is shared, while layout, fold, pins and drag are per
bar. One popup exists across all bars, owned by the bar it opened from. Each bar
shows its output's workspaces, window chips (focus, close, minimize, new
instance via the matching `.desktop` entry), a clock, widgets (ADR 0065:
Now Playing, System Usage, Volume, network, bluetooth, battery, tray, clock
and user `widget` blocks; non-important ones shrink together with the chips
as room runs out, down to a drag bar), network/bluetooth/battery drawers, and the SNI tray with an
overflow drawer. It folds per `bar.*` (ADR 0042). Chips and widgets are
placed by one layout solver and move under `bar.motion.*` (ADR 0065). The event thread `poll`s the
socket with a 500 ms ceiling and coalesces a burst into one refetch;
`window {change: "title"}` follows renames.

hyperion is an **add-on** (ADR 0066): `eclipseos-meta` only suggests it, and
its manifest `/usr/share/eclipse/addons/hyperion.kdl` turns on the
`taskbar-widgets` hook. Without it abyss keeps no widget collection, no premade
catalog and no command approval, and the session still starts: the launcher
and center have their own binds. A command widget runs only once the owner has
approved it in the compositor-drawn prompt (ADR 0067, COMP-10 §3.11).

**ec-pairing** — the one BlueZ pairing agent (ADR 0053), an
`ec-services` binary with its own user unit, so pairing works with no
taskbar. PINs go through `ec-secret-prompt`.

**ec-toasts** — the notification stack. Owns
`org.freedesktop.Notifications` in-process via `ec_services::notifications`.
Bodies are never logged; only `Critical` may pin itself on screen.

**ec-center** — the control center: status readout plus lock, log out,
suspend, hibernate, reboot, power off, each greyed out when logind's `Can*`
says no, and a refusal shown rather than worked around. A menu: it exits after
one action.

**ec-launcher** — filters `.desktop` entries (`ec_services::apps`)
and spawns one, detached. `Terminal=true` entries run as `$term -e <argv>`
when `misc.terminal-command` is set and are **hidden**, not refused, when it is
not. The shipped `abyss.kdl` is empty, so by default they are hidden.

**ec-settings** — §6. Takes an optional pane name as `argv[1]`
(hyperion's drawers use this). Panes: Appearance, Taskbar, Display, Network,
Input, Session, System, Privacy.

**ec-policy-viewer** — reads `/etc/eclipse/policy.kdl` then
`$XDG_CONFIG_HOME/eclipse/policy.kdl` off disk (`src/read.rs`) and renders
it. No write path exists and none may be added. An absent file is normal; a
malformed one names the file and the reason; unreadable reads as granting
nothing.

**ec-secret-prompt** — one password field, then exit (ADR 0053).
`wifi <ssid>` hands the passphrase to NetworkManager through
`ec_services::status::Actions`; `bt <addr> pin|passkey|authorize|confirm
<n>|show <code>` answers ec-pairing over the session-bus door
`org.eclipse.Services.Pairing`. Every owned copy of the secret is wiped; no
`Debug` on anything that holds it. App-id `ec-secret-prompt` is
load-bearing (§5).

**ec-screensaver** — owns `org.freedesktop.ScreenSaver` on
`/org/freedesktop/ScreenSaver` and `/ScreenSaver`, one cookie per `Inhibit`,
cookies dropped when their owner leaves the bus, and calls
`set_idle_inhibit {inhibit}` whenever "any cookie held" flips (ADR 0051). If
the name is taken it exits non-zero rather than stealing it.

## 3. Surfaces

The control socket is `$XDG_RUNTIME_DIR/eclipse/abyss.sock`, owner-only
(COMP-13 §2); `ec-ipc` is the client. Every socket read is fail-soft:
nothing listening renders empty, never crashes.

| Component | Socket methods | Events | D-Bus |
|---|---|---|---|
| hyperion (bar) | `get_workspaces`, `get_windows`, `get_focused`, `get_outputs`, `get_config`, `focus_window`, `close_window`, `set_minimized`, `switch_workspace` | `window`, `workspace`, `focus`, `output`, `config_error`, `config` | system: NetworkManager, BlueZ, UPower. session: SNI watcher/host, MPRIS players (`org.mpris.MediaPlayer2.*`). PipeWire: default sink volume/mute, monitor tap (ADR 0065) |
| ec-pairing | — | — | system: BlueZ `org.bluez.Agent1`. session: serves `org.eclipse.Services.Pairing` |
| ec-toasts | `get_config` (`decoration.rounding`, startup) | — | session: serves `org.freedesktop.Notifications` |
| ec-center | `get_config` (`decoration.rounding`, startup) | — | system: NetworkManager, BlueZ, UPower (read), logind `login1.Manager` / `login1.Session` |
| ec-launcher | `get_config` (`misc.terminal-command`, `decoration.rounding`, startup) | — | — |
| ec-settings | `get_config {schema: true}`, `set_config_value`, `set_config_collection` (`widget`, `bar.widgets.*`), `review_widget` (re-show a withheld command widget's approval prompt; ADR 0067), `get_outputs`, `set_output`, `calibrate_output` | `output`, `config_error`, `config` | system: NetworkManager, BlueZ (Network pane only). session: SNI host via `tray::observe` (Taskbar pane only; never serves the watcher, cannot click) |
| ec-policy-viewer | `get_config` (`decoration.rounding`, startup) | — | — |
| ec-secret-prompt | — | — | system: NetworkManager. session: calls `org.eclipse.Services.Pairing` |
| ec-screensaver | `set_idle_inhibit` | — | session: serves `org.freedesktop.ScreenSaver` |

The tray watcher (`org.kde.StatusNotifierWatcher`) is served by hyperion if
the name is free and queued for if not, so ours takes over when another
watcher leaves; the host reads from whoever owns it (`tray/mod.rs`). NetworkManager and BlueZ are
spoken to over zbus directly; nothing shells out to `nmcli` or `bluetoothctl`,
because a subprocess argv is where a secret would leak. Actions run on their
own threads and report back as updates; none runs on a draw path.

## 4. Configuration read

All keys are `abyss.kdl` keys in `abyss/crates/ec-abyss-config/src/schema.rs`. No DE
component keeps a config file of its own.

| Key | Reader | Reload |
|---|---|---|
| `bar.fold-when-inactive`, `bar.fold-height`, `bar.fold-when-idle`, `bar.idle-seconds`, `bar.fold-duration-ms`, `bar.fold-curve` | hyperion, re-read on `config` | live |
| `bar.position` | hyperion, once, before the surface exists | restart |
| `bar.tray.pinned`, `bar.tray.hidden` | hyperion (read); ec-settings Taskbar pane (read/write) | live |
| `bar.rounding` | hyperion, re-read on `config` | live |
| `decoration.rounding` | hyperion and ec-settings, re-read on `config` | live |
| `decoration.rounding` | toasts, center, launcher, policy viewer, once at startup | next start |
| `misc.terminal-command` | ec-launcher, at startup | next launch |

Policy keys are never asked for over the socket: `get_config {file: "policy"}`
stays closed in `ipc/gate.rs`, which is why the viewer reads the file itself.

## 5. Launch, and trust

| Component | Started by | Trust |
|---|---|---|
| hyperion (add-on) | `ec-hyperion-bar.service` | ordinary client |
| ec-pairing | `ec-pairing.service` | ordinary client |
| ec-toasts | `ec-toasts.service` | ordinary client |
| ec-screensaver | `ec-screensaver.service` | ordinary client |
| ec-launcher | `Super+E` / `Super+R` default binds; hyperion's launcher button | ordinary client |
| ec-center | `Super+N` default bind; `ec-center.desktop` | ordinary client |
| ec-settings | `ec-settings.desktop`; hyperion drawer links | ordinary client |
| ec-policy-viewer | `ec-policy-viewer.desktop` | ordinary client |
| ec-secret-prompt | hyperion (wifi), ec-pairing (bluetooth) | ordinary client, surface classified `secret` |

The four DE units in `packaging/` (`hyperion`, `ec-pairing`, `ec-toasts`,
`ec-screensaver`) install to `/usr/lib/systemd/user/`, are
`PartOf=graphical-session.target`, `Requisite=`/`After=abyss-session.target`,
`Restart=on-failure`, and the PKGBUILD links each into
`abyss-session.target.wants/` (a preset alone never fires for an existing
account). The fifth, `ec-policyd.service`, is TCB and ordered `Before=` the
target instead. Everything else is spawned by name, so `PATH` must carry it:
`/usr/bin` packaged, `target/debug` under `packaging/abyss-dev-session`. Default
binds: `abyss/crates/ec-abyss-config/src/lib.rs`. `.desktop` files: `packaging/applications/`.

**Wallpaper** is a core component (ADR 0068), not yet written:
`ec-wallpaper`, a layer-shell client on `Background`, one surface per
output, started by `ec-wallpaper.service`
(`WantedBy=abyss-session.target`), package `eclipseos-wallpaper`, a dependency
of `eclipseos-meta`. It reads the `wallpaper` node, reloads on `config` and
follows `output` events. Default is a solid `color::BASE`; no image ships
(D-01 §6.3).

**None of these is TCB.** They hold no capability, enforce no policy, and are
refused or obliged by the compositor like any client (ADR 0038). Trusted UI is
compositor-drawn and never a layer-shell client; a DE binary that would need
to be trusted means ADR 0038 was applied too widely. The one elevated
treatment is a *restriction*: `packaging/etc/policy.kdl` ships
`windowrule "sensitivity secret" { app-id "^ec-secret-prompt$"; }`, which
buys capture redaction and no direct scanout. Session actions are authorised
by logind/polkit, not by us.

`ec-secret-prompt` and `ec-pairing` ship in `eclipseos-desktop`,
so neither depends on the taskbar being installed.

## 6. Settings (COMP-17 §3)

`ec-settings` is a client of the COMP-13 §1.4 write API and nothing else:
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

- No `mode "wm" | "de"` key (COMP-17 §2) and no `components {}` slots
  (COMP-17 §2.2). The DE described here is the only profile, equivalent to
  COMP-17 §2.1's Standard with defaults; the tiling defaults are always on.
  The graphical installer (D-07) is what will choose among profiles.
- No terminal of our own. `foot` is the default (`eclipseos-meta` depends on
  it, and the default `Super+Return` bind spawns it) until `cataclysm` exists.
  This is the D-01 §1.3 substitution, said out loud.
- No portal. `xdg-desktop-portal*` is not in `eclipseos-meta`.
- No policy editor; that is COMP-10 §3.9, compositor-drawn, milestone 15.
- No clipboard manager, OSD or desktop icons.
- No in-process widget plugins. Custom taskbar widgets are argv commands run
  off the draw path, or declarative (ADR 0065).
- No add-on code in a host. An add-on is a package plus a manifest that turns
  on small host hooks; its own code runs in its own process (ADR 0066).
- Settings cannot approve a command widget. It shows the disclaimer, marks
  premade widgets and can re-queue a pending prompt; only the compositor-drawn
  prompt approves (ADR 0067).

## 8. Open questions

1. **Desktop icons** (COMP-17 §6.2): in v1 scope or not. Nothing is built.
2. ~~**The `mode` key** (COMP-17 §2): whether it lands, and what the WM profile
   drops from the autostart set above.~~ Decided by ADR 0060: it lands, and the
   autostart set moves to `components {}` (COMP-17 §2.2), which will replace
   §5's fixed `.wants/` links.
3. **Packaging `ec-secret-prompt`** (§5): which package carries it. It
   belongs with hyperion's actions but is reusable by any bar (ADR 0053).
4. **Center from the taskbar.** ADR 0052 says hyperion spawns `ec-center`
   by name; as built it spawns only the launcher, settings and the secret
   prompt. Either the ADR is stale or a button is missing.
