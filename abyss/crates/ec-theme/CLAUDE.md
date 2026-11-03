# ec-theme — seed colours to palette

Read the root `CLAUDE.md` first. Governing spec: ADR 0084 (themes), `docs/STYLE.md`
"Colour" and "Light". **Not TCB.**

- State-free and dependency-free: no smithay, no iced, no serde. Linked by the DE
  (`ec-ui` converts `Rgba` to `iced::Color` at its boundary), the compositor and ec-ctl.
- `derive(&DARK_SEEDS)` must reproduce the historical dark tokens bit-for-bit
  (`dark_golden` test). Change a dark number only together with that table and STYLE.md.
- Light alphas are STYLE.md "Light" starting points, tuned in the per-pane visual pass.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
