# Fog

The EclipseOS file explorer. The spec is `FOG-SPEC.md` — read §Implementation
guide first; the rest is reference. Standalone cargo workspace (the spec's
`fog/`), sibling of `AbyssCompositor/` and `Oracle-Eyes/`.

## Invariants (never violate)

- `fogd` never runs with privileges. Only `fog-elevate` runs as root, and only for the lifetime of one elevated tab.
- The UI thread never does filesystem I/O or sorting. All of it happens in `fogd`.
- Every mutation goes through the job queue and the undo journal. No direct writes from the UI, CLI or portal.
- Renames use `RENAME_NOREPLACE`. No code path may silently overwrite a file.
- Unknown config keys are errors, and the last valid config stays active.
- Without the `activity` feature, the binary contains no `agentd` client code.
- Agents never reach `fogd.sock`. Agent calls arrive only via `agentd`.

## Crates

- `fog-proto` — IPC message types, versioned framing; no I/O.
- `fog-daemon` — `fogd`: backends, cache, watch, jobs, journal, thumbnails.
- `fog-widgets` — virtual list, glass shader, reusable widgets.
- `fog-ui` — iced app: windows, views, input, animation.
- `fog-bench` — latency and frame-time benchmarks against §Performance model.

## Features

`gio` (remote backends) and `activity` (agent integration) must stay off in
base builds. Base builds compile and pass tests with neither.

## Gate (from `FogFileExplorer/`)

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo build --workspace --no-default-features`, `cargo test --workspace`, `cargo deny check`.
