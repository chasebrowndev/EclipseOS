# abyss — architecture

`abyss` is the EclipseOS Wayland compositor and one half of the trusted
computing base (the other is `policyd`). It is a Rust compositor built on
Smithay `=0.7.0` used as a library, not a fork of an existing compositor
(ADR 0002).

Authoritative specs: `C-00 COMPOSITOR` and `COMP-01`..`COMP-16` in the spec
bundle. Once code exists, `docs/` and the code are the source of truth
(F-07 §7); where they and a spec disagree, amend the spec (Appendix B, C).

## Workspace layout (F-07 §1)

Several independent cargo workspaces, each with its own lockfile and CI job
(`.github/workflows/gate.yml`). Every crate has a `CLAUDE.md` naming its
governing spec, local invariants and whether it is TCB, and a one-line
`description` in its `Cargo.toml`. `[TCB]` crates and paths get a line-by-line
owner review on every change (F-07 §4).

```
abyss/          compositor + system daemons (AGPL; protocol crates Apache-2.0)
  crates/ec-abyss/            the compositor; module map below
  crates/ec-abyss-render/     state-free render: damage, scanout, sync, blur, effects, annotations
  crates/ec-abyss-wire/       state-free control-socket types; gate.rs is authz enforcement [TCB]
  crates/ec-abyss-config/     abyss.kdl schema, parse, validate, edit (no smithay)
  crates/ec-config-schema/    schema types shared with Apotheosis (Ty, Key, Meta, Value)
  crates/ec-theme/            seed colours → derived palette, contrast checks (ADR 0084)
  crates/ec-ipc/ ec-ctl/      control-socket client and CLI
  crates/ec-protocols/        eclipse_agent_v1 XML + bindings (Apache-2.0)
  crates/ec-policyd/ ec-policy-eval/   policy daemon + shared evaluator [TCB]
  crates/ec-agentd/           agent gateway; sandbox.rs [TCB]
  crates/ec-audit/            verify, trace, query the audit store
  crates/ec-brokerd/ ec-secret/        secrets broker + owner CLI
  crates/ec-inferenced/ ec-inference-wire/   inference router + wire contract
  crates/ec-claude-agent/ ec-ref-agent/      agents
  crates/ec-cataclysm-pub/    terminal grid publisher (P-04)
  crates/ec-abyss-wlcs/       WLCS conformance shim
  bench/ ci/                  frame-time harness; wlcs skip list, doc-refs, GUI-coverage exceptions
shell/crates/   DE panes and services (iced via ec-ui; none is TCB)
  ec-ui                       shared theme, tokens, widgets
  ec-hyperion-bar ec-toasts ec-center ec-launcher ec-settings ec-policy-viewer
  ec-secret-prompt ec-lock ec-desktop ec-wallpaper ec-cheatsheet ec-console ec-greeter
  ec-services                 D-Bus: notifications, tray, status, screensaver, portal
apotheosis/     the terminal (docs/ECLIPSEOS_SPECS_APOTHEOSIS.md)
portal/ installer/ fog/ oracle-eyes/   own workspaces, own CLAUDE.md
browser/        firefox-glass extension
packaging/      session, units, PKGBUILD, repo, ISO, /etc defaults
docs/           specs, this file, STATUS, KNOWNBUGS, CONFIG, STYLE, design/, decisions/ (ADRs)
```

## Licensing split (F-05 §3, ADR 0005)

System crates are **AGPL-3.0-only**. Protocol crates and SDKs — anything an
external agent links against — are **Apache-2.0**. Every source file carries an
SPDX header naming which.

## Core model (COMP-01 §3, ADR 0018)

- One `calloop` event loop owns `AbyssState`. No locks on the hot path.
- State is a tree of plain structs; children are referenced by `u64` handle or
  index, never by pointer. No `Rc<RefCell<_>>` graph.
- Rendering runs on the loop thread: every output's composite, blur and capture
  happen inside the `calloop` loop that owns `AbyssState`. There is no render
  thread; ADR 0105 sets the measured threshold at which one would be considered.
  Blocking work runs on dedicated threads that talk back to the loop over
  channels (screenshot PNG encode, clipboard serving, the brokerd prompt socket,
  ordered session spawns, approval writes). There is no shared pool.
