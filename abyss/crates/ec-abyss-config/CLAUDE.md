# ec-abyss-config — abyss.kdl schema, parser, editor

Read the root `CLAUDE.md` first. Governing spec: COMP-13 §1. **Not TCB.**

- Extracted from `ec-abyss/src/config/` so `ec-settings` and `ec-ctl` can
  depend on the schema without linking the compositor. **No smithay, no
  calloop, no `AbyssState`** — adding any of them defeats the crate. Keysyms
  are xkbcommon's (smithay re-exports the same type).
- Holds: `schema.rs` (declarative key table), `lib.rs` (`Config`, parser,
  defaults), `edit.rs` (byte-splice edits), `approvals.rs`, `widget_hash.rs`,
  `catalog.rs` (reader only), plus plain-data types the parser produces:
  `input.rs` (binds, `Action`), `outputs.rs` (`Overscan`, `Step`, mode and
  transform grammar), `trust.rs` (`AppTrust`, `SeatCompat`).
- `ec-abyss` re-exports this crate as `crate::config` and keeps everything
  that needs state: `apply_loaded`, `watch`, the catalog inotify watcher,
  `withhold`. Smithay-dependent geometry (`OverscanGeometry`) and the
  `ModifiersState` -> `Mods` conversion (`input::held_mods`) live there too.
- `tests/config_doc.rs` checks the schema against `docs/CONFIG.md`:
  `UPDATE_CONFIG_DOC=1 cargo test -p ec-abyss-config --test config_doc`.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
