# apoth-config

Governing spec: T-10 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). AGPL-3.0-only. Pure: no I/O
outside the `Fs` trait (`RealFs` / `MemFs`), no logging of values.

## Layout
- `rows.rs`: the one `rows!` table that generates `TABLE` and `META` in the same order.
- `types.rs`, `schema.rs`, `meta.rs`: key types, lookups, presentation rows.
- `load.rs` + `collections.rs`: layers 1-8, includes (depth cap 8, cycles), tolerant parse.
- `edit.rs`: lossless `Editor` (kdl 6), write-target rule, atomic write, `SelfWrites`.
- `diff.rs`: key and collection diff, strictest reload class.
- `overrides.rs`: `-o` parsing and layer 8, gated by override class (fail-closed).

## X1 follow-up
`Ty::Argv`, `CodepointRanges`, `Match`, `StringSet`, `Map`, `ThemeRef`, `Reload::NewPane`,
`Reload::NewWindow` and `Dv::Computed` are defined locally in `types.rs` because
`ec-config-schema` lacks them. Move them upstream (abyss/, owner-reviewed) and delete the
local copies; `Ty::generic` / `Reload::generic` are the bridge until then.

## Left for integration
- Binds, rules, profiles are parsed to local decls (`model.rs`); resolve `ActionCall` against
  the `apoth-action` registry and match/template checks in `lang.rs` still structural. Chord and argv
  validation already delegate to `apoth-action` (`chord.rs`, `lang.rs`).
- inotify watcher and importers (M15); `DEBOUNCE_MS` / `POLL_FALLBACK_MS` are exported.
- `Dv::Computed` defaults evaluate to `Value::Null` ("Automatic"); the consumer computes them.

## Limits and spec notes
- Bounds: file 1 MiB regular files only, 256 file loads, `{` depth 64 (pre-parse scan), action
  nesting 32, 500 diagnostics, 4096 binds, 1024 rules, regex 4 KiB compiled by `regex-automata`
  (the runtime engine), chords/argv parsed by `apoth-action`.
- `remote.clients` keeps the spec's own dotted path, so `remote` is a top-level node; T-10 §3's
  node list and §1.1's security list do not name it (spec gap, reported).
- Deferred: watcher tests (M15), fuzz target `config_parse`, node-coverage and doc-drift checks.
