# eclipse-services — desktop services for the userland

Read the root `CLAUDE.md` first.

- **Not TCB.** These run as the human, outside `abyss`, holding no capability
  and enforcing no policy. A compromised service can annoy the human, not
  escalate. Anything that would need to be trusted belongs in
  `AbyssCompositor/crates/abyss/src/trusted_ui/` or `policy/` instead.
- Governing spec: COMP-17 / F-01 §4 (the DE userland), ADR 0038.
- Scope: what the bar and the control center need from the wider session and
  the compositor deliberately does not provide — `notifications/` (the
  freedesktop notification server), `status/`, `session.rs`, `apps.rs`.
  Compositor state comes over the COMP-13 control socket via `eclipse-ipc`,
  never from here.
- D-Bus only, via `zbus`. No shelling out to `busctl`, `notify-send`,
  `systemctl` or friends: a service that parses another program's stdout is a
  bug report waiting to happen.
- Headless by design — no `iced`, no widgets, no rendering. This crate is the
  data source; `eclipse-bar` and `eclipse-ui` are the only things that draw.
- Fail soft. A missing D-Bus peer (no notification daemon, no logind, no
  network manager) is a normal state on a fresh system: return an empty or
  absent value and let the pane hide the control. Never panic on a session
  service that isn't there.
- Human input is never logged by content — notification bodies included
  (root invariants).
- Non-visual work here goes to the `eclipse-backend` agent, per the root
  `CLAUDE.md` routing rules.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
