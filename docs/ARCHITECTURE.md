# abyss — architecture

`abyss` is the EclipseOS Wayland compositor and one half of the trusted
computing base (the other is `policyd`). It is a Rust compositor built on
Smithay `=0.7.0` used as a library, not a fork of an existing compositor
(ADR 0002).

Authoritative specs: `C-00 COMPOSITOR` and `COMP-01`..`COMP-16` in the spec
bundle. Where this document and a spec disagree, the spec wins and this
document is the bug.

## Workspace layout (F-07 §1)

```
Cargo.toml            workspace
CLAUDE.md             root invariants + commands
docs/                 specs and this document
decisions/            ADRs (F-08 format)
crates/
  abyss/             compositor                       [TCB]
  eclipse-ctl/        human CLI over the COMP-13 IPC socket
  wlcs-abyss/         wlcs conformance cdylib, drives the headless backend
  policyd/            policy + audit daemon            [TCB]   (not yet)
  policy-eval/        shared evaluator, linked by both [TCB]   (not yet)
  agentd/             agent gateway                            (not yet)
  registryd/          perception aggregation                   (not yet)
  proto-agent/        eclipse_agent_v1 bindings (generated)    (not yet)
  proto-semantic/     eclipse_semantic_v1 bindings (generated) (not yet)
  audit/              audit store, hash chain, verify tool     (not yet)
  sandbox/            grant → bwrap/Landlock/seccomp compiler  (not yet)
  sdk-rust/ sdk-python/  agent SDKs (Apache-2.0)               (not yet)
tests/{golden,wlcs,compat,redteam}/
fuzz/                 cargo-fuzz targets
bench/                COMP-14 benchmarks
```

Only `crates/abyss`, `crates/eclipse-ctl` and `crates/wlcs-abyss` exist today
(see `docs/STATUS.md`). TCB crates get a line-by-line owner review
on every change (F-07 §4).

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
`headless.rs` (COMP-01 §10) for wlcs and tests, with no display or input hardware at all. Nothing outside this module names a backend type. |
| `render/` | COMP-02 | Damage tracking, composition, direct scanout, explicit sync, fractional scaling, capture redaction, effects. |
| `outputs/` | COMP-03 | Output discovery and hotplug, layout, persistence, virtual outputs for agent workspaces, DPMS/power. |
| `input/` | COMP-04 | Seats (one human, one per agent), libinput plumbing, xkb, focus arbitration, agent injection, atomic batches, the reserved override chord, keybindings. |
| `shell/` | COMP-05 | Window management: dwindle/master layouts, workspaces, window rules, app identity, launch and cgroup principal mapping. |
| `protocols/standard/` | COMP-06 | `wl_compositor`, `wl_shm`, `xdg_shell`, seat, selection/clipboard, layer-shell, dmabuf, session lock, idle, and the rest of the support matrix. |
| `protocols/agent/` | COMP-08 | `eclipse_agent_v1` server side, on the privileged socket only. Every request crosses `policy::check()` first. |
| `protocols/semantic/` | COMP-09 | `eclipse_semantic_v1` server side: semantic tree publication and change notification. |
| `trusted_ui/` | COMP-10 | Compositor-drawn consent prompts, agent-activity indicator, emergency panel. Never a client (ADR 0009). |
| `policy/` | COMP-11 | The compiled enforcement table and `check()`. Fail-closed; no state mutation before `Allow`; `defer` may only tighten. |
| `audit/` | COMP-12 | Audit and provenance event emission. Never records human input by content. |
| `ipc/` | COMP-13 | Human JSON-RPC socket — the bar, the launcher, `eclipse-ctl`. Unprivileged, human-principal only. |
| `config/` | COMP-13 | KDL parse, validate, hot-reload (ADR 0016). A bad config never takes down a live session. |
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
