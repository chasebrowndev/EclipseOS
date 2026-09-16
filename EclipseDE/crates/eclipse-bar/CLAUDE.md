# eclipse-bar — the bar, toasts, control centre and launcher

Read the root `CLAUDE.md` first. Governing spec: COMP-17 (DP-5).

- **Not TCB.** Four layer-shell clients (`eclipse-bar`, `eclipse-toasts`,
  `eclipse-center`, `eclipse-launcher`), all ordinary Wayland clients with no
  authority of their own. Anything here that would only be safe if it could be
  trusted belongs in `abyss/src/trusted_ui/` instead — trusted UI is
  compositor-drawn, never a layer-shell client (root invariant).
- **They run under any layer-shell host**, Hyprland included; abyss is not
  required to develop against them. `conn.rs` is fail-soft — `Conn::ensure()`
  leaves `client: None` and retries, so the UI renders with empty live data
  when nothing is listening. That is the screenshot loop's enabling fact, and
  it must stay true.
- **Never fail silently.** `DENIED (-32000)` and the `ConfigError` object
  render the same way here as in `eclipse-settings`.
- **Human input is never logged by content** (root invariant) — the launcher's
  query box included. No tracing of what was typed.
- No literal colour, radius or size: `eclipse_ui::tokens` is the only source,
  and `eclipse_ui::widget` is the composition vocabulary. See
  `docs/STYLE.md` and `docs/COMPOSITION.md` before writing a view.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
