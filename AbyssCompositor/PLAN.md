# Phase 2 — the agent protocol: bucket, location, and starting sequence

> **How to use this file.** This is `PLAN.md` at the repo root
> (`AbyssCompositor/PLAN.md`), a working handoff document, not a spec and not a
> source of truth. `docs/` is the source of truth once code exists (F-07 §7).
>
> - **As work lands, write it into the real files, not here.** Each milestone's
>   PR updates `docs/STATUS.md`'s Phase 2 row, drops the `(not yet)` marker in
>   `docs/ARCHITECTURE.md:12-35`, adds an ADR under `decisions/` for any choice
>   the specs leave open, and adds the new crate's own `CLAUDE.md` naming its
>   governing spec and TCB status. Ticking a box in this file is not progress.
> - **Delete `PLAN.md` when Phase 2 is done.** Once milestone 17 lands and the
>   Phase 2 table in `docs/STATUS.md` reflects it, this file is stale scaffolding
>   — remove it in the same PR. Do not leave it behind for a future agent to
>   mistake for current guidance.
> - Do not add it to `.gitignore`; commit it so the next agent picks it up.

## Context

Phase 1 (compositor) is substantially built. You asked to start enacting Phase 2
and specifically: is it compositor work, OS work, or DE work, and does it need a
new directory?

**Answers, with citations:**

- **Bucket:** neither OS/distribution nor DE. `claude/OS_WORK.md` draws the three
  lines explicitly and says it out loud: *"System services are not distribution
  work. `policyd` and `agentd` are daemons, so they read as 'OS', but they are
  Tier 2–3 and sequenced as Phase 2 milestones. They are the product; the distro
  is the wrapper."* Phase 2 = **Tier 2–3 system services + TCB modules inside
  `abyss`**. Distribution is Tier 6 / Phase 6. The DE (`hyperion`,
  `eclipse-settings`, `eclipse-policy-viewer`, `EclipseDE/`) is untouched by
  Phase 2 except as a later consumer.
- **New dir?** **No.** F-07 §1, recorded in `docs/ARCHITECTURE.md:12-35`, already
  fixes the layout: one monorepo, one cargo workspace, new crates under the
  existing `AbyssCompositor/crates/` tree — `policyd/` [TCB], `policy-eval/`
  [TCB], `agentd/`, `audit/`, `sandbox/`, `proto-agent/`, `proto-semantic/`,
  `registryd/`, `sdk-*/` — plus new modules under `crates/abyss/src/`
  (`policy/`, `audit/`, `trusted_ui/`, `protocols/agent/`, `protocols/semantic/`).
  (`docs/ARCHITECTURE.md` was stale here — it predated the DE crates — and Step 0a
  fixed it; its inventory is current as of `1714a19`.)
  Only `brokerd` (m19), the egress proxy (m20) and `cataclysm` (m23) have no
  F-07 §1 slot yet; they are late in the phase and can take their slot when
  written.
- **Specs are ready.** S-01..S-11 and A-01..A-07 are all DONE; only S-12/S-13
  (supply chain, update security) are unwritten, and the oracle confirms neither
  blocks any milestone 10–25 — they gate release and D-02, not this phase.
  COMP-08 v0.2 cleared the Appendix A block.

**Licensing (F-05 §3, ADR 0005):** system crates `AGPL-3.0-only`; `proto-agent`,
`proto-semantic`, `sdk-*` are `Apache-2.0`. SPDX header on every file.

