# Apotheosis — brief for every implementing agent

Bound on every agent in every Apotheosis workflow. The orchestrator pastes nothing
else; read this file first, then the T-section you were given.

## What you are building
A unit of `apotheosis/` to the spec in `docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`. Your task
names a milestone, a T-section and the directories you own. The spec section is the
contract; the feature-matrix rows it cites
(`docs/internal/apotheosis/research/FEATURE-MATRIX.md`, search by id) give the detail.
If the spec is silent or wrong, **stop and report it** — do not pick silently.

## Hard rules
1. **Directory confinement.** Touch only the directories in your task. Workspace
   `Cargo.toml`, `Cargo.lock`, shared crates, `docs/` and anything outside
   `apotheosis/` belong to the orchestrator; say what you need changed.
2. **Language rule (T-00 §3).** Rust only. No C, C++, assembly, build-time bindgen, or a
   dependency whose build script compiles C. New dependencies: name, version, licence
   and why, in your report — the orchestrator adds them after `cargo deny`.
3. **Prior art (T-00 §3.4).** Never open another terminal's source, and never write
   from memory of it. To learn how one solves something, ask the `apoth-prior-art` agent
   and implement from its prose answer. Dev-dependency oracles (`vte`,
   `alacritty_terminal`) are called from tests, not read.
4. **SPDX header on every file**: `// SPDX-License-Identifier: AGPL-3.0-only`, or
   `Apache-2.0` in `apoth-vt`, `apoth-unicode`, `apoth-input`, `apoth-action`,
   `apoth-proto`, `apoth-terminfo`.
5. **No content in logs.** Never log keystrokes, paste content, clipboard, pane text or
   command lines. Log sizes, counts, ids and sequence names.
6. **`unsafe`** only in `apoth-pty`, `apoth-wl` and media-helper FFI modules, each block
   with a `// SAFETY:` comment. Every other crate is `#![forbid(unsafe_code)]`.
7. **No allocation on hot paths** named in the spec (parser scan loop, key → PTY write,
   per-cell render build). Hold a pane's term lock only as long as the spec allows.
8. **You do not commit, push, stash, or touch git state.** Never `git worktree prune`
   or `git gc`.
9. **Tests are part of the unit.** Every behaviour you add has a unit test, a golden
   fixture (T-25 §1) or a property test. A fixture you cannot make pass is reported,
   not deleted or blessed.
10. **Third-party APIs are read, not guessed.** Open the crate's source under
    `~/.cargo/registry/src/` for the pinned version before calling it.

## Building (the build barrier)
- **No cargo command of any kind until the orchestrator says "GO BUILD".** Finish your
  code, report `READY TO BUILD` with a summary, and wait. Builds are serialized across
  agents; the disk is small and the machine is in use.
- When released, always:
  `CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$HOME/.cache/apotheosis-target nice -n 19 cargo <cmd> -j 2 …` (`-j` goes after the subcommand)
  from `apotheosis/`. `touch` the files you changed before building (the target dir is
  shared and mtimes can lie).
- Gate order: `cargo fmt --all --check` → `cargo clippy --workspace --all-targets
  --all-features -- -D warnings` → `cargo build --workspace --all-targets` →
  `cargo test --workspace`. Fix only inside your own directories.

## Running the terminal
- Only in the hidden headless session: start `ec-abyss --backend headless` through
  `apoth-testkit` (T-25 §6), capture its pid, kill **that pid**. Never a visible window,
  never the live session.
- Never `pkill`, `killall` or `kill` by name. Never touch `foot`, `kitty`, `zsh`,
  `claude`, `Xwayland` or the running `ec-abyss` of the host session.

## Glass interface work
Read `docs/STYLE.md` and T-08 first. No gradients, no glow, no halo: focus and selection
are a brighter fill plus a crisp rim. Every control has hover, pressed, focused and
disabled states. Tokens only — no literal colours or radii. Screenshot your own output
headlessly and iterate before reporting.

## Your report
`READY TO BUILD` / `DONE` / `BLOCKED`, then: files changed; spec sections and matrix
rows implemented; tests added; dependencies requested; spec gaps or disagreements found;
anything left undone, plainly.
