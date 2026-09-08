# abyss — compositor crate

Read the root `CLAUDE.md` first. This file only adds crate-local rules.

- Smithay is pinned `=0.7.0`. Read the vendored source under
  `~/.cargo/registry/src/*/smithay-0.7.0` before using any API; do not guess.
- One thread, one `AbyssState`, one calloop loop. No `Arc<Mutex<..>>` around state.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
- Layout: `backend/` (winit, drm), `input/` (bindings + event routing),
  `protocols/standard/` (one file per Wayland protocol handler), `shell/` (window
  management), `state.rs` (globals + seat). Agent protocol will live in
  `protocols/agent/` later; don't put it elsewhere.
- Human keystrokes are never logged by content. Log keysym names only behind `trace`.
- Logs go to journald: `journalctl --user -t abyss -o cat --since "5 min ago"`.
- Nested test under Hyprland: `WAYLAND_DISPLAY=wayland-1 ./target/debug/abyss --backend winit`,
  then `WAYLAND_DISPLAY=wayland-2 kitty`. DRM backend must be tested on a real TTY.
- `cargo build` must be warning-free; CI runs clippy with `-D warnings`.
