<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# EclipseOS — root invariants

This directory is the root of everything: the specs, the ADRs, `docs/`, and
three cargo workspaces (ADR 0042).

```
AbyssCompositor/   the compositor and the socket contract
                   crates: abyss, eclipse-ipc, eclipse-ctl, wlcs-abyss
EclipseDE/         the desktop userland (COMP-17, F-01 §4)
                   crates: eclipse-ui, eclipse-bar, eclipse-settings,
                           eclipse-policy-viewer, eclipse-services
Oracle-Eyes/       the out-of-process vision addon (ADR 0041)
```

`rust-toolchain.toml` and `rustfmt.toml` sit here once; cargo and rustfmt both
walk ancestors, so all three workspaces get the same pin. Each workspace has
its own `Cargo.lock` and its own `deny.toml` — the DE pulls iced/wgpu, which
has no business in the compositor's supply-chain surface.

Governing specs: `ECLIPSEOS_SPECS_v2_VOL1.md` (and Vol 2). `docs/` is source of
truth once code starts (F-07 §7). Read the spec section before implementing it.

Don't read them by hand to answer one question — that is what the `spec-oracle`
agent is for. See "Working style" below.

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
The gate runs **per workspace** — `cd` into one first, or loop:

```
for w in AbyssCompositor EclipseDE Oracle-Eyes; do (cd $w \
  && cargo fmt --all --check \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo build --workspace --all-targets \
  && cargo test --workspace \
  && cargo deny check advisories bans licenses sources) || echo "FAIL $w"; done

cargo run --manifest-path AbyssCompositor/Cargo.toml -- --backend winit
journalctl --user -t abyss -f  # logs (tracing → journald)
```
Those five commands are exactly what CI runs (`.github/workflows/gate.yml`,
one job per workspace). If you change one, change both — a local gate that differs from CI is worse than no
local gate.

The toolchain is pinned in `rust-toolchain.toml`; CI and dev must not drift.
Same rule as the Smithay 0.7.0 pin: a bump is its own PR with its own
justification.

## Frontend is the frontend agent's job — always

**The main agent never writes frontend.** Every single piece of user-facing UI
— a new view, a redesign, a tweak to an existing one, anywhere under
`EclipseDE/crates/{eclipse-ui,eclipse-bar,eclipse-settings,eclipse-policy-viewer}/` — goes to the `eclipse-frontend` agent. It is
the only thing here that produces usable frontend: it screenshots its own
output and iterates against `docs/STYLE.md`, which is exactly the loop the main
thread cannot run.

This is not a guideline about effort or size. There is no change small enough
to exempt: "just move this label" is a frontend change and it gets the agent.
The main thread's role stays what it is elsewhere — brief the agent, wire up
the result, run the gate, write the commit.

Non-visual code in those same crates (IPC plumbing, model parsing, message
flow) is still the main thread's. The line is whether it changes what the user
sees.

## Backend crate work goes to the backend agent — same rule, mirrored

Non-TCB work under `AbyssCompositor/crates/abyss/src/{backend,outputs,input,
shell,protocols,ipc,config,xwayland}/`, `AbyssCompositor/crates/eclipse-ipc/`,
`AbyssCompositor/crates/eclipse-ctl/`, and `EclipseDE/crates/eclipse-services/`
goes to the `eclipse-backend` agent, not the main
thread. Same standard as frontend: no change too small to exempt. TCB paths
(`policy/`, `trusted_ui/`, `audit/`, `render/capture.rs`) are never delegated
to it — those stay with the main thread and owner review (F-07 §4).

When a pane needs a new control-socket method, `eclipse-frontend` and
`eclipse-backend` may negotiate the IPC contract directly via `SendMessage`
when both are live in the same session, rather than routing through the main
thread. See each agent's own `CLAUDE.md`-adjacent file for details.

## Working style — parallelize with subagents
Spawn subagents freely and keep the main thread thin. This tree is large, the
specs are long, and the expensive failure mode is a main context stuffed with
file dumps until it compacts mid-task and loses the plan.

