# abyss — compositor crate

Read the root `CLAUDE.md` first. This file only adds crate-local rules.

- Smithay is pinned `=0.7.0`. Read the vendored source under
  `~/.cargo/registry/src/*/smithay-0.7.0` before using any API; do not guess.
- One thread, one `AbyssState`, one calloop loop. No `Arc<Mutex<..>>` around state.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
- Layout: `backend/` (winit, drm, headless, gpu), `render/` (`capture.rs`, TCB, plus a
  re-export of `ec-abyss-render`, which holds damage, blur, effects,
  annotations, FB damage-clip sanitising and the overscan arithmetic),
  `outputs/` (layout, EDID,
  calibration, overscan, persistence, DPMS), `input/` (bindings, keyboard,
  pointer, touch, tablet, touchpad swipe gestures, idle, injection), `shell/`
  (layout, workspaces, rules, focus), `protocols/standard/` (one file per
  Wayland protocol handler), `ipc/` (control socket, config RPC; the gate table, `RpcError`, `EVENTS`
  and `Hook`/`HookSet` live in `ec-abyss-wire`, re-exported here),
  `config/` (re-exports `ec-abyss-config`, which holds KDL parse, schema, in-place edit; here: apply, watch, widget catalog watcher, approval withholding), `xwayland/` (XWM,
  security), `state.rs` (globals + seat), `session.rs`. `ec-abyss-config/tests/config_doc.rs`
  checks the config schema against `docs/CONFIG.md`.
- `trusted_ui/` (TCB): the modal prompt primitive (COMP-10 §3.11, §4) and
  its owners: command-widget approval (`approval.rs`, ADR 0067) and the §3.10
  destructive-action prompt (`erase.rs`, asked over the root-only `socket.rs`,
  ADR 0061).
- `policy/` (TCB): per-agent grant admission (`mod.rs`) and the agent scene
  filter (`scene.rs`), the one choke point every agent-facing window path
  goes through. Scopes themselves are evaluated in `policy-eval`.
- `protocols/agent/`: the privileged `ec-agent.sock` (hook-gated, 0600,
  `SO_PEERCRED` uid) and the `eclipse_agent_v1` handlers. Not TCB: it calls
  `policy::Agent` and `policy::scene` and decides nothing itself. M14 adds,
  beside `seat.rs`: `atomic.rs` (batches), `dedupe.rs`, `generation.rs`,
  `lock.rs` (compat locks; `input/` calls its `hold_*` hooks) and `wait.rs`
  (`wait_for`). Their per-agent state is `Agents::aux`, never in `policy::Agent`.
- `audit/` (TCB): provenance emission to `policyd` (COMP-12). Agent records
  stall the agent when the socket is full; human records ring, never wait.
- `protocols/semantic/`: `eclipse_semantic_v1` (COMP-09, public socket). Not TCB. `tree.rs` is the
  state-free model (raise-only classification, budget, clamping); `mod.rs` is the Wayland side and
  the seams the TCB and agent protocol call (`capture_facts`, `read_tree`, `request_action`). It
  consumes the scene filter's class and never decides one. Tests: `tests.rs` (wire + redaction live-tree
  arm), `fuzz.rs` (COMP-15 §3 loops), `refclient.rs` (reference client over `ec-cataclysm-pub`).
- `protocols/protected/`: `eclipse_protected_surface_v1` (COMP-19, public socket). Not TCB. `mod.rs` is the
  registry and the Wayland side, `gate.rs` the input rules (physical only; Enter and the slot rectangle
  belong to the slot), `hooks.rs` the stubs the TCB replaces (`TCB-HOOK`: slot card, arming, commit, audit).
  `input/origin.rs` holds `Origin`, set by the entry point for the duration of the event
  (`with_origin`); untagged reads as `injected`. `is_protected(state, &WlSurface)` is the question capture and
  the agent scene ask. The `wlcs` feature accepts `injected` on protected surfaces and is never a release build.
- Human keystrokes are never logged by content. Log keysym names only behind `trace`.
- Logs go to journald: `journalctl --user -t ec-abyss -o cat --since "5 min ago"`.
- Nested test under the host session (itself abyss): `./target/debug/ec-abyss --backend winit & pid=$!`,
  then point a client (`foot`) at the nested socket it logs. Kill by `$pid`, never
  `pkill -x ec-abyss` (root `CLAUDE.md`). DRM backend must be tested on a real TTY.
- `cargo build` must be warning-free; CI runs clippy with `-D warnings`.
