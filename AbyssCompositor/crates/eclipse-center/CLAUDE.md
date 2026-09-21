# eclipse-center — the control center

Its own crate and package so it installs without the taskbar (ADR 0052).
Never depend on `hyperion`.

Read the root `CLAUDE.md` first. Governing spec: DP-4 (`claude/DESKTOP_PROFILES_PLAN.md`), ADR 0038.

- **Not TCB.** An ordinary layer-shell Wayland client with no authority of
  its own. Anything here that would only be safe if it could be
  trusted belongs in `abyss/src/trusted_ui/` instead — trusted UI is
  compositor-drawn, never a layer-shell client (root invariant).
- **It runs under any layer-shell host**, Hyprland included; abyss is not
  required to develop against it. With nothing listening on the
  bus it renders empty rather than failing — the screenshot loop depends on it.
- **Never fail silently.** `DENIED (-32000)` and the `ConfigError` object
  render the same way here as in `eclipse-settings`.
- **Human input is never logged by content** (root invariant). No tracing of what was typed.
- No literal colour, radius or size: `eclipse_ui::tokens` is the only source,
  and `eclipse_ui::widget` is the composition vocabulary. See
  `docs/STYLE.md` and `docs/COMPOSITION.md` before writing a view.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