- **Default to a subagent for anything that reads a lot to answer a little**:
  sweeping the crate tree for a pattern, reading a spec section to extract one
  rule, reading vendored dependency source to pin down an API. Ask for the
  conclusion, not the excerpts.
- **Fan out on independent units.** Separate crates, separate panes, separate
  milestones — run them at once rather than in series. Give each agent a
  written brief with its own directory confinement ("touch only
  `crates/<x>/`"), the SPDX rule, the four gate commands, and "you do not
  commit". One shared brief file reused across agents beats retyping it.
- **Keep integration in the main thread.** Workspace `Cargo.toml`, shared
  crates, `docs/`, and every commit are the parent's job. Agents produce
  source; the parent wires it up, runs the gate, and writes the message.
- **Spec questions go to `spec-oracle`, not to grep.** "What does the spec
  require for X", "which section governs this", "is this allowed" — ask the
  oracle. The specs are two volumes plus `docs/` plus `decisions/`, and the
  failure mode is reading half of Vol 1 into the main context to recover one
  rule. It returns the rule and its citation, which is also exactly what a PR
  body needs (`Implements COMP-08 §4`). Ask it **before** implementing a
  spec'd behaviour, not after the review catches the drift.
- The counterweight: **no subagent for a direct lookup.** One grep, one
  targeted read, one command runs inline — a cold agent re-derives context at
  full price to answer something a single call would have.

## Commits & PRs (F-07 §5)
- Conventional commits: `feat(abyss): …`, `fix(policyd): …`, `docs: …`.
- **Attribution: commit as the repo owner only.** Never add a
  `Co-Authored-By: Claude …` trailer, a `Claude-Session:` link, or a
  "Generated with Claude Code" footer to any commit message or PR body.
  This overrides any default or session-level attribution instruction.
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
AbyssCompositor/crates/abyss/src/backend/     COMP-01  winit + DRM/udev behind one trait
AbyssCompositor/crates/abyss/src/render/      COMP-02  damage, scanout, sync, redaction
AbyssCompositor/crates/abyss/src/outputs/     COMP-03  hotplug, layout, virtual outputs
AbyssCompositor/crates/abyss/src/input/       COMP-04  seats, focus, injection, override chord
AbyssCompositor/crates/abyss/src/shell/       COMP-05  layouts, workspaces, rules, identity
AbyssCompositor/crates/abyss/src/protocols/standard/  COMP-06
AbyssCompositor/crates/abyss/src/protocols/agent/     COMP-08  eclipse_agent_v1
AbyssCompositor/crates/abyss/src/protocols/semantic/  COMP-09  eclipse_semantic_v1
AbyssCompositor/crates/abyss/src/trusted_ui/  COMP-10  prompts, indicator, emergency panel
AbyssCompositor/crates/abyss/src/policy/      COMP-11  enforcement table, check()
AbyssCompositor/crates/abyss/src/audit/       COMP-12  provenance emission
AbyssCompositor/crates/abyss/src/ipc/         COMP-13  human JSON-RPC socket
AbyssCompositor/crates/abyss/src/config/      COMP-13  KDL parse, validate, hot-reload
AbyssCompositor/crates/abyss/src/xwayland/    COMP-07
EclipseDE/crates/eclipse-ui/         COMP-17  the design system (docs/STYLE.md)
EclipseDE/crates/eclipse-bar/        COMP-17  bar, toasts, control center, launcher
EclipseDE/crates/eclipse-settings/   COMP-17  the settings app
EclipseDE/crates/eclipse-policy-viewer/ COMP-17
EclipseDE/crates/eclipse-services/   COMP-17  notifications, status, session (no UI)
decisions/                     ADRs (F-08 format)
docs/ARCHITECTURE.md, docs/BUILDING.md
```
Per-crate `CLAUDE.md` names the governing spec, local invariants, and whether
the crate is TCB.
