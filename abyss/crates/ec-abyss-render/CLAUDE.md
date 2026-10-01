# ec-abyss-render — state-free render

Read the root `CLAUDE.md` first. Governing spec: COMP-02. **Not TCB.**

- Extracted from `ec-abyss/src/render/`. Everything here takes its inputs as
  parameters: **no `AbyssState`, no calloop, no locks.** Deps: smithay
  (`=0.7.0`), `ec-abyss-config`, `tracing`, `serde_json`.
- **`capture.rs` is not here.** It reads `AbyssState` and is TCB, so it stays
  at `ec-abyss/src/render/capture.rs`, which re-exports this crate
  (`pub use ec_abyss_render::*`) so `crate::render::` paths keep working.
- `output_overscan.rs`: overscan arithmetic (COMP-03 §2), re-exported as
  `ec-abyss::outputs::overscan`.
- `userdata.rs`: per-window/layer data the shell writes and the renderer reads
  (`RuleOpacity`, `RuleBlur`, `TileClip`, `LayerOffset`, `layer_geometry`);
  re-exported from `ec-abyss::shell`.
- `drop::DropGuides`: what the drop guides need of a drag; `ec-abyss::shell`
  implements it for `TileDrag`.
- Source-scanning tests that read `capture.rs` or `backend/*.rs` live in
  `ec-abyss/src/render/scan_tests.rs`, not here.
- No allocation added to hot paths. winit/DRM/libinput/GBM types stay in
  `ec-abyss/src/backend/`.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
