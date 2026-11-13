# ec-config-schema — shared config schema types

Read the root `CLAUDE.md` first. Governing spec: COMP-13 §1; Apotheosis ADR 0113. **Not TCB.**

- Licence **Apache-2.0** (a second product, the terminal, describes its keys with these types).
- Types only, no tables: `Ty`, `Owner`, `Reload`, `Value`, `Dv`, `Key`, `Meta`, plus the pure
  `parse_duration`/`parse_bytes` (and `format_*`) for the textual `Ty::Duration`/`Ty::Bytes` forms.
- The key TABLEs, getters over `Config` and tests that need a parser stay with their product
  (`ec-abyss-config`, which re-exports these types so existing paths are unchanged).
- No dependencies, `#![forbid(unsafe_code)]`.
- Consumers: `ec-abyss-config`, (later) `apoth-config`, and through them `ec-settings`, `ec-ctl`,
  `ec-policy-viewer`.
- Every `.rs` starts with `// SPDX-License-Identifier: Apache-2.0`.