**TCB rule (F-07 §4, root CLAUDE.md):** `policyd`, `policy-eval`, `sandbox`, and
`abyss`'s `policy/`, `audit/`, `trusted_ui/`, `render/capture.rs` are **never**
delegated to `eclipse-backend`. Main thread + line-by-line owner review. The
non-TCB halves (m11's socket plumbing, m13's seat wiring, m14's batching) can go
to `eclipse-backend`.

## Recommended approach — a vertical slice, 10 → 11 → 12 → 16

Follow COMP-16's numbering for the first three, then **pull 16 forward ahead of
13–15**. Rationale: 10/11/12 build the three spines (grants, socket, audit) and
16 is the enforcement table that makes them mean anything. With 16 in place,
13/14/15 are built against real `check()` calls instead of stubs that have to be
rewritten. 16's own gate also needs the 9f benchmark harness, which
landed in Step 0b (`1fcb767`) and is waiting for 16 to supply its subject.

## Agent assignments — who does what, decided

The repo has a fixed cast. These are the bindings for Phase 2; they are not
suggestions, and the TCB rule overrides any of them.

| Agent | Used for, in this phase | Never |
|---|---|---|
| `spec-oracle` | Every "what does the spec require" before writing a milestone: m10's A-04 §6 task-object fields and S-01 §4 grant format, m11's COMP-08 §4 request list, m12's S-04 record schema, m16's COMP-11 §11 table format. Ask **before** implementing, once per milestone, and use its citation as the PR body's `Implements …` line. | Never writes code |
| `Explore` | Step 0a's sweep for stale docs / orphaned files. Read-only fan-out. | Never edits |
| `eclipse-backend` | The **non-TCB** halves only: m11's socket listener plumbing and `proto-agent` binding generation, m13's seat wiring under `input/`, m14's batching under `protocols/agent/`'s non-decision paths, and any `eclipse-ipc`/`eclipse-ctl` surface the DE needs later. Give it a written brief with directory confinement, the SPDX rule, the five gate commands, and "you do not commit". | **Never** `crates/policyd/`, `policy-eval/`, `sandbox/`, `abyss/src/policy/`, `abyss/src/audit/`, `abyss/src/trusted_ui/`, `render/capture.rs` (F-07 §4) |
| **main thread (me), owner review** | All of m10 (`policyd`, `policy-eval`), all of m12's `audit/`, all of m16's `policy/`, all of m15's `trusted_ui/`, and the `check()` call sites inside `protocols/agent/`. Plus every commit, every workspace `Cargo.toml` edit, every `docs/` update. | — |
| `invariant-review` | Run on the diff **before every Phase 2 PR**, without exception — this phase is where all eleven invariants actually bite (fail-closed, ratchet, no-mutation-before-Allow, no allocation in the check path). | Never fixes |
| `pr-driver` (repo-shadowed) | The git→gate→push→PR→CI cycle once a milestone's diff is reviewed. It knows this repo's rules: conventional commits, `Implements <spec §>` in the body, **no Claude attribution trailers**. | Not before `invariant-review` is clean |
| `eclipse-frontend` | Nothing in milestones 10–16. Re-enters at m15 **only** if the trusted-UI prompt needs visual iteration — and even then only on non-TCB styling, never on `trusted_ui/`'s grab/focus logic. | — |
| `eclipse-ui-prober` | Not used this phase. |

Fan-out rule: milestones 10 and 12 have no dependency on each other and can run
concurrently (`policyd`/`policy-eval` on the main thread, `audit/` in a second
pass). 11 needs 10's grant format. 16 needs all three. 19/20/21 have no
compositor dependency at all and are the natural `eclipse-backend` second front
if you want one open.

## Step 0 — DONE. Both halves are on `main`; start at milestone 10.

Nothing in Step 0 remains. A fresh session opening this file starts directly at
the vertical slice **10 → 11 → 12 → 16** below.

**Step 0a — cleanup pass: landed.** PR #17, `1714a19`
(`docs: reconcile the crate inventory and Phase 1 status with the tree`).
`docs/ARCHITECTURE.md`'s F-07 §1 block and `docs/STATUS.md`'s Phase 1 rows now
match the tree; the six DE crates are listed; `EclipseDE/` (a stray `target/`,
no `Cargo.toml`) is gone. The DE is **not** moving — Phase 2 crates go in
`AbyssCompositor/crates/`.

**Step 0b — clear the runway: landed.**

1. `comp04-m01-focus-rules` — PR #14, `9240846`
   (`feat(abyss): derive focus from a first-class focused output`). Focus rules,
   `shell/focus.rs`, ADR 0042. Off the Phase 2 boundary.
2. **9f, the benchmark harness** — PR #18, `1fcb767`
   (`feat(bench): the COMP-14 benchmark harness`). `bench/` exists: a hand-rolled
   sampler keeping every sample and reporting p50/p99/p99.9/max, all fourteen
   COMP-14 §2.1/§2.1b budgets encoded as data, and §3's counting allocator so a
   bench on a no-alloc path fails on its first allocation. ADR 0043; COMP-14
   §4.1 amended away from Criterion. **Caveat for milestone 16:** none of §4.1's
   five subjects can run yet — `check()` and scope match arrive with 16 itself,
   audit encode with 12, tree serialize with 22. 16's gate
   (`≤50 µs p99 measured by the 9f harness`) is now *mechanically closable*: the
   harness is there, and 16 supplies the subject. COMP-14 §5's per-commit
   baselines and the >10% regression gate are still owed, deliberately (ADR 0043).
3. 9c (CI wlcs run), 9d (node redaction — since landed, PR #15 `be58a15`) and 9e
   stay as they are. They do not block Phase 2.

### Milestone 10 — `policyd` skeleton, task store, grants
Specs: **A-04 §6** (journaled task objects + counters), **S-01 §4** (grant format).

- New crates: `crates/policyd/` [TCB, AGPL], `crates/policy-eval/` [TCB, AGPL] —
  the evaluator both `policyd` and `abyss::policy` link.
- Task object: one active task per principal; `active`/`paused`/`draining` states;
  a task boundary is a process boundary.
- Grant issue/revoke/expiry; grants are `policyd`-signed. `agentd` cannot mint
  capabilities — binding happens in `abyss` at `create_agent` time.
- Journal the store so counters survive restart.
- **Exit gate (CI):** closing a task revokes every grant naming it in one
  operation; a second active/paused/draining task per principal is rejected;
  counters survive a `policyd` restart; a grant whose `expires` exceeds its
  task's `deadline` is rejected at issue.

### Milestone 11 — privileged socket, `agentd`, `list_toplevels`
Specs: **COMP-08 §4**, **A-01**.

- New crates: `crates/agentd/` (AGPL), `crates/proto-agent/` (**Apache-2.0**,
  generated bindings).
- New module: `crates/abyss/src/protocols/agent/` [TCB-adjacent — main thread].
- Second listening socket, mode **0600**, serving `agentd` only. The public
  `wayland-N` socket must never expose the agent globals.
- Grant verification on connect; `list_toplevels`, `get_toplevel`, `hit_test`.
  Replaces today's `get_agents` "not implemented".
- Side effect: closes **defect 13** — `agent-activity` IPC finally has a source.
- **Exit gate (CI):** scope-leakage suite green — a window outside `scene.list`
  scope never appears in listings, hit tests, events, captures or `wait_for`;
  `no-agent` windows absent for every agent; a normal client on `wayland-N`
  cannot bind agent globals; socket mode is 0600.

### Milestone 12 — audit spine
Spec: **COMP-12** (schema/chaining from S-04).

- New crate `crates/audit/` [TCB, AGPL] + module `crates/abyss/src/audit/`.
- Append-only journal, request-id chaining, the `trace` surface. `SOCK_SEQPACKET`
  for the journal path (COMP-12/S-04/A-04 §6).
- **Exit gate (CI):** chain reconstruction over synthetic records; no human
  keystroke content anywhere; capture records contain no pixels.

### Milestone 16 (pulled forward) — enforcement table, prompt/defer
Spec: **COMP-11**, ADR 0008.

- New module `crates/abyss/src/policy/` [TCB]. `check()` is an **in-process,
  allocation-free** lookup of the table `policyd` compiled and pushed — it does
  **not** call `policyd` on the hot path. Only `defer` takes the IPC slow path,
  bounded by `deferred_timeout`, failing closed.
- Fail-closed: unknown request, missing table entry, unavailable `policyd` = deny.
  With `policyd` not connected, `create_agent` fails `POLICY_UNAVAILABLE`
  (COMP-01 §6 degraded mode).
- Ratchet: `defer` may only tighten. No state mutation before `Allow`.
- **Do not mistake `crates/abyss/src/ipc/gate.rs` for this** — that is the
  narrower COMP-13 §2 human-socket gate and stays separate.
- **Exit gate (CI):** enforcement suite + F-07 §3 golden decision suite green; no
  state mutation on any denied path under fault injection at each step of
  COMP-08 §10; defer never widens; parked prompts re-validate; unsigned and
  rolled-back tables rejected; ≤50 µs p99 on the 9f harness.

### Then, in order
13 (agent seats, injection, `agent-override` chord — also closes **defect 11**,
press-time hit-test), 14 (atomic batches, `wait_for`, generations), 15 (trusted
UI; the indicator already landed via `render::capture::indicator()`), 17
(policy-driven sensitivity — removes the manual flag at `state.rs:110`).

**Known blockers, do not schedule these early:** 18 blocked on F-03 (defect 13,
S-07 §5 stamper enum); 24 requires C-00 §17 open item 1 to be decided; 21/23
blocked on the undecided `cataclysm` vendoring (defect 12); 25 is out of this
repo. 19/20/21 have no compositor dependency and can run in parallel with 10–18
if you want a second front.

## Critical files

| Path | Why |
|---|---|
| `Cargo.toml` | add each new crate to `members` as it lands; `default-members` stays `crates/abyss` |
| `docs/ARCHITECTURE.md:12-35` | the F-07 §1 layout block — drop the `(not yet)` markers as crates appear |
| `docs/STATUS.md` (Phase 2 table, lines ~107-131) | source of truth once code exists; update the row in the same PR |
| `crates/abyss/src/state.rs` | `AbyssState` gains the policy table + audit handles; `:110` sensitivity stub dies at m17 |
| `crates/abyss/src/ipc/gate.rs` | reference only — **not** the COMP-11 table |
| `crates/eclipse-policy-viewer/src/read.rs` | already models the policy surface off-disk; keep it file-based, don't add an IPC path |
| `decisions/` | new ADR for any layout or transport choice the specs leave open (e.g. policyd's socket path — no spec states one) |

## Reuse, don't rebuild

- `policy-eval` is deliberately **one** evaluator linked by both `policyd` and
  `abyss::policy` — do not write the rules twice.
- `render::capture::indicator()` (COMP-10 §3.6) already exists; m15 extends
  `trusted_ui/` around it rather than replacing it.
- `crates/eclipse-ipc` and `eclipse-ctl` patterns for JSON-RPC framing are the
  model for `agentd`'s human-facing surfaces — but the privileged socket is a
  separate listener, never the same one.
- KDL editing traps are already catalogued in `docs/HANDOFF.md` (silent
  `set_value()`, `From<i128>`, never `autoformat()` a user file) — read it before
  touching policy-file serialization.

## Verification

Per-milestone, the COMP-16 exit gate above is the acceptance test and each is
class **CI** — it lands as a test in `tests/` (`golden/` for m16's decision
suite, `redteam/` for m11's scope-leakage suite), not as a manual check.

Every PR runs the full gate, which must stay identical to `../.github/workflows/gate.yml`:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
cargo deny check advisories bans licenses sources
```

`cargo deny` is the licensing check — it will catch an Apache-2.0 SPDX header on
a system crate or vice versa. Every PR body cites its spec section
(`Implements COMP-08 §4`) or `spec-trail` blocks it. TCB PRs get line-by-line
owner review, no exceptions.

End-to-end for the slice: with 10/11/12/16 landed, `agentd` should be able to
connect over the privileged socket, present a `policyd`-signed grant, call
`list_toplevels`, and have the call appear as a chained audit record — while the
same call from a client on `wayland-N` cannot even bind the global.
