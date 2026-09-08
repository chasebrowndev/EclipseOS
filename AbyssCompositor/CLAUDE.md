<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# abyss / EclipseOS — root invariants

Governing specs: `ECLIPSEOS_SPECS_v2_VOL1.md` (and Vol 2). `docs/` is source of
truth once code starts (F-07 §7). Read the spec section before implementing it.

## Invariants (never violate; if a task seems to require it, stop and ask)
- No ambient authority. Every operation requires a capability check.
- Ratchet rule: classifier/`defer` may only tighten a decision, never grant.
- No state mutation before `check()` returns `Allow`. Policy is fail-closed:
  unknown request, missing table entry, or an unavailable `policyd` = deny.
- App-declared sensitivity may only raise a class, never lower it. Default
  class is `private`.
- `password`-role values are never delivered, logged, or stored.
- Human input is never logged by content (keystrokes, clipboard, IME).
- **Single-threaded core.** One `calloop` loop owns `AbyssState`. No locks on
  the hot path; blocking work goes to a pool and returns by channel.
- **Handle-based state.** No `Rc<RefCell<_>>` graph — plain structs owned by
  `AbyssState`, children referenced by `u64` handle/index.
- **Backends live behind the backend trait** (`backend/`). Nothing outside it
  may touch winit, DRM, libinput or GBM types directly.
- Trusted UI is compositor-drawn, never a layer-shell client.
- No allocation in the input-delivery or policy-check hot paths.
- SPDX header on **every** source file: `// SPDX-License-Identifier: AGPL-3.0-only`
  (system crates) or `Apache-2.0` (protocol crates, SDKs — F-05 §3).
- Specs and code disagreeing is a bug in one of them — do not silently pick.

## Smithay
Pinned `=0.7.0`, `default-features = false`. **Do not guess its API.** Read the
vendored source:
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/smithay-0.7.0/`
Version bumps are their own PR with its own ADR if behavior changes.

## Killing test processes (read this before any cleanup line)
`kitty` is the terminal hosting the interactive session. **Never `pkill`/`killall`
kitty, zsh, claude, Xwayland or quickshell** — that kills the session you are
running in, mid-command, and looks like a mysterious external SIGKILL (exit 137).
This has already happened more than once.

- Kill only our own binary, by exact name: `pkill -x abyss`. Never `pkill -f`.
- Kill a spawned test client by the pid you captured when you spawned it
  (`kitty ... & pid=$!` then `kill $pid`), never by process name.
- Host pid 2245 is the host's own Xwayland under Hyprland. Leave it alone.

## Build / test / run
```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
cargo deny check advisories bans licenses sources
cargo run -- --backend winit    # nested under Hyprland for dev
journalctl --user -t abyss -f  # logs (tracing → journald)
```
The first five are exactly what CI runs (`.github/workflows/gate.yml`). If you
change one, change both — a local gate that differs from CI is worse than no
local gate.

The toolchain is pinned in `rust-toolchain.toml`; CI and dev must not drift.
Same rule as the Smithay 0.7.0 pin: a bump is its own PR with its own
justification.

## Commits & PRs (F-07 §5)
- Conventional commits: `feat(abyss): …`, `fix(policyd): …`, `docs: …`.
- Trunk-based; short-lived branches named for the milestone
  (`comp16-m03-multi-output`).
- Every PR body cites the spec section it implements: `Implements COMP-08 §4`.
  The `spec-trail` job in `gate.yml` blocks the PR without it.
- A PR that changes specified behavior updates the doc in the same PR, or says
  why not.
- TCB areas (`abyss` enforcement path, `policyd`, `policy-eval`, `sandbox`)
  get a line-by-line owner review. No exceptions.

## Where things are
```
crates/abyss/src/backend/     COMP-01  winit + DRM/udev behind one trait
crates/abyss/src/render/      COMP-02  damage, scanout, sync, redaction
crates/abyss/src/outputs/     COMP-03  hotplug, layout, virtual outputs
crates/abyss/src/input/       COMP-04  seats, focus, injection, override chord
crates/abyss/src/shell/       COMP-05  layouts, workspaces, rules, identity
crates/abyss/src/protocols/standard/  COMP-06
crates/abyss/src/protocols/agent/     COMP-08  eclipse_agent_v1
crates/abyss/src/protocols/semantic/  COMP-09  eclipse_semantic_v1
crates/abyss/src/trusted_ui/  COMP-10  prompts, indicator, emergency panel
crates/abyss/src/policy/      COMP-11  enforcement table, check()
crates/abyss/src/audit/       COMP-12  provenance emission
crates/abyss/src/ipc/         COMP-13  human JSON-RPC socket
crates/abyss/src/config/      COMP-13  KDL parse, validate, hot-reload
crates/abyss/src/xwayland/    COMP-07
decisions/                     ADRs (F-08 format)
docs/ARCHITECTURE.md, docs/BUILDING.md
```
Per-crate `CLAUDE.md` names the governing spec, local invariants, and whether
the crate is TCB.