- Frame scheduling is per-output, driven by page-flip completion. The loop
  never busy-waits. Input and client requests only mark outputs dirty; once per
  dispatch round, after clients are flushed, each dirty idle output renders at
  `next_vblank - render_estimate` (COMP-02 §2; the estimate is an EWMA of that
  output's recent render time, backed off when a flip lands late). DRM backend
  only: winit and headless render from their own frame callback or timer.
- No allocation in input delivery or the policy check.
- Failure model (ADR 0102): a panic ends the process, unwinding so the DRM
  session and VT are restored, except inside a *contained* feature (today the
  overview, the annotation socket methods and the read-only socket queries).
  A panic raised in such a feature's own code turns that feature off for the
  session, resets it to closed/empty, logs its location once and emits
  `feature-disabled`; the session carries on. The TCB, the session lock,
  input delivery and the frame path are never contained. The panic hook logs
  location only, never the payload.

## Module map — `abyss/crates/ec-abyss/src/`

| Module | Spec | Responsibility |
|---|---|---|
| `backend/` | COMP-01 | Session, devices, presentation, behind one trait. `winit.rs` for nested dev; `drm.rs` (DRM/udev/libinput) for real sessions;
`headless.rs` (COMP-01 §10) for wlcs and tests, with no display or input hardware at all; `gpu.rs` ranks DRM devices deterministically (COMP-01 §4). Nothing outside this module names a backend type — except `input::configure_device`, which takes a libinput `Device` (drm feature only) and is called from `drm.rs`. |
| `render/` (+ `ec-abyss-render`) | COMP-02 | Everything but `capture.rs` lives in the state-free `abyss/crates/ec-abyss-render` and is re-exported as `render::`; `capture.rs` (TCB, reads `AbyssState`) stays here. Damage tracking, composition, direct scanout, explicit sync, fractional scaling, capture redaction, blur and effects, FB damage-clip sanitising (`sanitize.rs`), COMP-18 annotations and the region selector. |
| `outputs/` | COMP-03 | Output discovery and hotplug, layout, EDID, overscan calibration, persistence, DPMS/power. Virtual outputs for agent workspaces are not yet (M24). |
| `input/` | COMP-04 | The human seat: xkb, keybindings, pointer, touch, tablet, touchpad swipe gestures, move/resize grabs, idle, and synthetic injection (`inject.rs`, used by wlcs and IPC). The override and attention chords parse and dispatch but are stubs until agent seats exist (M11); agent seats and atomic batches are not yet. |
| `shell/` | COMP-05 | Window management: radiant/dwindle/master layouts, workspaces, window rules (matching on app id, title, cgroup), focus. |
| `protocols/standard/` | COMP-06 | `wl_compositor`, `wl_shm`, `xdg_shell`, seat, selection/clipboard, layer-shell, dmabuf, session lock, idle, and the rest of the support matrix. |
| `protocols/agent/` (M11 subset) | COMP-08 | `eclipse_agent_v1` server side, on the privileged socket `$XDG_RUNTIME_DIR/eclipse/ec-agent.sock` only (0600, `SO_PEERCRED` uid, `agents` hook on). So far `create_agent`/`add_grant` and the read-only `list_toplevels`, `get_toplevel`, `hit_test`; every window goes through `policy::scene`. Acting requests will cross `policy::check()` first. |
| `protocols/semantic/` (not yet) | COMP-09 | `eclipse_semantic_v1` server side: semantic tree publication and change notification. |
| `trusted_ui/` (destructive-action prompt only) | COMP-10 | Compositor-drawn consent prompts, agent-activity indicator, emergency panel. Never a client (ADR 0009). So far only the destructive-system-action confirmation (§3.10, ADR 0061) and its root-only `trusted.sock`; the rest is not yet. |
| `policy/` (not yet) | COMP-11 | The compiled enforcement table and `check()`; today a stub in `state.rs`. Fail-closed; no state mutation before `Allow`; `defer` may only tighten. |
| `audit/` (not yet) | COMP-12 | Audit and provenance event emission. Never records human input by content. |
| `ipc/` | COMP-13 | Human JSON-RPC socket (gate table in `ec-abyss-wire`) — the taskbar, the launcher, settings, `ec-ctl`. Unprivileged, human-principal only. |
| `config/` | COMP-13 | KDL parse, validate, hot-reload (ADR 0016), in-place edit for `set_config_value`. A bad config never takes down a live session. |
| `xwayland/` | COMP-07 | X11 client support, window identity mapping, scaling. |
| `state.rs` | COMP-01 | `AbyssState` itself: the single owner of everything above. |

## Trust boundaries

- The **public** Wayland socket serves ordinary clients. It never carries
  `eclipse_agent_v1`.
- The **privileged** socket serves `agentd` only, and is where the agent and
  semantic protocols live.
- `policyd` is consulted over IPC only on the explicitly-marked `defer` path;
  every other check is a table lookup in-process (ADR 0008).
- `registryd` is outside the TCB and never receives `secret`-class surfaces.

## Data flow of an agent action

```
agent → agentd → privileged socket → protocols/agent
      → policy::check()  ──allow──→ input/ or shell/ or render/
                         ──prompt─→ trusted_ui/ → human → (allow|deny)
                         ──defer──→ policyd (tighten-only) → (deny|prompt|allow-as-tabled)
                         ──deny───→ error to agent
      → audit/ (every outcome, with provenance)
```

## Progress feed (`ec-ctl progress`, `ec-services` progress widget)

Not a compositor concept, so it is not on the abyss control socket and not in
the gate table. `ec-services` serves a per-user feed at
`$XDG_RUNTIME_DIR/eclipse/progress.sock` (0600 in a 0700 directory, peer uid
checked; `ECLIPSE_SOCKET_DIR` overrides the directory). The wire contract is
`ec_ipc::progress`; the commands are:

```
ec-ctl progress start ID [--label L] [--total BYTES]
ec-ctl progress set ID 0.4|40%|BYTES    a decimal or % is a fraction; a whole number is bytes and needs --total
ec-ctl progress done ID
ec-ctl progress list
```

Ids are `[A-Za-z0-9._:-]`, up to 64 bytes. A feed job that is not updated for 60 s
expires. Alongside the feed, the service scans `/proc` for `cargo`, `rustc`,
`pacman`, `cp`, `mv`, `rsync`, `curl`, `wget` and `tar` owned by the session user
(every second while something runs, every five seconds while nothing does). A
scanned copy gets a fraction from its bytes written against the source size;
other tools are indeterminate. A scanned job whose label matches a feed job is
hidden. The service runs inside whichever process calls
`ec_services::progress::spawn`: the taskbar, which starts it at its first
configure and keeps it for its whole life (ADR 0065, 2026-11-03 amendment). With
no taskbar running the feed is unreachable and `ec-ctl progress` says so.
Scanning stays on; `Handle::set_scanning` remains for a host that wants it off.

## System update and power (`ec_services::updates`, ADR 0085)

The taskbar's power widget offers "Update & reboot" / "Update & power off" only
while an Eclipse package is pending (`checkupdates`, every 30 min and on
`refresh`). Running it is always a full upgrade (`pacman -Syu`) of every pending
package; the count shown is all of them. It runs through `pkexec` on polkit action
`org.eclipse.update.apply`, whose helper `/usr/lib/eclipse/ec-update-apply`
accepts exactly `all`. Progress reports as feed job `eclipse-update`. Only pacman
exit 0 leads to logind's Reboot or PowerOff; any other outcome leaves the machine
running. Without `checkupdates` the item is never offered (ADR 0085).

Every upgrade of `/usr/bin/ec-abyss`, from the widget or a hand-typed pacman,
first runs the `eclipseos-abyss-keep.hook` PreTransaction hook
(`/usr/lib/eclipse/ec-abyss-keep`, `AbortOnFail`): the build running as a
session is copied to `/usr/lib/eclipse/previous/`. `ec-abyss-launch` starts
that copy when the greeter's compositor dies within 10 s of starting, and the
"Abyss (previous)" session entry at the login screen picks it by hand (ADR 0101). Kernel, driver and library breakage are not covered.
