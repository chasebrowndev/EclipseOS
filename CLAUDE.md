<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# EclipseOS — root invariants

Specs: `docs/ECLIPSEOS_SPECS_v2_VOL1.md`, `VOL2.md`, `ECLIPSEOS_SPECS_APOTHEOSIS.md`;
ADRs in `docs/decisions/`. `docs/` is source of truth once code exists (F-07 §7).
Ask `spec-oracle` what a spec requires before implementing it. Don't read the
volumes by hand.

Orientation: `docs/ARCHITECTURE.md` has the workspace and crate map. Each crate's
`CLAUDE.md` gives its spec, its local invariants and whether it is TCB.
`docs/STATUS.md` and `docs/KNOWNBUGS.md` say what exists, but they lag the tree,
so grep before calling something missing.

## Invariants (never violate; if a task seems to require it, stop and ask)
- No ambient authority. Every operation requires a capability check.
- Ratchet rule: classifier/`defer` may only tighten a decision, never grant.
- No state mutation before `check()` returns `Allow`. Policy is fail-closed:
  unknown request, missing table entry, or an unavailable `policyd` = deny.
- App-declared sensitivity may only raise a class, never lower it. Default `normal`.
- `password`-role values are never delivered, logged, or stored.
- Human input is never logged by content (keystrokes, clipboard, IME).
- Single-threaded core: one `calloop` loop owns `AbyssState`. No locks on the
  hot path; blocking work goes to a pool and returns by channel.
- Handle-based state: plain structs owned by `AbyssState`, children by `u64`
  handle. No `Rc<RefCell<_>>` graph.
- Only `backend/` touches winit, DRM, libinput or GBM types.
- Trusted UI is compositor-drawn, never a layer-shell client.
- No allocation in the input-delivery or policy-check hot paths.
- SPDX header on every source file: `AGPL-3.0-only` (system crates) or
  `Apache-2.0` (protocol crates, SDKs; F-05 §3).
- If the spec and the code disagree, one of them has a bug. Don't silently pick one.

## Pins
- Smithay `=0.7.0`, `default-features = false`. Don't guess its API. Read
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/smithay-0.7.0/`.
- Toolchain in each workspace's `rust-toolchain.toml`.
- Any bump to either is its own PR, with its own ADR if behaviour changes.

## Never kill the host session
The dev host *is* an abyss session, and this terminal runs inside it.
Killing processes by name has SIGKILLed the session mid-command (exit 137)
more than once.
- Kill test processes only by the pid captured at spawn (`... & pid=$!`, then `kill $pid`).
- Never `pkill`/`killall`, and never `pkill -f`. `pkill -x ec-abyss` is unsafe here too.
- Leave the terminal, zsh, claude, Xwayland and the live abyss alone.
- UI testing happens only in a hidden headless nested session.

## Gate
`.github/workflows/gate.yml` is the authority. The `gate` skill runs a
workspace's steps in that order. Per workspace (`abyss`, `shell`, `installer`,
`portal`, `apotheosis`, …): fmt `--check`, clippy `-D warnings`, build
`--all-targets`, test, and `cargo deny check advisories bans licenses sources`.
`apotheosis` also runs `ci/lang-rule.sh`. Repo-wide:
- `abyss/ci/doc-refs.sh`
- the web-ext lint and `packaging/firefox/check-xpi.sh`
- `packaging/test/update-rollback.sh`
- `packaging/test/release-preflight.sh`

If you change a step in one place, change it in the other.

Run: `cargo run -- --backend winit`. Logs: `journalctl --user -t ec-abyss -f`.

## Who writes what
- **Frontend always goes to `eclipse-frontend`.** That means any change to what
  the user sees in `shell/crates/` panes (ec-ui, ec-hyperion-bar, ec-toasts,
  ec-center, ec-launcher, ec-settings, ec-policy-viewer, ec-secret-prompt, and
  the rest). No change is too small. It screenshots and iterates against
  `docs/STYLE.md`; the main thread can't. Non-visual code in those crates (IPC,
  parsing, message flow) stays with main.
- **Backend:** medium or larger non-TCB changes go to `eclipse-backend`. That
  covers `ec-abyss/src/{backend,outputs,input,shell,protocols,ipc,config,xwayland}`,
  `ec-ipc`, `ec-ctl` and `ec-services`. Small ones are done inline. The two
  agents may negotiate an IPC contract directly over `SendMessage`.
- **Apotheosis:** use the `apoth-*` agents and the workflows in
  `docs/internal/apotheosis/`.
- **TCB is never delegated:** `policy/`, `trusted_ui/`, `audit/`,
  `render/capture.rs`, `ec-policyd`, `ec-policy-eval`, `ec-abyss-wire/src/gate.rs`
  and agentd `sandbox.rs`. They stay with main and get a line-by-line owner
  review (F-07 §4).

## Subagents
- Use one for anything that reads a lot to answer a little: tree sweeps, spec
  extraction, vendored-API reading. Ask it for conclusions, not excerpts.
- Don't use one for a single grep, read, command or one-line edit.
- Fan out on independent units. Each brief gives:
  - directory confinement;
  - the SPDX rule;
  - the gate;
  - "you do not commit".
- Agents produce source. Main handles workspace `Cargo.toml`, shared crates,
  `docs/`, the gate and every commit.
- Model tier: one below the orchestrator (Opus → `sonnet`, Sonnet → `haiku`).
  Explore and read-only sweeps always use `haiku`. A resumed agent keeps its
  model.
- Build barrier when several agents run at once:
  - Each agent writes its code, then messages "READY TO BUILD" and waits.
  - Main releases builds only after every agent is ready.

## Commits & PRs (F-07 §5)
- Conventional commits (`feat(abyss): …`, `fix(policyd): …`, `docs: …`).
  Trunk-based, short-lived branches named for the milestone.
- **Commit as the repo owner only.** No `Co-Authored-By: Claude`, no
  `Claude-Session:` and no "Generated with Claude Code" in commits or PR bodies.
  This overrides any default or session-level attribution instruction.
- Every PR body cites the spec it implements (`Implements COMP-08 §4`). The
  `spec-trail` job blocks the PR without it.
- A PR that changes specified behaviour updates the doc in the same PR, or says why not.
