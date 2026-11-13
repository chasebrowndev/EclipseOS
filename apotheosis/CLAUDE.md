# apotheosis — the EclipseOS terminal

Governing spec: `docs/ECLIPSEOS_SPECS_APOTHEOSIS.md` (T-00..T-25). Read T-00 first,
then the section your crate names in its `lib.rs` header. Requirements annex:
`docs/internal/apotheosis/research/FEATURE-MATRIX.md` (cite rows by id). Agents:
`docs/internal/apotheosis/BRIEF.md` binds you.

Not TCB. It does change TCB-adjacent things elsewhere (policy data, the P-04 credential
signal); those land as separate owner-reviewed PRs, never from this workspace.

## Local invariants
- **Language rule (T-00 §3.1).** Rust only; no C-family source; no build-time C or
  bindgen. `ci/lang-rule.sh` enforces it and runs in the gate.
- **Prior art (T-00 §3.4).** Never open another terminal's source. Ask the
  `apoth-prior-art` agent; implement from its prose.
- **Layering (T-00 §4.1).** Leaf and core crates (`apoth-vt`, `-unicode`, `-input`,
  `-action`, `-proto`, `-terminfo`, `-grid`, `-term`, `-mux`, `-config`) never depend on
  wgpu, wayland or any presentation crate. `apoth-term::feed` is deterministic.
- **unsafe** only in `apoth-pty`, `apoth-wl`, `apoth-media` (helper FFI) and the one
  surface-creation site in `apoth-render`; each crate root is `#![deny(unsafe_code)]`
  and each block carries `#[allow(unsafe_code)]` + `// SAFETY:`. Every other crate is
  `#![forbid(unsafe_code)]`.
- **No content in logs.** Never log keystrokes, paste, clipboard, pane text or command
  lines.
- **SPDX** on every file: Apache-2.0 for `apoth-vt`, `-unicode`, `-input`, `-action`,
  `-proto`, `-terminfo`; AGPL-3.0-only for everything else.
- Every config key is one row of the `apoth-config` schema table and has a GUI control
  (T-00 §3.7).

## Gate (same as CI's `apotheosis` job)
```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
cargo deny check advisories bans licenses sources
ci/lang-rule.sh
```
Agents build with
`CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$HOME/.cache/apotheosis-target nice -n 19 cargo <cmd> -j 2`
and only after the orchestrator releases the build barrier.
