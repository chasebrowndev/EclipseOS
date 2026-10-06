<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# abyss / EclipseOS — root invariants

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
The dev host runs abyss itself, and the interactive session lives in a
terminal on it (`foot` or `kitty`). **Never `pkill`/`killall` the terminal,
shells (zsh), claude, Xwayland or the running abyss session** — that kills the
session you are running in, mid-command, and looks like a mysterious external
SIGKILL (exit 137). This has already happened more than once.

- Kill a spawned test process by the pid you captured when you spawned it
  (`./target/debug/ec-abyss ... & pid=$!` then `kill $pid`), never by process
  name. `pkill -x ec-abyss` is **not** safe on a host whose session is abyss.
- Never `pkill -f`.
- Xwayland on the host belongs to the host session. Leave it alone.

## Build / test / run
```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
cargo deny check advisories bans licenses sources
cargo run -- --backend winit    # nested window under the host session (abyss)
journalctl --user -t ec-abyss -f  # logs (tracing → journald)
```
The first five are exactly what CI runs (`../.github/workflows/gate.yml` — the
workflows sit at the repository root, one level above this crate tree). If you
change one, change both — a local gate that differs from CI is worse than no
local gate.

The toolchain is pinned in `rust-toolchain.toml`; CI and dev must not drift.
Same rule as the Smithay 0.7.0 pin: a bump is its own PR with its own
justification.

## Frontend is the frontend agent's job — always

**The main agent never writes frontend.** Every single piece of user-facing UI
— a new view, a redesign, a tweak to an existing one, anywhere under
`shell/crates/ec-ui/`, `shell/crates/ec-hyperion-bar/`, `shell/crates/ec-toasts/`,
`shell/crates/ec-center/`, `shell/crates/ec-launcher/`, `shell/crates/ec-settings/`,
`shell/crates/ec-policy-viewer/` or `shell/crates/ec-secret-prompt/` — goes to the `eclipse-frontend` agent. It is
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

Non-TCB work under `abyss/crates/ec-abyss/src/{backend,outputs,input,shell,protocols,
ipc,config,xwayland}/`, `abyss/crates/ec-ipc/`, `abyss/crates/ec-ctl/`, and
`shell/crates/ec-services/` goes to the `eclipse-backend` agent when the change
is medium or larger — the point is keeping the main context clean. Small
changes (a default, a one-liner and its test/doc fallout) the main thread makes
directly. Unlike frontend, size matters here. TCB paths
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
- The counterweight: **no subagent for a direct lookup or a small change.**
  One grep, one targeted read, one command, a one-line edit runs inline — a
  cold agent re-derives context at full price to do what a single call would
  have. Subagents are for medium and larger changes, where keeping the main
  context clean pays. The one exception: **all** frontend goes to
  `eclipse-frontend`, every time, whatever the size.
- **Subagents run one model tier below the orchestrator.** An Opus
  orchestrator spawns Sonnet subagents (`model: "sonnet"`); a Sonnet
  orchestrator spawns Haiku ones. Resuming an agent keeps its old model, so
  to change tier, start a fresh agent pointed at the partial worktree diff.
  Exploratory agents (`Explore`, read-only sweeps, lookups) always use
  Haiku (`model: "haiku"`), whatever the orchestrator's tier.
- **Build barrier when several agents run at once.** Each brief says: no
  cargo build/check/clippy/test, gate, or headless screenshot until the main
  thread gives the go-ahead. An agent finishes its code, messages main
  "READY TO BUILD" with a short summary, and waits. Main gives the go-ahead
  only after **every** running agent has signalled ready, never to one early.

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
- TCB areas (`abyss` enforcement path, `policyd`, `ec-policy-eval`, `sandbox`)
  get a line-by-line owner review. No exceptions.

## Where things are
```
abyss/crates/ec-abyss/src/backend/     COMP-01  winit, DRM/udev, headless, gpu.rs, behind one trait
abyss/crates/ec-abyss/src/render/      COMP-02  capture.rs only (TCB, state-bound); re-exports ec-abyss-render
abyss/crates/ec-abyss-render/       COMP-02  state-free: damage, scanout, sync, blur, effects, annotations, overscan arithmetic
abyss/crates/ec-abyss/src/outputs/     COMP-03  hotplug, layout, EDID, calibration, power
abyss/crates/ec-abyss/src/input/       COMP-04  keyboard, pointer, touch, tablet, gestures, injection
abyss/crates/ec-abyss/src/shell/       COMP-05  layouts, workspaces, rules, focus
abyss/crates/ec-abyss/src/protocols/standard/  COMP-06
abyss/crates/ec-abyss/src/protocols/agent/     COMP-08  ec-agent.sock, eclipse_agent_v1 (M11: admission, scene queries)
abyss/crates/ec-abyss/src/protocols/semantic/  COMP-09  eclipse_semantic_v1    (not yet)
abyss/crates/ec-abyss/src/trusted_ui/  COMP-10  prompts (modal primitive; §3.10 erase, §3.11 command approval); indicator, emergency panel not yet
abyss/crates/ec-abyss/src/policy/      COMP-11  grant admission, agent scene filter; enforcement table, check() not yet
abyss/crates/ec-abyss/src/audit/       COMP-12  provenance emission to policyd (M12: scene requests, focus, lifecycle)
abyss/crates/ec-abyss/src/ipc/         COMP-13  human JSON-RPC socket, methods, config RPC (gate.rs is a re-export shim)
abyss/crates/ec-abyss-wire/         COMP-13  state-free: gate table + check, Decision/Peer, RpcError + codes, EVENTS, Hook/HookSet (gate.rs = authz enforcement, owner review)
abyss/crates/ec-abyss-config/       COMP-13  KDL schema, parse, validate, edit (no smithay; ec-settings/ec-ctl link it)
abyss/crates/ec-abyss/src/config/      COMP-13  re-exports ec-abyss-config; apply, hot-reload, catalog watch, approval withholding
abyss/crates/ec-abyss/src/xwayland/    COMP-07
abyss/crates/ec-policyd/, abyss/crates/ec-policy-eval/   A-04, S-01 §4, S-04 §4  policy daemon + shared types (TCB)
abyss/crates/ec-protocols/          eclipse_agent_v1 XML + bindings (Apache-2.0)
abyss/crates/ec-agentd/             agentd skeleton (M11)
abyss/crates/ec-audit/              ec-audit: verify, trace, query the audit store (S-04 §4–§5)
abyss/crates/ec-ipc/           control-socket client + types
abyss/crates/ec-ctl/           CLI over the control socket, config migrate
abyss/crates/ec-abyss-wlcs/            WLCS conformance shim
shell/crates/ec-services/      D-Bus services: notifications, tray, status, screensaver
shell/crates/ec-ui/            shared iced theme/tokens/widgets
shell/crates/{ec-hyperion-bar,ec-toasts,ec-center,ec-launcher,
        ec-settings,ec-policy-viewer,ec-secret-prompt}/  DE panes
abyss/bench/                   COMP-16 M9f frame-time harness
abyss/ci/                      wlcs skip list, GUI-coverage exceptions
packaging/                          session, units, PKGBUILD, repo, ISO, /etc defaults
decisions/                     ADRs (F-08 format)
docs/ARCHITECTURE.md, BUILDING.md, STATUS.md, KNOWNBUGS.md,
     PROPOSEDFEATURES.md, internal/HANDOFF.md (+ internal/handoff/ archive), CONFIG.md,
     STYLE.md, COMPOSITION.md, design/ (D-NN)
```
Per-crate `CLAUDE.md` names the governing spec, local invariants, and whether
the crate is TCB.
