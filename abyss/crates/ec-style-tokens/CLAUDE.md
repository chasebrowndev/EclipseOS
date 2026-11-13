# ec-style-tokens — non-colour STYLE.md tokens

Read the root `CLAUDE.md` first. Governing spec: `docs/STYLE.md`, T-08 §1 (principle 5),
milestone X2 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). **Not TCB.**

- Plain `const`s only: radii, spacing, sizes, durations, per-pane geometry. No iced, no
  smithay, no dependencies. Colours live in `ec-theme`; fonts and the `color`/`font`
  runtime modules stay in `shell/crates/ec-ui/src/tokens.rs`, which re-exports this crate
  (`pub use ec_style_tokens::*`) so `ec_ui::tokens::radius::CARD` etc. keep working.
- A value changes only together with `docs/STYLE.md`. `tests/values.rs` pins every token to
  its pre-move literal; update it with the change.
- Licence AGPL-3.0-only (system code, not a protocol or SDK, F-05 §3), like `ec-theme`.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
