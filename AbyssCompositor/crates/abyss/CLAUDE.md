# abyss — compositor crate

Read the root `CLAUDE.md` first. This file only adds crate-local rules.

- Smithay is pinned `=0.7.0`. Read the vendored source under
  `~/.cargo/registry/src/*/smithay-0.7.0` before using any API; do not guess.
- One thread, one `AbyssState`, one calloop loop. No `Arc<Mutex<..>>` around state.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
- Layout: `backend/` (winit, drm, headless, gpu), `render/` (damage, blur,
  effects, annotations, FB damage-clip sanitising), `outputs/` (layout, EDID,
  calibration, overscan, persistence, DPMS), `input/` (bindings, keyboard,
  pointer, touch, tablet, touchpad swipe gestures, idle, injection), `shell/`
  (layout, workspaces, rules, focus), `protocols/standard/` (one file per
  Wayland protocol handler), `ipc/` (control socket, gate table, config RPC),
  `config/` (KDL parse, schema, watch, in-place edit), `xwayland/` (XWM,
  security), `state.rs` (globals + seat), `session.rs`. `tests/config_doc.rs`
  checks the config schema against `docs/CONFIG.md`.
- Not yet present: `protocols/agent/`, `protocols/semantic/`, `trusted_ui/`,
  `policy/`, `audit/`. When they land they go there, not elsewhere.
- Human keystrokes are never logged by content. Log keysym names only behind `trace`.
- Logs go to journald: `journalctl --user -t abyss -o cat --since "5 min ago"`.
- Nested test under the host session (itself abyss): `./target/debug/abyss --backend winit & pid=$!`,
  then point a client (`foot`) at the nested socket it logs. Kill by `$pid`, never
  `pkill -x abyss` (root `CLAUDE.md`). DRM backend must be tested on a real TTY.
- `cargo build` must be warning-free; CI runs clippy with `-D warnings`.
