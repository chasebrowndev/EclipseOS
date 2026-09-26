# eclipse-services — the userland's D-Bus services

Read the root `CLAUDE.md` first. Governing spec: ADR 0038 (DE services),
ADR 0051 (screensaver bridge), ADR 0053 (status actions, secret prompt),
ADR 0065 (taskbar widgets: `media`, `audio`, `usage`, `custom`).

- **Not TCB.** Ordinary services running as the human, outside `abyss`. They
  hold no capability and enforce no policy; a compromised service can annoy
  the human, not escalate. Anything that would need to be trusted belongs in
  `abyss/src/trusted_ui/` instead.
- Lib plus one bin. The lib is linked by the panes (`notifications`, `tray`,
  `status`, `session`, `apps`); `src/bin/eclipse-screensaver.rs` owns
  `org.freedesktop.ScreenSaver` and forwards inhibits to the compositor
  (`dist/eclipse-screensaver.service`).
- Deps stay at `zbus` (already in the tree via iced_layershell → mundy, so no
  new supply chain), `eclipse-ipc` and `serde_json`, plus the two ADR 0065
  admits for the taskbar widgets: `pulseaudio` (pure-Rust PulseAudio
  protocol, served by pipewire-pulse; no libclang) and `realfft`. Blocking zbus only: iced
  runs no tokio runtime, so each server owns its own thread and talks to its
  GUI over a channel.
- **`password`-role values** (wifi passphrase, Bluetooth PIN) travel only as
  `status::secret::Secret`: no `Debug`, bytes overwritten on drop, readable
  only crate-private. Never log, store or echo one. The prompt that collects
  them is a separate process (`eclipse-secret-prompt`), reached through the
  session-bus door in `status/agent.rs`.
- `tests/live_bus.rs` is `#[ignore]`d: it spawns a private `dbus-daemon` and
  must never run against the human's session bus.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
