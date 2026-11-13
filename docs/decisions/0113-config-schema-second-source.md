# 0113 — `ec-config-schema`: a second product uses the schema types; ec-settings edits the terminal file directly
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0107 puts a config key's presentation beside its schema row in `ec-abyss-config`, and ec-settings reaches Abyss settings through the compositor. Apotheosis needs the same table shape (T-10 §1) for several hundred keys, with new value types, and its config is not the compositor's business.

## Options
1. Put terminal keys into `ec-abyss-config`. One table; couples the compositor's config crate and its reload path to a client application.
2. Apotheosis invents its own schema types; ec-settings special-cases them. Two widget vocabularies.
3. Extract the schema/meta types into a shared pure crate; each product owns its own table; ec-settings links `apoth-config` and edits `apotheosis.kdl` itself.

## Decision
Option 3. New crate `abyss/crates/ec-config-schema` (Apache-2.0) holding `Ty` (existing variants kept; `Color` reused, no `Colour`) and constraints, new variants `Font`, `FontFeatures`, `KeyChord`, `Duration`, `Path`, `Palette16`, `Action`, `Regex`, `Template`, `Bytes`, `Argv`, `CodepointRanges`, `Match`, `StringSet`, `Map`, `ThemeRef`; a generic `Reload {Live, NewPane, NewWindow, Restart}` (Abyss maps Live→Live, NeedsRestart→Restart); and `Dv::Computed`. `Owner` and scope stay per-product. Presentation stays in a per-product paired `meta::META` table, exactly as ADR 0107 Option 2. `ec-abyss-config` re-exports it; X1 updates every exhaustive match on `Ty`/`Reload` (ec-settings search.rs and tests/coverage.rs, ec-abyss ipc/config_rpc.rs, ec-abyss-config tests.rs and tests/config_doc.rs) in the same PR. ec-settings gains a Terminal section generated from `apoth-config` and writes the file with `apoth-config::edit`; the terminal hot-reloads. ADR 0107 is unchanged; its paired-table rule applies to each product's table.

## Consequences
Milestone X1 touches `abyss/` and `shell/` and needs a serialized build slot. `shell/crates/ec-settings/tests/coverage.rs` extends to every Apotheosis key. ec-settings can edit terminal settings with no compositor running.

## Revisit when
A third product needs the table, or the two tables need cross-product validation.
