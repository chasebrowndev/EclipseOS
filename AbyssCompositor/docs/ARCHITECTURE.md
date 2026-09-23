# abyss — architecture

`abyss` is the EclipseOS Wayland compositor and one half of the trusted
computing base (the other is `policyd`). It is a Rust compositor built on
Smithay `=0.7.0` used as a library, not a fork of an existing compositor
(ADR 0002).

Authoritative specs: `C-00 COMPOSITOR` and `COMP-01`..`COMP-16` in the spec
bundle. Once code exists, `docs/` and the code are the source of truth
(F-07 §7); where they and a spec disagree, amend the spec (Appendix B, C).

## Workspace layout (F-07 §1)

```
Cargo.toml            workspace
CLAUDE.md             root invariants + commands
docs/                 specs and this document
decisions/            ADRs (F-08 format)
crates/
  abyss/             compositor                       [TCB]
  eclipse-ctl/        human CLI over the COMP-13 IPC socket
  eclipse-ipc/        blocking client for the COMP-13 §2 socket, no async runtime
  eclipse-ui/         DE design system: tokens, type, widgets over iced 0.14
  hyperion/           the taskbar (internal name; users see "taskbar") (DP-4)
  eclipse-toasts/     notification toasts, layer-shell (DP-5)
  eclipse-center/     notification + status center, layer-shell (DP-5)
  eclipse-launcher/   application launcher, layer-shell (DP-5)
  eclipse-settings/   settings panes; every write via set_config_value (COMP-17 §3)
  eclipse-policy-viewer/  read-only policy.kdl inspector, deliberately powerless
  eclipse-secret-prompt/  one-shot wifi passphrase / Bluetooth PIN prompt (ADR 0053)
  eclipse-services/   D-Bus services: notifications, tray, status, session (ADR 0038);
                      lib plus the eclipse-screensaver bin (ADR 0051)
  wlcs-abyss/         wlcs conformance cdylib, drives the headless backend
  policyd/            policy + audit daemon            [TCB]
  policy-eval/        shared evaluator, linked by both [TCB]
  agentd/             agent gateway                            (not yet)
  registryd/          perception aggregation                   (not yet)
  proto-agent/        eclipse_agent_v1 bindings (generated)    (not yet)
  proto-semantic/     eclipse_semantic_v1 bindings (generated) (not yet)
  audit/              verify tool + retention; the store itself
                      lives in policyd/src/audit.rs (ADR 0046)  (not yet)
  sandbox/            grant → bwrap/Landlock/seccomp compiler  (not yet)
  sdk-rust/ sdk-python/  agent SDKs (Apache-2.0)               (not yet)
tests/{golden,wlcs,compat,redteam}/                            (not yet)
fuzz/                 cargo-fuzz targets                       (not yet)
bench/                COMP-14 benchmarks: the M9f frame-time harness
ci/                   wlcs skip list, GUI-coverage exceptions
dist/                 session scripts, user units, PKGBUILD, repo, ISO
```

The fifteen crates above without a `(not yet)` marker exist today (plus
`bench/`, the sixteenth workspace member); everything marked `(not yet)` is
Phase 2 or later (see `docs/STATUS.md`). All of them are `AGPL-3.0-only` via
`license.workspace = true` — the Apache-2.0 half of the F-05 §3 split has no
code yet. None of the DE crates (`eclipse-ui` through `eclipse-services`) is
TCB. TCB crates
get a line-by-line owner review on every change (F-07 §4).

The DE crates are members of this one workspace, not a separate tree; an
`EclipseDE/` directory is not part of the layout.

## Licensing split (F-05 §3, ADR 0005)

System crates are **AGPL-3.0-only**. Protocol crates and SDKs — anything an
external agent links against — are **Apache-2.0**. Every source file carries an
SPDX header naming which.

## Core model (COMP-01 §3, ADR 0018)

- One `calloop` event loop owns `AbyssState`. No locks on the hot path.
- State is a tree of plain structs; children are referenced by `u64` handle or
  index, never by pointer. No `Rc<RefCell<_>>` graph.
- Concurrency only where it pays: a render thread per GPU, and blocking work
  (config parse, screenshot encode, audit serialization) on a small pool that
  talks back to the loop over channels.
- Frame scheduling is per-output, driven by page-flip completion. The loop
  never busy-waits.
- No allocation in input delivery or the policy check.

## Module map — `crates/abyss/src/`

| Module | Spec | Responsibility |
|---|---|---|
| `backend/` | COMP-01 | Session, devices, presentation, behind one trait. `winit.rs` for nested dev; `drm.rs` (DRM/udev/libinput) for real sessions;
`headless.rs` (COMP-01 §10) for wlcs and tests, with no display or input hardware at all; `gpu.rs` ranks DRM devices deterministically (COMP-01 §4). Nothing outside this module names a backend type — except `input::configure_device`, which takes a libinput `Device` (drm feature only) and is called from `drm.rs`. |
| `render/` | COMP-02 | Damage tracking, composition, direct scanout, explicit sync, fractional scaling, capture redaction, blur and effects, FB damage-clip sanitising (`sanitize.rs`), COMP-18 annotations and the region selector. |
| `outputs/` | COMP-03 | Output discovery and hotplug, layout, EDID, overscan calibration, persistence, DPMS/power. Virtual outputs for agent workspaces are not yet (M24). |
| `input/` | COMP-04 | The human seat: xkb, keybindings, pointer, touch, tablet, touchpad swipe gestures, move/resize grabs, idle, and synthetic injection (`inject.rs`, used by wlcs and IPC). The override and attention chords parse and dispatch but are stubs until agent seats exist (M11); agent seats and atomic batches are not yet. |
| `shell/` | COMP-05 | Window management: dwindle/master layouts, workspaces, window rules (matching on app id, title, cgroup), focus. |
| `protocols/standard/` | COMP-06 | `wl_compositor`, `wl_shm`, `xdg_shell`, seat, selection/clipboard, layer-shell, dmabuf, session lock, idle, and the rest of the support matrix. |
| `protocols/agent/` (not yet) | COMP-08 | `eclipse_agent_v1` server side, on the privileged socket only. Every request crosses `policy::check()` first. |
| `protocols/semantic/` (not yet) | COMP-09 | `eclipse_semantic_v1` server side: semantic tree publication and change notification. |
| `trusted_ui/` (not yet) | COMP-10 | Compositor-drawn consent prompts, agent-activity indicator, emergency panel. Never a client (ADR 0009). |
| `policy/` (not yet) | COMP-11 | The compiled enforcement table and `check()`; today a stub in `state.rs`. Fail-closed; no state mutation before `Allow`; `defer` may only tighten. |
| `audit/` (not yet) | COMP-12 | Audit and provenance event emission. Never records human input by content. |
| `ipc/` | COMP-13 | Human JSON-RPC socket and its gate table — the taskbar, the launcher, settings, `eclipse-ctl`. Unprivileged, human-principal only. |
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
