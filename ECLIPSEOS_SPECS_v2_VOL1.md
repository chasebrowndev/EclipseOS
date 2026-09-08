# EclipseOS — Specification Bundle, Volume 1 of 2

**Foundation, compositor, and policy core.** Tiers 0, 1, and the S-01..S-04
core of Tier 2, plus P-01 (the semantic tree schema everything references).

Volume 2 holds the security semantics (S-05..S-11), perception (P-02..P-07),
the agent gateway (A-01..A-07), and **Appendix A — amendments that are not
yet applied inline to the documents in this volume.**

> **Appendix A is applied.** All amendment batches (v1–v4) are merged inline
> as of 2026-09-07; Volume 2's Appendix A is now a log, not a pending queue.
> COMP-08 is at v0.3, S-01 and S-05 at v0.2.

The full planning index is reproduced in both volumes so either can start a
session on its own.


---

<!-- ===== FILE: PLANNING_INDEX.md ===== -->

# Planning Index — All Documents

Status legend: **DONE** · **NEXT** · planned · deferred (post-v1)

Ordering rule: a document is written only after its dependencies, because
downstream specs inherit decisions. Numbers are stable identifiers, not
priority.

---

## Tier 0 — Foundation

| ID | Document | Status | Depends on |
|---|---|---|---|
| F-01 | Charter (goals, non-goals, phases, quality gates) | **DONE** (CHARTER.md) | — |
| F-02 | Threat model | **DONE** | F-01 |
| F-03 | Naming & trademark resolution | **DONE** | — |
| F-04 | Reference hardware & performance baseline | **DONE** | — |
| F-05 | Licensing (AGPL dual vs source-available), CLA, commercial tier | **DONE** | F-01 |
| F-06 | Glossary & naming conventions (principal, handle, capability, grant, class) | **DONE** | F-02 |
| F-07 | Repo layout, branching, CI/CD, release engineering | **DONE** | F-05 |
| F-08 | Decision log (ADR format & index) | **DONE** | — |

## Tier 1 — Compositor (`abyss`)

| ID | Document | Status | Depends on |
|---|---|---|---|
| C-00 | Compositor master spec | **DONE** | F-01, F-02 |
| COMP-01 | Core, backend, process model, startup, crash resilience | **DONE** | C-00 |
| COMP-02 | Rendering: damage, scanout, sync, scaling, redaction, effects | **DONE** | C-00 |
| COMP-03 | Outputs: hotplug, layout, virtual outputs, power | **DONE** | C-00 |
| COMP-04 | Input & seats: multi-seat, injection, focus arbitration, override | **DONE** | C-00, F-02 |
| COMP-05 | Window management: layouts, workspaces, rules, identity, launch | **DONE** | C-00 |
| COMP-06 | Standard protocol support matrix | **DONE** | C-00 |
| COMP-07 | XWayland | **DONE** | C-00 |
| COMP-08 | `eclipse_agent_v1` protocol (full XML) | **DONE** | C-00, F-02, S-01 |
| COMP-09 | `eclipse_semantic_v1` protocol (full XML) | **DONE** | C-00, P-01 |
| COMP-10 | Trusted UI: prompts, indicators, anti-spoof, emergency panel | **DONE** | F-02 |
| COMP-11 | Policy enforcement hooks & table format | **DONE** | S-01 |
| COMP-12 | Audit & provenance emission | **DONE** | S-04 |
| COMP-13 | IPC & configuration schema (KDL) | **DONE** | C-00 |
| COMP-14 | Performance targets & benchmarking | **DONE** | F-04 |
| COMP-15 | Testing, fuzzing, client compat matrix | **DONE** | C-00 |
| COMP-16 | Milestones & sequencing | **DONE** | C-00 |

## Tier 2 — Security & Policy (`policyd`)

| ID | Document | Status | Depends on |
|---|---|---|---|
| S-01 | Capability model & grant format (signing, time-boxing, scoping) | **DONE** | F-02 |
| S-02 | Policy language & compiler (rules → enforcement table) | **DONE** | S-01 |
| S-03 | Sandbox profiles (bubblewrap, Landlock, seccomp, cgroups, netns) | **DONE** | F-02 |
| S-04 | Audit schema, storage, hash chaining, retention, query | **DONE** | S-01 |
| S-05 | Sensitivity classification: classes, rules, app trust classes | **DONE** | F-02 |
| S-06 | Irreversible-action taxonomy & HITL rules | **DONE** | S-05 |
| S-07 | Provenance model (surface → agent → channel → action) | **DONE** | F-02 |
| S-08 | Secrets handling: keyring, credential brokering for agents | **DONE** | S-03 |
| S-09 | Network egress policy & enforcement | **DONE** | S-03 |
| S-10 | Red-team suite: injection corpus, escape attempts, redaction proofs | **DONE** | S-01..S-07 |
| S-11 | Incident response: detection, kill switch, forensics from audit | **DONE** | S-04 |
| S-12 | Supply chain: crate vetting, package signing, model pinning, reproducible builds | planned | F-07, A-07 |
| S-13 | Update & rollback security (system + policy + model updates) | planned | S-12 |

## Tier 3 — Perception (`registryd`)

| ID | Document | Status | Depends on |
|---|---|---|---|
| P-01 | Unified semantic tree schema (roles, states, actions, sensitivity) | **DONE** | F-02 |
| P-02 | AT-SPI2 bridge: aggregation, caching, coordinate join, quirks | **DONE** | P-01 |
| P-03 | Toolkit bridges: GTK4, Qt6, Electron/Chromium, Firefox | planned | COMP-09 |
| P-04 | Terminal perception: text grid, TUI structure, scrollback | **DONE** | P-01 |
| P-05 | Tree pruning, ranking, relevance, token budgeting | **DONE** | P-01 |
| P-06 | Vision fallback: when, OCR/detection pipeline, element synthesis | planned | P-01, P-05 |
| P-07 | Change detection: damage, generations, diffing, `wait_for` predicates | **DONE** | P-01 |
| P-08 | Web perception: DOM a11y tree, iframes, shadow DOM, scrolling | **NEXT** | P-02 |
| P-09 | Perception benchmark suite & accuracy metrics | planned | P-01..P-08 |

## Tier 4 — Agent Gateway (`agentd`)

| ID | Document | Status | Depends on |
|---|---|---|---|
| A-01 | Gateway architecture, agent lifecycle, authentication | **DONE** | S-01 |
| A-02 | MCP surface: tool schemas, mapping to protocol requests | **DONE** | COMP-08 |
| A-03 | Channels: message passing, provenance stamping, quotas | **DONE** | S-07 |
| A-04 | Session & task model: what an agent "run" is, cancellation, resume | **DONE** | A-01 |
| A-05 | Error taxonomy & retry semantics (incl. request-id dedupe) | **DONE** | COMP-08 |
| A-06 | Agent SDK (Rust + Python): ergonomics, idempotence helpers | **DONE** | A-02 |
| A-07 | Agent packaging & manifest (declared capabilities, review) | **DONE** | S-01 |

## Tier 5 — Inference & Routing

| ID | Document | Status | Depends on |
|---|---|---|---|
| I-01 | Local model serving: runtime, GPU/VRAM sharing with compositor | planned | F-04 |
| I-02 | Router: rules first, cost/latency/quality policy, provider abstraction | planned | I-01 |
| I-03 | Classifier: task definition, features, ratchet constraint, eval harness | planned | S-02, S-04 |
| I-04 | Training data pipeline from audit log; labeling; privacy | planned | S-04 |
| I-05 | Model supply chain: pinning, signing, update policy, rollback | planned | S-12 |
| I-06 | Budgets, quotas, spend controls | planned | I-02 |
| I-07 | Provider integration (Anthropic first; agnostic?) | planned | I-02 |

## Tier 6 — Base System & Distribution

| ID | Document | Status | Depends on |
|---|---|---|---|
| D-01 | Base package set, kernel config & patches, init/systemd layout | planned | F-01 |
| D-02 | Package repository: build infra, signing, mirrors | planned | F-07, S-12 |
| D-03 | ISO build (archiso) & installer | planned | D-01 |
| D-04 | Update strategy: rolling vs snapshots, atomic updates, rollback | planned | D-02 |
| D-05 | Default userland: bar, launcher, terminal (`eclipse-term`, P-04), portal, notifications | planned | C-00, P-04 |
| D-06 | Hardware support matrix, GPU drivers, firmware | planned | F-04 |
| D-07 | First-run experience & agent onboarding | planned | D-03 |
| D-08 | Telemetry & crash reporting (opt-in; privacy stance) | planned | F-02 |

## Tier 7 — Cross-cutting

| ID | Document | Status | Depends on |
|---|---|---|---|
| X-01 | Performance engineering: boot, memory, GPU contention, thermals | planned | F-04 |
| X-02 | Observability: logs, metrics, tracing across all daemons | planned | S-04 |
| X-03 | Accessibility for humans (we own the a11y stack; do not regress it) | planned | P-01, P-02 |
| X-04 | Internationalization: keymaps, IME, RTL, locale | planned | COMP-04 |
| X-05 | Backup, state, and disaster recovery | planned | D-04 |
| X-06 | End-to-end benchmark suite (the §7 quality gate tasks) | planned | P-09, A-06 |
| X-07 | Documentation plan: user, agent developer, contributor, security | planned | A-06 |
| X-08 | Community, governance, contribution, security disclosure policy | planned | F-05 |

## Tier 8 — Deferred (post-v1, tracked so they are not forgotten)

| ID | Document | Depends on |
|---|---|---|
| Z-01 | Native desktop environment / shell | v1 ship |
| Z-02 | Cross-agent scene grants (agent *groups* superseded by A-04 §10 task nesting) | A-03 in use |
| Z-03 | Multi-user & enterprise deployment | v1 ship |
| Z-04 | Remote agents (network MCP) & fleet management | S-09 |
| Z-05 | HDR & color management | COMP-02 |
| Z-06 | Compositor hot-restart preserving clients | COMP-01 |
| Z-07 | Formal verification of the enforcement path | S-02 |
| Z-08 | Vulkan renderer | COMP-02 |

---

## Critical Path

The original critical path (S-01 → COMP-08/S-02 → A-01/A-02 → enforcement)
is **complete as specification**. What remains splits into two independent
chains that do not block each other:

```
IMPLEMENTATION (unblocked now)
  COMP-01..07  Phase 1 daily-driver compositor
    └─ COMP-16 milestones 1-9  ── no pending amendments except
                                  COMP-02 §7 (A-10) and COMP-05 (A-12, A3-07)

SPECIFICATION (remaining)
  Appendix A applied inline
    └─ P-08 web  ─┐
       P-06 vision ┼─ P-09 perception benchmark
       P-03 toolkit┘
    └─ S-12 supply chain ─ S-13 update security ─ I-05 model supply chain
    └─ I-01 local serving ─ I-02 router ─ I-03 classifier
```

Phase 2 implementation (COMP-16 milestones 10-18) is **no longer blocked on
Appendix A**: it is applied. COMP-08 is at v0.3 and its wire signatures are
settled — `button` gained a target and generation, the seat interface gained
lease, preflight and secret_fill requests, and every acting request carries
`provenance_ids`. Implement against the documents, not against the appendix.

## New Components Introduced in Session 2

These are real build artifacts with no owning Tier 6 document yet. They need
to appear in D-01/D-05 and in COMP-16 sequencing:

| Component | Introduced by | Note |
|---|---|---|
| `brokerd` | S-08 §2 | Separate TCB daemon for secrets; TPM-sealed store |
| `eclipse-term` | P-04 §1 | Patched terminal emulator (foot fork) publishing the grid |
| per-agent egress proxy | S-09 §2 | Userspace proxy with stub resolver and optional MITM |
| task store | A-04 §6 | Journaled task objects and counters inside `policyd` |

## Progress

Session 1 (2026-09-05): F-06, F-08, S-01, P-01, COMP-09, COMP-08, S-02,
S-03, S-04 written. Tiers 0 and 1 complete.

Session 2 (2026-09-05): S-05..S-11, A-01..A-07, P-02, P-04, P-05, P-07
written. **Tiers 2 (except S-12/S-13) and 4 complete.** Four amendment sets
were produced and collected in Appendix A.

Session 3 (2026-09-07): interaction leases, pointer hardening, trusted-UI
agent status and deferred consent, and remote-vision consent designed
(batch v4, A4-01..A4-24). **All five amendment batches applied inline** in
order v1, v2, v3a, v3b, v4. COMP-08 → v0.3; S-01, S-05 → v0.2. Appendix A is
now a log. Two errors in the amendment text were caught by merging in order
and are recorded there.

Two scope decisions were taken in session 2 that are not yet reflected in
COMP-16 milestones:
- **A-04**: the task object; one active task per principal; a task boundary
  is a process boundary.
- **P-04**: EclipseOS ships a patched terminal emulator (`eclipse-term`,
  foot fork) because terminal perception has no other viable source.

Remaining: S-12, S-13; P-03, P-06, P-08, P-09; all of Tiers 5, 6, 7.

## Suggested Writing Order (remaining)

1. **P-08** web perception — the browser is where most agent work lands and
   is the largest open fidelity question.
2. **P-06** vision fallback — everything AT-SPI cannot reach; the S-06
   confidence gates assume it exists.
3. **P-03** toolkit bridges, then **P-09** benchmark suite (last, so it can
   measure the full source mix).
4. **S-12, S-13** supply chain and update security — gate any public
   release; A-07 §4 already fixes several of their inputs.
5. Tier 5 (I-01..I-07), then Tiers 6 and 7.

Appendix A is applied; nothing is gated on it. Before P-06 is written,
measure real perception accuracy on reference hardware — the coverage claim
P-06 is allowed to make depends on a number nobody has yet.

---

<!-- ===== FILE: CHARTER.md ===== -->

# EclipseOS — Project Charter (Draft v0.1)

Status: DRAFT — awaiting owner decisions marked [DECISION].
Owner: You. Planning partner: Claude. Builders: same two.

---

## 1. Vision

An Arch-based operating system designed from the compositor up for AI agents as
the primary operator, with humans as supervisors. Agents perceive the system
through structured, semantic state rather than pixels wherever possible, act
through a first-class control API, and operate inside a deterministic security
model that cannot be talked past.

## 2. Goals (v1)

1. **Agent-native perception.** A system-wide semantic registry of windows,
   elements, text, and state, aggregated from the compositor, accessibility
   layer, and cooperating applications, with relevance filtering so agents
   receive what matters, not raw trees.
2. **Agent-native action.** A control API (focus, input, launch, file, shell,
   network) exposed through a compositor we control, with atomic multi-step
   actions and rollback where feasible.
3. **Deterministic security.** Capability-scoped agents, per-agent sandboxing,
   mandatory audit of every action, human confirmation for irreversible
   operations. A classifier may accelerate decisions; it never *makes* them.
4. **Hybrid inference.** Small local models (1–2B) for routing, classification,
   and fast paths; cloud models for reasoning. Routing is observable and
   overridable.
5. **Quality bar.** Nothing ships publicly until it meets the standard in §7.

## 3. Non-Goals (v1)

- Cost leadership or token-economics marketing. Capability is the goal.
- Supporting every application. Non-cooperating apps get a vision fallback,
  not a fork.
- Beginner-friendliness, GUI installers polish, theming, or desktop-environment
  breadth.
- Multi-user or enterprise deployment. v1 is single-user: the owner.
- Training foundation models. We fine-tune small models at most, and only
  after we have real data.

## 4. Principles

- **Quality over speed.** No workaround that degrades correctness or
  performance ships. Workarounds that are purely sequencing are allowed.
- **Compositor is the center.** Anything that needs deep integration goes
  through the compositor. Everything else is a normal package.
- **Everything on screen is untrusted input.** Design as if every rendered
  string is adversarial.
- **Observable by default.** Every agent action, perception query, and routing
  decision is logged in a structured, queryable form.
- **Dogfood constantly.** The owner runs this daily from the first bootable
  build. Bugs that block daily use outrank features.

## 5. Architecture Pillars

| Pillar | Description | Owns |
|---|---|---|
| Compositor | Wayland compositor with an agent protocol extension: surface/element enumeration, input injection, focus control, screenshot on demand. | Perception source of truth, action execution |
| Registry daemon | Aggregates compositor surfaces, AT-SPI2 trees, and cooperating-app registrations into one queryable model. Prunes and ranks by relevance. | Semantic state |
| Policy engine | Deterministic capability + rule enforcement. Sandboxing (bubblewrap / Landlock / seccomp). Audit log. Human-in-the-loop gates. | Security |
| Inference layer | Local model server (1–2B), classifier, router to cloud. Telemetry on cost/latency/accuracy. | Routing |
| Agent SDK / protocol | Stable API agents link against. Likely MCP-compatible so existing agents work day one. | Developer surface |
| Base system | Arch base, custom repo, archiso build, systemd units, defaults, installer. | Distribution |

## 6. Foundational Decisions

Decided 2026-09-04 unless marked open.

- **Language: Rust.** Compile-time memory safety in a security-critical,
  untrusted-input-parsing component; better fit for a type-enforced
  capability model; safer long-term refactoring for a solo maintainer.
  Cost: slower initial velocity, younger compositor ecosystem than wlroots.
- **Compositor: custom, built on Smithay from scratch.** Not a niri fork.
  Rationale: charter principle "compositor is the center" — perception and
  action primitives must be first-class in the architecture, not retrofitted
  onto a human-oriented window manager. Cost: months before first login;
  accepted explicitly against the quality-over-speed principle.
- **Input/perception mechanism: custom Wayland protocol extension** in our
  compositor. Not libei / xdg-desktop-portal. Full control over semantics,
  latency, and policy hooks. Cost: nothing else speaks it; we own the spec.
- **Agent protocol: MCP-compatible from day one.** Existing agents work
  without an adapter layer.
- **License: AGPLv3 + CLA, dual-licensed commercially** (F-05, decided).
  All system code including the TCB is AGPL; protocol definitions and agent
  SDKs are Apache-2.0 so third-party agents are not forced to AGPL. No
  open-core component; classifier weights ship open.
- **Name: EclipseOS** (F-05 §8). USPTO wordmark search returns no results;
  this is not a clearance and does not cover registered "ECLIPSE" marks.
  Accepted risk; professional search required before organisational revenue.
- **Reference hardware: decided** (F-04). i5 10th gen / 64 GB DDR4 /
  RTX 4060 Ti 8 GB. NVIDIA-first; Intel tested on the iGPU; AMD validated
  before release. 8 GB VRAM is the binding constraint on local inference.

## 7. Quality Standard for Public Release

"Real quality standard" must be falsifiable. Proposed gates; edit them:

- Owner has used it as a daily driver for ≥ 90 consecutive days.
- Zero known data-loss bugs. Zero known sandbox escapes.
- Agent completes a defined benchmark suite (≥ 20 realistic tasks across
  browser, terminal, files, and one GUI app) at ≥ 95% success with zero
  unauthorized actions.
- Every action in the audit log is reconstructible to its causing agent,
  prompt, and policy decision.
- Fresh install from ISO to working agent session in under 30 minutes with
  documentation only.
- Cold boot to agent-ready under 20 seconds on reference hardware.

## 8. Phases and Gates

Solo capacity means phases are **sequential**, not parallel. Each has an
exit gate. Do not start the next until the gate is met.

**Phase 0 — Charter & decisions (now).**
Exit: §6 decisions made (done except name, hardware, commercial tier);
threat model written; reference hardware chosen.

*(Former "minimal ISO" phase removed: owner already daily-drives stock Arch,
so there is no distribution gap to close first. ISO/installer work moves to
Phase 6.)*

**Phase 1 — Compositor.**
Smithay-based compositor reaching daily-driver parity for the owner's
workflow (multi-monitor, keyboard-driven tiling, XWayland, screen sharing,
clipboard, idle/lock). No agent protocol yet — a compositor you cannot live
in is not a foundation.
Exit: owner replaces current compositor with it full time.

**Phase 2 — Agent protocol.**
Custom Wayland extension: surface/element enumeration, focus control, input
injection, on-demand capture, event subscription. Policy hook points stubbed.
Exit: a test agent can open an app, type, click, and read window state via
the protocol alone, over MCP.

**Phase 3 — Registry daemon + perception.**
AT-SPI2 aggregation, cooperating-app registration, pruning/ranking,
vision fallback for opaque surfaces.
Exit: a benchmark set of 10 apps yields correct element maps; token budget
per query measured and documented.

**Phase 4 — Policy engine + audit.**
Capability model, sandbox profiles, deterministic rules, HITL gates, audit
store. Adversarial test suite for injection.
Exit: red-team suite passes; every action logged and attributable.

**Phase 5 — Inference & routing.**
Local model server, rule-based router first, telemetry.
Exit: measured latency/cost/accuracy per route; router overridable.

**Phase 6 — SDK, distribution, docs, hardening toward §7.**
archiso build, signed repo, installer, SDK polish, documentation.
Exit: §7 gates met.

## 9. Workstreams (future delegation targets)

Each becomes an Opus manager's scope with its own spec, ADRs, and tests,
once Phase 2 exists. Until then, workstreams are documents, not teams.

1. Compositor & action layer
2. Perception & registry
3. Security & policy
4. Inference & routing
5. Agent SDK & protocol
6. Base system & distribution
7. Performance
8. Documentation, licensing, community

## 10. Known Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Solo bandwidth; multi-year scope | High | Sequential phases, hard gates, no parallel work until Phase 2 |
| Non-cooperating apps have no semantic tree | High | Vision fallback is a first-class path, not an afterthought |
| Prompt injection via screen content | High | Deterministic policy; classifier advisory only; HITL for irreversible ops |
| Compositor fork drifts from upstream | Medium | Keep agent protocol as a separable module; rebase regularly |
| Wayland ecosystem changes (ext protocols, libei) | Medium | Track upstream; prefer standard protocols where they don't cost capability |
| Trademark conflict on name | Medium | Resolve in Phase 0 |
| Scope creep from "everything" | High | Non-goals list is binding; additions require a charter amendment |

## 11. Working Method

- One planning session per workstream, each producing a spec document.
- Decisions recorded as ADRs in `decisions/` with context, options,
  choice, consequences.
- This charter is amended, not silently overridden. Changes are logged.
- Claude holds prior positions across sessions unless given new evidence.

## 12. Open Questions for Owner

1. ~~License~~ — DONE (F-05).
2. ~~Project name~~ — DONE: EclipseOS (F-05 §8).
3. Reference hardware (GPU matters for local inference and compositor).
4. Edit §7 until it's a standard you'd actually refuse to ship below.
5. Are the cloud models fixed (Anthropic only) or provider-agnostic?
6. ~~Threat model~~ — DONE (THREAT_MODEL.md).
7. Reference hardware — DONE (F-04): i5 10th gen, 64 GB, RTX 4060 Ti 8 GB,
   NVIDIA-first GPU strategy. Display refresh rate still open.


---

<!-- ===== FILE: THREAT_MODEL.md ===== -->

# Threat Model (Draft v0.1)

Scope: the whole system as chartered — compositor, `agentd`, `registryd`,
`policyd`, inference layer, agent SDK — for the v1 deployment: a single
human owner, one machine, multiple agents, cloud and local models.

This document settles assumptions left open in CHARTER.md and
COMPOSITOR.md, and is the input to the Security & Policy spec.

---

## 1. What We Are Protecting (Assets)

| Asset | Examples | Why it matters |
|---|---|---|
| A1 Credentials & secrets | Passwords, session cookies, SSH/GPG keys, API keys, TOTP codes, password-manager UI | Total compromise if leaked |
| A2 Private content | Email, documents, chat, browsing, financial data, health data | Privacy, extortion, legal |
| A3 Irreversible actions | Sending messages, payments, deleting files, `git push --force`, publishing, account changes | Cannot be undone; direct real-world harm |
| A4 System integrity | Kernel, bootloader, compositor, `policyd`, policy tables, audit log, package trust | If these go, nothing below holds |
| A5 Audit truth | The log's completeness and immutability | Needed to detect, investigate, and train the classifier |
| A6 Human agency | The human's ability to see what agents do and stop them | The core promise of the product |
| A7 Compute & money | GPU time, cloud API spend | Abuse cost |

---

## 2. Principals and Trust Levels

| Principal | Trust | Notes |
|---|---|---|
| P0 Human owner | Full | Root of authority. Can be deceived (phishing via agent-produced UI) but not malicious. |
| P1 Compositor (`abyss`) | Trusted computing base | Enforces policy, draws trusted UI, mediates all input/capture. Must be smallest possible. |
| P2 `policyd` | TCB | Compiles policy, stores audit, runs slow-path decisions. |
| P3 `agentd` | Semi-trusted | Multiplexes agents; authenticates them; must not be able to escalate an agent's capabilities beyond what `policyd` granted. Compromise of `agentd` = compromise of every agent's granted capabilities, but not more. |
| P4 `registryd` | Semi-trusted | Reads trees and captures; a compromise leaks perception (A1, A2) but cannot act. Deliberately has no action capabilities. |
| P5 Agents | **Untrusted** | Every agent is treated as potentially compromised at all times. This is not paranoia: an agent is a program whose control flow is determined by text it reads, some of which is attacker-controlled. |
| P6 Applications | Untrusted, per-app trust class | Firefox rendering an attacker's page is P6 carrying attacker content. Apps can lie in their semantic trees. |
| P7 Cloud model providers | Untrusted for confidentiality; semi-trusted for output | Anything sent leaves the machine. Output is treated like P5 output. |
| P8 Local models | Untrusted for correctness | Fooled easily; small. Advisory only (ratchet rule). |
| P9 Remote attacker | Hostile | Reaches the system through content agents read, packages, network services, or supply chain. |

Key stance: **P5 is untrusted even when it is "our" agent running "our" prompt.** Prompt injection makes the distinction between a malicious agent and a hijacked agent meaningless at enforcement time.

---

## 3. Trust Boundaries

```
                 ┌──────────────── TCB ────────────────┐
  P0 human ──►   │  abyss (compositor)   policyd       │
  physical input │  trusted UI, seats,    policy table  │
                 │  capture, enforcement  audit store   │
                 └───────▲──────────▲────────▲──────────┘
                         │ priv sock│ IPC    │ audit
                    ┌────┴────┐ ┌───┴────┐   │
                    │ agentd  │ │registryd│   │
                    └────▲────┘ └───▲────┘   │
                   MCP   │          │ AT-SPI / captures
                 ┌───────┴───────┐  │
                 │  P5 agents    │  │  ┌────────────┐
                 │ (sandboxed)   │  └──┤ P6 apps    │
                 └───────┬───────┘     └────────────┘
                         │ network
                 ┌───────┴───────┐
                 │ P7 cloud LLMs │
                 └───────────────┘
```

Boundaries that must hold:
- B1: Agents cannot reach the compositor except through `agentd` → privileged socket. No agent ever holds the privileged socket.
- B2: Agents cannot reach `policyd` or the audit store at all.
- B3: Agents run in per-agent sandboxes (§7) and cannot read other agents' state, the human's home directory beyond granted paths, or each other's sockets.
- B4: `registryd` has read-only capabilities; it cannot inject or launch.
- B5: Nothing an app renders can become a compositor instruction. Semantic trees are data; actions on them are still gated.
- B6: Trusted UI is unforgeable: drawn by P1, focused only by the human seat.

---

## 4. Attack Surfaces

| Surface | Entry | Reaches |
|---|---|---|
| S1 Rendered content | Web pages, emails, documents, chat, file names, window titles, a11y trees | Agent context (prompt injection) |
| S2 Semantic tree spoofing | App publishes false roles/names/rects via `eclipse_semantic_v1` or AT-SPI | Agent targeting (click the wrong thing), redaction bypass if sensitivity is app-declared |
| S3 Agent protocol | Every `eclipse_agent_v1` request | Compositor parser and enforcement logic (memory safety, logic bugs) |
| S4 MCP surface | Agent ↔ `agentd` | `agentd` parser; capability confusion |
| S5 Model outputs | Cloud/local responses | Agent control flow; `policyd` slow path if the classifier consumes them |
| S6 Capture paths | screencopy, portal, `eclipse_capture_v1` | A1/A2 exfiltration |
| S7 Clipboard | Read/write via protocol or data-control | A1 (password managers paste), A2 |
| S8 Launch & shell | `launch`, agent-run shells | A3, A4 |
| S9 Filesystem | Agent sandbox mounts | A1 (keys, cookie DBs), A2, A4 |
| S10 Network | Agent egress; remote MCP if ever enabled | A1/A2 exfil, C2 |
| S11 Policy & config | Policy files, compositor config, rules | A4 (an agent that can edit policy owns the system) |
| S12 Supply chain | Packages, model weights, Rust crates, AUR | A4 |
| S13 XWayland | X11 clients see each other's input/windows | A1/A2 across X11 apps |
| S14 Physical / local | Someone at the keyboard, USB devices | Out of scope for v1 beyond screen lock |

---

## 5. Threats and Mitigations

Format: threat → what an attacker gains → mitigation (component) → residual.

### T1 Prompt injection via rendered content (S1) — **primary threat**
Attacker controls text the agent reads (web page, email, file name, a11y
name). Agent's model follows it: exfiltrates data, performs actions, changes
its own task.
- Mitigation, structural: agents are P5. They only ever get the
  capabilities `policyd` granted for the task; a hijacked agent cannot
  request more (B1, B2). `capture.secret`, `clipboard.read` on secret
  sources, `launch` of shells, and network egress beyond an allowlist are
  not granted to general-purpose agents by default.
- Mitigation, deterministic: irreversible-action classes (§8) always
  `prompt`. A hijacked agent that tries to send/pay/delete hits trusted UI.
- Mitigation, provenance: `registryd` tags every string it delivers with
  its source (surface handle, node, app trust class). The SDK exposes this
  so agent frameworks can wrap untrusted content; `policyd` uses source in
  slow-path decisions (text from a `public` web surface influencing a
  `launch` is a red flag).
- Mitigation, advisory: classifier flags injection-shaped content and
  off-task action sequences → `prompt`/`deny` (ratchet rule).
- Residual: an agent with legitimately granted read on private data and
  legitimately granted `clipboard.write` can be induced to move data from
  one window to another within its grants. Bounded by grants, not
  eliminated. Accepted; documented in SDK guidance.

### T2 Semantic tree spoofing (S2)
A malicious or compromised app publishes a fake tree: a "Cancel" button
labeled "OK", a node whose rect covers another window, a password field
marked `public`.
- Mitigation: node rects are clamped to the publishing surface's geometry
  by the compositor; a node can never point outside its own window.
- Mitigation: app-declared sensitivity can only *raise* the class, never
  lower it below the rule-assigned class. Rules and `policyd` own the floor.
- Mitigation: trust class per app (rule-assigned). Trees from `untrusted`
  apps are delivered with that flag; actions targeting them get stricter
  `defer` treatment.
- Residual: an app lying about its own button labels can mislead an agent
  within that app. The app could equally mislead a human. Accepted.

### T3 Compositor exploitation via protocol (S3)
Malformed requests trigger memory corruption or logic bypass in P1.
- Mitigation: Rust; fuzzing of every request handler in CI (COMP-15);
  privileged socket reachable only from `agentd` (defense in depth — a
  malicious agent must first compromise `agentd`).
- Mitigation: enforcement checks are performed on the parsed request
  *before* any state mutation; no partial application.
- Residual: logic bugs. Mitigated by the agent-protocol test suite and
  redaction proofs (COMP-15).

### T4 `agentd` compromise (S4)
An agent exploits `agentd` and inherits the privileged socket.
- Mitigation: capability sets are bound in the compositor per agent object
  at `create_agent` time from a `policyd`-signed grant; `agentd` cannot
  mint capabilities. A compromised `agentd` can misuse any *already
  granted* capability of any connected agent, nothing more.
- Mitigation: `agentd` itself runs sandboxed (no home access, no network
  unless remote MCP is enabled).
- Mitigation: high-value capabilities (`capture.secret`, `seat.focus.human`,
  `output.virtual`) additionally require per-request trusted-UI prompt or
  time-boxed grants, so even a compromised `agentd` cannot use them silently.
- Residual: aggregate of all live grants. Reduced by keeping grants
  minimal and short-lived.

### T5 Perception exfiltration (S6, S7)
Agent (or `registryd`) reads secrets from captures, trees, or clipboard and
sends them to P7 or P9.
- Mitigation: sensitivity classes enforced on every read path (tree, text,
  capture, clipboard) in the compositor. Default class is `private`;
  `secret` for password managers, auth prompts, terminals showing key
  material (rule-based, plus app-declared raise), and any node with role
  `password`.
- Mitigation: agents get `scene.read` (public+private) only when the task
  needs a human-facing app; `capture.secret` is never granted to
  general-purpose agents; `clipboard.read` is gated on the source
  toplevel's class.
- Mitigation: network egress for agents is allowlisted per agent (§7);
  exfil to arbitrary hosts fails at the sandbox.
- Mitigation: every capture is hashed and logged; every tree/text read is
  logged with the node set. Exfiltration is at least detectable.
- Residual: data sent to the *legitimate* cloud model provider is by
  definition disclosed to P7. Documented; the router lets the owner pin
  sensitive tasks to local models.

### T6 Irreversible actions (S8, A3)
Agent sends, pays, deletes, publishes, or force-pushes — by mistake or
under T1.
- Mitigation: an **irreversible-action class** (§8) is always `prompt`
  through trusted UI for general-purpose agents. Agents that need
  unattended irreversible actions get an explicit, per-target, time-boxed
  grant (e.g., "may send email to @company.com for 2 h").
- Mitigation: semantic actions carry role/name; `policyd` matches known
  irreversible verbs and app-specific rules. Coordinate clicks on
  vision-only surfaces in irreversible-capable apps default to `prompt`
  because intent cannot be inferred.
- Mitigation: request-id dedupe prevents double-execution on retry
  (COMP §0.5).
- Residual: novel irreversible actions not in the class list. Classifier
  helps; audit makes them visible after the fact. Accepted with the
  expectation that the list grows from audit data.

### T7 Agent escapes sandbox / modifies TCB (S9, S11, A4)
Agent edits policy, compositor config, its own grant, or installs packages.
- Mitigation: policy and config files are owned by the human's identity
  and mounted read-only (or not at all) in every agent sandbox. Policy
  changes require a trusted-UI confirmation even when initiated by the
  human through an agent ("apply this rule change" is itself `prompt`).
- Mitigation: no agent gets a shell with the human's uid. `launch` runs in
  the agent's sandbox and cgroup slice. Package installation is a
  `prompt`-class action.
- Mitigation: Landlock + seccomp + bubblewrap namespaces; per-agent
  writable scratch; explicit read grants for anything else.
- Residual: kernel bugs. Standard Linux residual.

### T8 Cross-agent interference and *ambient* access
Agent A reads or tampers with agent B's state, input, or windows **without a
grant**. The risk is T1 amplification: if any agent can read every other
agent by default, one injected web page compromises the whole fleet.
- Mitigation: cross-agent access is **default-deny but grantable** (§7.5).
  Isolation is the default, not the rule. Collaboration is designed for.
- Mitigation: per-agent seats (no input crossover), per-agent sandboxes and
  sockets, per-agent scene filtering (`list_toplevels` returns what the
  agent is *granted* to see — grants may include another agent's
  workspace).
- Mitigation: shared data carries a **provenance chain** (§7.5) so B knows
  a string originated with A and before that with an untrusted surface;
  `policyd` reads the chain in irreversible-action decisions, preventing a
  hijacked A from laundering untrusted content into a trusted instruction
  for B.
- Residual: within a group (§7.5), the blast radius is the group. That is
  an explicit, per-group choice by the owner.
- Residual: shared apps. Two agents granted the same browser window can
  step on each other. Atomic batches and generation checks make this a
  clean failure, not a mis-click.

### T9 Human deception via agent-driven UI (A6)
An agent (hijacked or not) draws a fake consent dialog, or manipulates a
real app to look like trusted UI, to trick the human into approving.
- Mitigation: trusted UI is compositor-drawn above everything, with a
  distinctive, configurable secret element (a phrase or image only the
  compositor knows) shown on every real prompt. Clients cannot render into
  that layer.
- Mitigation: prompts are focused only by the human seat; agents cannot
  answer or dismiss them.
- Mitigation: the persistent activity indicator shows which agent is
  active and which windows it is touching.
- Residual: human habituation ("click OK"). Prompt design must make the
  irreversible ones visually distinct and rate-limit their frequency;
  frequent prompting is a bug to be fixed by better grants, not accepted.

### T10 Model-provider or model-output attacks (S5)
Cloud output includes tool calls or content crafted to misuse capabilities;
a poisoned local classifier model.
- Mitigation: model output is P5 output; nothing changes at enforcement.
- Mitigation: ratchet rule — the classifier cannot grant.
- Mitigation: model weights are signed and pinned (S12).
- Residual: none beyond T1.

### T11 Audit tampering (A5)
Agent or compromised component deletes or alters records.
- Mitigation: agents have no path to the audit store (B2). Records flow
  compositor → `policyd` over a lossless channel; the compositor stalls the
  offending *agent* on backpressure, never drops.
- Mitigation: append-only store with hash chaining; periodic anchors
  written to a location outside any agent sandbox.
- Residual: `policyd` compromise. TCB residual.

### T12 Resource abuse (A7)
Agent spins up virtual outputs, floods input, streams captures at 60 fps,
burns cloud spend.
- Mitigation: per-agent rate limits on requests, input events, capture FPS;
  quotas on virtual outputs and workspaces; cloud spend budgets per agent in
  the router with hard stops.

### T13 XWayland (S13)
X11 apps observe each other.
- Mitigation: X11 is a separate trust domain; all X11 toplevels default to
  `private`; agents interacting with X11 apps are flagged in audit; secret
  material should not be displayed in X11 apps (documented limitation).
- Residual: inherent to X11. Accepted; XWayland is a compatibility path.

### T14 Supply chain (S12)
Malicious crate, package, or model.
- Mitigation: `cargo vet`/`cargo audit` in CI; vendored lockfile; signed
  repo; model weights hashed and pinned; AUR excluded from the TCB.
- Residual: standard.

---

## 6. Decisions This Model Settles

| Question | Decision | Basis |
|---|---|---|
| Default sensitivity class | **`private`**. `public` by rule only. | T5: fail closed on unknown apps. |
| Human keystroke logging | **Never by content.** Focus events only. | Building a keylogger into the TCB creates an A1 asset the TCB must then protect; not worth it. |
| Can app-declared sensitivity lower the class? | **No.** Raise only. | T2. |
| Are semantic trees trusted? | **No.** Data with provenance; actions still gated. | T2, B5. |
| Can `agentd` mint capabilities? | **No.** `policyd`-signed grants bound in compositor. | T4. |
| Classifier authority | **Tighten only** (ratchet). | T10. |
| Irreversible actions for general agents | **Always `prompt`.** | T6. |
| Agent network egress | **Allowlist per agent**, default: model provider endpoints only. | T5. |
| Agent filesystem | **Sandboxed**, per-agent scratch + explicit read grants; policy/config never mounted. | T7. |
| Remote agents over TCP in v1 | **No.** | Removes S10 for v1. |
| Agent access to audit | **None.** | T11. |
| Trusted-UI anti-spoof | **Owner-set secret phrase/image** on every real prompt. | T9. |

---

## 7. Sandbox Baseline (input to Security spec)

Every agent process runs under:
- systemd transient scope in slice `agents.slice/agent-<id>.slice` (cgroup
  identity is the principal identity for `launch` correlation).
- bubblewrap: new user/pid/ipc/uts/net namespaces; read-only root; tmpfs
  home; explicit bind mounts for granted paths.
- Landlock: filesystem allow-list matching the grants.
- seccomp: default deny list for `ptrace`, `mount`, `kexec`, keyring
  syscalls, raw sockets.
- Network: `slirp4netns`/`pasta` with an egress allowlist; no ingress.
- No access to `$WAYLAND_DISPLAY`, `$DISPLAY`, the privileged socket, the
  session bus (a filtered proxy if a task needs D-Bus), or `~/.ssh`,
  `~/.gnupg`, browser profile dirs, password-manager data.
- Only the MCP socket is bind-mounted in.

Apps launched *by* an agent inherit the agent's sandbox by default; a rule
may launch specific apps outside it (e.g., the human's real browser) with
the window bound to the agent's grants.

---

## 7.5 Cross-Agent Collaboration (message passing)

Decided 2026-09-04: collaboration happens through **explicit message
passing**, not shared state. Direct cross-agent scene/state grants and
agent groups are deferred (see §10).

**Model.** `agentd` hosts named channels. An agent with a `channel.post`
grant for channel C publishes structured messages; an agent with
`channel.read` for C receives them. No agent can enumerate channels it has
no grant for. Channels are the only sanctioned inter-agent path; sandboxes
still block sockets, shared memory, and filesystem crossover.

**Why messages rather than shared state.** B usually needs A's
*conclusion*, not A's scratch context. Messages are auditable, carry
provenance, are versionable, and keep the blast radius of a hijacked agent
at one agent plus whatever it posted — rather than every agent's full
context.

**Message envelope.**
```
{ id, channel, from_agent, ts, schema, body,
  provenance: [ {source: surface_handle|node|url|model|agent, trust_class,
                 ts}, ... ] }
```
**Provenance is mandatory and non-forgeable.** `agentd` stamps `from_agent`
and appends to, never replaces, the chain: if A read a `public` web surface
and posts a derived string, B receives the chain showing the untrusted
origin. An agent cannot claim cleaner provenance than its inputs.

**Policy consequences.**
- `policyd` reads the provenance chain in irreversible-action decisions.
  Content originating from an untrusted surface does not become
  trustworthy by transiting an agent. This is the specific defense against
  a hijacked A laundering an injected instruction into a trusted-looking
  instruction for B.
- Channel grants are per-agent, per-channel, directional (post/read),
  time-boxable, and audited like any capability.
- Message rate and size are quota'd per agent (T12).

**Persistence.** Channels are session-scoped by default with a bounded ring
buffer; a channel may be marked durable by rule, in which case its store
lives outside every agent sandbox and is reachable only through the grant.

**Not included.** No agent reads another's memory, prompt, scratch
filesystem, or window contents. If B needs to see A's window, that is a
scene grant — deferred per §10.

**New capabilities:** `channel.post:<name>`, `channel.read:<name>`,
`channel.create`.

---

## 8. Irreversible-Action Class (initial)

Semantic actions or synthesized inputs targeting nodes/apps that match:
- Communication send: email, chat, SMS, social post/publish, comment.
- Financial: pay, transfer, purchase, subscribe, checkout, approve invoice.
- Destructive: delete (non-trash), empty trash, format, `rm -rf`, force
  push, drop table, revoke, uninstall.
- Identity/account: change password, add device, grant permission, share,
  change 2FA, log out others.
- System: package install/remove, service enable/disable, policy or config
  edit, firmware.
- Commitments: submit form on `public`-class web surface where the form
  contains payment or identity fields; sign; agree.

Matching sources: node role+name patterns, app-specific rules, URL patterns
for browsers, shell command patterns for terminals. Coordinate-only actions
in apps that *can* perform these default to `prompt`.

---

## 9. Out of Scope for v1

- Multi-user machines.
- Physical attackers beyond screen lock.
- Malicious owner.
- Side channels (timing, GPU memory) between agents.
- Formal verification of the enforcement path (desirable later).

---

## 10. Open Decisions

1. Per-app trust classes: how many (`trusted`, `standard`, `untrusted`?)
   and who assigns (rules only, or human-promptable).
2. Time-box defaults for high-value grants (proposed: 1 h, renewable via
   prompt).
3. D-Bus policy: filtered proxy (xdg-dbus-proxy) allowlist contents.
4. Whether `registryd` runs inside the TCB (it reads secrets) or stays
   semi-trusted with capture rights only for non-secret surfaces. Proposed:
   semi-trusted; `secret` surfaces never reach it, and agents needing them
   go through a separate prompted path.
5. Prompt-frequency budget before it counts as a design defect (proposed:
   >3 prompts per task = fix the grants).
6. **Deferred collaboration mechanisms**, to revisit once channels are in
   use: (a) cross-agent scene grants (B may read A's workspace/toplevels);
   (b) agent groups with a shared principal, workspace, and grant set —
   the blast radius becomes the group, chosen explicitly per group. The
   manager/subagent pattern will likely need (b); channels are sufficient
   until it does.


---

<!-- ===== FILE: F-04_F-07_HARDWARE_REPO.md ===== -->

# F-04 Reference Hardware & F-07 Repo / CI Conventions

---

# F-04 — Reference Hardware & Performance Baseline

Decided 2026-09-05.

## 1. Reference machine

| Component | Spec |
|---|---|
| CPU | Intel Core i5, 10th gen (+ iGPU, used for Intel-path testing) |
| RAM | 64 GB DDR4 |
| GPU | NVIDIA RTX 4060 Ti, 8 GB VRAM |
| Display | **[OPEN]** refresh rate — pins the input-latency target |

All COMP-14 performance targets are measured on this machine. A number
without this machine attached to it is not a target.

## 2. GPU strategy: NVIDIA-first

Rationale: designing against the most constrained target beats retrofitting
it later, which is why NVIDIA-on-Wayland has the reputation it does.

Baseline assumptions (COMP-02 is written to these):
- **Explicit sync (`linux-drm-syncobj-v1`) is mandatory.** No implicit-sync
  fallback path is designed for or relied on.
- **No assumption of direct scanout or overlay-plane availability.**
  Composition must be correct and fast without them; they are an
  optimization detected at runtime.
- Buffer allocation via GBM with modifier negotiation; nothing
  vendor-specific.

Honest limit of this approach: NVIDIA-first does not surface Intel/AMD
quirks (modifier negotiation differences, older Intel plane limits, AMD VRR
behavior). Therefore:
- **Intel**: tested regularly on the reference machine's iGPU so the path
  does not silently rot.
- **AMD**: a v1 support target, validated before release rather than
  continuously during development.

## 3. VRAM budget (8 GB, shared with the desktop)

This is the binding constraint on the whole inference design.

| Consumer | Budget |
|---|---|
| Compositor buffers, 3 outputs @ 1440p + apps | ~2 GB |
| Browser / GUI apps in agent workspaces | ~2 GB |
| Local model (1–2B, Q8) + KV cache | ~2 GB |
| Headroom / spikes | ~2 GB |

Consequences for I-01:
- The local model gets an **explicit VRAM budget with an eviction policy**;
  it may not assume it stays resident.
- Nothing larger than ~2B at Q8 runs resident. Larger local models are a
  different-hardware story, not a v1 one.
- Model load/unload latency must be measured; if eviction thrashes, the
  router (I-02) prefers cloud over local rather than stalling the desktop.

## 4. CPU consequences

A 10th-gen i5 running compositor + four daemons + agents makes the COMP-14
targets achievable but not free. Therefore:
- Damage tracking is a **requirement**, not an optimization.
- No daemon may do per-frame work. `registryd`/`policyd` are event-driven.
- The policy hot path stays in-process, table-lookup only (S-02 §1).

## 5. Open

1. Display refresh rate (pins the ≤1-frame input-latency target: 16.7 ms
   @60 Hz vs 6.9 ms @144 Hz).
2. Whether a second test machine (AMD) is acquired before v1 or borrowed.

---

# F-07 — Repository, Branching & CI

## 1. Repository shape

**Monorepo**, Cargo workspace. The protocol crate is shared by four
components and cross-cutting changes are constant; separate repos would
mean version-juggling for no benefit at this team size.

```
/
  Cargo.toml                  # workspace
  CLAUDE.md                   # root invariants + build/test commands
  docs/                       # all specs (CHARTER, COMP-*, S-*, P-*, …)
  decisions/                  # ADRs (F-08 format)
  crates/
    abyss/                   # compositor            [TCB]
    policyd/                  # policy + audit        [TCB]
    policy-eval/              # shared evaluator crate (linked by both)
    agentd/                   # agent gateway
    registryd/                # perception aggregation
    proto-agent/              # eclipse_agent_v1 bindings (generated)
    proto-semantic/           # eclipse_semantic_v1 bindings (generated)
    audit/                    # audit store, hash chain, index, verify tool
    sandbox/                  # grant → bwrap/Landlock/seccomp compiler
    sdk-rust/                 # agent SDK
    sdk-python/               # agent SDK
  tests/
    golden/                   # S-02 golden decision suite
    wlcs/                     # conformance harness
    compat/                   # client compatibility matrix
    redteam/                  # S-10 injection & escape corpus
  fuzz/                       # cargo-fuzz targets
  bench/                      # COMP-14 benchmarks
```

## 2. Branching

Trunk-based. Short-lived branches named for the milestone they implement
(`comp16-m03-multi-output`, `s02-evaluator`). Merge on green CI. No
long-running feature branches — with one reviewer they rot.

## 3. CI

Runs on every push. Self-hosted runner on the reference machine for
anything needing a GPU; cloud runner for the rest.

**From day one:**
- `cargo build --workspace`
- `cargo clippy --workspace -- -D warnings`
- `cargo fmt --check`
- `cargo test --workspace`

**Added as they become possible:**
| Gate | Added after |
|---|---|
| `wlcs` headless conformance | COMP-16 milestone 1 |
| `cargo-fuzz` smoke run (protocol handlers) | proto crates exist |
| Golden decision suite (evaluator ≡ compositor) | S-02 implemented |
| `cargo audit` + `cargo vet` | S-12 |
| Benchmark regression gate (COMP-14 targets) | milestone 4 |
| Redteam suite (S-10) | S-01..S-07 implemented |
| Client compat matrix | milestone 7 (XWayland) |

Benchmarks gate on regression, not absolute numbers: a >10% frame-time or
latency regression fails the build.

## 4. Merge policy

| Area | Review |
|---|---|
| `abyss` enforcement path, `policyd`, `policy-eval`, `sandbox` | **Owner reads every line.** No exceptions. |
| Everything else | Merge on green CI; owner reviews at leisure |

Write this down because in six months you will not remember which crates
were TCB.

## 5. Commits & PRs

- Conventional commits (`feat(abyss): …`, `fix(policyd): …`).
- Every PR body cites the spec section it implements
  (`Implements COMP-08 §4`). This is what makes the spec→code trail
  auditable when work is delegated.
- Every PR that changes behavior specified in a doc updates the doc in the
  same PR, or explains why not.

## 6. `CLAUDE.md`

Root file states invariants; per-crate files state local context. Root
content:

```markdown
# Invariants (never violate; if a task seems to require it, stop and ask)
- No ambient authority. Every operation requires a capability check.
- Ratchet rule: classifier/defer may only tighten a decision, never grant.
- No state mutation before `check()` returns Allow.
- App-declared sensitivity may only raise a class, never lower it.
- `password`-role values are never delivered, logged, or stored.
- Human input is never logged by content.
- Specs in docs/ are authoritative. If code and spec disagree, that is a
  bug in one of them — do not silently pick.

# Build / test
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings

# Where things are
docs/COMPOSITOR.md      compositor master spec
docs/COMP-08_*.md       agent protocol
docs/S-01_*.md          capability model
...
```

Per-crate `CLAUDE.md` names the governing spec, the crate's invariants, and
whether it is TCB.

## 7. Open

1. Self-hosted runner setup (the reference machine is also the dev machine;
   CI competing with dev work for the GPU).
2. Whether `docs/` is the source of truth or mirrors an external planning
   repo. Proposed: `docs/` is source of truth once code starts.


---

<!-- ===== FILE: F-05_LICENSING.md ===== -->

# F-05 — Licensing, CLA & Commercial Terms (Draft v0.1)

Decided 2026-09-05. Depends on: F-01. Consumed by: F-07, X-08, D-02.

**Not legal advice.** This records intent. Before taking money from any
organization, have a lawyer review the licence texts, the CLA, and the
trademark position.

---

## 1. Decision

**AGPLv3 for all first-party code, with a contributor licence agreement,
dual-licensed commercially.**

- Everyone may use, modify, and redistribute EclipseOS under AGPLv3, at no
  cost, forever. No feature gating, no nag screens, no time limits, no
  telemetry-based enforcement.
- Organisations that will not or cannot comply with AGPLv3 — or that want
  support, indemnity, or proprietary modifications — buy a commercial
  licence.
- Nothing breaks if nobody pays. The paid path exists because AGPL
  compliance is something legal departments cannot ignore, not because the
  software degrades. (This is the WinRAR spirit with actual leverage:
  WinRAR's honour system works only because the source is closed and cannot
  be forked; an open-source project has no such lock, so the leverage has
  to come from the licence itself.)

## 2. Why AGPL and not the alternatives

| Option | Rejected because |
|---|---|
| MIT / Apache | No revenue mechanism; anyone may ship a closed fork. |
| GPLv3 | Companies may modify and use internally without publishing; the paid path is weak for an OS nobody runs as a service. |
| Open core | Closing security-relevant components (classifier, policy rules) undercuts the trust story the product depends on. |
| BSL / FSL (source-available) | Not OSI open source. For an OS asking users to trust it with system-level agent control, the resulting fight over the "open source" label is more expensive than the extra revenue. |
| Proprietary + nagware (WinRAR) | Contradicts open source; a nag screen in the compositor of a security product is untenable. |

AGPL's network clause matters here specifically for the case worth
charging for: a company building a hosted agent service on top of
EclipseOS must publish its modifications or buy a licence.

## 3. Scope — what is AGPL

**All of it, including everything security-relevant.** The TCB must be
auditable or the product's central claim is unverifiable.

| Component | Licence |
|---|---|
| `abyss`, `policyd`, `policy-eval`, `sandbox`, `agentd`, `registryd` | AGPLv3 |
| Protocol definitions (`eclipse_agent_v1`, `eclipse_semantic_v1`) | **Permissive (Apache-2.0 or MIT)** — see §4 |
| SDKs (`sdk-rust`, `sdk-python`) | **Permissive (Apache-2.0)** — see §4 |
| Toolkit bridges (P-03), upstreamed patches | Licence of the upstream project |
| Default policy rulesets, classifier weights | AGPLv3 / open weights (§5) |
| Documentation | CC BY-SA 4.0 |
| Packaging, ISO build scripts | AGPLv3 |

## 4. Why SDK and protocol are permissive

An agent linking the SDK would otherwise be forced to AGPL. That kills
third-party agent development, which is the ecosystem the project needs.
Protocol definitions and client SDKs are therefore Apache-2.0 so anyone may
write agents under any licence. The line: **anything that runs as part of
the system is AGPL; anything an external agent links against is Apache-2.0.**

Apache-2.0 rather than MIT for the patent grant.

## 5. Classifier weights

Weights are trained on the owner's audit data (S-04 §6, I-04). Default:
released under AGPLv3 alongside the code, or a permissive open-weights
licence if a court-tested weights licence emerges. Not withheld as an
open-core component — a security decision component that users cannot
inspect contradicts §3.

**[OPEN]** Whether models trained on *other users'* contributed audit data
(if that ever exists) need separate terms and consent. Revisit before any
data collection beyond the owner's own machine.

## 6. Contributor Licence Agreement

Dual-licensing is impossible without holding the rights, so a CLA is
required **before accepting the first external contribution.** Retrofitting
one is often impossible.

- Form: Apache ICLA-derived, granting the project a perpetual,
  irrevocable copyright and patent licence with the right to relicense.
  Contributors retain their copyright.
- Corporate CCLA for contributions made on company time.
- Mechanism: CLA-bot on pull requests; no merge without a signature.
- Alternative considered: DCO only (no relicensing rights), which would
  forfeit dual-licensing. Rejected given §1.
- Honesty requirement: the CLA's purpose — that the project sells
  commercial licences of contributed code — is stated plainly in
  CONTRIBUTING.md. Contributors deserve to know before they sign.

## 7. Commercial licence — what it includes

Not defined in detail until there is a customer, but the shape:
- Proprietary-terms licence removing AGPL obligations.
- Support with response-time terms.
- Indemnity.
- Not: extra features, earlier releases, or anything that makes the AGPL
  version second-class. Feature parity is a commitment.

**[OPEN]** Pricing, and whether a size threshold (e.g. under N employees or
revenue) is licence-free by policy.

## 8. Trademark

The name and logo are **not** licensed by the AGPL. Standard practice
(Firefox/Iceweasel, Red Hat/CentOS): forks may use the code, not the brand.

Status: a USPTO wordmark search for "EclipseOS" returns no results
(2026-09-05). This is not a clearance — it does not cover the registered
"ECLIPSE" marks in software classes, and infringement turns on likelihood
of confusion, not exact string match. Accepted risk for a personal project;
**get a professional clearance search before taking organisational money.**

**[OPEN]** Whether to file for the mark, and whether to hold it personally
or in an entity.

## 9. Third-party obligations

- Smithay (MIT) and most Rust crates are permissive — compatible.
- Any GPLv2-only dependency is **incompatible with AGPLv3** and must be
  avoided or replaced; `cargo deny` gates this in CI (F-07 §3, S-12).
- Linux kernel patches (D-01) are GPLv2 and stay GPLv2, distributed
  separately from AGPL userspace.

## 10. Files to create

`LICENSE` (AGPL-3.0), `LICENSE-APACHE` (SDK/protocol crates), `NOTICE`,
`CONTRIBUTING.md` (CLA rationale), `TRADEMARK.md`, SPDX headers on every
source file, `cargo-deny` config.

## 11. Open Items

1. Legal review of licence texts and CLA before commercial sales.
2. Professional trademark clearance before organisational revenue (§8).
3. Commercial pricing and any small-org exemption (§7).
4. Terms for models trained on contributed data (§5).
5. Entity formation — needed to hold copyright assignments and sell
   licences; sole proprietorship works initially.


---

<!-- ===== FILE: F-06_F-08_GLOSSARY_ADR.md ===== -->

# F-06 Glossary & F-08 Decision Records

## F-06 — Glossary (canonical terms; use these exactly in all specs and code)

| Term | Meaning | Not to be confused with |
|---|---|---|
| **Principal** | An identity that can hold capabilities: `human`, `agent:<id>`, `system:<daemon>`, later `group:<id>` | user (a Unix uid) |
| **Agent** | An untrusted program controlling the desktop through `agentd`; sandboxed; holds grants | model (the LLM an agent calls) |
| **Capability** | Named permission (`scene.tree`, `seat.key`…), optionally scoped | grant |
| **Grant** | Signed, time-boxed statement that a principal holds capabilities under scopes/quotas | capability |
| **Scope** | Constraint on targets a capability applies to | sensitivity class |
| **Sensitivity class** | `public` / `private` / `secret`, assigned per toplevel and per node; floor set by rules | trust class |
| **Trust class** | `trusted` / `standard` / `untrusted`, per app, describes how much to believe its content/tree | sensitivity class |
| **Handle** | Stable `u64` id of a toplevel for the session | pid, node id |
| **Node** | Element of a semantic tree | widget (toolkit-internal object) |
| **Generation** | Monotonic counter per handle bumped on structural/geometry change | `value_rev` (text-only changes) |
| **Seat** | A Wayland input identity; one human seat, one per agent | keyboard/pointer device |
| **Atomic batch** | Input sequence applied with no interleaving or focus change | transaction (no rollback of app effects) |
| **Enforcement table** | Compiled policy the compositor evaluates in-process | policy file |
| **Outcome** | `allow / deny / prompt / defer` | result status |
| **Prompt** | Trusted-UI consent request to the human | LLM prompt |
| **Defer** | Slow-path decision by `policyd`, tighten-only | prompt |
| **Ratchet rule** | Classifier/defer may only tighten, never grant | — |
| **Provenance** | Ordered chain of sources a piece of data passed through | audit |
| **Channel** | Named message stream between agents in `agentd` | Wayland event stream |
| **Trusted UI** | Compositor-drawn surfaces above all clients | layer-shell |
| **TCB** | `abyss` + `policyd` | `agentd`, `registryd` (semi-trusted) |
| **Perception** | Reading system state (scene, tree, text, capture) | vision (one perception source) |
| **Action** | Semantic action on a node (`activate`…) or synthesized input | irreversible action (a policy taxonomy) |
| **Irreversible action** | Taxonomy id (`communication.send`…) matched by policy | destructive (subset) |
| **Human override** | Reserved chord pausing all agent seats | screen lock |

Process names: `abyss` (compositor), `agentd`, `registryd`, `policyd`.
Protocol names: `eclipse_agent_v1`, `eclipse_semantic_v1` (rename with
project name per F-03).

---

## F-08 — Architecture Decision Records

Location: `decisions/NNNN-slug.md`. One decision per file. Never edited
after acceptance except to change status; superseding is a new ADR.

```markdown
# NNNN — <Title>
Status: proposed | accepted | superseded by NNNN | rejected
Date: YYYY-MM-DD
Deciders: <owner>, Claude (advisory)

## Context
What forces are at play; what we know; what we don't.

## Options
1. …  (pros / cons)
2. …

## Decision
What we chose, in one paragraph.

## Consequences
What becomes easier, harder, or forbidden. What we now owe (tests, docs).

## Revisit when
Concrete trigger that should reopen this.
```

Seeded ADRs (from decisions already made):
```
0001 Rust as implementation language
0002 Custom compositor on Smithay, not a fork
0003 Custom Wayland protocol for agent control, not libei/portal
0004 MCP-compatible agent surface from day one
0005 Dual license AGPLv3 + commercial (tentative)
0006 No ISO-first; compositor first
0007 Agent seats default, compat lock fallback
0008 Policy enforced in-process from compiled table; defer is tighten-only
0009 Trusted UI compositor-drawn, never layer-shell
0010 WM mode only, no DE in v1
0011 Hyprland-style human UX; agent workspaces untiled
0012 Default sensitivity private; app-declared class raise-only
0013 Human keystrokes never logged by content
0014 Cross-agent collaboration via channels with non-forgeable provenance
0015 registryd outside TCB; never receives secret surfaces
```


---

<!-- ===== FILE: COMPOSITOR.md ===== -->

# Compositor — Master Design Specification (Draft v0.1)

Component codename: `abyss` (placeholder; see charter §6 name decision).
Language: Rust. Foundation: Smithay. Event loop: calloop.
Scope: Charter Phases 1 (daily-driver compositor) and 2 (agent protocol).

Sub-documents to be written (one per §):
COMP-01 Core & Backend · COMP-02 Rendering · COMP-03 Outputs · COMP-04 Input
& Seats · COMP-05 Window Management · COMP-06 Standard Protocols · COMP-07
XWayland · COMP-08 Agent Protocol (`eclipse_agent_v1`) · COMP-09 Semantic
Protocol (`eclipse_semantic_v1`) · COMP-10 Trusted UI · COMP-11 Policy
Enforcement Hooks · COMP-12 Audit & Provenance · COMP-13 IPC & Configuration ·
COMP-14 Performance · COMP-15 Testing & Fuzzing · COMP-16 Milestones.

---

## 0. Design Thesis

Every existing compositor treats input as coming from one human at one seat,
and treats the screen as an opaque array of pixels. This compositor treats:

- **Input as multi-principal.** Humans and agents are separate principals
  with separate seats, separate focus, and separate permissions. Their
  input never silently interleaves.
- **Surfaces as semantic objects.** Each toplevel has an identity, an owner
  process, a title, an app id, a geometry in global space, a *sensitivity
  class*, and (when the app cooperates) a live element tree.
- **Capture and injection as audited, capability-gated operations**, never
  ambient abilities.
- **The compositor as the only trusted UI.** Consent prompts and security
  indicators are drawn by the compositor itself in a layer clients cannot
  cover or spoof.

Everything below follows from these four commitments.

---

## 0.5 Perception & Reliability Model (cross-cutting)

### Sources of truth, in preference order
1. **Compositor**: surface geometry (global logical px), scale, stacking,
   focus, app id, title, pid, popups. Known exactly, no image analysis.
2. **Native semantic tree** (`eclipse_semantic_v1`): apps publish widgets
   and actions; stored in the compositor; queries are memory reads.
3. **AT-SPI2 tree** via `registryd`: browsers expose the DOM a11y tree,
   GTK/Qt expose widget trees, terminals expose text grids.
4. **Vision** via `registryd`: only for surfaces/nodes with no semantic
   data (canvas, games, custom-drawn). Never the default.

### The coordinate join
AT-SPI on Wayland cannot report global element positions (clients don't
know their window position). `registryd` joins surface-local element rects
with compositor toplevel geometry and scale to yield clickable global
coordinates. This join is only possible with compositor cooperation and is
a core reason the compositor is the center of the design.

### Action preference
1. Semantic action by node id (`activate`, `set_value`, `focus`) — app
   performs it natively; immune to layout shift, overlap, animation, scale.
2. Text commit via `text_input_v3` to a focused field.
3. Synthesized pointer/keyboard at coordinates — fallback only.

### Reliability guarantees (compositor-provided)
- Stable `u64` surface handles, never reused per session; node ids stable
  within a tree generation.
- **Generation-checked actions**: tree queries return a generation; actions
  carry the observed generation; mismatch → `stale_generation`, no
  execution. Check-and-act.
- **Atomic batches**: no human input or focus change interleaves; target
  loss aborts the whole batch with a report.
- **`wait_for`**: server-side conditions (toplevel appears, title matches,
  semantic predicate) replace polling and sleeps.
- **Request-id deduplication**: a retry with the same agent request id
  within the dedupe window returns the cached result and does not
  re-execute. Exactly-once *input delivery*.
- **Result events carry post-action state** (focus handle, generation) to
  avoid follow-up queries.
- **Change detection without a model**: damage events and capture content
  hashes.

### Explicit non-guarantee
App-level idempotence (clicking "Send" twice sends twice) is not a
compositor property. The compositor guarantees the intended input reached
the intended target as it existed when observed, exactly once. Gating
repeatable-vs-irreversible actions is `policyd`'s job (HITL rules).

---

## 1. Core Architecture & Backend (COMP-01)

### 1.1 Process model
Single compositor process. Privileged helper processes are separate:

| Process | Role | Privilege |
|---|---|---|
| `abyss` | Compositor: DRM, input, rendering, protocol servers | User, with DRM/input via logind/seatd |
| `agentd` | Agent gateway: speaks `eclipse_agent_v1` to compositor as a privileged client, exposes MCP to agents | User; holds the agent socket |
| `registryd` | Semantic aggregation (AT-SPI2 + `eclipse_semantic_v1` + vision fallback) | User; separate doc (Perception) |
| `policyd` | Compiles policy, serves decisions, stores audit | User; separate doc (Security) |

Rationale: compositor crash = session loss, so the compositor stays small.
Anything that can crash without killing the display lives outside.

### 1.2 Smithay modules used
- `backend::drm` (KMS/atomic), `backend::session::libseat`, `backend::udev`
- `backend::libinput` for input devices
- `backend::renderer::gles` (v1), `backend::renderer::vulkan` (later)
- `backend::winit` and `backend::headless` for development and CI
- `wayland::compositor`, `shell::xdg`, `shell::wlr_layer`, `seat`, `output`,
  `shm`, `dmabuf`, `presentation`, `viewporter`, `fractional_scale`,
  `xdg_decoration`, `pointer_constraints`, `relative_pointer`,
  `input_method`, `text_input`, `selection::data_device`,
  `selection::primary_selection`, `session_lock`, `idle_inhibit`,
  `idle_notify`, `foreign_toplevel_list`, `security_context`,
  `xdg_activation`, `keyboard_shortcuts_inhibit`
- `desktop::{Space, Window, LayerSurface, PopupManager}`
- `xwayland::{XWayland, X11Wm}`

Verify against current Smithay at project start: screencopy /
`ext-image-copy-capture` support status determines whether capture is
implemented via Smithay's helpers or our own renderer path.

### 1.3 Startup sequence
1. Acquire session (libseat), enumerate GPUs (udev), pick primary render node.
2. Init renderer, load cursor theme, load config.
3. Create Wayland display and socket `wayland-N`; create **second** listening
   socket `abyss-agent-N` restricted by filesystem permissions and used only
   by `agentd`.
4. Start XWayland lazily on first X11 client.
5. Spawn `agentd`, `registryd`, `policyd` as systemd user units (compositor
   does not exec them directly; it signals readiness via `sd_notify`).
6. Restore output layout and workspace state from previous session.

### 1.4 Crash resilience
- Compositor state snapshot (outputs, workspaces, toplevel → workspace
  mapping) written on change to `$XDG_RUNTIME_DIR/abyss/state`.
- On restart, clients are gone (Wayland limitation), but layout is restored
  so re-launched apps land where they were.
- Panic handler: log, dump state, attempt to switch VT so the machine is not
  bricked.

Open: whether to pursue compositor hot-restart preserving clients (requires
client-side support via `wp_linux_drm_syncobj` + session-restore protocols;
not v1).

---

## 2. Rendering (COMP-02)

### 2.1 Pipeline
- Damage-tracked rendering: only redraw dirty regions per output.
- Direct scanout for fullscreen and single-surface cases (bypass composition).
- Overlay planes for cursor and, where hardware allows, video surfaces.
- Explicit synchronization (`linux-drm-syncobj-v1`) required for correctness
  on modern NVIDIA/Intel; implicit sync fallback.
- Fractional scaling per output with `wp_fractional_scale_v1`; integer
  scale fallback for legacy clients via `wp_viewporter`.
- VRR/adaptive sync per output, toggleable.
- Color: sRGB only in v1. HDR and color management are Phase 7 candidates.

### 2.2 Agent-specific rendering requirements
- **Off-screen render targets for agent workspaces**: agents may operate
  windows on a virtual output that is never scanned out. Rendering cost
  must be bounded (render on demand for capture, not every frame).
- **Per-surface capture path** that renders a single toplevel (and its
  popups) to a buffer without neighbors, cursor, or decorations, at a
  requested scale and format. This is the primary vision-fallback input.
- **Redaction compositing**: surfaces with sensitivity `secret` are replaced
  by a solid placeholder in any capture not authorized for that class.
- **Trusted overlay layer**: rendered last, above all client layers
  including `overlay` layer-shell surfaces, cannot be covered.

### 2.3 Renderer abstraction
Trait over GLES and Vulkan so v1 ships GLES and Vulkan can land without
touching protocol code.

---

## 3. Outputs (COMP-03)

- Hotplug, mode setting, arbitrary layout in global coordinate space.
- Per-output: scale, transform, VRR, position, enabled, preferred mode.
- Output configuration via `wlr-output-management` for existing tools and
  via native IPC.
- **Virtual outputs**: created at runtime, not connected to a connector, any
  resolution, used for agent workspaces and headless testing. A virtual
  output is a first-class output to clients; they cannot tell the difference.
- Output identity persisted by EDID hash + connector so layouts survive
  reboots.
- Power management (DPMS) with idle-notify integration; agents can hold an
  idle inhibitor **only through a capability**.

---

## 4. Input & Seats (COMP-04)

### 4.1 Multi-seat model
- **Human seat** `seat0`: all physical devices from libinput.
- **Agent seats** `agent-<id>`: created per agent connection, each with a
  virtual keyboard, virtual pointer, and optional virtual touch. Each has
  its own keyboard and pointer focus.
- Consequence: an agent can type into window B while the human types into
  window A. Focus for the human is unaffected by agent activity.
- Known risk: some clients (notably GTK3 and older Qt) mishandle a second
  seat. Mitigation: a per-app compatibility flag that routes agent input
  through the human seat with focus-stealing under a lock (§4.4).

### 4.2 Human input
- libinput: keyboards, mice, touchpads (gestures), touchscreens, tablets
  (basic; full tablet protocol later).
- Keymaps via xkbcommon; per-seat layout; runtime switching.
- Pointer constraints and relative pointer for games/CAD.
- Keyboard shortcuts inhibitor honored, with a compositor-reserved escape.
- Compositor keybindings: configurable, evaluated before client delivery.

### 4.3 Synthetic input (agents)
- Injection primitives: key press/release by keycode or by keysym with
  automatic keymap mapping; text commit (via text-input protocol when
  supported by the focused client, else keysym sequence); pointer motion
  absolute (global coords) or relative; button; axis (scroll) discrete and
  continuous; touch down/up/motion.
- Every synthetic event carries a **provenance tag** internally (agent id,
  request id) that flows into audit. Clients do not see the tag (Wayland has
  no field for it); the seat identity is the client-visible signal.
- Rate limiting per agent seat, configurable, to prevent input floods.
- **Atomic sequences**: a batch of input events applied within one frame
  with the guarantee that no human input interleaves and no focus change
  occurs mid-batch. If focus changes (window closed), the batch aborts and
  reports the failure.

### 4.4 Focus arbitration
- Human focus follows the human seat only. Agents cannot move human focus
  without the `focus.human` capability, which triggers trusted UI
  notification.
- Agent focus is per agent seat and set explicitly by request.
- Focus-steal lock (compat mode): agent acquires exclusive focus on a
  toplevel for N ms; human input to that toplevel is queued, not dropped,
  and delivered after release. Trusted UI shows an indicator.

### 4.5 Human override
- Reserved global chord (default `Super+Escape`) that cannot be inhibited
  by any client or agent: pauses all agent seats, surfaces the agent
  activity panel. This is the emergency brake and is implemented in the
  compositor input path before any client delivery.

---

## 5. Window Management (COMP-05)

### 5.1 Model
- Workspaces per output, dynamic. Each workspace has an **owner
  principal**: human or a specific agent. Agent-owned workspaces are hidden
  from the human's workspace switcher by default and may live on virtual
  outputs.
- Layout, human workspaces (decided 2026-09-04, Hyprland-style UX):
  **dwindle** (binary-split tiling) as default, **master** as alternative,
  floating layer, gaps, per-workspace layout selection. "Hyprland-style"
  is a statement about human UX only; nothing from Hyprland's code,
  architecture, or IPC is inherited.
- Layout, agent workspaces (proposed): **no tiling.** Each toplevel gets
  its requested size, placed non-overlapping on a virtual output that
  grows to fit; no gaps, no animations, no decorations. Geometry must not
  change between an agent's query and its action unless the client itself
  changes it. Layout engine is a trait so both coexist.
- Visuals (Hyprland-style): animations (open/close/move/workspace switch),
  rounded corners, borders with active/inactive colors, shadows, dim
  inactive, background blur for transparent surfaces and layer-shell.
  Blur is multi-pass and must cooperate with damage tracking and disable
  direct scanout for affected regions; scheduled as Phase 1 stretch
  (milestone 9b), not in the daily-driver exit gate.
- Rules engine: match on app id, title, pid, cgroup, launching principal →
  workspace, floating, size, opacity, sensitivity class, seat compat flags.

### 5.2 Toplevel identity
Every toplevel gets a stable **surface handle** (`u64`, never reused within
a session) exposed to agents. Associated metadata:

```
handle, app_id, title, pid, cgroup, launching_principal,
workspace, output, geometry (global logical px), scale,
state {focused_by: [seats], minimized, maximized, fullscreen, floating},
sensitivity: {public | private | secret},
semantic: {none | atspi | native}, parent (for dialogs), children (popups)
```

`launching_principal` is set when the app was spawned via the compositor's
launch request (agent or human); otherwise inferred from cgroup when the
process was started by `agentd`'s per-agent slice.

### 5.3 Agent-driven window operations
Move, resize, set workspace, set output, minimize, close (graceful
`xdg_toplevel.close`), fullscreen, float — all capability-gated and
audited. Killing a client process is **not** a compositor operation; it goes
through `policyd`.

### 5.4 Launch
`launch(argv, env, principal, workspace_hint)` runs the process inside a
systemd transient scope under the agent's slice, so cgroup ↔ principal
mapping is exact. The first toplevel from that pid is associated with the
request and returned to the agent. Timeout configurable.

---

## 5.5 Shell mode

**WM mode only** (decided 2026-09-04). Config file + user-chosen
bar/launcher/notifier via layer-shell and the human IPC (§13). No native
DE, no DE-mode IPC. The compositor should not preclude a shell process
later — keep the human IPC surface clean and the config schema
machine-readable — but no work is spent on it now.

---

## 6. Standard Protocol Support (COMP-06)

Required for daily-driver parity (Phase 1 exit):

| Protocol | Notes |
|---|---|
| `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output`, `wl_data_device` | Core |
| `xdg_shell`, `xdg_decoration`, `xdg_activation`, `xdg_output` | Windows |
| `wlr_layer_shell` | Bars, notifications, lockers |
| `zwp_linux_dmabuf_v1`, `linux-drm-syncobj-v1` | GPU buffers |
| `wp_presentation`, `wp_viewporter`, `wp_fractional_scale_v1` | Rendering |
| `zwp_pointer_constraints`, `zwp_relative_pointer` | Games, CAD |
| `text_input_v3`, `input_method_v2` | IMEs; also the preferred agent text path |
| `zwp_primary_selection`, `wlr_data_control` | Clipboard managers |
| `ext_session_lock_v1` | Screen lock |
| `ext_idle_notify_v1`, `zwp_idle_inhibit` | Power |
| `ext_foreign_toplevel_list_v1` | Taskbars; also read by `registryd` |
| `wp_security_context_v1` | Sandboxed clients (Flatpak) |
| `zwp_tablet_v2` | Basic tablet |
| `wlr_output_management`, `wlr_output_power_management` | Existing config tools |
| `wlr_screencopy` / `ext_image_copy_capture_v1` | Screen sharing via xdg-desktop-portal; **gated by capability and sensitivity** |
| `zwlr_virtual_pointer`, `zwp_virtual_keyboard` | **Disabled by default.** Ambient injection is exactly what we are replacing. Enabled only for whitelisted clients. |

Clipboard: agents read/write the clipboard through `eclipse_agent_v1`, not
`wlr_data_control`, so it is audited and can be gated by sensitivity.

---

## 7. XWayland (COMP-07)

- Rootless XWayland via Smithay `X11Wm`, started lazily.
- X11 windows get surface handles like native toplevels. Semantic layer is
  weaker (AT-SPI via the X11 app if it supports it; otherwise vision).
- Scaling: XWayland scaled by compositor; optional per-app "let X11 handle
  DPI" mode.
- X11 clients cannot participate in `eclipse_semantic_v1`; they are always
  `semantic: atspi|none`.
- Security: X11 clients can snoop each other. Policy default: X11 windows
  are `private` and X11 is a separate trust domain; an agent talking to an
  X11 app is flagged in audit.

---

## 8. Agent Protocol `eclipse_agent_v1` (COMP-08)

### 8.1 Transport and trust
- A Wayland protocol served **only** on the `abyss-agent-N` socket.
  Connections on the normal socket never see these globals.
- `agentd` is the sole intended client. It authenticates agents, assigns
  agent ids and capability sets (from `policyd`), and multiplexes.
- Each agent gets a distinct protocol object tree so the compositor can
  attribute every request to an agent id for audit and rate limiting.

### 8.2 Object model
```
eclipse_agent_manager_v1
  ├─ create_agent(id, capabilities) → eclipse_agent_v1
eclipse_agent_v1
  ├─ get_seat() → eclipse_agent_seat_v1
  ├─ get_scene() → eclipse_scene_v1
  ├─ get_capture() → eclipse_capture_v1
  ├─ get_clipboard() → eclipse_clipboard_v1
  ├─ get_launcher() → eclipse_launcher_v1
  ├─ get_workspace_ctl() → eclipse_workspace_v1
  ├─ subscribe(event_mask) → eclipse_events_v1
  └─ destroy
```

### 8.3 `eclipse_scene_v1` — perception
Requests:
- `list_outputs` → outputs with geometry, scale, virtual flag.
- `list_toplevels(filter)` → array of toplevel metadata (§5.2), filter by
  workspace/output/app_id/principal/visibility.
- `get_toplevel(handle)` → full metadata + child popups + decoration geometry.
- `get_tree(handle, depth, filter)` → native semantic tree if the app speaks
  `eclipse_semantic_v1` (§9); otherwise returns `semantic: atspi|none` and
  the agent goes to `registryd`.
- `hit_test(x, y)` → (handle, local coords, semantic node id if available).
- `get_text(handle, node)` → text content for native semantic nodes.
- `wait_for(condition, timeout)` → server-side wait on toplevel
  appearance, title change, focus, or semantic node predicate. Avoids
  polling; a key latency win.

All geometry is in global logical pixels; physical pixel conversion is
provided per output.

### 8.4 `eclipse_agent_seat_v1` — action
- `focus(handle)`; `key(keycode|keysym, state)`; `text(string)`;
  `pointer_motion(abs|rel)`; `button(code, state)`; `axis(...)`;
  `touch(...)`; `begin_atomic` / `commit_atomic` / `abort_atomic`.
- `click(handle, node|point)`: composite convenience — focus, move, press,
  release, all atomic, all one audit record.
- Every request returns a `result` event with `request_id`, status, and
  the post-action focus handle so agents do not re-query.
- Errors are structured: `no_capability`, `rate_limited`, `focus_lost`,
  `sensitivity_denied`, `policy_denied(reason_code)`, `client_gone`.

### 8.5 `eclipse_capture_v1`
- `capture_toplevel(handle, scale, format, region?)` → dmabuf or shm buffer.
- `capture_output(output, ...)`; `capture_region(global_rect)`.
- Redaction applied per sensitivity and capability (§2.2).
- Cursor excluded by default; opt-in.
- Continuous capture (`stream`) exists for vision agents, damage-driven,
  with a max FPS per agent.

### 8.6 `eclipse_clipboard_v1`
`read(mime)`, `write(mime, data)`, `list_mimes`; audited; gated by
sensitivity of the source toplevel of the current selection.

### 8.7 `eclipse_launcher_v1`
`launch(argv, env, workspace_hint, principal)` per §5.4. Returns a
`launched(handle)` or `timeout` event.

### 8.8 `eclipse_workspace_v1`
Create/destroy agent workspaces, move toplevels, create virtual outputs
(capability `output.virtual`).

### 8.9 `eclipse_events_v1`
Subscription mask: `toplevel_added/removed/changed`, `focus_changed(seat)`,
`workspace_changed`, `output_changed`, `semantic_changed(handle)`,
`damage(handle)` (throttled), `human_override`, `policy_prompt_result`.
Events carry serials for ordering against `result` events.

### 8.10 Capabilities (initial set; finalized by threat model)
```
scene.read            scene.read.private   scene.read.secret
scene.tree            scene.text
seat.key              seat.pointer.motion  seat.pointer.button
seat.text             seat.lease.release   seat.lease.take
seat.focus.agent      seat.focus.human     seat.atomic
capture.toplevel      capture.output       capture.stream
capture.private       capture.secret
clipboard.read        clipboard.write
launch                workspace.manage     output.virtual
window.move_resize    window.close         idle.inhibit
```
Capabilities are granted per agent by `policyd`, may be time-boxed, may
require a trusted-UI prompt on first use, and are enforced **inside the
compositor** on every request (deterministic; no model in the loop).

### 8.11 Versioning
Protocol is versioned per Wayland conventions. Breaking changes bump the
interface name (`_v2`). `agentd` translates between MCP tool schema
versions and protocol versions so agents are insulated.

---

## 9. Semantic Protocol `eclipse_semantic_v1` (COMP-09)

For cooperating applications (our own, and patches upstreamed to toolkits).

- A client attaches a `eclipse_semantic_surface_v1` to an `xdg_toplevel` and
  publishes a node tree: `node_id, role, name, description, value, states,
  rect (surface-local), actions[], children[]`.
- Incremental updates (`node_changed`, `node_added`, `node_removed`) with a
  generation counter; the compositor stores the tree and serves it to
  agents via `get_tree`, so a query is a memory read, not an IPC round trip
  to the app.
- Actions: `activate`, `set_value`, `focus`, `scroll_to`, `expand`, plus
  custom. The compositor forwards an `action_requested` event to the client;
  the client performs it natively. This is more reliable than synthesized
  clicks and is preferred by `agentd` when available.
- Sensitivity per node (`secret` for password fields) so redaction is
  granular, not per-window.
- Roles and states mirror the AT-SPI2/ARIA vocabulary so `registryd` can
  unify native and AT-SPI trees without translation loss.
- GTK4 and Qt6 bridges (shipping an a11y backend that speaks this protocol)
  are Perception-workstream deliverables, not compositor ones, but the
  protocol is designed here so they have a target.

---

## 10. Trusted UI (COMP-10)

The compositor's own rendering, above all clients. Not a layer-shell client
(those can be spoofed by any client that binds layer-shell).

Surfaces:
- **Agent activity indicator**: persistent, small, per output. Shows which
  agents are active, which seats have focus where, and pulses on injection.
  Cannot be disabled while any agent seat exists.
- **Consent prompts**: capability first-use, sensitivity escalation,
  irreversible-operation confirmation (as classified by `policyd`). Rendered
  with a compositor-owned keyboard grab on the human seat; agents cannot
  answer them (their seats are excluded from the prompt's focus).
- **Focus-steal indicator** during compat-mode locks.
- **Emergency panel** on human override: list of agents, pause/resume/kill.
- **Sensitivity badge** on windows classified `secret` when an agent with
  `scene.read` is connected, so the human knows what is hidden.

Rendering uses a minimal internal widget set; no toolkit dependency inside
the compositor process.

---

## 11. Policy Enforcement Hooks (COMP-11)

Division of labor:
- `policyd` **compiles** policy (rulesets, capability grants, sensitivity
  classification of apps, HITL rules) into a compact table and pushes it to
  the compositor over IPC on change.
- The compositor **enforces** the table on every request synchronously,
  in-process, with no IPC in the hot path. Denials are immediate.
- Requests that the table marks `prompt` are suspended, a trusted-UI prompt
  is shown, and the request resumes or fails on the human's answer.
  Suspended requests have a timeout.
- Requests that the table marks `defer` are sent to `policyd` for a
  decision (the slow path: classifier-assisted, may consult context). The
  compositor bounds this with a timeout and a fail-closed default.
  **Ratchet rule:** a `defer` decision may resolve to `deny` or `prompt`,
  or fall through to the table's underlying `allow`; the classifier can
  only tighten, never grant. A compromised or fooled classifier therefore
  degrades to rules-only enforcement, never to expanded access.
- Sensitivity classification of a toplevel is set by rule (app id, title
  pattern), by the app itself via `eclipse_semantic_v1`, or by `policyd` at
  runtime; the compositor stores the current class and applies it to
  capture, tree, text, and clipboard paths.

Assumption pending threat model: default sensitivity is `private`;
`public` must be opted into by rule.

---

## 12. Audit & Provenance (COMP-12)

- Every `eclipse_agent_v1` request produces an audit record:
  `{ts, agent_id, request_id, interface, request, args (with secrets
  elided), target handle(s), policy decision, result, latency}`.
- Every synthetic input event is linked to its request id; every focus
  change records which seat caused it.
- Records are emitted to `policyd` over a lossless local channel
  (SOCK_SEQPACKET, backpressure → compositor stalls the *agent*, never the
  human).
- Capture requests record a content hash of the delivered buffer so a later
  investigation can prove what an agent saw.
- Human input is **not** logged by content (keystrokes), only by focus
  events, to avoid building a keylogger. Threat model to confirm.

---

## 13. IPC & Configuration (COMP-13)

- Config file: KDL or TOML (decision open; KDL is the niri precedent and
  reads well for rules). Hot-reload on change with validation; invalid
  config keeps the last good one.
- Human-facing IPC: Unix socket with a JSON-RPC surface for bars, launchers,
  and scripts: list workspaces, focus, move, output config, reload. This is
  *not* the agent path and has no injection ability.
- `agentd` IPC: the privileged Wayland socket (§8.1). `agentd` exposes MCP
  over `$XDG_RUNTIME_DIR/abyss/mcp.sock` and optionally TCP+auth for
  remote agents (off by default).
- `policyd` IPC: policy table push, `defer` decisions, audit stream.

---

## 14. Performance Targets (COMP-14)

Measured on reference hardware (charter open item). Targets, not hopes:

| Metric | Target |
|---|---|
| Human input → frame submitted | ≤ 1 frame (≤ 16.7 ms @ 60 Hz, ≤ 6.9 ms @ 144 Hz) |
| Frame time, idle desktop, 3 outputs | ≤ 1 ms CPU, ≤ 2 ms GPU |
| `list_toplevels` (50 windows) | ≤ 1 ms |
| `get_tree` native semantic (5k nodes) | ≤ 5 ms |
| `click` atomic, request → result | ≤ 3 ms + one frame |
| `capture_toplevel` 1080p to dmabuf | ≤ 8 ms |
| `wait_for` wake-up latency | ≤ 1 ms after condition |
| Compositor RSS, 50 windows | ≤ 150 MB |
| Agent seats supported concurrently | ≥ 64 without measurable frame-time impact when idle |

Continuous benchmarking in CI on the headless backend; regressions block merge.

---

## 15. Testing & Fuzzing (COMP-15)

- **Conformance**: run `wlcs` (Wayland Conformance Suite) against the
  headless backend in CI.
- **Protocol fuzzing**: `cargo-fuzz` harnesses on every request handler for
  both the public and agent sockets. Wayland arg parsing is untrusted input.
- **Agent protocol test suite**: a reference agent that exercises every
  request and asserts on events, results, and audit records. Doubles as the
  compat suite for future protocol versions.
- **Client compatibility matrix**: Firefox, Chromium, Electron (VS Code),
  GTK3/4 apps, Qt5/6 apps, foot/alacritty/kitty, mpv, Steam + one Proton
  game, LibreOffice, a Java app, XWayland-only app. Multi-seat behavior is
  tested per client and recorded in the compat-flag rules.
- **Sensitivity/redaction tests**: prove a `secret` surface never appears
  in any capture path lacking the capability, including via output capture
  overlap and popups.
- **Injection isolation tests**: prove human input is never delivered on an
  agent seat and vice versa; prove atomic batches abort on focus loss.
- **Soak**: 72-hour run with synthetic agents at rate limits; zero leaks,
  no frame-time drift.

---

## 16. Milestones (COMP-16)

Phase 1 — daily driver:
1. Boots on DRM, one output, one xdg toplevel, keyboard/mouse. (winit dev
   backend first.)
2. Tiling layout, workspaces, layer-shell, keybindings, config file.
3. Multi-output, fractional scale, hotplug, output persistence.
4. dmabuf, explicit sync, direct scanout, VRR.
5. Clipboard, primary selection, data-control, IME.
6. Session lock, idle, power.
7. XWayland.
8. Screen sharing via portal.
9. Human IPC for a bar. **Exit gate: owner switches full time.**
9b. (Stretch) Animations, rounded corners, shadows, blur, dim-inactive.

Phase 2 — agent protocol:
10. Privileged socket, `agentd` skeleton, `list_toplevels`, audit stream.
11. Agent seats, key/pointer/text injection, focus arbitration, override chord.
12. Atomic batches, `click`, `wait_for`.
13. Region-level redaction and policy-driven sensitivity classes.
    (Frame-level redaction and the capture gate landed early, in milestone 8.)
14. Trusted UI: indicator, consent prompt, emergency panel.
15. Policy table enforcement, `prompt`/`defer` paths.
16. `eclipse_semantic_v1` server side + reference client.
17. Launcher with cgroup principal mapping; agent workspaces; virtual outputs.
18. MCP surface in `agentd`; reference agent passes full suite.
**Exit gate: test agent completes open/type/click/read via protocol alone.**

---

## 17. Decisions & Open Items (this document)

Decided 2026-09-04:
- Agent seats are the default input model; per-app compat fallback to
  human-seat-with-lock (§4.1, §4.4).
- Policy enforced in-process from a compiled table; `defer` path only for
  explicitly marked requests, fail-closed (§11).
- Trusted UI is compositor-drawn, never a layer-shell client (§10).
- WM mode only; no DE (§5.5).
- Hyprland-style: dwindle default, master alternative, Hyprland-class
  visuals as Phase 1 stretch (§5.1).
- Config format: **KDL** with Hyprland-like block structure. hyprlang is
  not reimplemented; Hyprland-config compatibility is a non-goal (§13).
- Default sensitivity class: `private` (settled in THREAT_MODEL §6).
- Human keystrokes are never logged by content (THREAT_MODEL §6, S-04 §2).
- GPU baseline: NVIDIA-first; explicit sync mandatory, no implicit-sync
  fallback, no assumption of direct scanout or overlay planes (F-04 §2).

Open:
1. Agent-workspace layout: no-tiling growable virtual output (proposed
   §5.1). Confirm.
2. Vulkan in v1 or later (proposed: later).
3. Remote agents over TCP in v1 (proposed: no — also settled in
   THREAT_MODEL §6; retained here only as a compositor-side note).


---

<!-- ===== FILE: COMP-01_CORE.md ===== -->

# COMP-01 — Core Architecture, Backend & Lifecycle (Draft v0.1)

Depends on: COMPOSITOR.md (C-00), F-04. Consumed by: COMP-02..07, COMP-13,
COMP-16, A-01.

Crate: `crates/abyss`. TCB. Language: Rust. Foundation: Smithay.
Event loop: `calloop`.

---

## 1. Process Model

Single compositor process. Everything that may crash without killing the
display lives outside it.

| Process | Unit | Role | Trust |
|---|---|---|---|
| `abyss` | `abyss.service` (user) | DRM, input, rendering, protocol servers, policy enforcement | TCB |
| `policyd` | `eclipse-policyd.service` | Policy compilation, audit store, defer path | TCB |
| `agentd` | `eclipse-agentd.service` | Agent gateway, MCP surface | semi-trusted |
| `registryd` | `eclipse-registryd.service` | AT-SPI aggregation, coordinate join, vision fallback | semi-trusted |

All are **systemd user units**. `abyss` does not `exec` them; it declares
readiness with `sd_notify(READY=1)` and the others are ordered
`After=abyss.service` with `BindsTo=` so they stop when it stops.

Rationale for keeping `abyss` small: a crash ends the session (Wayland
clients cannot survive compositor loss). Every line in this process is a
line that can take down your desktop, and it is also TCB, so it is a line
that must be reviewed by hand (F-07 §4).

---

## 2. Crate Layout (internal modules)

```
abyss/
  main.rs           argument parsing, logging, session bring-up
  state.rs          AbyssState — the single mutable root
  backend/
    drm.rs          udev + DRM/KMS, device selection (§4), hotplug
    winit.rs        nested dev backend
    headless.rs     CI backend (no GPU)
    session.rs      libseat wrapper, VT switch, device pause/resume
  render/           (COMP-02)
  outputs/          (COMP-03)
  input/            (COMP-04)
  shell/            (COMP-05) xdg, layer-shell, popups, layout
  protocols/
    standard/       (COMP-06)
    agent/          (COMP-08) — privileged socket only
    semantic/       (COMP-09)
  policy/           (COMP-11) table eval — links `policy-eval` crate
  audit/            (COMP-12) emission only; store lives in `policyd`
  trusted_ui/       (COMP-10)
  ipc/              (COMP-13) human JSON-RPC socket
  xwayland/         (COMP-07)
  config/           (COMP-13) KDL parse, validate, hot-reload
```

---

## 3. State & Event Loop

- **Single-threaded core.** One `calloop` loop owns `AbyssState`; no locks
  on the hot path. Concurrency where it pays: a render thread per GPU
  (COMP-02) and blocking work (config parse, screenshot encode, audit
  serialization) on a small `rayon`-style pool, communicating by channel
  back into the loop.
- **No `Rc<RefCell<>>` graph.** State is a tree of plain structs owned by
  `AbyssState`; children are referenced by index/handle (`u64` surface
  handles per C-00 §5.2), not by pointer. This keeps borrow-checker pain
  low and makes state snapshotting (§7) trivial.
- Event sources registered on the loop: libseat, udev monitor, libinput,
  DRM device fds, Wayland display fd (public), Wayland display fd
  (privileged), `policyd` IPC socket, `registryd` IPC socket, human IPC
  socket, config inotify, timer wheel.
- **Frame scheduling** is per-output, driven by DRM page-flip completion
  (COMP-02); the loop never busy-waits.
- Hot-path budget (F-04 §4): no allocation in input delivery or policy
  check paths; both are `#[inline]`-friendly and benchmarked in `bench/`.

---

## 4. GPU / Device Selection

**Decided 2026-09-05: auto-select the best GPU; user may override.**

Deterministic ranking, applied to all DRM render-capable devices found via
udev:

1. Discrete before integrated (PCI class + `boot_vga` attribute + whether
   the device is on the CPU's root complex).
2. Larger VRAM (`drm` sysfs / device query).
3. Stable tiebreak: PCI domain:bus:device.function, ascending.

Selection is logged at startup with the full ranked list and the reason.

Override, highest precedence first:
1. `--render-device /dev/dri/renderD128` CLI flag.
2. `ECLIPSE_RENDER_DEVICE` env var (one-off testing).
3. Config: `render_device "pci:0000:01:00.0"` or a card path (COMP-13).

**Multi-GPU: not supported in v1.** One render device drives everything;
outputs on other GPUs are not lit.

**Laptops are a target** (decided 2026-09-05), scoped as:

| Class | v1 status |
|---|---|
| Single-GPU laptops (integrated only) | **Fully supported.** Same path as desktop with a different device, plus §8.5 mobile concerns. |
| Hybrid dGPU laptops (Optimus, AMD switchable) | **Not in v1.** Auto-selection picks the dGPU; outputs wired to the iGPU stay dark, so such machines are unusable. This is a known, documented limitation, not a silent failure — refuse to start with a clear message when connectors exist only on a non-selected device. |

Hybrid support ships later as an experimental feature. To keep that from
being a rewrite, the render path is **abstracted over a device set from day
one**: no global "the GPU" singleton, buffers carry their source device,
and the renderer trait (COMP-02) takes a device handle. Implementing
cross-device sharing later then means filling in an existing seam rather
than restructuring.

Rationale: cross-device dmabuf sharing with per-vendor modifier
negotiation, per-client offload, and DRM leases is a large bug surface, and
NVIDIA Optimus — the worst case — is also our reference vendor. Paying the
architectural cost now and the implementation cost later is the cheap
ordering.

**Revisit when** single-GPU laptop support is stable and the compositor is
a daily driver.

NVIDIA specifics (F-04 §2): require `nvidia-drm.modeset=1`; refuse to start
on NVIDIA without it, with an explicit error naming the kernel parameter.
Explicit sync is mandatory; there is no implicit-sync fallback path.

### 4.1 Mobile concerns (single-GPU laptops, v1)

- **Lid switch**: via libinput; configurable action (suspend / turn off
  internal output / ignore) with the "external display connected" case
  handled separately.
- **Internal panel identity**: eDP connectors are matched by connector
  name, not EDID hash, since some panels report no serial.
- **DPMS and idle**: aggressive defaults on battery; `ext_idle_notify`
  timeouts differ on AC vs battery.
- **Battery-aware behaviour**: on battery, cap agent capture stream FPS,
  reduce compositor repaint on unfocused outputs, and let the router
  (I-02) prefer cloud over local inference — a 1–2B model on battery is a
  meaningful power draw.
- **Suspend/resume**: connectors re-probed on resume; a laptop that
  suspends with an external monitor and resumes without it must not
  restore a layout referencing a missing output.
- **Touchpad**: full libinput gesture support is a COMP-04 requirement, not
  optional, once laptops are a target.
- **Keyboard backlight / brightness keys**: handled via logind, not
  directly.

---

## 5. Startup Sequence

```
1. Parse args, init logging (tracing → journald, env-filtered).
2. Acquire session via libseat. Fail → exit with diagnostic.
3. Load + validate config (COMP-13). Invalid → exit at startup
   (unlike hot-reload, where the last good config is kept).
4. Enumerate DRM devices (udev), rank, select (§4), open, set master.
5. Init renderer on the selected device (COMP-02).
6. Enumerate connectors, restore saved output layout (§7), set modes.
7. Init libinput on the seat; load keymap.
8. Create the Wayland display; bind standard globals (COMP-06).
9. Create the public socket `wayland-N`; export $WAYLAND_DISPLAY.
10. Create the privileged socket at
    $XDG_RUNTIME_DIR/eclipse/abyss-agent.sock, mode 0600, owned by the
    user. Agent globals are advertised only on this socket.
11. Connect to policyd (§6). Non-blocking.
12. Open human IPC socket (COMP-13).
13. sd_notify(READY=1). Start XWayland lazily on first X11 client.
14. Run the loop.
```

Steps 3 and 4 are the two that fail on new hardware; both emit a single
actionable error rather than a backtrace.

---

## 6. Daemon Coupling & Degraded Mode

**Decided 2026-09-05: the compositor starts without `policyd` and runs in
degraded mode. No agent connection is accepted until policy is live.**

| Condition | Behaviour |
|---|---|
| `policyd` not yet connected | Full human desktop. Privileged socket exists but `create_agent` fails with `POLICY_UNAVAILABLE`. Trusted UI shows a persistent "agents disabled" indicator. |
| `policyd` connects, pushes signed table | Agents may connect. Indicator clears. |
| `policyd` dies while agents are live | **Fail closed for agents:** all agent seats paused (as in human override, C-00 §4.5), in-flight requests return `paused`, no new requests accepted. Human session continues untouched. Reconnect resumes. |
| `policyd` pushes an invalid/unsigned table | Rejected; previous table stays live; error surfaced in trusted UI and journal. |
| `agentd` dies | Its agents' objects are destroyed; seats and agent workspaces torn down; human session untouched. |
| `registryd` dies | `get_tree` for `atspi` sources returns `source_unavailable`; native trees unaffected; agents keep running. |

Rationale: a policy misconfiguration must not lock you out of your own
machine, and an unpoliced agent must never run. Those two requirements are
satisfiable simultaneously, so we satisfy both.

The `policyd` public key used to verify grants and tables (S-01 §6) is read
at startup from `/etc/eclipse/policyd.pub` — not received over IPC, so a
process impersonating `policyd` cannot supply its own key.

---

## 7. Session State & Crash Resilience

- **Snapshot** written to `$XDG_RUNTIME_DIR/eclipse/state` (atomic
  write-and-rename) whenever output layout, workspace set, or
  workspace↔toplevel mapping changes, coalesced to ≤1 write/second:
  output layout keyed by EDID hash + connector, workspace list, per-app
  placement history, focused workspace per output.
- On restart, layout and workspaces are restored. Clients are gone —
  Wayland has no client-survivable restart — but relaunched apps land where
  they were via the rules engine (COMP-05).
- **Panic handler**: log the panic and a state dump to journald, attempt
  `libseat` VT switch to a free console so the machine is usable, then
  abort. Never leave the GPU in modeset limbo.
- **Watchdog**: `WatchdogSec` in the unit; the loop pings each iteration.
  A wedged compositor is restarted rather than left frozen.
- Client-preserving hot restart is explicitly **out of scope** (Z-06).

---

## 8. Session Management

- VT switch: release DRM master and pause input devices on `libseat`
  disable; reacquire and restore modes on enable. Agent seats are paused
  across a VT switch and resumed after.
- Suspend/resume: DPMS off, release devices, re-probe connectors on resume
  (layout may have changed). A layout referencing an output that is gone on
  resume must degrade gracefully, not crash or blank (§4.1).
- Lid close/open on laptops per §4.1.
- Logout: `xdg_toplevel.close` to every client, 5 s grace, then terminate
  the session scope.

---

## 9. Logging & Observability (feeds X-02)

- `tracing` with structured fields; journald sink; per-module env filter.
- Every agent request logs at `debug` with `req_id`; audit is separate
  (COMP-12) and is not a log.
- Metrics counters exported over the human IPC (frame times, protocol
  request counts, policy decision latencies) for a bar or `eclipse-top`.
- `--dump-state` on the human IPC returns the full state tree as JSON for
  debugging.

---

## 10. Backends

| Backend | Use | Notes |
|---|---|---|
| `drm` | real hardware, VM (virtio-gpu) | production path |
| `winit` | nested in an existing session | milestone 1 and fast iteration |
| `headless` | CI | `wlcs` conformance, protocol tests, no GPU |

The backend is a trait; everything above it is backend-agnostic. CI runs
the full protocol and policy suites on `headless`, so most of the codebase
is testable without a GPU (F-07 §3).

---

## 11. Test Plan (into COMP-15)

- Startup on: NVIDIA (reference), Intel iGPU (override), virtio-gpu (VM),
  headless. Assert device ranking picks the 4060 Ti unaided.
- Single-GPU laptop: lid close/open with and without external display;
  suspend with external, resume without; battery vs AC idle timeouts;
  touchpad gestures.
- Hybrid laptop: assert a clear refusal message rather than a black
  screen when all connectors are on a non-selected device.
- Degraded mode: start without `policyd`; assert human desktop works and
  `create_agent` fails; start `policyd`; assert agents connect.
- `policyd` kill while agents live: assert seats pause, human unaffected,
  resume on reconnect.
- Privileged socket: assert a normal client on `wayland-N` cannot bind
  agent globals; assert socket mode is 0600.
- VT switch and suspend/resume with agents active.
- Snapshot/restore of a 3-output, 9-workspace layout.
- Panic injection: assert VT switch happens and journal contains the dump.

---

## 12. Open Decisions

1. ~~Whether `abyss` should refuse to start if the config names a
   `render_device` that does not exist, or fall back to auto-selection with
   a warning.~~ **Resolved: refuse** (ADR 0033) — silent fallback hides
   typos. Absent or `"auto"` still auto-selects.
2. Snapshot location: `$XDG_RUNTIME_DIR` (lost on reboot) vs
   `$XDG_STATE_HOME` (survives). Proposed: state dir for layout, runtime
   dir for volatile focus state.
3. Whether degraded mode should be visually loud (persistent banner) or
   quiet (small indicator). Proposed: small indicator; a banner trains you
   to ignore banners.
4. Watchdog interval (proposed: 10 s, ping per loop iteration).
5. Lid-close default when an external display is connected (proposed:
   turn off internal output, keep session running).
6. Whether battery-aware agent throttling is policy (policyd) or
   compositor-local. Proposed: compositor-local for capture FPS; router
   handles inference (I-02).


---

<!-- ===== FILE: COMP-02_RENDERING.md ===== -->

# COMP-02 — Rendering (Draft v0.1)

Depends on: COMP-01, F-04 (NVIDIA-first). Consumed by: COMP-03, COMP-05,
COMP-08 (capture), COMP-10, COMP-14.

---

## 1. Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Renderer API v1 | **GLES 2/3** via Smithay `GlesRenderer` | Mature, works on all three vendors, fastest path to daily driver. Vulkan is cleaner for explicit sync but Smithay's Vulkan renderer is young; we would debug it ourselves. |
| Abstraction | **`Renderer` trait taking a device handle** | Vulkan lands later without touching call sites; also the multi-GPU seam (COMP-01 §4). |
| Blur | **Render graph designed for it now, implemented in milestone 9b** | Retrofitting blur means rewriting damage tracking. Designing for it costs a pass abstraction; not implementing it costs nothing at runtime. |
| Color | **sRGB output, linear compositing math** | HDR (Z-05) needs a linear working space; doing the math in linear now costs one shader detail and avoids a pipeline rewrite later. |
| Sync | **Explicit only** (`linux-drm-syncobj-v1`) | NVIDIA baseline (F-04 §2). No implicit-sync path is designed or maintained. |

---

## 2. Frame Pipeline

Per output, driven by DRM page-flip completion — never a timer, never a
spin.

```
page_flip_done(output)
  └─ schedule_repaint(output, deadline = next_vblank - render_estimate)
       └─ at deadline:
            1. collect damage (§3)
            2. if damage empty and no animation → skip frame entirely
            3. assign surfaces to planes (§5); try direct scanout
            4. build render graph (§4)
            5. execute passes into the output buffer
            6. submit DRM atomic commit with an out-fence
            7. send frame callbacks + presentation feedback
```

`render_estimate` is an EWMA of recent frame render times per output, so
the compositor renders as late as safely possible (lower latency), backing
off when it starts missing. Target: human input → frame submitted within
one frame (COMP-14).

Idle desktops render nothing. This is the primary reason the F-04 CPU is
adequate.

---

## 3. Damage Tracking

- Per-output damage accumulated from: client surface damage, surface
  move/resize/stacking changes, cursor movement (unless on a plane),
  animation ticks, trusted-UI updates.
- Damage is kept as a small region set (≤8 rects, merged beyond that) in
  output-local coordinates after transform/scale.
- **Age-aware buffering**: with N-buffered outputs, track per-buffer damage
  age so only the region changed since *that* buffer was last used is
  redrawn.
- **Blur invalidation rule** (the reason blur must be designed in): a
  blurred surface samples what is behind it, so damage behind a blurred
  region must be expanded to cover the blurred region plus the blur kernel
  radius. The damage subsystem therefore takes a list of "sampling
  regions" that expand damage, rather than assuming damage is local. Even
  with blur disabled, the abstraction is present and free.

---

## 4. Render Graph

A frame is a short list of passes. Passes are declared, then executed, so
the pass list can be inspected for redaction correctness (§7) and for
capture reuse (§8).

```
Pass = Clear | SurfaceTree{ layer, surfaces } | Effect{ kind, region }
     | TrustedUI | CursorFallback
```

Layer order (bottom to top): background layer-shell, bottom layer-shell,
tiled/floating toplevels in stacking order (with per-surface effects),
top layer-shell, fullscreen override, overlay layer-shell, drag icon,
cursor (if not on a plane), **TrustedUI last, always**.

Trusted UI is a pass, not a client surface, and nothing can be scheduled
after it (COMP-10, T9).

---

## 5. Planes & Direct Scanout

- **Direct scanout** when a single surface covers the output, its buffer is
  scanout-capable with a compatible modifier, no effects apply, and no
  trusted-UI or overlay content is present. Disabled the moment a prompt or
  indicator is drawn — correctness over the optimization.
- **Cursor plane** when the cursor buffer fits hardware limits; otherwise
  composited (`CursorFallback`).
- **Overlay planes** used opportunistically for video-like surfaces where
  the hardware allows.
- Per F-04 §2, **no plane availability is assumed**. Plane assignment is a
  runtime capability probe with a full-composition fallback that must meet
  frame budget on its own.

---

## 6. Buffers, Modifiers, Sync

- Client buffers: `wl_shm` and `zwp_linux_dmabuf_v1`. dmabuf is the
  expected path; shm is supported and slow.
- Modifier negotiation via `dmabuf` feedback: advertise the render device's
  supported modifiers, with scanout-capable tranches for surfaces likely to
  be scanned out.
- Allocation via GBM. Nothing vendor-specific (F-04 §2).
- **Explicit sync**: acquire/release timeline points per surface commit.
  A client that has not signalled its acquire point by the render deadline
  is skipped for that frame using its previous buffer, and logged; it never
  stalls the compositor.
- Buffers carry their source device handle (COMP-01 §4 seam).

---

## 7. Redaction (security-critical)

Surfaces whose sensitivity class exceeds the requester's authorization are
replaced during composition — not blurred, not blacked out afterwards, but
never rendered into the target buffer at all. A solid placeholder of the
surface's geometry is drawn instead.

Rules:
- Applies to capture targets (COMP-08 §5), never to the physical output the
  human is looking at.
- Redaction is decided when the pass list is built, before any GPU work,
  and the decision is recorded in the pass list so tests can assert on it
  without reading pixels.
- Popups and subsurfaces inherit their parent's class.
- A surface that is partially occluded by a `secret` surface still cannot
  leak it: redaction happens per-surface in the pass list, so occlusion
  culling never "sees through" a redacted surface.
- Content hash (COMP-08 §5) is computed after redaction.
- Redaction operates at **two granularities**. A *surface* whose class
  exceeds authorization is replaced wholesale. A *node* classified `secret`
  inside a surface that is not (S-05 §4) produces a redaction rectangle in
  the surface's local coordinates, transformed with the surface and clipped
  to it, filled with a solid placeholder before the surface is composited
  into the capture target.
- Node rectangles come from the semantic tree and are therefore only as
  accurate as the tree. If the tree for a surface is stale (`generation`
  older than the surface's current generation) or absent while a `secret`
  node is known to exist, the **whole surface** is redacted. Fail closed at
  the surface level rather than trusting a stale rectangle.
- Both decisions are recorded in the pass list with the rectangle list, so
  tests assert on geometry without reading pixels.

Test requirement (COMP-15): for every capture path, prove a `secret`
surface never contributes a pixel without `capture.secret`.

---

## 8. Capture Rendering

- `capture_toplevel`: renders one surface tree (plus its popups) to an
  offscreen target at requested scale/format, without neighbours,
  background, decorations, or cursor unless asked. Reuses the pass
  builder, not the output pipeline.
- `capture_output` / `capture_region`: full pass list into an offscreen
  target with redaction applied.
- **On-demand only for offscreen (virtual) outputs**: an agent workspace on
  a virtual output renders when captured or when a client needs a frame
  callback, not every vblank. Otherwise 64 agent seats would each cost a
  render loop (COMP-14 target).
- `stream`: damage-driven, capped by the grant's FPS quota; a stream with
  no damage emits nothing rather than duplicate frames.
- Format conversion (RGBA/BGRA/NV12) and scaling happen on GPU in the same
  pass to avoid a CPU readback where possible; dmabuf export preferred over
  shm.

---

## 9. Effects (milestone 9b)

All optional, all off by default until 9b, all designed for now:

| Effect | Implementation | Cost note |
|---|---|---|
| Rounded corners | Fragment-shader mask in the surface pass | Negligible; disables direct scanout for that surface |
| Borders | Quad pass around surface geometry | Negligible |
| Shadows | Signed-distance-field pixel shader over the window's grown rect | Cheap; expands damage |
| Dim inactive | Colour multiply in the surface pass | Negligible |
| Blur | Dual-Kawase downsample/upsample, N passes on the region behind translucent surfaces | Expensive; expands damage by kernel radius; disables direct scanout; skipped entirely when the blurred surface is opaque |
| Animations | Interpolated geometry driven by the frame clock | Forces repaint while running; must not extend past the animation |

The rounding mask is computed in framebuffer coordinates from the *window's*
geometry, not each surface's own, so a window with subsurfaces (or client-side
decorations) rounds as one shape rather than rounding each piece. An output
whose transform rotates the framebuffer keeps square corners rather than
masking in the wrong place.

The shadow is one pixel-shader element per window covering the window rect
grown by `border-size + shadow.range` on every side, with the falloff computed
from a rounded-rect signed distance field so it follows the corner radius
exactly. A nine-slice texture would need an upload per (radius, range) pair and
would not track `rounding`; the SDF costs one cheap element and no texture
memory. The shader discards everything inside the window rect, so a translucent
window is never darkened by its own shadow.

Animations are geometry-only in v1. **They must not affect what an agent
sees**: `scene`/`get_tree` geometry reports the *target* geometry, not the
interpolated one, so an agent never clicks where a window was mid-flight.
The compositor keeps this true by construction: the shell maps every window at
its target, and the animation store holds only a render-time offset that decays
to zero — nothing outside the render path can observe it. A backend that
repaints on damage keeps asking for frames while a move is in flight and stops
the frame it finishes.
This is the single most important interaction between effects and the agent
protocol.

---

## 10. Performance Targets (restated from COMP-14, measured on F-04 hw)

| Metric | Target |
|---|---|
| Idle desktop, 3 outputs | 0 frames rendered |
| Frame time, typical desktop, no effects | ≤1 ms CPU, ≤2 ms GPU |
| Frame time with blur on one surface | ≤4 ms GPU |
| `capture_toplevel` 1080p → dmabuf | ≤8 ms |
| Direct scanout hit rate, fullscreen video | >95% of frames |
| Missed-frame rate, 144 Hz, typical load | <0.1% |
| `classify()` recompute for one surface | ≤50 µs; full reclassification of 50 surfaces ≤1 frame at 144 Hz (S-05 §5) |
| Irreversible matcher in `check()` | ≤20 µs (S-06 §3.1) |
| Provenance resolution of ≤8 chain ids | ≤30 µs (summary lookup only) |
| Lease lookup in the enforcement path | ≤1 µs (single map lookup) |
| Button-press hit-test resolution (COMP-08 §10 step 6e) | ≤20 µs |
| Egress proxy added latency, splice mode | ≤2 ms p99 |
| Egress proxy added latency, MITM mode | ≤8 ms p99 |

Benchmarks live in `bench/` and gate CI on regression (F-07 §3).

---

## 11. Test Plan (into COMP-15)

- Damage correctness: property test that rendering only damaged regions
  produces the same buffer as a full redraw (headless, pixel compare).
- Blur damage expansion: assert damage behind a blurred surface expands
  correctly; assert no expansion when blur is off.
- Redaction: §7 test requirement; assert on pass lists *and* pixels.
- Explicit sync: client that never signals its fence must not stall the
  compositor; assert frame continues with the previous buffer.
- Scanout: assert fallback path meets frame budget with planes disabled.
- Animation/agent interaction: assert `get_tree` geometry equals target
  geometry during an animation.
- Vendor matrix: NVIDIA (reference), Intel iGPU, virtio-gpu.

---

## 12. Open Decisions

1. Number of output buffers (2 vs 3). Proposed: 3 with age-aware damage,
   2 on low-memory systems.
2. Blur algorithm: dual-Kawase (proposed) vs Gaussian separable.
3. Whether shm clients get a fast path or are simply accepted as slow.
   Proposed: accepted as slow; no optimization effort in v1.
4. Vulkan renderer timing (Z-08). Revisit when GLES limits are measured,
   not before.


---

<!-- ===== FILE: COMP-03_OUTPUTS.md ===== -->

# COMP-03 — Outputs (Draft v0.1)

Depends on: COMP-01, COMP-02. Consumed by: COMP-05, COMP-08, COMP-13.

---

## 1. Model

An **output** is a rectangle in the global logical coordinate space with a
scale, transform, and refresh rate. Physical outputs map to DRM connectors;
virtual outputs have no connector and may never be scanned out.

```
Output {
  id: u32, name: String,             // "DP-1", "virtual-1"
  kind: Physical{connector, edid_hash} | Virtual,
  position: (i32, i32),              // global logical
  mode: {w, h, refresh_mhz},
  scale: f64,                        // fractional allowed
  transform: Normal|90|180|270|Flipped*,
  vrr: bool, enabled: bool,
  owner: Principal,                  // human, or agent:<id> for virtual
}
```

Global space is in **logical pixels**. Every geometry an agent sees
(COMP-08 §3) is in this space; per-output scale converts to physical.

---

## 2. Identity & Persistence

- Physical outputs are identified by `hash(EDID manufacturer + model +
  serial)` plus connector name as a fallback when EDID is absent or
  duplicated (cheap monitors and some eDP panels report no serial —
  COMP-01 §4.1).
- Layout is persisted per *output set*: the saved configuration is keyed by
  the sorted set of present output identities, so docking, undocking, and
  moving between desks each restore their own layout without manual
  reconfiguration.
- Persistence lives in `$XDG_STATE_HOME/eclipse/outputs.kdl`, written
  atomically, coalesced to ≤1 write/s.

---

## 3. Hotplug

```
udev event → re-probe connectors → diff against current set
  ├─ added:   look up saved layout for the new set; if none, place to the
  │           right of the rightmost output at its preferred mode
  ├─ removed: move its workspaces to the fallback output (§5); never
  │           destroy a workspace because a monitor was unplugged
  └─ changed: re-apply mode/scale
```

All layout changes are applied in a **single atomic DRM commit** where
possible so the desktop does not flash through intermediate states.

A resume-with-different-outputs case (laptop suspended docked, resumed
undocked) is the same code path as hotplug, run once at resume.

---

## 4. Configuration

Declarative in KDL (COMP-13), matched by identity or glob:

```kdl
output "DP-1" {
  mode "2560x1440@144"
  position 0 0
  scale 1.0
  vrr #true
}
output "eDP-*" {
  scale 1.5
  lid-close "off"        // off | suspend | ignore
}
```

Runtime changes via:
- `wlr-output-management` (so existing tools like `wdisplays`,
  `kanshi` work), and
- the human IPC (COMP-13).

Both are the same code path; the config file is canonical and runtime
changes are persisted back to `outputs.kdl`, not to the user's config.

Invalid configurations (mode not supported, outputs overlapping, all
outputs disabled) are rejected with the previous layout retained. **The
compositor never leaves the human with no enabled output**; disabling the
last one is refused.

---

## 5. Fallback Output

There is always exactly one *fallback output*: the primary physical output
if any is enabled, else a virtual output created automatically. Workspaces
whose output disappears move here rather than being destroyed. This
guarantees no window is ever unreachable, which matters more when agents
are creating workspaces the human may not be watching.

---

## 6. Virtual Outputs

Created by agents with `output.virtual` (S-01 §2.5) or by the human for
testing/headless work.

- Arbitrary size, including sizes no monitor supports; **growable** at
  runtime (`resize_virtual_output`) — this is what makes the untiled agent
  workspace layout work (COMPOSITOR §5.1).
- Never scanned out. Rendered **on demand only** (COMP-02 §8): when
  captured, or when a client on it needs a frame callback. An idle agent
  workspace costs nothing.
- Clients cannot distinguish a virtual output from a physical one.
- Quota per agent (default 2, grant-configurable). Counted against the
  agent, released on agent exit.
- Not shown in the human's output list or workspace switcher unless the
  human asks (`eclipse-ctl outputs --all`).
- A virtual output may be *promoted* to visible: mirrored into a floating
  window on a physical output so the human can watch an agent work. This is
  a human-initiated action only.

---

## 7. Power Management

- DPMS per output, driven by `ext_idle_notify_v1` timeouts.
- Timeouts differ on AC vs battery (COMP-01 §4.1).
- `zwp_idle_inhibit` honoured for clients; agents need `idle.inhibit`
  (S-01 §2.5) and cannot hold it silently — the trusted-UI indicator shows
  when an agent is preventing sleep.
- Agent activity does **not** by itself inhibit idle. An agent working on a
  virtual output while the human is away should let the screens sleep;
  virtual outputs are unaffected by DPMS since they are never scanned out.
- `wlr-output-power-management` supported for external tools.

---

## 8. VRR

Per-output toggle. Enabled: the compositor commits frames as they are
ready within the panel's range. Interaction with the agent path: VRR is
disabled while a capture stream is active on that output, because variable
timing makes stream FPS quotas meaningless.

---

## 9. Test Plan (into COMP-15)

- Hotplug add/remove/change with 1, 2, 3 outputs; assert single atomic
  commit and no workspace loss.
- Output-set keyed layouts: dock/undock cycle restores both layouts.
- Refuse to disable the last output.
- EDID-less panel identified by connector name.
- Virtual output: create, grow, capture, destroy; assert zero frames
  rendered while idle and uncaptured.
- Quota enforcement on `output.virtual`.
- Suspend docked → resume undocked.
- Fractional scale 1.25/1.5/1.75 with a mix of scale-aware and legacy
  clients.

---

## 10. Open Decisions

1. Default placement for a newly attached output (proposed: right of
   rightmost, preferred mode).
2. Whether runtime changes persist automatically (proposed: yes, to
   `outputs.kdl`, separate from the user's config file).
3. Mirroring/clone mode in v1 (proposed: yes, simple duplicate; needed for
   presentations).
4. Maximum virtual output dimensions (proposed: 16384² clamped by renderer
   limits).


---

<!-- ===== FILE: COMP-04_INPUT_SEATS.md ===== -->

# COMP-04 — Input & Seats (Draft v0.1)

Depends on: COMP-01, THREAT_MODEL, S-01. Consumed by: COMP-05, COMP-08,
COMP-10.

The subsystem where the agent-native design most departs from every other
compositor. Read C-00 §4 first; this is the detail.

---

## 1. Seat Model

| Seat | Devices | Focus | Created |
|---|---|---|---|
| `seat0` (human) | all libinput devices | independent | at startup, always exists |
| `agent-<id>` | virtual keyboard, pointer, optional touch | independent | on `get_seat`, destroyed with the agent |

Consequences:
- An agent types into window B while the human types into window A. Neither
  sees the other's events.
- Each seat has its own keyboard focus, pointer focus, keymap, and
  modifier state.
- `wl_seat` capability advertisement differs per seat (agent seats
  advertise keyboard+pointer, touch only if granted).

**Human input is never synthesized and never logged by content**
(THREAT_MODEL §6). Synthetic events exist only on agent seats.

---

## 2. Human Input Path

```
libinput event → session filter (paused during VT switch)
  → compositor bindings (§5) → focus resolution → client delivery
```

- Devices: keyboard, pointer, touchpad, touchscreen, tablet, switch (lid).
- Per-device config (COMP-13): accel profile and speed, natural scroll,
  tap-to-click, tap-and-drag, disable-while-typing, click method, scroll
  method, calibration.
- **Touchpad gestures are required, not optional** (laptops are a target,
  COMP-01 §4.1): 3/4-finger swipe and pinch, delivered to
  `zwp_pointer_gestures` for clients and bindable for compositor actions
  (workspace switch, overview).
- Keymap via xkbcommon; per-seat layout; runtime switch; layout-per-window
  optional.
- `pointer_constraints` + `relative_pointer` for games and CAD.
- `keyboard_shortcuts_inhibit` honoured, **except** the reserved chord
  (§6) which no client may inhibit.

Latency: input is processed at arrival and triggers a repaint schedule; it
never waits for the next frame to be *read*. Target ≤1 frame to submission
(COMP-14).

---

## 3. Agent Input Path

```
eclipse_agent_seat_v1 request
  → enforcement order (COMP-08 §10), including the lease check (6d)
    and button-press resolution (6e)
  → synthetic event on the agent's seat
  → focus resolution on that seat → client delivery
  → audit record with req_id (COMP-12)
```

A lease-gated request against a handle held by another principal returns
`busy` and never reaches the synthetic-event stage: nothing is partially
applied and no modifier state is touched.

Primitives per COMP-08 §4. Notes on the ones with real complexity:

**`text`** — preference order:
1. `text_input_v3` commit to the focused text field, if the client bound
   it. Correct for IMEs, non-Latin scripts, emoji, and long strings.
2. `keysym` sequence synthesized against the seat's keymap, temporarily
   remapping unused keycodes for characters the layout cannot produce.
3. Fail with `unsupported`; the agent may fall back to clipboard paste if
   granted (COMP-08 §12.1).

The path used is reported in `result.detail` so agents can adapt.

**Modifier state** is per seat and explicitly tracked; an agent that
presses Shift and never releases it does not affect the human, and its own
stuck modifier is cleared when its atomic batch ends or its seat is
destroyed.

**`button` press resolution.** On every agent-seat button press, before the
policy check, the compositor resolves the pointer's current global
coordinates to `(handle, node)` using the same scene-graph and semantic-tree
walk that serves `hit_test`. The resolved node enters `RequestCtx`
(COMP-11 §3) and the audit record (COMP-12). Where the agent supplied a
`handle` (COMP-08 §4) and the resolved handle differs, both are recorded and
the divergence is available to policy as `target_divergence`. Divergence is
not an error — it is the fact the policy engine most needs to see.

Press carries `expected_generation`; release does not, because the press
already established the target. A drag is therefore: generation-checked
press → unchecked motion → release. Atomic batches are the wrong tool for a
drag: `max_frames` defaults to 4 and exists for sub-frame determinism, not
for a human-timescale gesture.

**Rate limiting** per grant constraints (S-01 §4), enforced before
execution; excess returns `rate_limited` with `retry_after_ms`.

---

## 4. Atomic Batches

`begin_atomic` … `commit_atomic` (COMP-08 §4).

Guarantees:
- All queued events are delivered within `max_frames` frames.
- No human input to the *target surface* is interleaved: human events for
  that surface are queued and delivered after the batch completes.
- No focus change on the agent's seat occurs mid-batch.
- If the target surface is destroyed, unmapped, or loses agent focus, the
  entire batch is discarded and `focus_lost` is returned. **Nothing is
  partially applied.**
- Batches have a hard timeout (`max_frames`, default 4); exceeding it
  aborts.

This is the mechanism that makes multi-step interactions reliable in the
presence of a human using the machine at the same time.

---

## 5. Compositor Bindings

- Bindings are evaluated before client delivery, per seat. Agent seats have
  **no** bindings — an agent cannot trigger a workspace switch by pressing
  Super, only by calling the workspace API. This prevents an injected
  keystroke sequence from driving the compositor.
- Configurable in KDL (COMP-13) with modifiers, keysyms, and mouse
  bindings; `bindsym`-style with a dispatcher name and arguments.
- Repeat/hold bindings supported (e.g. hold to show overview).

---

## 6. Human Override (safety-critical)

Reserved chord, default `Super+Escape`, configurable but **not
disableable**.

- Evaluated in the input path **before** any client delivery, binding
  lookup, or inhibitor check. No client, no agent, no config can intercept
  it.
- Effect: all agent seats are paused immediately; in-flight atomic batches
  abort; new agent requests return `paused`; the emergency panel opens
  (COMP-10).
- Resume is explicit, from the panel, on the human seat only.
- Pressing it when no agents are running still opens the panel, so the
  muscle memory is reliable.

Second reserved chord, `Super+Shift+Escape`: pause **and** terminate all
agent processes via `policyd`. For when pausing is not enough.

---

## 7. Focus Arbitration

| Case | Rule |
|---|---|
| Human focus | Follows the human seat only. **Default: focus-follows-mouse**; click-to-focus configurable. Agents cannot change it without `seat.focus.human` (prompt-class). |
| Agent focus | Set explicitly per agent seat via `focus`; scoped to what the agent may see (S-01 §3). |
| `xdg_activation` | A client requesting activation gets it only if the requesting principal may focus the target; agent-launched clients cannot steal human focus. |
| Compat lock | For apps flagged as multi-seat-broken (§8), the agent takes an exclusive lock on the toplevel; human input to that surface is **queued, not dropped**, and delivered on release. Trusted UI shows an indicator. Lock has a timeout. |
| Overlap | Two agents focusing the same surface is allowed; their input is still separated by seat. Generation checks make conflicting actions fail cleanly (COMP §0.5). |

---

## 8. Multi-Seat Compatibility

Known risk (C-00 §4.1): some clients assume one seat. Handling:

- A per-app compat flag in the rules engine (COMP-05): `seat-compat
  "multi" | "lock"`. Default `multi`.
- The compat matrix (COMP-15) tests each client in the matrix with a second
  seat and records the correct flag; results ship as default rules.
- Detection heuristics for the unlisted case: a client that never binds the
  second `wl_seat`, or that crashes/misroutes on a second seat's focus, is
  logged so the flag can be set. No automatic switching — silent behaviour
  changes are worse than a known-bad interaction.
- XWayland clients are **always** `lock`: X11 has a single input model and
  cannot represent a second seat (COMP-07).

---

## 9. Test Plan (into COMP-15)

- Isolation: human keystrokes never appear on an agent seat and vice
  versa; property-tested under load.
- Atomic batch: abort on target close mid-batch; assert nothing applied;
  assert queued human input delivered after.
- Override chord: works while a client holds a shortcuts inhibitor, while
  fullscreen, while an agent floods input, and while a modal prompt is up.
- Stuck modifier: agent presses and holds Shift, is killed; assert human
  seat unaffected and agent modifier state cleared.
- Rate limits: assert `rate_limited` before any event is delivered.
- Text path: assert IME path used where supported, keysym fallback
  otherwise, correct result for non-Latin and emoji.
- Compat lock: human input queued and delivered in order.
- Touchpad gestures on the laptop test config.

---

## 10. Open Decisions

1. ~~Default focus model~~ — **decided 2026-09-05: focus-follows-mouse**,
   click-to-focus configurable.
2. Whether agent seats should advertise touch by default (proposed: no).
3. Compat-lock default timeout (proposed: 2 s, grant-overridable).
4. Whether a second reserved chord for kill-all is worth the muscle-memory
   cost (proposed: yes, §6).


---

<!-- ===== FILE: COMP-05_WINDOW_MANAGEMENT.md ===== -->

# COMP-05 — Window Management (Draft v0.1)

Depends on: COMP-01..04. Consumed by: COMP-08, COMP-13, P-02.

---

## 1. Toplevel Identity

Every mapped toplevel (xdg or XWayland) gets a **handle**: `u64`, assigned
on map, never reused within a session. This is the only identifier agents
ever use; pids and titles change, handles do not.

```
Toplevel {
  handle, app_id, title, pid, cgroup,
  launching_principal,               // §6
  workspace, output, geometry, scale,
  states: {focused_by[], minimized, maximized, fullscreen, floating,
           urgent, sticky},
  sensitivity: public|private|secret,     // S-05, default private
  app_trust: trusted|standard|untrusted,  // S-05
  semantic: none|atspi|native,
  seat_compat: multi|lock,                // COMP-04 §8
  irreversible_capable: bool,             // S-06 §3.3; from rules, default
                                          // true for browsers, mail clients,
                                          // terminals, file managers, and any
                                          // app with a matching irreversible rule
  class_source: u8,                       // which rule produced sensitivity
  parent, popups[],
  generation,                             // P-01 §4
}
```

`generation` bumps on geometry, state, or structural change — the value
agents check against (COMP §0.5).

---

## 2. Workspaces

- Dynamic, per output. Created on demand, destroyed when empty unless
  named or pinned.
- Every workspace has an **owner principal**: `human` or `agent:<id>`.
- Human workspaces are in the switcher and on physical outputs.
- Agent workspaces default to a virtual output (COMP-03 §6), are hidden
  from the human switcher, and are listed only via `eclipse-ctl` or the
  agent activity panel (COMP-10).
- A workspace whose output disappears moves to the fallback output
  (COMP-03 §5). Workspaces are never destroyed by hardware changes.
- Named workspaces persist in the session snapshot (COMP-01 §7).

---

## 3. Layout

Layout is a trait; two implementations plus floating in v1.

### 3.1 Human workspaces — Hyprland-style
- **dwindle** (default): binary split, each new window splits the focused
  one; split direction follows the larger dimension unless forced.
- **master**: one master area plus a stack; master count and ratio
  adjustable.
- **floating layer** above the tiled layer, per-window togglable.
- Gaps (inner/outer), border width, per-workspace layout override.
- Resize by keyboard (adjust split ratio) and mouse (drag borders).
- Fullscreen: two modes — real fullscreen (client informed) and "maximize
  to output" (client not informed), as Hyprland distinguishes.

### 3.2 Agent workspaces — untiled
- Each toplevel gets its requested size; placed non-overlapping on a
  growable virtual output (COMP-03 §6).
- No gaps, no decorations, no animations.
- **Geometry stability is a hard requirement**: once placed, a toplevel
  does not move because another window appeared. New windows extend the
  output instead. This is what makes an agent's observed coordinates valid
  when it acts on them.
- Human-initiated inspection (promoting a virtual output, COMP-03 §6) does
  not reflow anything.

---

## 4. Rules Engine

Matched at map time, re-evaluated on title change.

```kdl
windowrule "float" {
  app-id "pavucontrol|org.gnome.Calculator"
}
windowrule "workspace 3" {
  app-id "firefox"; title "^Meet —"
}
windowrule "sensitivity secret" {
  app-id "org.keepassxc.KeePassXC"
}
windowrule "seat-compat lock" {
  app-id "some-gtk3-app"
}
windowrule "no-agent" {                 // agents cannot see or touch it
  app-id "org.signal.Signal"
}
```

Matchers: `app-id`, `title` (regex), `pid`, `cgroup`,
`launching-principal`, `output`, `workspace`, `xwayland`.
Actions: float/tile, size, position, workspace, output, opacity, fullscreen,
sensitivity, app-trust, seat-compat, no-agent, no-focus-steal, idle-inhibit.

`no-agent` is worth calling out: it removes a window from every agent's
scene entirely — not merely redacted, but absent from `list_toplevels`,
`hit_test`, and events. The escape hatch for windows you never want an
agent to know exist.

---

## 5. Focus

- Human: **focus-follows-mouse** by default (COMP-04 §7, decided
  2026-09-05); click-to-focus configurable. Focus does not follow the mouse
  across an output boundary while a drag is in progress.
- Focus history per workspace for `alt-tab`-style cycling and for restoring
  focus when a window closes.
- `xdg_activation`: honoured only when the requesting principal may focus
  the target (COMP-04 §7). Otherwise the window is marked `urgent`.
- Agent focus is per agent seat and never changes human focus.

---

## 6. Launch & Principal Attribution

`eclipse_launcher_v1.launch` (COMP-08 §7):

1. `policyd`-checked against the `launch` capability and its argv scope.
2. Process started in a systemd transient scope inside the agent's slice:
   `agents.slice/agent-<id>.slice/launch-<req_id>.scope`.
3. Sandbox applied per S-03 §5 (nested profile) unless
   `launch.outside_sandbox` was granted and prompted.
4. The compositor watches for the first toplevel whose cgroup is under that
   scope, binds it to the request, and returns `launched(handle, pid,
   cgroup)`.
5. `launching_principal` is set from the cgroup, not from a client claim.

Cgroup-based attribution is the point: a client cannot lie about who
started it, and the `principal:launched_by_self` scope (S-01 §3) is
therefore trustworthy.

Timeout (default 10 s) → `launch_failed(timeout)`; the process is **not**
killed, since some apps are slow, but it is no longer bound to the request.

---

## 7. Window Operations

| Operation | Human | Agent |
|---|---|---|
| move/resize/float/fullscreen | bindings, mouse | `window.control`, scoped |
| move to workspace/output | bindings | `workspace.manage` / `window.control` |
| close | binding | `close` → `xdg_toplevel.close` (graceful only) |
| kill process | `eclipse-ctl` | **not a compositor operation** — goes via `policyd` with `window.kill` |
| minimize | binding | `window.control` |

Agents never get SIGKILL through the compositor. Terminating a process is a
policy decision with its own capability and audit record.

---

## 8. Popups & Subsurfaces

- `xdg_popup` positioned per its positioner with constraint adjustment;
  grabs honoured.
- Popups inherit their parent's sensitivity, app-trust, and `no-agent`
  status.
- Popups appear in the parent's semantic tree as nodes (COMP-09 §3), not as
  separate toplevels — agents think in windows.
- Subsurfaces are composited as part of the parent; they are not separate
  handles.

---

## 9. Session Restore

On restart (COMP-01 §7), the snapshot restores workspaces, names, output
assignment, and per-app placement history. Relaunched apps are placed by
the rules engine plus placement history. Agent workspaces are **not**
restored — agents and their grants are session-scoped (S-01 §5).

---

## 10. Test Plan (into COMP-15)

- Handle stability: 10k map/unmap cycles, assert no reuse.
- Agent workspace geometry stability: open 20 windows, assert no existing
  window's geometry changed; assert output grew.
- `no-agent`: assert the window is absent from `list_toplevels`,
  `hit_test`, events, and captures for every agent.
- Launch attribution: assert `launching_principal` from cgroup; assert a
  client that spoofs app_id cannot claim another principal.
- Rules: title-change re-evaluation; conflicting rules resolved by order.
- Focus-follows-mouse across outputs and during drags.
- Workspace survives output removal.

---

## 11. Open Decisions

1. Whether empty unnamed workspaces are destroyed immediately or after a
   grace period (proposed: immediately).
2. dwindle split direction heuristic (proposed: split the longer dimension;
   `preselect` binding to force).
3. Whether `no-agent` windows should be visibly marked in trusted UI
   (proposed: yes, a small badge, so you can confirm the rule is active).
4. Agent workspace growth direction (proposed: right, then wrap downward at
   a configured max width).


---

<!-- ===== FILE: COMP-06_07_PROTOCOLS_XWAYLAND.md ===== -->

# COMP-06 — Standard Protocol Support (Draft v0.1)

Depends on: COMP-01..05. Consumed by: COMP-15 (compat matrix), D-05.

Scope: every protocol needed for the Phase 1 exit gate (owner switches full
time), plus the security-relevant decisions about which protocols we
deliberately do **not** implement.

---

## 1. Required for Phase 1

Completing this table is owned by COMP-16 milestone 9a. `wl_subcompositor`
ships with `wl_compositor` (one Smithay global pair) and `xdg_output` with
`wl_output`; they are not separate work items.

| Protocol | Version | Notes |
|---|---|---|
| `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output`, `wl_data_device_manager` | core | Multiple seats advertised (COMP-04 §1) |
| `xdg_shell` | 6 | toplevels, popups, positioners |
| `xdg_decoration` | 1 | prefer server-side; client-side honoured |
| `xdg_activation` | 1 | gated per COMP-05 §5 |
| `xdg_output` | 3 | logical geometry; superseded by `wl_output` v4 but clients still use it |
| `wlr_layer_shell` | 5 | bars, launchers, notifications, lock fallback |
| `zwp_linux_dmabuf` | 5 | feedback with per-surface tranches (COMP-02 §6) |
| `linux_drm_syncobj` | 1 | **mandatory** (F-04 §2) |
| `wp_presentation` | 2 | timing feedback |
| `wp_viewporter` | 1 | scaling for legacy clients |
| `wp_fractional_scale` | 1 | fractional scaling |
| `wp_single_pixel_buffer` | 1 | trivial, used by many toolkits |
| `zwp_pointer_constraints`, `zwp_relative_pointer` | 1 | games, CAD |
| `zwp_pointer_gestures` | 3 | touchpad gestures (laptops) |
| `text_input_v3`, `input_method_v2` | — | IMEs; also the preferred agent text path (COMP-04 §3) |
| `zwp_primary_selection` | 1 | middle-click paste |
| `wlr_data_control` | 1 | clipboard managers |
| `ext_session_lock` | 1 | screen lock |
| `ext_idle_notify`, `zwp_idle_inhibit` | 1 | power (COMP-03 §7) |
| `ext_foreign_toplevel_list` | 1 | taskbars; read by `registryd` |
| `wp_security_context` | 1 | sandboxed clients; carries agent identity for nested launches (S-03 §5) |
| `zwp_tablet_v2` | 2 | basic tablet |
| `wlr_output_management` | 4 | `wdisplays`, `kanshi` |
| `wlr_output_power_management` | 1 | DPMS tools |
| `xdg_foreign` | 2 | cross-app window embedding (dialogs) |
| `wlr_screencopy` / `ext_image_copy_capture` | 3 / 1 | screen sharing via portal — **capability- and sensitivity-gated** (§3) |
| `wlr_gamma_control` | 1 | night light |
| `content_type`, `wp_alpha_modifier`, `cursor_shape` | 1 | small, expected by modern toolkits |

## 2. Deliberately Not Implemented

| Protocol | Reason |
|---|---|
| `zwlr_virtual_pointer`, `zwp_virtual_keyboard` | **Ambient input injection is exactly what this project replaces.** Any client could inject input with no capability, no scope, no audit. Disabled by default; enabled only for an explicit client allowlist in config, with a trusted-UI indicator while active. |
| `wlr_foreign_toplevel_management` | Allows arbitrary clients to close/focus/fullscreen any window. Read-only `ext_foreign_toplevel_list` covers taskbars; management goes through the human IPC or the agent protocol, both audited. |
| `wlr_export_dmabuf` | Superseded by screencopy/image-copy-capture; one fewer capture path to secure. |
| `wl_drm` (legacy) | Superseded by dmabuf. |

Rationale: every capture and injection path is an attack surface (S6, T5).
Fewer paths, each gated, beats broad compatibility.

## 3. Capture Gating

`wlr_screencopy` and `ext_image_copy_capture` are how xdg-desktop-portal
does screen sharing, so they must work — but they are not ambient:

- A client requesting capture triggers the portal's own picker, and the
  compositor applies **redaction by sensitivity** (COMP-02 §7) regardless
  of what the portal selected. A `secret` window in a shared screen is a
  placeholder, always.
- Agents never use these protocols; they use `eclipse_capture_v1`, which is
  capability-gated and audited. A client on the public socket cannot reach
  the agent capture path.
- Active capture is shown in trusted UI (COMP-10) — persistent, not a
  transient toast.

## 4. Clipboard

- Human clipboard is normal `wl_data_device` + primary selection.
- `wlr_data_control` for clipboard managers, allowlisted by config since it
  reads everything.
- **Agents do not use `wlr_data_control`.** They use
  `eclipse_clipboard_v1` (COMP-08 §6), which is audited and gated on the
  source window's sensitivity class.
- The clipboard's source toplevel and its class are tracked so
  `clipboard.read` on a `secret` source requires `clipboard.read.secret`.

## 5. Test Plan
- `wlcs` conformance in CI (F-07 §3).
- Compat matrix (COMP-15) exercises each protocol against the real clients
  that use it.
- Assert virtual-keyboard/pointer globals are absent for non-allowlisted
  clients.
- Assert redaction applies to portal screen sharing.

## 6. Open Decisions
1. `wlr_foreign_toplevel_management` for a taskbar that wants click-to-close
   (proposed: no; use human IPC).
2. Whether the virtual-keyboard allowlist is worth having at all
   (proposed: yes, for `wtype`-style scripting and IME edge cases).

---

# COMP-07 — XWayland (Draft v0.1)

Depends on: COMP-01, COMP-05, THREAT_MODEL T13.

## 1. Model

Rootless XWayland via Smithay's `X11Wm`, started lazily on first X11 client
and shut down when the last one exits.

X11 toplevels get handles and appear in `list_toplevels` like native ones,
with `xwayland=true`. Override-redirect windows (menus, tooltips) are
mapped as popups of their parent where the hint allows, else as unmanaged
surfaces excluded from the agent scene.

## 2. Security Posture

X11 is a **separate trust domain**. Within XWayland, clients can read each
other's input and window contents; nothing the compositor does changes
that.

Consequences, all enforced:
- All X11 toplevels default to `sensitivity: private` and
  `app_trust: standard` at best; a rule may raise but the default never
  drops to `public`.
- X11 windows are **never** classified `secret`. If a rule tries, the
  compositor refuses and logs it: marking a window secret inside a domain
  where any X11 client can screenshot it is a false guarantee. Secret
  material belongs in native clients.
- `seat_compat` is always `lock` (COMP-04 §8) — X11 has one input model and
  cannot represent a second seat. Agent input to X11 windows therefore
  takes an exclusive focus lock, with the trusted-UI indicator.
- Agent interaction with an X11 window is flagged in audit
  (`ext.xwayland=true` on the record) so investigations can see that the
  isolation guarantees were weaker.
- Documented limitation in the user docs, not buried.

## 3. Perception

- `semantic: atspi | none`. X11 apps cannot bind `eclipse_semantic_v1`.
- AT-SPI still works for X11 apps that support it (most GTK/Qt apps do,
  including under XWayland); `registryd` handles them through the same
  bridge (P-02) with the coordinate join using the compositor's geometry
  for the XWayland surface.
- Vision fallback (P-06) is more often needed here.

## 4. Scaling

- XWayland is scaled by the compositor by default: X11 clients render at
  scale 1 and are upscaled, which is blurry but always correct.
- Per-app rule `xwayland-scaling "client"` sets `Xft.dpi`/`GDK_SCALE`-style
  hints and lets the client render natively for apps that handle DPI well.
- Mixed-DPI multi-output with X11 clients is a known-imperfect area; the
  compositor picks the scale of the output the window is mostly on.

## 5. Other Behaviours

- Clipboard bridged both directions between X11 and Wayland selections,
  including primary.
- DnD bridged.
- Fullscreen, override-redirect, and `_NET_WM_STATE` hints mapped onto
  native state where meaningful.
- `_NET_WM_PID` is **not** trusted for principal attribution; cgroup is
  (COMP-05 §6).
- Xwayland is started with `-noTouchPointerEmulation` and without
  `MIT-SHM` to trim surface area where it costs nothing.

## 6. Test Plan
- Compat matrix X11 entries: a Java/Swing app, an old GTK2 app, Steam, one
  Proton game.
- Assert `secret` classification on an X11 window is refused and logged.
- Assert `seat_compat=lock` is forced and cannot be overridden by rule.
- Clipboard round-trip X11 ↔ Wayland including primary selection.
- Lazy start/stop: assert XWayland is not running with no X11 clients.

## 7. Open Decisions
1. Whether to support XWayland-native scaling (`-scale`) once it stabilises
   upstream (proposed: track it, adopt when reliable).
2. Whether to offer a config toggle disabling XWayland entirely for users
   who want the stronger isolation (proposed: yes).


---

<!-- ===== FILE: COMP-08_AGENT_PROTOCOL.md ===== -->

# COMP-08 — `eclipse_agent_v1` Protocol (Draft v0.3)

Depends on: C-00 §8, S-01, P-01, COMP-09. Consumed by: A-01, A-02, A-05,
COMP-11, COMP-12.

Privileged Wayland protocol served only on `abyss-agent-N`. Sole intended
client: `agentd`. Every object is attributed to one agent principal.

Conventions: all requests that act carry `req_id: uint` (agent-assigned,
unique per agent within the dedupe window) and, where relevant,
`expected_generation: uint`. Every acting request produces exactly one
`result` event. Geometry is global logical px unless stated.

---

## 1. `eclipse_agent_manager_v1` (global)

```
request create_agent(id: new_id<eclipse_agent_v1>, grant: array<u8>)
  -- grant = COSE_Sign1 CBOR (S-01 §4). Compositor verifies signature,
  -- expiry, principal. On failure: protocol error INVALID_GRANT.
request set_policy_key(key: array<u8>)     -- only from policyd's IPC path; not exposed to agentd
event   revoked(principal: string, reason: uint)   -- informational to agentd
event   dedupe_window(seconds: uint)
```

## 2. `eclipse_agent_v1`

```
request get_seat(id: new_id<eclipse_agent_seat_v1>)
request get_scene(id: new_id<eclipse_scene_v1>)
request get_capture(id: new_id<eclipse_capture_v1>)
request get_clipboard(id: new_id<eclipse_clipboard_v1>)
request get_launcher(id: new_id<eclipse_launcher_v1>)
request get_workspace_ctl(id: new_id<eclipse_workspace_v1>)
request subscribe(id: new_id<eclipse_events_v1>, mask: uint)
request add_grant(grant: array<u8>)         -- additional signed grant
request destroy()                           -- revokes everything, destroys seat/workspaces

event   capabilities(caps: array<string>)   -- effective set after grants
event   result(req_id: uint, status: uint, detail: string,
               focus_handle: uint, generation: uint, latency_us: uint)
event   grant_expired(grant_id: string)
event   paused(reason: uint)                -- human override (COMP §4.5)
event   resumed()
```

### 2.1 `result.status` (shared by all interfaces)
```
0  ok
1  no_capability          detail: capability name
2  out_of_scope           detail: scope that failed
3  rate_limited           detail: retry_after_ms
4  stale_generation       detail: current generation
5  focus_lost             (atomic batch aborted)
6  client_gone
7  client_timeout         (semantic action not acked)
8  no_such_action
9  sensitivity_denied     detail: class
10 policy_denied          detail: reason_code (S-02)
11 prompt_denied          (human said no)
12 prompt_timeout
13 revoked
14 paused                 (human override active)
15 invalid_argument
16 quota_exceeded         detail: quota name
17 duplicate              (req_id seen; result replayed, no execution)
18 deferred_timeout       (policyd slow path timed out → fail-closed)
19 class_changed          (read spanned a sensitivity raise; nothing delivered)
20 broker_locked          (S-08 §2)
21 secret_rotated
22 batch_exhausted        (preflight token does not cover this target)
23 circuit_breaker        (S-06 §8; agent is being paused)
24 provenance_required    (acting request arrived with no resolvable chain)
25 task_closed            (principal's task is closed or draining; class denied)
26 busy                   detail: retry_after_ms; lease held by another
                          principal (§4.1)
```

### 2.3 Provenance on acting requests

Every acting request carries a trailing argument:

```
provenance_ids: array<u8>    -- CBOR array of chain_ids the agent asserts
                             -- as inputs to this action. Recorded verbatim;
                             -- the compositor resolves each against its own
                             -- stamped set and uses the resolved union for
                             -- policy. Unresolvable ids → recorded as
                             -- claimed-only and flagged provenance_mismatch.
```

This affects `focus`, `key`, `keysym`, `text`, `pointer_*`, `button`, `axis`,
`touch_*`, `click`, `action`, `secret_fill`, `launch`, `write` (clipboard),
and the mutating `eclipse_workspace_v1` requests. An empty array is legal and
means "no asserted inputs"; it does not mean trusted.

### 2.2 Dedupe
The compositor keeps `(agent, req_id) → result` for `dedupe_window` (default
60 s). A repeated `req_id` returns the stored result with status
`duplicate`+original status in `detail` and executes nothing.

Retention is `max(dedupe_window, prompt_timeout + 30 s)` for any request that
entered the prompt path (A-05 §3). Without this, a retry issued after a slow
prompt executes a second time.

---

## 3. `eclipse_scene_v1`

```
request list_outputs(req_id)
event   output(req_id, id: uint, name: string, x, y, w, h: int, scale: fixed,
               transform: uint, virtual: uint, owner: string)
event   outputs_done(req_id)

request list_toplevels(req_id, filter: string)   -- KDL filter (S-01 §3 scope grammar)
event   toplevel(req_id, handle: uint, app_id: string, title: string,
                 pid: uint, workspace: uint, output: uint,
                 x, y, w, h: int, scale: fixed,
                 states: uint, sensitivity: uint, semantic: uint,
                 parent: uint, generation: uint, app_trust: uint,
                 launched_by_self: uint)
event   toplevels_done(req_id)

request get_toplevel(req_id, handle)
event   toplevel_detail(req_id, ... as above ..., decor_x, decor_y, decor_w, decor_h: int,
                        popups: array<uint>, focused_by_seats: array<string>)

request get_tree(req_id, handle, mode: uint, budget: uint, expected_generation)
  -- mode: 0 full, 1 pruned (registryd assists), 2 diff_since(expected_generation)
event   tree(req_id, handle, generation: uint, value_rev: uint,
             complete: uint, source_mix: uint, app_trust: uint,
             placement: array<u8>, cbor: array<u8>)   -- P-01 Tree, CBOR
  -- For semantic=atspi|none the compositor returns tree with nodes=[] and
  -- source_mix=0; agentd then consults registryd (P-02). Placement is
  -- always returned so registryd can do the join.

request get_text(req_id, handle, node: uint, mode: uint)   -- 0 node, 1 subtree reading order
event   text(req_id, handle, node, value_rev, text: string, truncated: uint)

request hit_test(req_id, x, y: int)
event   hit(req_id, handle, local_x, local_y: int, node: uint, generation: uint)

request wait_for(req_id, predicate: string, timeout_ms: uint)
  -- predicate KDL: toplevel_appears{app_id, title}, title_matches{handle, regex},
  --   focus{seat, handle}, node{handle, id, state|role|name}, generation_gt{handle, n},
  --   text_contains{handle, node, needle}, unmapped{handle},
  --   idle{handle, ms}, command_finished{handle, exit_code?},
  --   node_gone{handle, id}, lease_free{handle}
event   waited(req_id, satisfied: uint, handle, node, generation)
  -- generation is the generation AT SATISFACTION, not at request (P-07 §5.3).
request cancel_wait(req_id)

event   provenance(req_id, chain_id: array<u8>, min_trust: uint,
                   max_sensitivity: uint, len: uint, head_kind: uint)
  -- Emitted immediately before every tree/text/hit/toplevel_detail event.
  -- chain_id is 16 bytes. The agent may not construct one.
```

`lease_free{handle}` is the blocking counterpart to the non-blocking `busy`
status (§4.1). An agent that wants to wait for a contended surface waits
here rather than spinning on retries; the protocol keeps one blocking
mechanism, not two.

Visibility: every response is filtered to the agent's `scene.list` scope
and sensitivity class. Nothing outside scope appears, including in
`hit_test` (returns handle=0).

---

## 4. `eclipse_agent_seat_v1`

One virtual seat per agent, created lazily. Compat fallback per app rule
routes through the human seat under lock (COMP §4.4).

```
request focus(req_id, handle, expected_generation)
request key(req_id, keycode: uint, state: uint, mods: uint)
request keysym(req_id, keysym: uint, state: uint)        -- mapped via seat keymap; may emit multiple keycodes
request text(req_id, handle, node: uint, text: string, expected_generation)
  -- path: text_input_v3 commit to focused field if client supports it;
  -- else keysym sequence. Detail reports which path.
request pointer_abs(req_id, x, y: int)
request pointer_rel(req_id, dx, dy: fixed)
request button(req_id, handle, button: uint, state: uint, expected_generation)
  -- handle and expected_generation are REQUIRED on press (state=1) and
  -- IGNORED on release (state=0): the press already established the target.
  -- Generation mismatch → stale_generation, nothing executed.
request axis(req_id, axis: uint, value: fixed, discrete: int, source: uint)
request touch_down(req_id, touch_id: int, x, y) / touch_up / touch_motion

request begin_atomic(req_id, max_frames: uint)
request commit_atomic(req_id)
request abort_atomic(req_id)
  -- Events queued between begin and commit are applied within ≤max_frames
  -- with no human input or focus change interleaved on the target. Any
  -- focus loss → whole batch aborted, result focus_lost.

request click(req_id, handle, node: uint, x, y: int, button: uint, expected_generation)
  -- Composite: focus → pointer to node center (or x,y if node=0) → press → release,
  -- atomic. If node has action `activate` and the grant includes seat.action,
  -- the compositor prefers the semantic action (detail reports "semantic").
request action(req_id, handle, node: uint, verb: uint, args: string, expected_generation)
  -- Semantic action via COMP-09 §4.

request preflight(req_id, taxonomy_id: string, handles: array<uint>,
                  nodes: array<uint>, summary: string)
event   batch_token(req_id, token: uint, covered: uint, expires_ms: uint)
  -- Approval mints a token bound to the enumerated (handle, node) set.
  -- Subsequent acting requests carry token; a target outside the set returns
  -- batch_exhausted. Single-use per target, expires with the task (S-06 §6).
  -- summary is agent text and is rendered untrusted.

request secret_fill(req_id, handle, node: uint, secret_name: string,
                    expected_generation)
  -- Preconditions checked before any value is read from brokerd: node role is
  -- password, or textfield with ext.credential=true; target app_id/url matches
  -- the secret's bound_to; principal holds secret.use:<name> and seat.text in
  -- scope; app is on the owner's field_fill list (S-08 §3.2).
  -- Failures: no_capability, out_of_scope, invalid_argument, broker_locked,
  -- secret_rotated, stale_generation. The value is committed on the agent's
  -- seat and the buffer zeroed. result.detail carries "filled:<len>" only.

request lease_hold(req_id, handle, duration_ms: uint)
  -- Explicit acquisition, for a critical section across a slow model call.
  -- duration_ms is clamped to the task deadline (A-04).
request lease_release(req_id, handle)
  -- Voluntary release. Not required; the lease dies with the task.

request compat_lock(req_id, handle, timeout_ms)   -- explicit focus-steal lock
  -- Implies an exclusive lease (§4.1) on handle for its duration, acquired
  -- atomically with the lock and released with it. A second agent's
  -- compat_lock on a held handle returns busy.
request compat_unlock(req_id)

event   focus_changed(handle, generation)
event   keymap(fd, size)                          -- the seat's active keymap
event   lease_lost(handle, reason: uint)
  -- reason: 0 idle_expiry, 1 task_ended, 2 released_by_supervisor,
  --         3 human_override
event   human_active(handle)
  -- The human seat delivered input to a leased toplevel. Advisory: the lease
  -- is NOT broken. A well-behaved agent yields.
```

Rate limits per grant constraints; excess → `rate_limited`.

Every acting request in this interface carries the trailing
`provenance_ids: array<u8>` argument specified in §2.3.

### 4.1 Interaction leases

A toplevel is refused to a second principal by default. A lease is **mutual
exclusion between principals**, not a permission: it confers nothing an agent
does not already hold, and it prevents two principals from acting on one
toplevel at once.

- **Gated:** `focus`, `key`, `keysym`, `text`, `pointer_abs`, `pointer_rel`,
  `button`, `axis`, `touch_*`, `click`, `action`, `secret_fill`,
  `begin_atomic`, and the mutating `eclipse_workspace_v1` requests.
- **Not gated:** every `eclipse_scene_v1` read, `get_text`, `hit_test`,
  `wait_for`, and all capture. Two agents observing one window is a supported
  pattern and generations already handle staleness; gating reads would break
  the observer case for no security gain, since read authority is already
  bounded by `scene.list` scope.
- **Acquisition is implicit** on the first gated request against a handle,
  and explicit via `lease_hold`. Implicit acquisition is not an authority
  expansion, so it does not violate no-ambient-authority.
- **Lifetime is the A-04 task.** Completion, cancellation, or process death
  releases every lease that task holds. No reaper is required.
- **Idle expiry: 30 s** (configurable, COMP-13), measured from the last gated
  request against that handle. Holder receives `lease_lost{idle_expiry}`.
- **Contention never blocks.** A gated request against a handle leased by
  another principal returns `busy` immediately. Requests are never queued.
  With no queueing there is no wait-for graph, so multi-window deadlock is
  impossible by construction: an agent copying from window A into window B
  cannot deadlock against an agent doing the reverse. Any future revision
  that adds queueing must preserve this property or replace it.
- **Disclosure.** `busy` names the holder principal only if the requesting
  principal's scope already covers that principal; otherwise it carries an
  opaque lease token plus `retry_after_ms`. Without this rule, `busy` is an
  unstamped cross-agent information channel of exactly the kind S-07 exists
  to make visible. Audit records the holder unconditionally (COMP-12).
- **The human is never a holder and is never blocked.** Human-seat input to a
  leased toplevel is delivered normally and does not break the lease; the
  holder receives `human_active`. Implicit preemption by human input is
  rejected deliberately: it produces silent partial completion of agent work,
  which is worse than an agent that finishes and yields. The escape hatch for
  a wedged agent is the emergency panel (COMP-10), not preemption.

Per-toplevel is the enforcement unit. Two windows of one process are two
handles but one application state; that hazard is covered by the
`lease_sibling_holder` predicate (S-02 §3), not by coarsening the lease.

---

## 5. `eclipse_capture_v1`

```
request capture_toplevel(req_id, handle, scale: fixed, format: uint,
                         region_x, region_y, region_w, region_h: int, cursor: uint)
request capture_output(req_id, output, scale, format, cursor)
request capture_region(req_id, x, y, w, h, scale, format, cursor)
request stream_start(req_id, handle_or_output, max_fps: uint, scale, format)
request stream_stop(req_id)

event   frame(req_id, buffer: fd|wl_buffer, width, height, stride, format,
              content_hash: array<u8>, redacted_regions: array<int>, generation)
event   stream_frame(req_id, ...same...)
```
Redaction (COMP §2.2) is applied before hashing. `redacted_regions` tells the
agent where placeholders are.

---

## 6. `eclipse_clipboard_v1`

```
request list_mimes(req_id)         event mimes(req_id, mimes: array<string>, source_handle, source_class)
request read(req_id, mime)          event data(req_id, fd, size)
request write(req_id, mime, fd, size)
```
`read` on a `secret`-class source requires `clipboard.read.secret` (prompt).

---

## 7. `eclipse_launcher_v1`

```
request launch(req_id, argv: array<string>, env: array<string>,
               workspace_hint: uint, outside_sandbox: uint, timeout_ms)
event   launched(req_id, handle, pid, cgroup: string)
event   launch_failed(req_id, reason: uint)
```
Process runs in a systemd transient scope under the agent's slice; the first
toplevel from that cgroup is bound to the request.

---

## 8. `eclipse_workspace_v1`

```
request create_workspace(req_id, output, layout: uint)   event workspace_created(req_id, id)
request destroy_workspace(req_id, id)
request move_toplevel(req_id, handle, workspace, expected_generation)
request set_geometry(req_id, handle, x, y, w, h, expected_generation)
request set_state(req_id, handle, state: uint, value: uint)   -- minimized|maximized|fullscreen|floating
request close(req_id, handle)                                 -- xdg_toplevel.close (graceful)
request create_virtual_output(req_id, w, h, scale)   event virtual_output_created(req_id, id)
request resize_virtual_output(req_id, id, w, h)
request destroy_virtual_output(req_id, id)
```

---

## 9. `eclipse_events_v1`

```
mask bits: TOPLEVEL_ADDED 1, TOPLEVEL_REMOVED 2, TOPLEVEL_CHANGED 4,
           FOCUS 8, WORKSPACE 16, OUTPUT 32, SEMANTIC 64, DAMAGE 128,
           OVERRIDE 256, PROMPT_RESULT 512, GRANT 1024

event toplevel_added(serial, handle, app_id, title, workspace, output, ...)
event toplevel_removed(serial, handle)
event toplevel_changed(serial, handle, changed_fields: uint, ...)
event focus_changed(serial, seat: string, handle)
event workspace_changed(serial, id, output, active: uint)
event output_changed(serial, id, change: uint)
event semantic_changed(serial, handle, generation, value_rev)
event damage(serial, handle, x, y, w, h)          -- throttled to ≤ 10/s per handle
event human_override(serial, active: uint)
event prompt_result(serial, req_id, approved: uint)
event grant_changed(serial, grant_id, added: uint)
request set_mask(mask)
request ack(serial)     -- optional flow control; compositor drops DAMAGE if unacked backlog > N
```
Serials are shared with `result` events for total ordering per agent.

---

## 10. Enforcement Order (every acting request)

1.  Parse & validate args → `invalid_argument`.
2.  Dedupe lookup → `duplicate`.
3.  Paused? → `paused`. Task closed or draining? → `task_closed`.
4.  Capability + scope (S-01) → `no_capability` / `out_of_scope`.
5.  Rate/quota → `rate_limited` / `quota_exceeded`.
6.  Sensitivity of target vs class caps → `sensitivity_denied`.
6b. Resolve `provenance_ids` → chain summary. Unresolvable → record mismatch.
6c. Circuit-breaker counters (S-06 §8) → `circuit_breaker` (+ pause).
6d. Lease check (§4.1). If the request is lease-gated and the target handle
    is leased by a different principal → `busy`, stop. If unleased, acquire
    implicitly for the calling task. One `HashMap<handle, LeaseHolder>`
    lookup; zero cost when a single agent is running.
6e. If the request is a `button` press, resolve the pointer's current global
    coordinates to `(handle, node)` using the same scene-graph and semantic-
    tree walk that serves `hit_test`. The resolved node enters `RequestCtx`
    (COMP-11 §3) and the audit record. Where the agent supplied a `handle`
    and the resolved handle differs, **both** are recorded and the divergence
    is available to policy as `target_divergence`. Divergence is not an
    error — it is the fact the policy engine most needs to see.
7.  Enforcement table (COMP-11/S-02): allow → 8; deny → `policy_denied`;
    prompt → batch token check first (S-06 §6), else trusted UI, await;
    defer → `policyd`, await ≤ timeout, fail-closed → `deferred_timeout`.
8.  Generation check → `stale_generation`.
8b. Re-check sensitivity class (it may have risen during a prompt) →
    `class_changed`. This second check is mandatory: a prompt can take 120 s
    and the target can navigate in that time.
9.  Execute (atomic if batched). Emit `result`. Emit audit (COMP-12).

No state mutation before step 9.

**Step 8b is the one to be careful about in implementation.** A prompt
answered "allow" for a Gmail send button is not an allow for whatever now
occupies that node id.

**Ordering rationale for 6d, to be preserved.** The lease check must follow
capability and scope so that a principal without scope on a handle receives
its normal denial rather than `busy`, which would disclose that another
principal is active on a handle it cannot otherwise see.

**Ordering rationale for 6e.** S-02's irreversible taxonomy matches on node
facts, and a `pointer_abs` + `button` pair supplied none. Coordinate-only
input was not *uncovered* — S-06 §3.3's app-capable fallback already resolves
it to `prompt` with taxonomy `unknown.capable_app` — but it was covered only
coarsely, and only in apps flagged `irreversible_capable`. Step 6e converts
that catch-all into precise matching: the resolved node feeds the real
taxonomy, so a Pay button prompts as `financial.pay` with its own reversal
wording rather than as a generic "this app can do irreversible things".
S-06 §3.3 says the response to fallback annoyance is better node-level
matching, not disabling the fallback; this is that better matching. Two
further effects: coordinate-only input in apps *not* flagged
`irreversible_capable` becomes matchable at all, and `node_source` is
populated, without which `rule "vision-in-irreversible-apps"` cannot fire on
the coordinate path it was written for.

The fallback in S-06 §3.3 is retained. Step 6e narrows how often it is the
only thing standing; it does not replace it.

---

## 11. Versioning & Extension

Wayland interface versioning; new requests append. Breaking: `_v2`.
`agentd` pins the compositor version it targets and translates MCP tool
schemas (A-02) accordingly.

## 12. Open Decisions

1. `text` fallback when neither text_input nor a usable keymap path exists
   (e.g., emoji on a client without IME support). Proposed: `unsupported`
   status and let the agent choose clipboard paste (if granted).
2. Should `click` ever auto-prefer semantic action, or only when the agent
   asks? Proposed: auto-prefer, reported in `detail`. When `click` resolves
   to a semantic action, the irreversible taxonomy is matched against the
   **action**, not the coordinates, and the app-capable fallback (S-06 §3.3)
   therefore does not fire. This is the main reason to prefer semantic.
3. Dedupe window 60 s vs per-grant configurable. Proposed: configurable,
   60 s default, max 600 s.


---

<!-- ===== FILE: COMP-09_SEMANTIC_PROTOCOL.md ===== -->

# COMP-09 — `eclipse_semantic_v1` Protocol (Draft v0.1)

Depends on: P-01, C-00 §9. Consumed by: P-03 toolkit bridges, A-02.

A Wayland protocol extension by which a client publishes a live semantic
tree for one of its toplevels to the compositor. The compositor stores it
and serves it to agents (`eclipse_scene_v1.get_tree`) and to `registryd`.
Available on the **public** socket; any client may publish about its own
surfaces only.

---

## 1. Design Constraints

- Publishing is **cheap for the client**: incremental updates, no
  round-trips per node, batched in one `commit`.
- The compositor holds the authoritative copy; agent queries never touch
  the client.
- A client can only describe its own surfaces; it cannot reference another
  client's surface (enforced by object ownership).
- Everything the client says is data (T2). Rects are clamped; sensitivity
  can only be raised; `password` values are discarded.

---

## 2. Interfaces

### `eclipse_semantic_manager_v1` (global)
```
request get_semantic_surface(id: new_id<eclipse_semantic_surface_v1>,
                             toplevel: xdg_toplevel)
  -- one per toplevel; second call → protocol error ALREADY_EXISTS
request destroy()
event   capabilities(flags: uint)  -- bitmask: ACTIONS, DIFF, SENSITIVITY
```

### `eclipse_semantic_surface_v1`
```
request set_root(node: uint)
request add_node(node: uint, parent: uint, index: uint)
request remove_node(node: uint)           -- removes subtree
request move_node(node: uint, new_parent: uint, index: uint)

request set_role(node: uint, role: uint)              -- enum per P-01 §1.1
request set_name(node: uint, name: string)
request set_description(node: uint, desc: string)
request set_rect(node: uint, x: int, y: int, w: int, h: int)  -- surface-local logical
request set_states(node: uint, states: array<uint>)    -- full set, replaces
request set_value_text(node: uint, text: string, cursor: int,
                       sel_start: int, sel_end: int)
request set_value_number(node: uint, value: fixed, min: fixed, max: fixed, step: fixed)
request set_value_url(node: uint, href: string)
request clear_value(node: uint)
request set_actions(node: uint, verbs: array<uint>)    -- full set, replaces
request set_action_label(node: uint, verb: uint, label: string)
request set_sensitivity(node: uint, class: uint)      -- raise only
request set_ext(node: uint, key: string, value: string) -- bounded count/size

request commit()
  -- atomically applies all pending changes as one generation bump (if
  -- structural) or value revision (if value-only). Nothing is visible to
  -- agents until commit.

request ack_action(serial: uint, status: uint)
  -- client reports result of an action_requested

event   action_requested(serial: uint, node: uint, verb: uint, args: string)
  -- compositor asks the client to perform a semantic action on behalf of
  -- an agent (already policy-checked). Client MUST ack within timeout
  -- (default 2 s) or the compositor reports `client_timeout` to the agent.

event   focus_hint(node: uint)   -- compositor tells client which node an
  -- agent seat is targeting for text commit, so toolkits can route
  -- text_input to the right widget when their internal focus differs.

event   budget_exceeded()        -- tree exceeds node budget; client should
  -- prune (e.g., virtualize offscreen lists). Compositor truncates anyway.

request destroy()
```

Protocol errors: `INVALID_NODE`, `INVALID_PARENT`, `CYCLE`, `ALREADY_EXISTS`,
`NOT_OWNER`, `LOWER_CLASSIFICATION` (was `LOWER_SENSITIVITY`; same code),
`EXT_LIMIT`, `UNCOMMITTED_DESTROY`.

---

## 3. Compositor Behavior

- Maintains `Tree` (P-01 §2) per surface with `generation` and `value_rev`.
- On `commit`: validate (no cycles, parents exist, root set), clamp rects
  to surface bounds (flag `ext.clamped`), apply sensitivity floor from
  S-05 rules (client raises only), discard `Text` values for `password`,
  bump generation if any structural/rect/role/state/action change,
  else bump `value_rev`.
- Node budget: default 4,000 (P-01 §10.1). Beyond it, the compositor keeps
  the subtree containing focus and the first N in document order, marks
  `complete=false`, emits `budget_exceeded`.
- Rate: commits coalesced to at most one visible generation per frame.
- Popups (`xdg_popup`) of the toplevel are represented by the client as
  `popup`/`menu` nodes with rects in the toplevel's coordinate space; the
  compositor validates against the popup's actual geometry.
- On surface unmap: tree retained for 1 s (so an agent's in-flight query
  gets `client_gone` rather than nothing), then dropped.
- Native trees are exposed to `registryd` over its IPC so it can unify with
  AT-SPI for apps that publish both (native wins per node id space; AT-SPI
  fills gaps only if the client sets `ext.atspi_merge=true`).
- Client-settable extensions gain:
  ```
  ext.irreversible = "<taxonomy_id>"   -- raise only; S-06 §3.4
  ext.credential   = bool              -- this field takes a credential; S-08 §3.2
  ```
  Both are **raise-only**: a client may declare a node irreversible or
  credential-bearing; it may not clear a classification the policy assigned.
  Attempting to lower either is protocol error `LOWER_CLASSIFICATION`
  (renamed from `LOWER_SENSITIVITY` in v0.2; the wire code is unchanged).
- Validate `ext.irreversible` against the compiled taxonomy. Unknown ids are
  dropped with a `budget_exceeded`-style warning rather than accepted, so a
  client cannot invent categories that no rule matches.

---

## 4. Action Semantics

Agent → compositor `seat.action(handle, node, verb, args, expected_generation)`:

1. `check` capability & scope (S-01); enforcement table (COMP-11).
2. Generation check → `stale_generation` on mismatch.
3. Node must list `verb` in `actions` → else `no_such_action`.
4. Emit `action_requested(serial, node, verb, args)` to the client.
5. Await `ack_action(serial, status)`; forward `result` to the agent with
   post-action `generation`, focus handle.
6. Audit record includes serial, node, verb, status, latency.

Why route through the client instead of synthesizing a click: reliability
under animation/overlap, exact intent (activate vs. context menu), no
pointer movement for the human to see, and correct behavior for widgets
whose hit area differs from their visual rect.

---

## 5. Toolkit Bridge Requirements (for P-03)

A bridge is an a11y backend in the toolkit process that mirrors the
toolkit's accessible tree into this protocol. Requirements:
- Emit stable node ids tied to widget identity (pointer/handle hash with
  reuse guard).
- Batch all changes per frame into one `commit`.
- Implement `action_requested` by invoking the toolkit's accessible action.
- Publish `rect` in surface-local logical coordinates *including* CSD
  offsets.
- Set `role=password` for secret entry widgets; never publish their text.
- Honor `budget_exceeded` by virtualizing lists (publish visible + margin).

Targets: GTK4 (via `gtk_accessible` backend), Qt6 (via `QAccessible`
bridge plugin), Chromium/Electron (via a Ozone/Wayland platform a11y
sink), Firefox (via widget/gtk a11y). Feasibility per target is P-03's job;
the protocol assumes only that each toolkit already has an accessible tree,
which all four do.

---

## 6. Test Plan (into COMP-15)

- Reference client that publishes a synthetic tree and asserts round-trip
  through `get_tree`.
- Fuzz all requests (cycles, huge trees, negative rects, id reuse).
- Sensitivity: attempt to lower → protocol error; `password` text discarded.
- Action round-trip latency ≤ 3 ms compositor-side (excluding client).
- Budget behavior: 100k-node publish → truncated deterministically.

---

## 7. Open Decisions

1. Coordinates as `int` logical px vs `fixed` (sub-pixel). Proposed: `int`.
2. Whether to allow clients to publish trees for `wlr_layer_shell` surfaces
   (bars, launchers). Proposed: yes in v1.1; v1 toplevels only.
3. `ext` limits: 16 keys × 256 bytes per node proposed.


---

<!-- ===== FILE: COMP-10_TRUSTED_UI.md ===== -->

# COMP-10 — Trusted UI (Draft v0.1)

Depends on: COMP-02, COMP-04, THREAT_MODEL (T9, A6), S-01, S-02.
Consumed by: COMP-11, S-06.

The compositor's own rendering, above every client. The only surface in the
system the human can trust, and therefore the anchor for every consent
decision and every "what is happening right now" question.

---

## 1. Why It Is Not a Client

A layer-shell client can be impersonated: any client may bind
`wlr_layer_shell` and draw something that looks like a consent dialog. If
consent lived in a client, a hijacked agent could induce an app to render a
fake prompt and harvest an approval (T9).

Therefore trusted UI is:
- A **render pass**, always last (COMP-02 §4). Nothing can be scheduled
  above it.
- Focusable only by the human seat. Agent seats are excluded from its
  focus set entirely — an agent cannot answer, dismiss, or even see the
  input focus of a prompt.
- Rendered with a minimal internal widget set. **No toolkit dependency in
  the compositor process** — no GTK, no Qt, no web engine. Text via
  `cosmic-text`/`fontdue`-class shaping, shapes via the existing renderer.
- Never disabled while any agent seat exists.

---

## 2. Anti-Spoofing: the Personal Secret

On first run the owner sets a **personal secret phrase** (decided
2026-09-05; images and emoji sequences rejected — a phrase is simpler,
survives config sync, and is unambiguous at any output scale). It is stored
in the compositor's config, is never exposed over any protocol or IPC, and
is not readable by any client.

Every genuine trusted-UI surface displays it. A fake prompt drawn by a
client cannot show it, because nothing outside the compositor knows it.

Rules:
- The phrase is shown on prompts, the emergency panel, and the lock screen
  handoff — not on the small persistent indicators (they carry no
  decision).
- Rendered in the trusted UI's own fixed typeface and position, so its
  appearance is not something a client can approximate by guessing at
  theming.
- Length-clamped (proposed 4–48 chars) and rendered verbatim; no markup.
- Setup prompts for something recognisable but not sensitive — it appears
  on screen whenever a prompt fires, including during screen sharing, so
  it must not be a password or anything reused elsewhere. First-run
  wording must say this plainly.
- Setup is mandatory at first run; a skipped secret means prompts render a
  visible warning that anti-spoofing is unconfigured.
- Changing it requires the human seat and a prompt.

This is the same idea as bank sitekeys, and it fails the same way if the
human stops looking. Mitigated by §6.

---

## 3. Surfaces

### 3.1 Agent activity indicator (persistent)
Small, per output, corner-anchored, always present while any agent seat
exists. Shows:
- Number of active agents; per-agent colour keyed to its seat.
- Which agent currently has focus on which output, as a thin outline in the
  agent's colour around the affected toplevel.
- A pulse on input injection, so injected typing is visibly distinct from
  your own.
- Icons for: capture in progress, idle inhibited, compat lock held,
  `policyd` unavailable (degraded mode, COMP-01 §6).

Cost: it must not force a repaint at idle. The indicator animates only when
something is happening, per COMP-02 §2.

### 3.2 Consent prompt (modal, human seat only)
Shown for `prompt` outcomes (S-02 §5). Content:

```
[personal secret]

agent:research-7  wants to:
  Click "Send"  in Gmail — Firefox
  window: "Compose: Q3 invoice"

Task (stated by agent, untrusted):
  "Summarize this week's invoices"

This content came from: acme-invoices.com (untrusted web page)
Category: communication.send (irreversible)

[Allow once] [Allow for this task] [Allow unattended 1h ▾]  [Deny ←focus]
```

Mandatory elements, none of them suppressible:

1. **Untrusted-provenance line.** When the resolved chain has
   `min_trust == untrusted`, the prompt renders above the buttons, in the
   compositor's warning style:
   `⚠ Part of this action's input came from an untrusted source.`
   plus the head source.
2. **Reversal wording.** Every irreversible prompt states the taxonomy's
   `reversal` property in plain words — "This cannot be undone from here" /
   "This goes to the trash" / "Undoing this needs the other party" — taken
   from the S-06 §2 table, never free text.
3. **Default focus is on Deny**, and Escape means Deny. This is stronger
   than "no default-focused affirmative".
4. **Scope display for "Allow for this task"**, not only for the unattended
   option: the exact predicate set that would be written into the grant is
   shown before the human answers. §7 asserts the minted grant is
   byte-identical to the scope displayed.
5. `statement` renders as compositor text; `agent_note` renders in the
   untrusted block (A-04 §11). Two fields, two treatments. §7 asserts an
   agent cannot cause its text to render in the trusted position.

```
[Deny] [Deny & pause agent]
```

Requirements:
- The agent-supplied task/reason string is rendered **visually marked as
  untrusted** (distinct background, explicit label). An agent must not be
  able to make its own text look like compositor text — no markup, no
  colour control, length-clamped, control characters stripped.
- Irreversible categories are visually distinct from routine ones
  (different accent, explicit "irreversible" label) so habituation on
  routine prompts does not carry over.
- "Allow unattended" shows the exact scope and duration being granted, and
  defaults to the narrowest scope that covers this request.
- Keyboard-driven, with no default-focused affirmative button. Enter does
  not mean Allow.
- **Timeout** (default 120 s, S-02) → `prompt_timeout`, fail-closed.

### 3.3 Emergency panel
Opened by `Super+Escape` (COMP-04 §6), or by clicking the indicator.
Lists every agent: id, task, grants held, current activity, recent actions
(last 20 from the audit tail), and per-agent **Pause / Resume / Revoke
grants / Terminate**. Plus global pause-all and terminate-all.

Opening it pauses nothing by itself; the chord that opens it pauses
everything (COMP-04 §6) — the panel is what you see after the brakes are
already on.

### 3.4 Focus-steal / compat-lock indicator
While an agent holds a compat lock (COMP-04 §8), the affected toplevel gets
a visible border in the agent's colour and the indicator shows a lock icon
with a countdown. The human's queued input is shown as a count so it is
obvious why typing "isn't working".

### 3.5 Sensitivity badge
When any agent with `scene.read` is connected, windows classified `secret`
get a small badge showing they are hidden from agents. Confirms the
classification is actually applied rather than merely configured.

### 3.6 Capture indicator
Any active capture — portal screen share or agent capture stream — shows a
persistent indicator naming the consumer. Never a transient toast.

---

## 4. Input Handling

- Prompts take a compositor-owned keyboard grab on the human seat. Clients
  do not receive those keystrokes; `keyboard_shortcuts_inhibit` does not
  apply.
- The human override chord still works while a prompt is up (COMP-04 §6).
- Pointer input to a prompt is compositor-handled; no client sees the
  events.
- Prompts do not steal the *pointer* focus for hover purposes — moving the
  mouse away and back must not dismiss anything.
- Agent seats: a prompt is not in their focus set; agent input during a
  prompt goes to whatever their seat was focused on, unaffected.

### 3.7 Batch prompt (`preflight`)

Rendering for COMP-08 §4 `preflight`: taxonomy, count, and an enumerated
scrollable target list capped at 50 shown with an explicit "+N more" the
human can expand. Approving grants **only** the enumerated set.

### 3.8 Install review

A new trusted-UI surface for agent package install (A-07): capability list,
version diff, compatibility check results, and the strike-a-capability
control.

### 3.9 Agent status indicators

A compositor-drawn indicator per active agent principal, in the trusted
overlay layer. It is trusted UI and not a bar widget because a status dot is
a claim about system state, and any client can bind `wlr_layer_shell` and
draw a convincing green dot.

| State | Source |
|---|---|
| `awaiting_approval` | a live prompt-class decision parked for this principal (COMP-11 §4) |
| `blocked` | `busy` on a contended lease (COMP-08 §4.1) |
| `needs_attention` | deferred consent pending (§3.10) |
| `error` | task failure, circuit breaker, session limit, provider error |
| `active` | task running, nothing pending |

Colour per state, placement, size, per-output vs focused-output, and disable
are all configurable in KDL (COMP-13). Per-*agent* colour follows §8 open
decision 3 (deterministic hash of agent id, colourblind-safe palette);
per-*state* colour is a separate axis and fully user-defined.

Two constraints that are security properties, not preferences:

- **The capture indicator (§3.6) is neither configurable nor disableable**,
  and must remain visually distinct from agent status indicators. "An agent
  is working" is suppressible; "your screen is being captured" is not. The
  same holds for the remote-vision indicator (§3.11).
- **Disabled means not rendered, not unavailable.** With indicators off, the
  same state must remain reachable through `eclipse-ctl` and the emergency
  panel (§3.3). Otherwise disabling the overlay is a supported way to run
  agents invisibly.

### 3.10 Deferred consent

An agent needing an authorization the human must give does not steal focus
and does not block indefinitely. It raises `needs_attention`, §3.9 shows it,
and the human answers when ready.

The `agent-attention` bind (COMP-13 §1.1) opens the **pending decision
queue**. It grants nothing by itself: each pending item names the principal,
the concrete action, and the capability at issue, and is answered
individually. An earlier formulation had one chord meaning "take my screen
for a second"; overloading it to also mean "approve sending pixels to a
remote provider" would have the human pressing one key without knowing which
authorization they were granting. Surfacing the queue generalizes to every
prompt-class decision and needs no new chord per capability.

`agent-attention` and `agent-override` are both evaluated **on the human seat
only**. Agent seats have no bindings (COMP-04 §5), so an injected keystroke
sequence cannot open, answer, or dismiss the queue.

**Absence.** Where the human is demonstrably absent — `idle-notify` idleness
beyond the configured threshold **and** no pending human-seat input — a
deferred item of class *focus steal* may proceed without an answer.

**Absence never authorizes an irreversible action.** The taxonomy match
(S-06) is evaluated independently of presence. A task that goes unattended
into a checkout flow parks on the prompt rather than proceeding. Collapsing
these two would make stepping away from the keyboard an authority upgrade.
The threshold is configurable per app and per irreversibility class, never
globally.

### 3.11 Remote vision prompt and indicator

Prompt content: principal, task statement, app_id and title, the resolved
sensitivity class, the destination provider, and the owner's anti-spoof
phrase. Options: **Allow once** · **Allow for this session, this app and
origin** · **Deny**.

- **No password field.** Trusted UI is already unspoofable: a render pass
  above every client, focusable only by the human seat, agent seats excluded
  from its focus set. A password defends against nothing in the threat model,
  introduces credential handling inside the TCB that does not otherwise
  exist, adds a typo failure mode, and trains the reflex of typing a password
  into a dialog — which is the reflex phishing depends on. Authenticity runs
  the other way: the phrase is the system proving itself to the human.
- **"Allow for this session" mints an S-01 grant**, not a consent-cache
  entry, so `revoke_grants`, audit, and scope display all apply without a
  parallel mechanism.
- **No auto-approve on absence** (§3.10). Pixels leaving the machine cannot
  be un-sent. An unattended task that hits vision fallback parks. Practical
  consequence: unattended agents cannot use remote vision unless the app was
  pre-granted.

A **non-suppressible indicator** is shown while pixels are in flight to a
remote provider, visually distinct from both §3.6 and §3.9.

---

## 5. Rendering Constraints

- Trusted UI must render correctly when the GPU is under load, when a
  fullscreen client holds direct scanout (scanout is dropped for that
  frame, COMP-02 §5), and during a session lock.
- Fixed, non-configurable layout for prompts. No theming, no user CSS. A
  themeable trusted surface is a spoofable one.
- Scales with output scale; readable at 1.0 and 2.0.
- Renders in the compositor process only; no IPC in the render path, so a
  hung `policyd` cannot blank the prompt that is waiting on it.

---

## 6. Habituation (the real failure mode)

The secret and the styling defend against spoofing. Nothing defends against
a human who clicks Allow reflexively. Countermeasures, all mandatory:

- **Prompt budget** (S-02 §5, F-02 §10.5): more than 3 prompts in one
  task's grant marks the grant as mis-sized and surfaces it in the panel.
  Frequent prompting is treated as a configuration defect, not a normal
  state.
- Irreversible prompts are rate-limited: a burst of them from one agent
  collapses into a single prompt listing all pending actions, rather than a
  stream of individually-approvable ones.
- No "don't ask again" checkbox on irreversible categories. The only way to
  stop being asked is an explicit unattended grant with a visible scope.
- The panel shows a weekly count of approvals per agent so drift is
  visible.

---

## 7. Test Plan (into COMP-15)

- Spoof test: a client renders a pixel-perfect fake prompt; assert it
  cannot show the personal secret, cannot take the keyboard grab, and that
  the real indicator is still visible above it.
- Assert agent seats cannot focus, answer, or dismiss a prompt, including
  via synthesized Enter/Escape at the exact moment a prompt opens.
- Assert a prompt renders above a fullscreen client holding direct scanout.
- Assert `policyd` hang does not prevent prompt rendering or answering.
- Assert prompt timeout fails closed.
- Assert untrusted agent text cannot inject control characters, ANSI
  sequences, or excessive length into the prompt.
- Assert override chord works with a prompt open.
- Idle cost: assert zero frames rendered with the indicator present and
  nothing happening.

---

## 8. Open Decisions

1. ~~Personal secret format~~ — **decided: phrase.**
2. Whether the indicator is per-output or on the focused output only.
   Proposed: per-output; agents may act on outputs you are not looking at.
3. Per-agent colour assignment (proposed: deterministic hash of agent id,
   with a colourblind-safe palette).
4. Whether the panel should show a live capture thumbnail of what an agent
   is doing (useful, but it means rendering agent workspaces continuously —
   proposed: on demand only, per COMP-02 §8).


---

<!-- ===== FILE: COMP-11_12_ENFORCEMENT_AUDIT.md ===== -->

# COMP-11 — Policy Enforcement Hooks (Draft v0.1)

Depends on: S-01, S-02, COMP-08. Consumed by: COMP-15, S-10.

The compositor is the enforcement point. `policyd` decides *what the rules
are*; the compositor decides *this request, right now*, with no IPC in the
common path.

---

## 1. Split of Responsibility

| | `policyd` | `abyss` |
|---|---|---|
| Parse & compile policy | ✓ | |
| Hold grants, sign them | ✓ | |
| Verify grant signatures | | ✓ |
| Evaluate every request | | ✓ |
| Prompt rendering & answer | | ✓ (COMP-10) |
| Slow-path (`defer`) decisions | ✓ | |
| Store audit | ✓ | emits only (COMP-12) |

Both link the **same `policy-eval` crate** (F-07 §1). The evaluator is one
implementation used in two processes; the golden decision suite (S-02 §7)
runs against both to prove they cannot drift.

---

## 2. The Table

`policyd` pushes a signed `Table` (S-02 §4) over its IPC. The compositor:

1. Verifies the signature against `/etc/eclipse/policyd.pub` (read at
   startup, never over IPC — COMP-01 §6).
2. Validates version monotonicity (no rollback to an older table).
3. Builds it into an immutable structure and **atomically swaps** the
   pointer. In-flight requests finish against the table they started with.
4. Emits a `policy` audit record with the table version and source hashes.

A rejected table leaves the previous one live and raises a trusted-UI
error. With no table at all, agents cannot connect (degraded mode,
COMP-01 §6).

---

## 3. The Check

Called at step 7 of the enforcement order (COMP-08 §10), after capability,
scope, rate, and sensitivity checks and **before** any state mutation.

```rust
fn check(&self, ctx: &RequestCtx) -> Outcome  // Allow | Deny | Prompt | Defer
```

`RequestCtx` carries only facts the compositor already has:
principal, grant facts, capability, target handles, app_id, title, class,
app_trust, node role/name/source/confidence, url (from tree ext), the
irreversible taxonomy match, provenance tag on the request, and current
rate counters. No allocation, no IPC, no blocking.

Evaluation order is S-02 §3: deny phase, then prompt, then defer, then
allow. First match in a phase wins. The matching rule id is returned and
recorded.

**Performance target: ≤50 µs p99** (S-02 §1), benchmarked in `bench/` and
gated in CI. This is why the table is prebuilt DFAs rather than regex
evaluation per request.

---

## 4. Prompt Path

1. Request is parked in a pending map keyed by `(agent, req_id)`; its
   protocol reply is withheld.
2. Trusted UI renders the prompt (COMP-10 §3.2).
3. On answer: `allow once` → proceed; `allow for task`/`unattended` →
   proceed *and* ask `policyd` to mint a grant; `deny` → `prompt_denied`;
   `deny & pause` → also pause the agent's seats.
4. Timeout → `prompt_timeout`, fail closed.
5. **The parked request re-validates before executing**: capabilities may
   have been revoked, the target may be gone, the generation may have
   moved. A human approval is permission, not a snapshot of the world.

Parked requests are bounded (default 8 per agent); beyond that, new
prompt-class requests fail immediately with `rate_limited` rather than
queueing a prompt storm.

---

## 5. Defer Path

1. Send `{ctx, provenance chain, recent audit window}` to `policyd`.
2. Await, bounded by `defer_timeout` (default 500 ms, S-02).
3. Outcome: `deny` → denied; `prompt` → §4; `fallthrough` → continue to the
   static allow.
4. Timeout or `policyd` gone → `deferred_timeout`, **fail closed**.

**Ratchet enforcement is structural, not a convention**: the compositor's
defer handler accepts only `Deny | Prompt | Fallthrough`. There is no
variant `policyd` can return that grants anything the static table did not
already permit. A compromised `policyd` cannot widen access this way — it
can only deny (a denial-of-service, not an escalation).

---

## 6. Sensitivity & Trust Assignment

The compositor holds the current class and trust for every toplevel:
- Assigned at map time from the table's `classify`/`trust` matchers.
- Re-evaluated on title change and on url change (from the semantic tree).
- Raised, never lowered, by client declaration (COMP-09 §3).
- Raised at runtime by `policyd` (e.g. classifier spotted a credential
  field); lowering requires a table change plus an audit record.
- X11 windows can never be `secret` (COMP-07 §2); attempts are refused and
  logged.

---

## 7. Revocation & Expiry

- Expiry is checked at request time against a monotonic clock; no grace.
- Revocation from `policyd` removes capabilities from the live agent object
  immediately; in-flight atomic batches abort with `revoked`.
- Agent destruction revokes everything, destroys seats, and tears down
  agent workspaces and virtual outputs.

---

## 8. Test Plan (into COMP-15, S-10)

- Golden suite: identical outcomes from `policyd` and compositor (S-02 §7).
- Assert no state mutation on any denied path (fault injection at each
  step of COMP-08 §10).
- Assert defer cannot widen: property test over arbitrary `policyd`
  responses, asserting the outcome is never broader than the static one.
- Table rollback attempt rejected; unsigned table rejected.
- Parked request re-validation: revoke the grant while a prompt is open,
  assert the request fails after approval.
- Prompt storm: assert bounded parking.
- ≤50 µs p99 check under a 500-rule table.

## 9. Open Decisions
1. Parked-request bound per agent (proposed: 8).
2. Whether a table swap should abort in-flight atomic batches (proposed:
   no; they finish against their original table, which is bounded by
   `max_frames`).

---

# COMP-12 — Audit & Provenance Emission (Draft v0.1)

Depends on: S-04, COMP-08. Consumed by: S-11, I-04.

The compositor **emits**; `policyd` stores. This document covers emission
only; the schema, hash chain, retention, and query surface are S-04.

## 1. Channel

- `SOCK_SEQPACKET` to `policyd`, one connection, message-per-record,
  CBOR-encoded.
- **Lossless.** On backpressure the compositor stalls the *agent* whose
  records cannot be accepted — its next request blocks until the queue
  drains. The human path is never stalled and records are never dropped.
- If `policyd` is gone: agents are already paused (COMP-01 §6), so no
  agent-attributable events can occur. Human-side records (focus changes)
  buffer in a bounded ring and are flushed on reconnect, with a gap marker
  if the ring overflowed.
- Emission is off the hot path: records are built into a preallocated
  buffer and written from the loop, never blocking rendering or input.

## 2. What the Compositor Emits

| Kind | Emitted when |
|---|---|
| `request` | every `eclipse_agent_v1` acting request, at parse |
| `decision` | after `check()`, with rule id, phase, latency |
| `prompt` | prompt shown, and again on answer |
| `result` | on every `result` event sent |
| `input` | every synthetic event on an agent seat, linked to `req_id` |
| `focus` | every focus change, with cause (human / agent / client / `req_id`) |
| `capture` | every capture, with content hash post-redaction and the redacted region list |
| `perception` | every `get_tree`/`get_text`, with handle, generation, node count, node-id-set hash, bytes |
| `launch` | argv, cgroup, sandbox hashes, resulting handle |
| `lifecycle` | agent create/destroy/pause/resume, human override, degraded mode entry/exit |
| `policy` | table swap with version and source hashes |

Not emitted by the compositor: `grant`, `revoke`, `channel`, `sandbox`,
`anchor` — those originate in `policyd` or `agentd`.

## 3. Provenance Tagging

The compositor is the origin of most provenance chains (S-07):

- Every perception delivery emits a `Link` (S-07 §2) stamped by the
  compositor, carrying `Surface{handle, generation, node_ids_hash}` or
  `Terminal`/`Url`, with `trust` and `sensitivity` from S-05 **at delivery
  time**. The link is appended to a chain owned by the requesting principal
  and the resulting `ProvenanceRef` is returned to the agent alongside the
  data (COMP-08 §3 `provenance` event). The compositor never accepts a chain
  from an agent as authoritative; asserted `provenance_ids` are resolved
  against the compositor's own stamped set.
- The tag is returned to the agent alongside the data, so the SDK can
  propagate it into channel messages (F-02 §7.5) without the agent having
  to reconstruct it.
- Acting requests carry the provenance tag the agent supplies; the
  compositor **does not trust it for authorization by itself** — it is a
  fact fed to `check()` and to the defer path, and it is recorded verbatim
  so a mismatch between claimed and actual provenance is detectable after
  the fact.

## 4. Elision at Emission

Per S-04 §3, applied before the record leaves the compositor:
- `seat.text` targeting `password`/`secret` nodes → length only.
- Human input: never emitted by content. Only `focus` records.
- Capture pixels: never; hash only.
- `secret`-class text reads: `[secret:<len>]`.

Elision happens at emission, not at storage, so unelided content never
crosses the process boundary.

## 5. Ordering

Records carry the compositor's event serial, shared with protocol events
(COMP-08 §9), giving a total order per agent between what the agent was
told and what was recorded. `policyd` assigns the global `seq`.

## 6. Test Plan
- Assert every acting request produces request+decision+result, and that
  `trace --req-id` reconstructs the full chain including synthetic inputs.
- Backpressure: stall `policyd`'s reader; assert the agent blocks, the
  human session is unaffected, and no record is lost.
- Assert human keystrokes never appear in any record.
- Assert capture records contain no pixels and hash post-redaction.
- Assert elision on secret-targeted text before the socket write.

## 7. Open Decisions
1. Ring size for human-side records while `policyd` is down (proposed:
   4096 records).
2. Whether `perception` records should include the node-id set in full or
   only a hash (proposed: hash, with full set behind a debug flag — the
   full set is large and rarely needed).


---

<!-- ===== FILE: COMP-13_IPC_CONFIG.md ===== -->

# COMP-13 — IPC & Configuration (Draft v0.1)

Depends on: COMP-01..07. Consumed by: D-05, A-01, X-02.

---

## 1. Configuration

**Format: KDL** (decided; COMPOSITOR §17). Hyprland-like block structure so
the shape is familiar, but not hyprlang and not Hyprland-compatible —
pretending compatibility with a different dispatcher model would be a trap.

Location, later overriding earlier:
```
/etc/eclipse/abyss.kdl
$XDG_CONFIG_HOME/eclipse/abyss.kdl
$XDG_CONFIG_HOME/eclipse/abyss.d/*.kdl        (sorted)
```

### 1.1 Shape

```kdl
general {
    gaps-in 5
    gaps-out 10
    border-size 2
    layout "dwindle"          // dwindle | master
    focus-follows-mouse #true  // default (COMP-04)
}

decoration {
    rounding 8
    active-opacity 1.0
    inactive-opacity 0.95
    blur { enabled #false; size 8; passes 2 }     // milestone 9b
    shadow { enabled #true; range 20 }
}

animations {
    enabled #true
    animation "windows" duration="150ms" curve="ease-out"
    animation "workspaces" duration="200ms" curve="ease-out"
}

input {
    kb-layout "us"
    repeat-rate 40
    repeat-delay 400
    touchpad { natural-scroll #true; tap-to-click #true; dwt #true }
    accel-profile "flat"
}

agent-indicators {
    enabled #true
    placement "top-right"        // or bottom-right|top-left|bottom-left|off
    per-output #true              // agents may act on outputs you are not watching
    size 10
    color "active"             "#4caf50"
    color "awaiting_approval"  "#ffc107"
    color "needs_attention"    "#ffc107"
    color "blocked"            "#9e9e9e"
    color "error"              "#f44336"
    // Every colour and position here is user-defined. The CAPTURE indicator
    // (COMP-10 §3.6) and the remote-vision indicator (§3.9) are NOT
    // configurable and cannot be disabled from this block.
}

lease {
    idle-expiry 30s              // COMP-08 §4.1
}

attention {
    absence-threshold 5m         // idle-notify threshold for deferred consent
    // Per-app and per-irreversibility overrides live in policy, not here:
    // absence may permit a focus steal and never an irreversible action.
}

output "DP-1" { mode "2560x1440@144"; position 0 0; scale 1.0; vrr #true }
output "eDP-*" { scale 1.5; lid-close "off" }

bind "SUPER" "Return"  { spawn "foot" }
bind "SUPER" "Q"       { close-window }
bind "SUPER" "1"       { workspace 1 }
bind "SUPER+SHIFT" "1" { move-to-workspace 1 }
// reserved, not rebindable to nothing:
bind "SUPER" "Escape"  { agent-override }
bind "SUPER" "space"   { agent-attention }

windowrule "float"              { app-id "pavucontrol" }
windowrule "sensitivity secret" { app-id "org.keepassxc.KeePassXC" }
windowrule "no-agent"           { app-id "org.signal.Signal" }

agents {
    enabled #true
    trusted-ui-phrase-set #true     // the phrase itself lives in a 0600 file
    indicator "per-output"
    virtual-keyboard-allowlist "wtype" "squeekboard"
}

misc {
    xwayland #true
    render-device "auto"           // or "pci:0000:01:00.0"
}
```

### 1.2 Semantics

- **KDL 2.0 syntax.** Booleans are `#true` / `#false`; bare `true` and
  `false` are identifiers, not values, and are rejected by the parser.
- **Validation is total.** Unknown keys are errors, not warnings — a typo
  that silently does nothing is worse than a refusal.
- **Startup**: invalid config → refuse to start with a precise
  `file:line:col` message and the offending token.
- **Hot reload** (inotify, debounced 100 ms): invalid config → keep the
  last good config, surface the error in trusted UI and journald. Never
  half-apply.
- Reload applies everything except `render-device` and `xwayland`, which
  require restart and say so.
- The trusted-UI phrase is **not** in this file: it lives in
  `$XDG_CONFIG_HOME/eclipse/phrase` mode 0600, so config can be synced or
  shared without leaking it (COMP-10 §2).
- Output runtime changes persist to `outputs.kdl`, separate from this file
  (COMP-03 §4).

---

## 2. Human IPC

Unix socket at `$XDG_RUNTIME_DIR/eclipse/abyss.sock`, mode 0600.
Line-delimited JSON-RPC 2.0. For bars, launchers, scripts, and
`eclipse-ctl`.

**This socket has no injection ability by default.** It is not a back door
to the agent protocol: no `get_tree`, no capture of `secret` surfaces, no
grant manipulation.

### 2.1 Methods

| Method | Returns / does |
|---|---|
| `get_workspaces` | list with output, active, window count, owner principal |
| `get_outputs` | list incl. virtual (hidden by default, `--all` to include) |
| `get_windows` | handle, app_id, title, workspace, class, trust, `no-agent` flag |
| `get_focused` | focused window + workspace per seat |
| `focus_window` / `close_window` / `move_to_workspace` / `set_floating` / `resize` | window ops |
| `switch_workspace` / `move_workspace_to_output` | |
| `set_output` | mode/scale/position/vrr/enabled; persists to `outputs.kdl` |
| `reload_config` | |
| `get_agents` | id, task, grants summary, state (running/paused), recent action count |
| `pause_agent` / `resume_agent` / `terminate_agent` / `revoke_grants` | mirrors the emergency panel; **requires the socket's owner uid** |
| `get_metrics` | frame times, protocol counts, policy latency histogram (X-02) |
| `dump_state` | full state tree as JSON, debug |
| `subscribe` | event stream: workspace, window, focus, output, agent-activity, config-error |

### 2.2 Scripted input (the `wtype` replacement)

`type_text` and `click_at` exist here, guarded by:
- socket ownership (uid check on connect),
- a config toggle `misc { scripted-input #false }`, default **off**,
- a trusted-UI indicator while a scripted-input session is active,
- an audit record per call attributed to `human:script`.

Rationale (COMP-06 §2): scripting is a legitimate need; ambient
`zwp_virtual_keyboard` is not the way to serve it. This path is
attributable and can be turned off.

### 2.3 `eclipse-ctl`

Thin CLI over the above. `eclipse-ctl agents`, `eclipse-ctl pause
agent:research-7`, `eclipse-ctl outputs --all`, `eclipse-ctl watch`.

---

## 3. Daemon IPC

| Peer | Socket | Transport | Notes |
|---|---|---|---|
| `policyd` | `$XDG_RUNTIME_DIR/eclipse/policyd.sock` | SEQPACKET, CBOR | table push, defer requests, audit stream (COMP-12) |
| `registryd` | `$XDG_RUNTIME_DIR/eclipse/registryd.sock` | SEQPACKET, CBOR | native tree mirroring, placement queries for the coordinate join |
| `agentd` | `$XDG_RUNTIME_DIR/eclipse/abyss-agent.sock` | Wayland, 0600 | the privileged protocol (COMP-08) |

All are peer-credential-checked (`SO_PEERCRED`) against the expected uid,
and `policyd`/`registryd` additionally against the cgroup of their systemd
unit, so a random user process cannot impersonate a daemon.

---

## 4. Test Plan
- Config: every documented key round-trips; unknown key errors with
  position; hot reload with a broken file keeps the old config.
- Assert the phrase file is never read by config parsing or dumped by
  `dump_state`.
- IPC: assert a non-owner uid is rejected; assert `type_text` fails with
  `scripted-input #false`.
- Assert `get_windows` over IPC respects `no-agent` only for agents, not
  for the human (the human sees everything).
- Fuzz the JSON-RPC parser and the KDL parser.

## 5. Open Decisions
1. Whether `subscribe` should be a separate socket to avoid slow readers
   blocking request/response (proposed: same socket, per-connection queue
   with drop-oldest for events).
2. Whether `eclipse-ctl` ships as part of `abyss` or its own crate
   (proposed: own crate, so it can be installed on a remote box later).


---

<!-- ===== FILE: COMP-14_PERFORMANCE.md ===== -->

# COMP-14 — Performance Engineering (Draft v0.1)

Depends on: F-04 (reference hardware), COMP-01..05. Consumed by: F-07 (CI
gates), X-01.

Performance is a charter-level non-negotiable. This document defines what
"fast" means numerically, where the budget is spent, what is architecturally
forbidden, and how regressions are caught automatically rather than
noticed months later.

---

## 1. Where Performance Actually Comes From

Not from Smithay. Smithay is a Rust library with an architecture comparable
to wlroots; wlroots is more mature and, today, at least as fast. Smithay was
chosen for memory safety in a TCB and for architectural control (CHARTER
§6). Performance is a consequence of the following choices, each of which is
enforced somewhere in the specs:

| Choice | Where | Effect |
|---|---|---|
| Damage tracking; idle renders nothing | COMP-02 §2–3 | 0 frames on an idle desktop |
| Render at the latest safe moment (EWMA deadline) | COMP-02 §2 | latency without missed frames |
| On-demand rendering for virtual outputs | COMP-02 §8, COMP-03 §6 | 64 idle agent workspaces cost ~0 |
| In-process policy check, prebuilt DFAs | COMP-11 §3 | ≤50 µs, no IPC on the common path |
| Native semantic trees stored in-compositor | COMP-09 §3 | tree query is a memory read, not an app round-trip |
| Single-threaded core, handle-based state | COMP-01 §3 | no locks on hot paths |
| Event-driven daemons, no per-frame work | F-04 §4 | the i5 is sufficient |
| `wait_for` instead of agent polling | COMP-08 §3 | agents cost nothing while waiting |

---

## 2. Targets (reference hardware, F-04)

Every number is measured on the reference machine. A target without that
machine attached is meaningless.

### 2.1 Latency
| Metric | Target | Hard fail |
|---|---|---|
| Human input → frame submitted | ≤1 frame | >2 frames |
| Input event processing (libinput → client dispatch) | ≤200 µs p99 | >1 ms |
| Policy `check()` | ≤50 µs p99 | >200 µs |
| `click` request → result | ≤3 ms + 1 frame | >10 ms |
| `list_toplevels`, 50 windows | ≤1 ms | >5 ms |
| `get_tree` native, 5k nodes | ≤5 ms | >20 ms |
| `get_text` subtree, 1k nodes | ≤2 ms | >10 ms |
| `wait_for` wake after condition | ≤1 ms | >5 ms |
| `capture_toplevel` 1080p → dmabuf | ≤8 ms | >25 ms |
| Config hot reload | ≤50 ms | >500 ms |
| Cold boot → agent-ready | ≤20 s | >40 s |

### 2.2 Throughput / frame
| Metric | Target |
|---|---|
| Idle desktop, 3 outputs | 0 frames rendered |
| Frame time, typical desktop, no effects | ≤1 ms CPU, ≤2 ms GPU |
| Frame time, blur on one surface | ≤4 ms GPU |
| Missed frames @144 Hz, typical load | <0.1% |
| Direct scanout hit rate, fullscreen video | >95% |
| Agent seats concurrently, idle | ≥64 with no measurable frame-time change |
| Agent requests/s sustained (1 agent) | ≥2,000 |
| Audit records/s | ≥50,000 |

### 2.3 Memory
| Metric | Target |
|---|---|
| `abyss` RSS, 50 windows, 3 outputs | ≤150 MB |
| `abyss` RSS growth over 72 h soak | 0 (no leak) |
| Per agent seat overhead | ≤512 KB |
| Per stored semantic tree, 5k nodes | ≤2 MB |
| VRAM, compositor only, 3×1440p | ≤600 MB |

VRAM matters more than usual: the 8 GB budget is shared with the local
model (F-04 §3). A compositor that quietly grows VRAM use pushes the model
out of residency.

---

## 3. Forbidden Patterns

Enforced by review and, where possible, by lint or test:

- **No allocation** in: input delivery, `check()`, damage accumulation,
  audit record construction (preallocated buffers), or the per-frame render
  path. `#[no_alloc]`-style tests via a counting allocator in benches.
- **No IPC** in: rendering, input delivery, policy check, or trusted-UI
  rendering. A hung daemon must never stall the display (COMP-10 §5).
- **No per-frame work in daemons.** `registryd` and `policyd` are woken by
  events, never by a frame clock.
- **No polling anywhere.** Agents use `wait_for`; daemons use event fds.
- **No unbounded queues.** Every queue has a bound and a documented
  behaviour on overflow (drop-oldest for events, stall-the-agent for
  audit).
- **No regex evaluation per request.** Policy matchers compile to DFAs at
  table build (COMP-11 §3).
- **No synchronous GPU readback** in capture unless the format demands it;
  prefer dmabuf export.

---

## 4. Measurement

### 4.1 Benchmarks (`bench/`)
- Criterion microbenchmarks: `check()`, damage merge, tree serialize,
  scope match, audit encode.
- Headless frame benchmarks: synthetic scenes (1/10/50 windows, with and
  without effects) driven on the headless backend so they run without a
  GPU in cloud CI.
- GPU benchmarks: real frame times on the reference machine via the
  self-hosted runner.
- Protocol benchmarks: request/s per interface with a synthetic agent.

### 4.2 Continuous instrumentation
- Frame timing histogram per output, exported via `get_metrics`
  (COMP-13 §2.1).
- Policy decision latency histogram.
- Per-agent request counters and rate-limit hits.
- `eclipse-top`: live view of frame times, agent activity, and daemon
  latencies. Its own overhead is part of the budget and must be ≤0.1 ms/s.

### 4.3 Profiling workflow
`perf` + `hotspot` for CPU, `tracy` integration behind a feature flag for
frame-level tracing, `renderdoc` for GPU captures. The tracy feature must
compile out to zero cost.

---

## 5. CI Enforcement (F-07 §3)

- Microbenchmarks and headless frame benchmarks run on every push (cloud
  runner). **>10% regression against the rolling baseline fails the build.**
- GPU benchmarks run on the self-hosted runner **nightly**, not per push —
  the reference machine is also the dev machine and CI must not fight the
  VM for the GPU (F-07 §7.1).
- Memory soak (72 h) runs weekly; any RSS growth fails.
- Baselines are stored per commit; a deliberate regression requires an
  explicit baseline update in the same PR, with justification in the body.

---

## 6. Degradation Rules

When the budget cannot be met, degrade in this order — never drop
correctness or security:

1. Disable blur, then shadows, then animations.
2. Reduce agent capture stream FPS to its floor.
3. Stop rendering unfocused virtual outputs entirely (they are already
   on-demand).
4. Reduce repaint rate on unfocused physical outputs.
5. Rate-limit agent requests more aggressively.

Never degraded: policy checks, audit emission, trusted-UI rendering,
redaction. If those cannot be afforded, the system pauses agents rather
than skipping enforcement.

On battery, steps 1, 2, and 4 apply proactively (COMP-01 §4.1).

---

## 7. Test Plan
- Assert 0 frames rendered over 60 s on an idle 3-output desktop.
- Assert 64 idle agent seats change frame time by <1%.
- Assert `check()` p99 under a 500-rule table.
- Assert allocation counts are zero on the forbidden paths.
- Assert a hung `policyd` does not affect frame time or input latency.
- 72 h soak with synthetic agents at rate limits: no RSS growth, no
  frame-time drift.

## 8. Open Decisions
1. Rolling baseline window for regression detection (proposed: median of
   last 10 green builds).
2. Whether tracy ships enabled in debug builds (proposed: yes, feature
   flag, off in release).
3. Whether to gate on p99 or p99.9 for input latency (proposed: p99 for CI,
   track p99.9).


---

<!-- ===== FILE: COMP-15_16_TESTING_MILESTONES.md ===== -->

# COMP-15 — Testing, Fuzzing & Compatibility (Draft v0.1)

Depends on: all COMP-*. Consumed by: F-07, S-10, X-06.

---

## 1. Test Tiers

| Tier | Runs | Where | Gate |
|---|---|---|---|
| Unit | every push | cloud | blocking |
| Protocol / integration (headless) | every push | cloud | blocking |
| `wlcs` conformance | every push | cloud (headless) | blocking |
| Fuzz smoke (60 s/target) | every push | cloud | blocking |
| Microbenchmarks | every push | cloud | blocking on >10% regression |
| Security suite (redaction, isolation, escape) | every push | cloud | blocking |
| GPU frame benchmarks | nightly | self-hosted | report + alert |
| Client compat matrix | nightly | self-hosted | report |
| Fuzz deep (hours) | nightly | cloud | report |
| Soak 72 h | weekly | self-hosted | blocking on leak |
| Red team (S-10) | weekly | self-hosted | blocking |

The headless backend (COMP-01 §10) is what makes most of this runnable
without a GPU, so the majority of the suite is fast and cheap.

---

## 2. Security Suite (never optional)

These encode the threat model's guarantees as executable assertions. A
failure here blocks merge regardless of what else is green.

**Redaction** (COMP-02 §7): for every capture path — `capture_toplevel`,
`capture_output`, `capture_region`, `stream`, portal screencopy — assert a
`secret` surface contributes no pixel without `capture.secret`. Includes
the occlusion case (secret window partially behind another) and the popup
case (secret window's menu). Asserted on both the pass list and the pixels.

**Seat isolation** (COMP-04 §9): human events never appear on an agent seat
and vice versa, property-tested under load. Stuck agent modifiers never
affect the human. Agent seats have no compositor bindings.

**Trusted UI** (COMP-10 §7): a client cannot render above it, cannot show
the phrase, cannot take the grab; agent seats cannot focus, answer, or
dismiss a prompt, including via a synthesized Enter timed to the prompt
opening.

**Enforcement** (COMP-11 §8): no state mutation on any denied path (fault
injection at each step of COMP-08 §10); defer can never widen (property
test over arbitrary `policyd` responses); parked prompts re-validate;
unsigned and rolled-back tables rejected.

**Scope leakage**: a window outside an agent's `scene.list` scope never
appears in `list_toplevels`, `hit_test`, events, captures, or `wait_for`
results. `no-agent` windows are absent from all of the above for every
agent.

**Audit completeness** (COMP-12 §6): every acting request produces
request+decision+result; `trace --req-id` reconstructs the chain; no human
keystroke content appears anywhere; capture records contain no pixels.

**X11 posture** (COMP-07 §6): `secret` classification refused and logged;
`seat_compat=lock` forced and not overridable.

---

## 3. Fuzzing

`cargo-fuzz` targets on every parser and every request handler — all of
these consume untrusted input:

- Public Wayland protocol request handlers (per interface).
- Privileged agent protocol handlers (per interface).
- `eclipse_semantic_v1` handlers: cycles, id reuse, huge trees, negative
  and out-of-bounds rects, sensitivity lowering.
- KDL config parser; JSON-RPC parser; CBOR decoders (grants, tables,
  audit).
- Policy expression compiler and its regex inputs.
- EDID parsing.

Corpus seeded from real traffic captured during compat-matrix runs.
Structure-aware fuzzing for the protocols via `arbitrary`.

---

## 4. Client Compatibility Matrix

Run nightly on the self-hosted runner. Each client is tested for: launch,
render, resize, fullscreen, clipboard, DnD, IME where applicable, **and
behaviour with a second (agent) seat present** — which determines its
`seat-compat` flag (COMP-04 §8). Results ship as default `windowrule`s.

| Category | Clients |
|---|---|
| Browsers | Firefox, Chromium |
| Electron | VS Code, Discord |
| GTK | 3 and 4 apps, Nautilus, GNOME Text Editor |
| Qt | 5 and 6 apps, Dolphin, KeePassXC |
| Terminals | foot, alacritty, kitty |
| Media | mpv, OBS (capture path) |
| Games | Steam, one Proton title, one native Vulkan title |
| Office | LibreOffice |
| X11-only | a Java/Swing app, an old GTK2 app |
| Bars/launchers | waybar, fuzzel, mako |
| Portals | xdg-desktop-portal screen share, file chooser |

Each entry records: works / works-with-flag / broken, plus the semantic
source available (native / atspi / none) which feeds P-02 and P-09.

---

## 4b. Security Suites (blocking in CI, F-07 §3)

S-05 §9 race harness and redaction proofs; S-06 §10 matcher corpus and
audit-replay harness; S-07 §10 algebra and two-hop relay; S-08 §8 broker
tests; S-09 §8 leak matrix. Added by this revision: a lease suite (implicit
acquisition, idle expiry, task-death release, `busy` disclosure limits,
absence of a wait-for graph under crossed multi-window contention) and a
press-resolution suite (claimed-vs-resolved divergence is recorded, and a
press whose hit test resolves to no node still reaches S-06 §3.3's fallback).

## 5. Agent Protocol Conformance

A reference agent exercising every request, event, and error status in
COMP-08. Doubles as the compatibility suite for future protocol versions
and as the example in the SDK docs. Asserts:
- every error status is reachable and correctly produced,
- `result` always follows an acting request exactly once,
- dedupe replays without re-executing,
- `stale_generation` fires on a changed tree,
- atomic batches abort cleanly.

---

## 6. Manual Test Plan (things automation cannot catch)

Before each milestone exit: multi-monitor hotplug by hand, VT switching,
suspend/resume, a real work session of at least a day, a game, a video
call (camera + screen share), and an agent performing a real multi-step
task while the human works in parallel.

## 7. Open Decisions
1. Whether to run `wlcs` against the DRM backend as well as headless
   (proposed: headless in CI, DRM nightly).
2. Recording and replaying real client traffic as regression fixtures
   (proposed: yes, from compat runs).

---

# COMP-16 — Milestones & Sequencing (Draft v0.1)

> Current progress against these milestones lives in `docs/STATUS.md`. The
> tables below stay status-free: they are the contract, not the tracker.

Sequential. Each milestone has an exit gate; do not start the next until it
is met. Estimates deliberately omitted — they would be invented.

## Phase 1 — Daily-driver compositor

| # | Milestone | Exit gate |
|---|---|---|
| 1 | winit backend; one xdg toplevel; keyboard + pointer; quit binding | `foot` opens, accepts typing, closes cleanly |
| 2 | Config (KDL), dwindle + master layouts, workspaces, floating, bindings, layer-shell | waybar + fuzzel + mako run; layout usable by hand |
| 3 | DRM backend, multi-output, hotplug, fractional scale, output persistence | 3 monitors, dock/undock restores layouts |
| 4 | dmabuf + explicit sync + direct scanout + VRR; damage tracking complete | Firefox and mpv render correctly; frame benchmarks meet §COMP-14 |
| 5 | Clipboard, primary selection, data-control, DnD, IME | copy/paste across apps; an IME works |
| 6 | Session lock, idle, DPMS, power, lid | lock/unlock, suspend/resume, laptop lid |
| 7 | XWayland | Steam + a Proton game + a Java app |
| 8 | Screen sharing via xdg-desktop-portal | a video call with screen share |
| 9 | Human IPC, `eclipse-ctl`, metrics | waybar driven by our IPC |
| **9a** | COMP-06 §1 protocol completeness: `xdg_decoration`, `xdg_activation`, `wp_single_pixel_buffer`, `zwp_pointer_constraints`, `zwp_relative_pointer`, `zwp_pointer_gestures`, `ext_foreign_toplevel_list`, `wp_security_context`, `zwp_tablet_v2`, `wlr_output_management`, `xdg_foreign`, `wlr_gamma_control`, `content_type`, `wp_alpha_modifier`, `cursor_shape` | every protocol in COMP-06 §1 appears in `wayland-info`; a third-party bar lists windows it does not own; mouse-look works in a Proton game; toolkits set their own cursors |
| **9b** | *(stretch)* animations, rounding, shadows, dim, blur | visuals at Hyprland parity; frame budget still met |
| — | **PHASE 1 EXIT** | **Owner has used it as the only compositor for 14 consecutive days** |

## Phase 2 — Agent protocol

| # | Milestone | Exit gate |
|---|---|---|
| 10 | Privileged socket; `agentd` skeleton; grant verification; `list_toplevels`; audit emission to `policyd` | a test agent lists windows; every call audited |
| 11 | Agent seats; key/pointer/text injection; focus arbitration; override chord | test agent types into an app while the human types into another |
| 12 | Atomic batches, `click`, `wait_for`, dedupe, generations | multi-step interaction survives concurrent human use |
| 13 | Region-level redaction; policy-driven sensitivity classes (frame-level redaction and the capture gate landed early in milestone 8) | redaction suite (§2) green |
| 14 | Trusted UI: indicator, prompt, emergency panel, phrase | spoof suite green; prompts usable |
| 15 | Policy table enforcement; prompt and defer paths | golden decision suite green; ≤50 µs p99 |
| 16 | `eclipse_semantic_v1` server + reference client | round-trip tree and semantic actions |
| 17 | Launcher with cgroup attribution; agent workspaces; virtual outputs | agent launches an app into its own workspace |
| 18 | MCP surface in `agentd`; SDK; reference agent | reference agent passes the full conformance suite (§5) |
| — | **PHASE 2 EXIT** | **An agent completes open → type → click → read via the protocol alone, with full audit and enforcement** |

## Dependencies outside the compositor

`inference.remote.vision` (S-01 §2.6c) cannot ship before **milestone 13**.
Frame-level redaction landed in milestone 8 and is all-or-nothing: an
opaque-black placeholder over the whole surface. Without region-level
redaction the only options are sending a black rectangle or sending the
entire surface. This puts a Phase 2 compositor milestone on the critical path
for a Tier 5 decision.

Phase 2 milestones 10, 15, and 18 need `policyd` (S-01..S-04) and `agentd`
(A-01, A-02) in parallel. Those are separable, testable, GPU-free crates —
the right candidates for delegated work while compositor milestones proceed
serially.

## Open Decisions
1. Whether 9b happens before or after Phase 1 exit (proposed: after; visuals
   should not delay daily-driver status).
2. Whether milestone 7 (XWayland) can be deferred past Phase 1 exit
   (proposed: no — Steam and Java tooling are part of real daily use).


---

<!-- ===== FILE: S-01_CAPABILITIES.md ===== -->

# S-01 — Capability Model & Grant Format (Draft v0.2)

Depends on: THREAT_MODEL.md. Consumed by: COMP-08, COMP-11, S-02, A-01.

---

## 1. Definitions

- **Principal**: an identity that can hold capabilities. Kinds: `human`,
  `agent:<id>`, `group:<id>` (deferred), `system:<daemon>`.
- **Capability**: a named permission to perform a class of operation,
  optionally scoped to targets. Capabilities are the *only* way any
  operation crosses the compositor or `agentd` boundary.
- **Grant**: a signed statement by `policyd` that a principal holds a set
  of capabilities under stated constraints for a stated time.
- **Scope**: a constraint on which targets a capability applies to.
- **Enforcement point**: compositor (`abyss`) for scene/seat/capture/
  clipboard/launch/workspace/output; `agentd` for channel and MCP-level
  operations; sandbox for filesystem/network/D-Bus.

Principle: **no ambient authority.** A principal with no grant can do
nothing, including list windows.

Hard rules, not expressible away by any policy file:

- Taxonomies `system.policy` and `system.firmware` (S-06 §2) resolve to
  `deny` for every non-`system:` principal. No grant, no rule, and no prompt
  answer can produce an allow. The prompt path is never entered; the request
  fails immediately with `policy_denied` and is audited with the taxonomy id.
- No secret value is ever an input to a policy predicate.

---

## 2. Capability Namespace

Dotted names. A capability may be scoped with `:<scope>`.

### 2.1 Scene (perception)
| Capability | Grants | Default for general agents |
|---|---|---|
| `scene.list` | `list_outputs`, `list_toplevels` (metadata only, filtered by class and scope) | yes, scoped to agent's own workspaces + `public` |
| `scene.read` | `get_toplevel`, `hit_test` on `public`+`private` | yes, task-scoped |
| `scene.read.secret` | above, on `secret` | **never** without per-request prompt |
| `scene.tree` | `get_tree` on `public`+`private` | yes, task-scoped |
| `scene.tree.secret` | `get_tree` on `secret` | prompt |
| `scene.text` | `get_text` on `public`+`private` | yes, task-scoped |
| `scene.text.secret` | `get_text` on `secret` | prompt |
| `scene.wait` | `wait_for` | yes |
| `scene.events` | `subscribe` | yes, filtered to visible set |

### 2.2 Seat (action)
| Capability | Grants | Default |
|---|---|---|
| `seat.key` | key events on own seat | yes |
| `seat.text` | text commit on own seat | yes |
| `seat.pointer.motion` | pointer motion (absolute and relative) and axis/scroll on own seat | yes |
| `seat.pointer.button` | button press and release on own seat | yes, task-scoped |
| `seat.touch` | touch on own seat | no |
| `seat.focus` | set own seat's focus to a toplevel within scope | yes |
| `seat.focus.human` | move the human seat's focus | prompt |
| `seat.atomic` | atomic batches | yes |
| `seat.compat_lock` | focus-steal lock on compat-flagged apps; implies an exclusive lease (COMP-08 §4.1) | yes, scoped |
| `seat.action` | invoke semantic actions on nodes within scope | yes |
| `seat.lease.release` | release another principal's interaction lease; the lease returns to unheld | no; unattended-grantable only to a validated supervisor (§4) |
| `seat.lease.take` | release **and** immediately acquire | **prompt** |

Motion and scroll change no application state and stay cheap; buttons are the
state-changing primitive and carry the gate. `seat.pointer` as a bare name is
**retired, not aliased**, so a stale rule fails compilation (S-02 §4
unknown-predicate error) rather than silently matching nothing.

The lease split exists because releasing is a cleanup operation while taking
is only useful if the holder intends to act. A supervisor needs the first and
must not silently acquire the second.

### 2.3 Capture
| Capability | Grants | Default |
|---|---|---|
| `capture.toplevel` | single-surface capture, `public`+`private` | yes, task-scoped |
| `capture.output` | whole-output capture (redaction applied) | no |
| `capture.stream` | continuous capture | no |
| `capture.secret` | include `secret` surfaces | **never** without per-request prompt; not grantable persistently |
| `capture.cursor` | include cursor | no |

### 2.4 Clipboard
| Capability | Grants | Default |
|---|---|---|
| `clipboard.read` | read selection when source is `public`/`private` | no |
| `clipboard.read.secret` | read when source is `secret` | prompt |
| `clipboard.write` | set selection | no |

### 2.5 Launch, workspace, output
| Capability | Grants | Default |
|---|---|---|
| `launch` | launch executables within scope (allowlist of argv[0]) | scoped |
| `launch.shell` | launch a shell or arbitrary argv | no |
| `launch.outside_sandbox` | launch outside agent sandbox (e.g., human's browser) | prompt |
| `workspace.manage` | create/destroy own workspaces, move own toplevels | yes |
| `workspace.human` | operate on human-owned workspaces | prompt |
| `output.virtual` | create virtual outputs (quota'd) | yes, quota 2 |
| `window.control` | move/resize/minimize/close toplevels in scope | yes, scoped |
| `window.kill` | terminate client process | no (goes via `policyd`) |
| `idle.inhibit` | hold idle inhibitor | no |

### 2.6 Channels (`agentd`)
| Capability | Grants |
|---|---|
| `channel.create` | create channels; quota is **per task** (A-03 §7) |
| `channel.post:<name>` | post to channel |
| `channel.read:<name>` | subscribe/read channel |

Channel names in grants are fully qualified `<creator>/<name>`.

### 2.6b Agent control (`agentd` / `policyd`)
| Capability | Grants | Default |
|---|---|---|
| `agent.control:<scope>` | `pause`, `resume`, `terminate` on another principal within scope | no |

Scope grammar (**open, see §9**): `lineage:<own_subtree>` — the principal may
control tasks descended from its own, bounded by the A-04 §10 `max_depth`
ceiling. Lineage needs no new grammar and cannot reach sideways into agents
the supervisor did not spawn. `principal:<glob>` is the more flexible
alternative a standalone watchdog would need. Proposed: lineage for v1.

`pause_agent`, `resume_agent` and `terminate_agent` exist today only as
owner-uid IPC rows reachable by the human. A-01's lifecycle model has no
notion of one agent terminating another and must be extended before this is
implementable.

### 2.6c Inference
| Capability | Grants | Default |
|---|---|---|
| `inference.remote.vision` | transmit captured pixels of a surface to a non-local provider for element synthesis | **no** |

Scope: `app_id:<glob>` **and** origin where the toplevel's tree carries a url;
`app_id` + `handle` where it does not. `app_id` alone is too wide for a
browser — `app_id "firefox"` is one identity across every tab, so approving a
canvas on one site would approve vision on a banking tab for the session.

Eligibility is additionally gated by S-05's fail-closed rule for opaque
unclassified surfaces. Transport is brokered through `agentd`, where `Model`
provenance links are already stamped; `registryd` does not egress directly
(**open, see §9**).

### 2.7 Sandbox-enforced (compiled into profile, not runtime-checked)
| Capability | Effect |
|---|---|
| `fs.read:<path>` / `fs.write:<path>` | bind mount + Landlock rule |
| `net.egress:<host[:port]>` | egress allowlist entry; SNI-filtered, no decryption (S-09 §2c) |
| `net.egress.mitm:<host>` | as above, plus TLS termination with a per-agent ephemeral CA; required for `proxy_header` secret injection |
| `net.local:<service>` | named local service over a bind-mounted unix socket (S-09 §3) |
| `net.bulk:<host>` | raises per-request and per-window egress volume quotas (S-09 §4c) |
| `dbus:<busname>[.<iface>]` | xdg-dbus-proxy allow rule |
| `secret.use:<name>` | broker may inject named secret at a boundary the agent cannot observe (S-08 §3.1, §3.2); agent never sees the value |
| `secret.expose:<name>` | **prompt-class.** Secret is materialized into the sandbox as env var or file; the agent *can* read it (S-08 §3.3) |

### 2.8 Meta
| Capability | Effect |
|---|---|
| `grant.request` | agent may ask `policyd` for additional capabilities (triggers prompt) |
| `policy.edit` | never granted to agents |

---

## 3. Scopes

A scope restricts targets. Types, combinable with AND:

```
workspace:<id|own|human>      output:<id|virtual|physical>
app_id:<glob>                 title:<regex>
handle:<u64>                  principal:<launched_by_self|any>
class:<public|private|secret> url:<glob>   (browsers; from tree)
path:<glob>                   host:<glob>
node_role:<role>              (for seat.action)
```

Unscoped capability = all targets of that kind the agent can otherwise
see. **`scene.list` scope defines visibility; nothing outside it exists for
the agent**, including in events and `hit_test`.

Scope evaluation is deterministic and total: every request resolves to
exactly one target set; if any target falls outside scope, the request
fails with `no_capability` and nothing is applied (no partial batches).

---

## 4. Grant Format

Grants are Cap'n Proto or CBOR structures (decision: CBOR + COSE_Sign1 for
signatures; widely available, small). Human-readable form for policy files
and audit is KDL.

```kdl
grant {
  id "01J7Q…"                       // ULID
  principal "agent:research-7"
  issued 2026-09-05T10:00:00Z
  expires 2026-09-05T12:00:00Z      // hard expiry; renewable via prompt
  issuer "policyd"
  task "Summarize this week's invoices"   // free text, shown in prompts
  capability "scene.list" scope="workspace:own" scope="class:public"
  capability "scene.tree" scope="app_id:firefox" scope="url:*.acme-invoices.com/*"
  capability "seat.action" scope="app_id:firefox"
  capability "capture.toplevel" scope="app_id:firefox" quota="fps:2"
  capability "channel.post:invoice-results"
  capability "launch" scope="argv0:/usr/bin/firefox"
  capability "net.egress:api.anthropic.com:443"
  constraints {
    rate "requests" 200 per="10s"
    rate "input_events" 500 per="1s"
    prompt_budget 3                 // per-task counter (A-04 §6); >3 prompts on
                                    // the task → policyd flags every grant on it
  }
  signature "…COSE_Sign1…"
}
```

Rules:
- Grants are **additive within a principal** and **never widened by
  merging**: the effective set is the union of live grants, each with its
  own scopes. A scope on one grant does not loosen another.
- Grants are immutable. Changes = new grant + revocation of old.
- **Revocation** is immediate: `policyd` pushes a revocation; the compositor
  drops the capability from the agent object; in-flight atomic batches
  abort with `revoked`.
- Expiry is checked at request time in the compositor; no grace.
- `capture.secret`, `scene.*.secret`, `clipboard.read.secret`,
  `seat.focus.human`, `launch.shell`, `launch.outside_sandbox`,
  `workspace.human`, `secret.expose:*`, `net.egress.mitm:*`,
  `seat.lease.take`, `inference.remote.vision` are **prompt-class**: a grant
  may *name* them, but every use triggers trusted UI unless the grant carries
  an explicit `unattended=true` that was itself approved via prompt with the
  exact scope shown. Unattended prompt-class grants max expiry: 1 h
  (F-02 §10.2, closed).
- A grant names `task_id "<ULID>"` referencing an A-04 task, not a free-text
  `task`. A grant's `expires` must be ≤ its task's `deadline`; a grant naming
  a `closed` task is rejected.
- Grants at run time are the **intersection** of manifest, install policy,
  and owner policy (A-07 §3). Install policy is a named input to grant
  compilation and is never re-widened by an update or a rollback.

**Validation at issue time.** `policyd` rejects or flags grant sets whose
capability combination defeats a control elsewhere. These are static checks
on the union of a principal's live grants, re-run on every addition:

| Combination | Result |
|---|---|
| Read capability (`scene.tree`, `scene.text`, `capture.*`, `fs.read`) over targets that can be above `public` **+** `net.egress` to any host not in the policy's `trusted_endpoint` list | **Reject** |
| `secret.expose:*` **+** any `net.egress` | **Reject** unless the prompt that minted the grant displayed the exposure and the destination |
| `channel.read:*` **+** egress outside `trusted_endpoint` | Warn |
| `secret.use:<n>` **+** egress hosts outside that secret's `bound_to` | Warn (binding still enforced at injection) |
| `net.egress:*` for a non-`system:` principal | **Reject** |
| Unattended `irreversible` allow rule with no narrowing scope | **Reject** (S-06 §7) |
| `seat.lease.release` or `agent.control:*` **+** any of `seat.{key,text,pointer.button,action}` with overlapping scope | **Prompt-class; no unattended grant** |

A rejected grant is audited (`grant{outcome:rejected, reason}`) and shown in
the trusted-UI panel. Rejection is not a prompt: there is no answer the human
can give that makes the combination safe, only a different grant.

The last row is the supervisor separation. An agent that can stop other
agents must not also be able to act in their place without a human in the
loop, and enforcing this at issue time rather than in a profile template
means it holds however the profile was written. Reference profile:

```
profile "supervisor" {
  scene.list, scene.read           // observe
  agent.control:<scope>            // pause / terminate within scope
  seat.lease.release               // release, never take
  // deliberately absent: seat.key, seat.text, seat.pointer.*, seat.action,
  //                      capture.*, clipboard.*, launch, net.egress
}
```

A hijacked supervisor is a denial of service against other agents, which is
recoverable. It is not an actor. Consequence to record, not to fix: because
`seat.lease.take` stays prompt-class, a supervisor that needs to act in a
stopped agent's place requires a human at the keyboard. Unattended
*supervision* exists; unattended *takeover* does not. Deliberate v1 boundary.

---

## 5. Grant Lifecycle

```
agent manifest (declared needs, A-07)
        │
        ▼
policyd: rule match ──► auto-grant (rules say yes)
        │                  │
        │                  ▼
        │            sign, push to abyss + agentd + sandbox builder
        │
        ├──► prompt (rules say ask) ──► trusted UI ──► human approves/edits/denies
        │                                                   │
        └──► deny (rules say no) ◄──────────────────────────┘
```

- `grant.request` at runtime follows the same path; the request carries the
  agent's stated reason, which is shown verbatim in the prompt **marked as
  untrusted agent text**.
- On agent exit or task completion, all its grants are revoked and its
  seats/workspaces destroyed unless a durable grant says otherwise.
- `policyd` keeps every grant, revocation, and prompt answer in audit (S-04).

---

## 6. Binding in the Compositor

- `agentd` calls `create_agent(id, grant_blob)`; the compositor verifies
  the COSE signature against `policyd`'s public key (provisioned at
  startup over the trusted IPC), checks expiry, and materializes the
  capability set on the `eclipse_agent_v1` object. **`agentd` cannot
  fabricate or extend a grant.**
- Additional grants and revocations arrive from `policyd` directly over
  its IPC, keyed by principal, not via `agentd`.
- Every request handler begins with `check(cap, targets) -> Allow | Deny |
  Prompt | Defer`; the enforcement table (COMP-11/S-02) refines Allow into
  the final outcome. No state mutation precedes `check`.

---

## 7. Default Profiles (rule templates, S-02 will formalize)

| Profile | Intended for | Notable |
|---|---|---|
| `observer` | read-only assistants | scene.*, capture.toplevel; no seat |
| `operator` | general task agents | observer + seat.*, window.control, workspace.manage, launch (allowlist), channels |
| `builder` | coding agents | operator + fs.write on a project path, launch of dev tools, net.egress to registries |
| `messenger` | agents that send on your behalf | operator + irreversible-send with per-target unattended grant, time-boxed |
| `trusted-ops` | owner-authored automation | operator + launch.shell within sandbox, longer expiries; still no policy.edit |

No profile includes any `.secret` capability persistently.

---

## 8. Open Decisions

1. CBOR+COSE vs. a simpler Ed25519-over-canonical-JSON. Proposed: CBOR+COSE.
2. Whether `scene.list` should ever be unscoped for general agents.
   Proposed: no; unscoped listing leaks window titles across the machine.
3. Quota semantics for `output.virtual` (count vs. pixel area).
4. Whether task text in grants is mandatory (proposed: yes; it is what the
   human sees in prompts).
5. `agent.control` scoping: `lineage` vs `principal` (§2.6b). Proposed:
   lineage for v1, on the same reasoning that deferred Z-02.
6. Whether remote vision egresses from `registryd` behind its own proxy or is
   brokered through `agentd` (§2.6c). Proposed: `agentd`. S-09's egress proxy
   is per-*agent* and does not cover a TCB daemon transmitting pixels.


---

<!-- ===== FILE: S-02_POLICY_LANGUAGE.md ===== -->

# S-02 — Policy Language & Compiler (Draft v0.1)

Depends on: S-01, THREAT_MODEL.md. Consumed by: COMP-11, I-03, S-06.

`policyd` reads human-authored policy (KDL), compiles it to an
**enforcement table** the compositor evaluates in-process with no IPC, and
handles the `prompt` and `defer` slow paths.

---

## 1. Goals

- Deterministic: same inputs → same decision, no model in the compositor.
- Total: every (principal, capability, target) resolves to exactly one of
  `allow | deny | prompt | defer`.
- Auditable: every decision cites the rule id that produced it.
- Small hot path: table lookup + scope match, ≤ 50 µs typical.
- Ratchet: `defer` resolves to `deny`/`prompt` or falls through to the
  static outcome; never to a broader outcome (F-02 §6).

---

## 2. Policy Sources (precedence, highest first)

1. **Hard rules** (compiled into `policyd`, not editable): `policy.edit`
   never granted to agents; `password` values never delivered;
   app-declared sensitivity never lowers class; prompt-class capabilities
   always prompt unless an approved unattended grant exists.
2. **Owner policy**: `/etc/eclipse/policy/*.kdl` (system) and
   `~/.config/eclipse/policy/*.kdl` (user). Editing requires the human's
   identity; not mounted in any sandbox.
3. **Grants** (S-01): per-principal, signed, time-boxed.
4. **Agent manifest** (A-07): declared needs; can only *request*, never
   confer.

A rule at a higher level cannot be relaxed by a lower one; it can be
tightened.

---

## 3. Language

KDL. Top-level node types: `classify`, `trust`, `rule`, `irreversible`,
`profile`, `defaults`.

```kdl
defaults {
  sensitivity "private"          // F-02 §6
  app_trust "standard"
  prompt_timeout 120s
  defer_timeout 500ms            // fail-closed after
  dedupe_window 60s
  downgrade_grace 500ms          // S-05 §5.2
  provenance_max_links 32        // S-07 §3.1
  irreversible_per_hour 20       // S-06 §8
  communication_per_hour 10
  financial_per_hour 3
  denied_irreversible_streak 3
  egress_upload_per_request 4MiB // S-09 §4c
  egress_upload_per_hour 64MiB
  lease_idle_expiry 30s          // COMP-08 §4.1
}

dedupe_exclude {
  irreversible "financial.*" "identity.credential" "identity.session"
               "destructive.overwrite"
  provenance_contains trust:"untrusted"
}

trusted_endpoint {
  host "api.anthropic.com"
}

// Sensitivity classification (S-05)
classify "secret" {
  app_id "org.keepassxc.KeePassXC" "1password" "bitwarden"
  title ~"(?i)authenticat|password|2fa|one-time"
  node_role "password"
  url "https://*.bank.example/*"
}
classify "public" {
  app_id "org.mozilla.firefox" when=url:"https://*.wikipedia.org/*"
}

// App trust (S-05)
trust "untrusted" { app_id "steam" "*.flatpak.*" ; url "http://*" }
trust "trusted"   { app_id "foot" "eclipse-*" }

// Irreversible action taxonomy (S-06)
irreversible "communication.send" {
  node { role "button" name ~"(?i)^(send|post|publish|reply|tweet)$" }
  url ~"mail.google.com|outlook.live.com|slack.com"
}
irreversible "financial.pay" {
  node { role "button" name ~"(?i)pay|checkout|confirm order|transfer" }
}
irreversible "shell.destructive" {
  terminal_cmd ~"^\s*(rm\s+-rf|git\s+push\s+.*--force|dd\s+|mkfs)"
}

// Rules: first match wins within a phase; phases evaluated deny → prompt → defer → allow
rule "no-secret-capture" deny {
  capability "capture.*" target_class "secret"
  unless grant_has "capture.secret" unattended=true
}
rule "irreversible-prompts" prompt {
  capability "seat.action" "seat.key" "seat.pointer.button" "click"
  irreversible "*"
  principal_profile "operator" "builder"
}
rule "messenger-unattended" allow {
  capability "seat.action" irreversible "communication.send"
  principal_profile "messenger"
  grant_scope url:"https://mail.google.com/*"
}
rule "vision-in-irreversible-apps" prompt {
  capability "click" node_source "vision"
  app_irreversible_capable true
}
rule "untrusted-origin-into-shell" defer {
  capability "seat.text" "seat.key" target_app_id "foot" "alacritty"
  provenance_contains trust:"untrusted"
}
rule "channel-laundering" defer {
  capability "seat.action" irreversible "*"
  provenance_contains source:"channel"
}
rule "human-workspace" prompt { capability "workspace.human" "seat.focus.human" }
rule "blind-irreversible" prompt {
  irreversible "*"
  provenance_absent                       // acting with no asserted inputs
}
rule "sibling-process-irreversible" prompt {
  irreversible "*"
  lease_sibling_holder                    // another agent live in the same process
}
rule "remote-vision" prompt { capability "inference.remote.vision" }
rule "default-allow" allow { capability "*" }   // reached only if the grant permits

profile "operator" { /* S-01 §7 template, expanded here */ }
```

Semantics:
- **Predicates**: `capability` (glob), `target_class`, `target_app_id`,
  `target_handle`, `node_role`, `node_name` (regex), `node_source`,
  `node_confidence_lt`, `irreversible` (taxonomy id glob), `url`,
  `principal`, `principal_profile`, `grant_has`, `grant_scope`,
  `provenance_contains`, `app_trust`, `time` (window), `rate_gt`,
  plus:

```
provenance_min_trust <untrusted|standard|trusted|human>
provenance_head source:"<kind>"
provenance_age_gt <duration>
provenance_mismatch
provenance_absent                      // acting request with empty provenance_ids
app_irreversible_capable <bool>        // used in the §3 example, never defined
egress_bytes_gt <bytes> window=<duration>
secret_bound_host_mismatch
node_credential <bool>                 // ext.credential, S-08 §3.2
lease_holder <principal-glob>          // target handle leased by a match
lease_sibling_holder                   // target's client process has another
                                       // live lease held by a different principal
target_divergence                      // agent's claimed handle differs from
                                       // the handle resolved at press time
                                       // (COMP-08 §10 step 6e)
```

  `provenance_*` predicates read the `ProvenanceRef` summary (S-07 §5) on the
  fast path. Any predicate needing the *full* chain (e.g. matching a specific
  `Url` link) is `defer`-only and the compiler enforces that, as it already
  does for other context-hungry predicates.

  `lease_sibling_holder` covers what the per-toplevel lease cannot: two
  browser windows are two handles, one process, one cookie jar. Agent A
  signing out in window 1 while agent B is mid-checkout in window 2 is two
  *uncontended* leases and one broken task. Enforcement stays per-toplevel
  and cheap; the shared-state hazard becomes a policy question instead of an
  invisible one. The compositor already has the pid from `list_toplevels`.
- **Phases**: all `deny` rules are checked first; any match → deny. Then
  `prompt`, then `defer`, then `allow`. This ordering makes the language
  monotone: adding a rule can only tighten unless it is an `allow`, and
  `allow` rules cannot override a `deny`/`prompt`/`defer` match. `unless`
  clauses are the only relaxation and are limited to grant facts.
- Everything a predicate needs must be **available in the compositor at
  request time** (class, app_id, node role/name/source, provenance tag on
  the request, grant facts). Predicates needing context the compositor lacks
  (e.g., "does this message body contain a credential?") are only legal in
  `defer` rules; the compiler rejects them elsewhere.

---

## 4. Compilation

Input: policy files + live grants. Output: **enforcement table**, pushed to
the compositor on every change (atomic swap).

```
Table {
  version: u64,
  classify: [ (matcher, class) ],           // for sensitivity assignment
  trust:    [ (matcher, trust) ],
  irreversible: [ (matcher, taxonomy_id) ],
  rules: {
    deny:   [ CompiledRule ],
    prompt: [ CompiledRule ],
    defer:  [ CompiledRule ],
    allow:  [ CompiledRule ],
  },
  per_principal: { principal → { caps, scopes, quotas, expiry } },
}
CompiledRule { id, predicates: [Pred], outcome, prompt_text_template?, defer_hint? }
```

- Regexes compiled once (Rust `regex`, DFA, bounded memory). Globs → DFA.
- Compiler errors: unknown predicate, `defer`-only predicate in a fast
  rule, unreachable rule (shadowed), rule referencing undefined taxonomy or
  profile. Errors keep the previous table live and surface in trusted UI.
- Table is signed by `policyd`; compositor verifies before swap.

---

## 5. Prompt Path

Compositor suspends the request, renders trusted UI (COMP-10) with:
principal, task text (from grant), the concrete action ("Click *Send* in
Gmail — Firefox, window 'Inbox'"), taxonomy id, provenance summary, and the
owner's anti-spoof secret. Options: **Allow once · Allow for this task ·
Allow unattended for 1 h (scope shown) · Deny · Deny & pause agent**.
Answers are audited and, for "for this task"/"unattended", turned into new
grants by `policyd`. Prompt frequency per grant is counted against
`prompt_budget`, which is a **task** counter (A-04 §6), not a per-grant one.
Exceeding it flags every grant on that task, since the mis-sizing is of the
task's authority as a whole (F-02 §10.5).

## 6. Defer Path

Compositor sends `{request, target facts, provenance chain, recent audit
window}` to `policyd`. `policyd` runs deterministic checks first (S-06
extended matchers, provenance analysis), then optionally the classifier
(I-03). Outcome ∈ {deny, prompt, fallthrough}. **Fallthrough** means "no
objection", and the compositor then applies the static `allow` (if the
grant permits) — never anything broader. Timeout → `deferred_timeout`
(fail-closed). Target latency ≤ 200 ms without classifier, ≤ 500 ms with.

## 7. Testing

- Property test: for all rule sets, `deny` phase result is invariant under
  addition of `allow` rules (monotonicity).
- Golden decision suite: 500 (request, expected outcome, rule id) cases,
  run on both `policyd` and the compositor's evaluator to prove they agree
  (the evaluator is one shared crate; the test guards against drift).
- Fuzz the KDL parser and regex inputs.

## 8. Open Decisions

1. Shared evaluator crate linked into both processes (proposed) vs. spec
   conformance only.
2. Whether `allow` rules exist at all, or the grant alone is the allow
   (proposed: keep `allow` for explicit exceptions like `messenger-unattended`;
   grant still required).
3. Time-window predicates (`time`) in v1. Proposed: yes, simple ranges.


---

<!-- ===== FILE: S-03_SANDBOX_PROFILES.md ===== -->

# S-03 — Sandbox Profiles (Draft v0.1)

Depends on: THREAT_MODEL.md §7, S-01 §2.7. Consumed by: A-01, S-08, S-09.

Every agent process, and every process an agent launches, runs inside a
sandbox compiled from the agent's grants. The sandbox is the enforcement
point for filesystem, network, D-Bus, and syscall capabilities; the
compositor and `agentd` enforce the rest.

---

## 1. Layers

| Layer | Mechanism | Enforces |
|---|---|---|
| L1 Identity | systemd transient scope in `agents.slice/agent-<id>.slice` | cgroup = principal identity; resource limits |
| L2 Namespaces | bubblewrap: user, pid, ipc, uts, net, mount | isolation from host processes, IPC, network |
| L3 Filesystem | read-only root + tmpfs home + explicit binds; **Landlock** ruleset matching grants | `fs.read/write` |
| L4 Syscalls | seccomp-bpf allowlist per profile | attack surface |
| L5 Network | `pasta` user-mode net with egress allowlist; no ingress | `net.egress` |
| L6 D-Bus | `xdg-dbus-proxy` with allow rules | `dbus:*` |
| L7 Wayland | no `$WAYLAND_DISPLAY`; only `$ECLIPSE_MCP_SOCKET` | agents cannot bypass `agentd` |

Defense in depth: L3 has both mount-level (bubblewrap) and kernel-level
(Landlock) enforcement so a mount-namespace escape still hits Landlock.

---

## 2. Base Profile (all agents)

```
bwrap
  --unshare-user --unshare-pid --unshare-ipc --unshare-uts --unshare-net
  --new-session --die-with-parent --clearenv
  --ro-bind /usr /usr --ro-bind /etc/ssl /etc/ssl --ro-bind /etc/resolv.conf /etc/resolv.conf
  --symlink usr/lib /lib --symlink usr/lib64 /lib64 --symlink usr/bin /bin
  --proc /proc --dev /dev                       # minimal /dev, no /dev/input, no /dev/dri (see §4)
  --tmpfs /tmp --tmpfs /home/agent --tmpfs /run
  --ro-bind $RUNTIME/eclipse/agents/<id>/mcp.sock /run/eclipse/mcp.sock
  --bind $STATE/agents/<id>/scratch /home/agent/scratch
  --setenv HOME /home/agent --setenv ECLIPSE_MCP_SOCKET /run/eclipse/mcp.sock
  --setenv ECLIPSE_AGENT_ID <id>
  [grant-derived binds]
  --seccomp <profile.bpf>
  -- <argv>
```
Never present: `$WAYLAND_DISPLAY`, `$DISPLAY`, `$DBUS_SESSION_BUS_ADDRESS`
(replaced by proxy socket if `dbus:*` granted), `~/.ssh`, `~/.gnupg`,
browser profiles, password-manager data, `/etc/eclipse/policy`,
`~/.config/eclipse`, `/dev/input`, the privileged compositor socket.

Landlock: `LANDLOCK_ACCESS_FS_READ_FILE|READ_DIR` on `/usr`, `/etc/ssl`;
`READ|WRITE|MAKE_*|REMOVE_*` on scratch; per-grant paths added; everything
else denied. Restricted-network Landlock (ABI ≥4) blocks `bind`/`connect`
on TCP entirely inside the namespace, since `pasta` handles egress.

seccomp base: deny `ptrace`, `process_vm_*`, `mount`, `umount2`,
`pivot_root`, `kexec_*`, `add_key`, `request_key`, `keyctl`, `bpf`,
`perf_event_open`, `userfaultfd`, `open_by_handle_at`, `pidfd_getfd`,
`io_uring_setup`/`io_uring_enter`/`io_uring_register`, `socket`
with `AF_PACKET|AF_NETLINK` (except `NETLINK_ROUTE` read), `bind` on
non-`AF_UNIX` families (S-09 §1), `personality`,
`unshare`/`setns`/`clone3` with namespace flags.

io_uring is denied outright, not "revisit": an io_uring instance performs
filesystem and network operations submitted through a shared ring, which is
exactly the shape that bypasses per-syscall filtering. Rust async runtimes
wanting io_uring fall back to epoll inside agent sandboxes.

Mounts: `hidepid=2` on `/proc`; `/sys` masked except the subset the GPU
userspace needs when a sandboxed app renders.

**D-Bus**: agent sandbox profiles deny `org.a11y.Bus` and the accessibility
bus socket. Direct AT-SPI access from an agent bypasses classification,
scoping, and sanitization entirely. `registryd` only.

Resource limits (cgroup): memory default 4 GiB, CPU weight 50, pids 512,
tasks-per-agent adjustable via grant `constraints`.

---

## 3. Grant → Sandbox Compilation

| Grant | Sandbox effect |
|---|---|
| `fs.read:<path>` | `--ro-bind <path> <path>` + Landlock READ |
| `fs.write:<path>` | `--bind` + Landlock READ/WRITE/MAKE/REMOVE |
| `net.egress:<host:port>` | `pasta` egress allowlist entry; DNS pinned to resolved IPs at grant time, re-resolved on TTL; wildcard hosts → SNI-based filtering via local proxy (S-09) |
| `dbus:<name>[.<iface>]` | `xdg-dbus-proxy --filter --talk=<name>` (+ `--call=`) with proxy socket bound in |
| `secret.use:<name>` | nothing in sandbox; credential broker (S-08) is reachable only via `agentd` |
| `launch` targets | launched binaries must exist under `/usr` inside the sandbox; scripts under scratch allowed only with `launch.shell` |
| `launch.outside_sandbox` | the *launched app* runs in the human's session with its window bound to the agent's grants; the agent process itself stays sandboxed |

Compilation is deterministic; the resulting `bwrap` argv, Landlock ruleset,
and seccomp program hash are recorded in audit with the grant id.

---

## 4. GPU & Display Access

Agents have **no** `/dev/dri` by default: they perceive through the
protocol, not by rendering. Agents that legitimately need GPU (local
inference runners under `system:` principals, not agents) run outside this
profile. An agent-launched GUI app (via `launch`) gets a *nested* profile
(§5) that includes `/dev/dri` and a Wayland socket to the compositor's
**public** socket with a `wp_security_context_v1` tag carrying the agent id,
so the compositor knows which principal launched it.

---

## 5. Nested Profile (apps launched by agents)

Same as base, plus: `/dev/dri` (render nodes only), `$WAYLAND_DISPLAY` to
the public socket via security-context, fonts, icons, locale, `/dev/shm`,
optional PipeWire socket if `dbus:org.freedesktop.portal.*` granted.
Inherits the agent's `fs.*` and `net.egress` grants; no MCP socket.

The compositor tags all toplevels from this cgroup with
`launched_by=agent:<id>`; the launching agent's scope
`principal:launched_by_self` matches them.

---

## 6. `agentd` and `registryd` Profiles

- `agentd`: no network (unless remote MCP, v2), no home, privileged
  compositor socket bound in, per-agent MCP sockets it creates, IPC to
  `policyd`. seccomp base.
- `registryd`: AT-SPI bus access (`dbus:org.a11y.*`), compositor
  perception IPC, no network, no input devices, no home. It receives
  capture buffers for non-`secret` surfaces only (F-02 §10.4 proposed
  stance adopted).

---

## 7. Escape Testing (into S-10)

- Verify each denied path from inside a running agent sandbox:
  `/dev/input`, `$WAYLAND_DISPLAY`, policy dirs, other agents' scratch,
  raw sockets, `ptrace` of sibling, network to non-allowlisted host, D-Bus
  call outside allow rules.
- Verify Landlock holds when bubblewrap is bypassed (simulate by running
  the Landlock ruleset alone).
- Verify launched-app toplevels carry the correct `launched_by` tag and
  cannot be claimed by another agent's scope.

## 8. Open Decisions

1. `io_uring` deny in v1 (proposed: deny; re-enable per profile later).
2. `pasta` vs `slirp4netns` (proposed: `pasta`, faster and maintained).
3. Whether agents may ever get `/dev/dri` directly (proposed: no; local
   inference is a `system:` service).
4. Default memory/pids limits per profile tier.


---

<!-- ===== FILE: S-04_AUDIT.md ===== -->

# S-04 — Audit Schema, Storage & Query (Draft v0.1)

Depends on: THREAT_MODEL.md (T11, A5), S-01, COMP-12. Consumed by: I-04
(training data), S-11 (incident response), X-02.

The audit log is the system's memory of what agents did, why it was
allowed, and what they saw. It must be complete, tamper-evident,
queryable, and useful as classifier training data — while never becoming
a keylogger.

---

## 1. Record

Canonical CBOR; JSON projection for tooling.

```
Record {
  seq:        u64                // strictly increasing, per store
  ts:         u64 ns (CLOCK_REALTIME) + mono: u64 (CLOCK_MONOTONIC)
  kind:       Kind
  principal:  string             // agent:<id> | human | system:<daemon>
  grant_id:   string?            // ULID of governing grant
  req_id:     u64?
  serial:     u64?               // compositor event serial
  body:       <per-kind struct>
  prev_hash:  [u8;32]
  hash:       [u8;32]            // BLAKE3(prev_hash || canonical(record sans hash))
}
```

### 1.1 Kinds and bodies

| Kind | Body |
|---|---|
| `request` | interface, request name, args (secrets elided per §3), target handles, target class, node ids, expected_generation |
| `decision` | outcome (allow/deny/prompt/defer), rule_id, phase, latency_us, defer_detail? (classifier score, features hash) |
| `prompt` | prompt text shown, options, answer, human latency, resulting grant_id? |
| `result` | status, detail, focus_handle, generation, latency_us |
| `input` | seat, event type, keycode/keysym/button/axis (agent seats only), req_id |
| `focus` | seat, from_handle, to_handle, cause (human/agent/req_id/client) |
| `capture` | kind, target, region, scale, format, content_hash, redacted_regions |
| `perception` | tree/text read: handle, generation, node count, node id list hash, source_mix, bytes |
| `channel` | channel, msg id, from, size, provenance chain hash |
| `grant` | full grant KDL, issuer, reason (rule/prompt/manifest) |
| `revoke` | grant_id, reason |
| `launch` | argv, cgroup, sandbox program hashes, resulting handle |
| `sandbox` | compiled bwrap argv hash, landlock hash, seccomp hash |
| `policy` | table version, source file hashes, compiler warnings |
| `lifecycle` | agent start/stop/pause/resume, override on/off |
| `net` | principal, grant_id, host, sni, ip, port, mode (`splice`\|`mitm`), bytes_up, bytes_down, duration_ms, outcome, rule_id? (S-09 §7) |
| `secret` | secret_id, rotation_counter, mode, destination, principal, grant_id, outcome, length? (S-08 §7) |
| `task` | task_id, principal, origin, origin_ref, parent_task_id, state, statement_hash, counters_snapshot, reason. Emitted on every state transition. |
| `lease` | event (`acquire`\|`release`\|`refuse`\|`expire`\|`override`), principal, handle, app_id, holder principal (recorded unconditionally on `refuse`, regardless of the disclosure limit applied to the agent in COMP-08 §4.1) |
| `remote_inference` | surface handle, app_id, resolved class, provider, content_hash, bytes, grant_id, outcome |
| `anchor` | periodic external anchor (§4) |

---

## 2. What Is Never Logged

- Human keystroke content or pointer coordinates. Human input produces
  only `focus` records.
- `password`-role values (never delivered anyway).
- Raw capture pixels; only `content_hash` + redaction map. Optional
  retention of capture buffers for N minutes is a separate, off-by-default,
  encrypted ring (S-11) and never leaves the machine.
- Full text of `secret`-class reads (logged as `[secret:<len>]`).

---

## 3. Elision Rules

`request.args` and `input` for agent seats are logged in full **except**:
`seat.text` payloads targeting `password`/`secret` nodes → length only;
`clipboard.write` payloads > 4 KiB → hash + length; `launch.env` values
matching `(?i)key|token|secret|passw` → elided.

Agent-typed text into non-secret fields **is** logged. This is agent
behavior, not human behavior, and it is the primary evidence in an
injection investigation.

---

## 4. Storage & Integrity

- Writer: `policyd` only. Sources push over `SOCK_SEQPACKET` with
  backpressure; the compositor stalls the *agent* whose records cannot be
  accepted (never the human path).
- Format: append-only segments (`audit-<date>-<n>.cbor.zst`), 64 MiB
  rotation, `fsync` per segment close and every 250 ms.
- Hash chain per store; each segment header carries the previous segment's
  final hash.
- **Anchors**: every 5 min and at rotation, `policyd` writes
  `{seq, hash, ts}` to a location outside every sandbox and outside
  `policyd`'s own writable tree (proposed: `/var/lib/eclipse/anchors/`
  owned by root, written via a tiny setuid-less helper socket; optionally
  mirrored to a hardware token or remote endpoint, off by default).
- Verification tool: `eclipse-audit verify` recomputes the chain and
  checks anchors.
- Retention: default 90 days full, then `request/decision/prompt/grant`
  kept 1 year, others dropped. Owner-configurable.

## 5. Query

- Local index (SQLite, rebuildable from segments) over `seq, ts, kind,
  principal, grant_id, req_id, handle, rule_id, outcome`.
- CLI: `eclipse-audit query --principal agent:x --kind decision --outcome deny
  --since 1h`; `eclipse-audit trace --req-id N` shows request → decision
  → prompt → input → result chain; `eclipse-audit replay --agent x --since`
  reconstructs a timeline.
- Trusted-UI panel (COMP-10) shows the live tail per agent.
- Agents have **no** access (B2). The owner may grant a *read-only
  projection* to a `system:` reviewer principal, never to agents.

## 6. Training Export (I-04)

`eclipse-audit export --for-classifier` emits joined tuples
`(request facts, provenance, decision, prompt answer)` with all free text
replaced by hashes unless the owner opts in per field. Prompt answers are
the labels. Export is itself an audited action.

## 7. Performance

- ≥ 50k records/s sustained on reference hardware; p99 write latency
  ≤ 1 ms into the socket.
- A `click` produces ~5 records (request, decision, input×2, result); a
  busy agent at 20 actions/s ≈ 100 records/s ≈ trivial.

## 8. Open Decisions

1. Anchor destination beyond local root-owned dir (hardware token? remote?).
   Proposed: local only in v1, hooks for both.
2. Whether agent-typed text into `private` fields should be logged in full
   or hashed. Proposed: full (see §3 rationale), owner-configurable.
3. SQLite index vs. a purpose-built columnar index. Proposed: SQLite.


---

<!-- ===== FILE: P-01_SEMANTIC_TREE_SCHEMA.md ===== -->

# P-01 — Unified Semantic Tree Schema (Draft v0.1)

Depends on: THREAT_MODEL.md, S-01. Consumed by: COMP-09, P-02..P-08,
A-02, I-03.

One schema for every perception source (native protocol, AT-SPI2, terminal
grid, vision synthesis) so agents and `policyd` never care where a node
came from — only what it is and how much to trust it.

---

## 1. Node

```
Node {
  id:          u64          // stable within a tree generation; see §4
  role:        Role
  name:        string?      // accessible name (label)
  description: string?
  value:       Value?       // text content, numeric value, selection…
  states:      set<State>
  rect:        Rect         // surface-local logical px; compositor joins to global
  actions:     list<Action>
  sensitivity: Class        // public | private | secret (floor from rules)
  source:      Source       // native | atspi | terminal | vision | synthesized
  confidence:  f32          // 1.0 for native/atspi; <1.0 for vision
  children:    list<u64>
  parent:      u64?
  ext:         map<string, any>   // source-specific extras (aria attrs, X11 class…)
}
```

### 1.1 Role (closed enum; mirrors AT-SPI2/ARIA; extension via `ext.role_raw`)
```
window dialog alert popup menu menubar menuitem toolbar statusbar
panel group section list listitem tree treeitem table row cell columnheader
button togglebutton checkbox radio switch link tab tablist
textfield textarea password searchbox combobox spinbutton slider progressbar
label heading paragraph text image icon canvas video
scrollbar scrollarea separator tooltip
terminal terminal_line terminal_cell
document article region navigation form landmark
unknown
```

### 1.2 State (set)
```
focused focusable selected selectable checked mixed expanded collapsed
disabled readonly required invalid busy modal hidden offscreen
editable multiline multiselectable pressed default_button
scrollable_x scrollable_y
```

### 1.3 Value
```
Text{ text, cursor?, selection?[start,end] }
Number{ value, min?, max?, step? }
Selection{ selected: list<u64> }
Url{ href }
Empty
```
Text for `password` role is always `Empty` at every source; the compositor
enforces this regardless of what the app publishes.

### 1.4 Action
```
Action { verb: Verb, label?: string, args?: schema }
Verb: activate | set_value | focus | scroll_to | expand | collapse |
      select | toggle | increment | decrement | dismiss | context_menu |
      custom:<name>
```
Actions are *offers*. Invoking one is a `seat.action` request; it may be
denied, prompted, or deferred like any action (S-01).

### 1.5 Source & confidence
| Source | Confidence | Notes |
|---|---|---|
| `native` | 1.0 | `eclipse_semantic_v1` |
| `atspi` | 1.0 | rect may be corrected by the coordinate join (P-02) |
| `terminal` | 1.0 | grid → lines/cells; TUI structure heuristic gets `synthesized` |
| `vision` | model-reported | OCR/detector output; rect from detector |
| `synthesized` | ≤0.9 | `registryd` inferred structure (TUI panes, table from text) |

Agents and `policyd` may condition on `source`/`confidence`: e.g., an
irreversible action targeting a `vision` node of confidence <0.8 is
`prompt` (S-06).

---

## 2. Tree

```
Tree {
  handle:      u64          // compositor surface handle (COMP §5.2)
  generation:  u64          // monotonically increasing per handle
  root:        u64
  nodes:       map<u64, Node>
  source_mix:  set<Source>
  app_trust:   trusted | standard | untrusted     // from S-05 rules
  timestamp:   monotonic ns
  complete:    bool         // false if truncated by budget
  truncated_ids: list<u64>? // subtrees elided (pruning, P-05)
}
```

One tree per toplevel; popups/menus are subtrees with role `popup`/`menu`
attached under the owning node, not separate trees, because agents think in
windows.

---

## 3. Global Coordinates & the Join

Node rects are **surface-local logical pixels**. `registryd` (or the
compositor for native trees) attaches to every delivered tree:

```
Placement { output, global_origin: (x, y), scale, transform, visible_region }
```
so `global_rect = transform(rect) + global_origin`. Agents receive both; the
compositor's `click(handle, node)` takes node ids and performs the join
itself, so agents never compute coordinates in the common path.

Clamping: the compositor clips node rects to the surface bounds. A node
that claims to extend outside its window is clipped and flagged
`ext.clamped=true` (T2).

---

## 4. Identity & Generations

- **Node ids** are stable for the life of the underlying widget within a
  tree generation. For `native`, the app assigns and must not reuse until
  the node is removed. For `atspi`, `registryd` maps AT-SPI object paths
  to ids and holds the mapping while the object lives. For `vision`,
  ids are stable across frames only if the detector matches the same
  region (IoU>0.7 and same text); otherwise new ids.
- **Generation** increments on any structural change (add/remove/reorder)
  or on a change to `rect`, `role`, `states.hidden/offscreen/disabled`, or
  `actions`. It does **not** increment on `value` text changes alone (a
  terminal scrolling would otherwise invalidate every action), but the
  `value` carries its own `value_rev`.
- Actions carry `expected_generation`; the compositor compares to the
  current generation for that handle and fails with `stale_generation` on
  mismatch (COMP §0.5).

---

## 5. Delivery Formats

- **Full**: entire tree, budget-limited (default 4,000 nodes; P-05 prunes).
- **Pruned**: relevance-ranked subset with `truncated_ids` (P-05).
- **Diff**: `{generation_from, generation_to, added[], removed[], changed[]}`
  for subscribers.
- **Flat text**: `get_text` returns `Text` for a node or a concatenated
  reading order for a subtree — the token-cheap path for reading a page.
- Serialization: CBOR over the wire; JSON in the MCP surface (A-02).
  Field names above are canonical in both.

Token guidance for `agentd`/SDK: a pruned tree of 300 nodes serializes to
~6–10k tokens of JSON; the SDK offers a compact table encoding
(`id|role|name|states|rect`) at ~2–3k for the same content.

---

## 6. Provenance

Every string an agent receives from a tree is tagged at the SDK level with
`{handle, node, source, app_trust, sensitivity}` and this tag is the first
link in any channel message's provenance chain (F-02 §7.5, S-07).

---

## 7. Sensitivity Rules at the Schema Level

- `role=password` → `secret`, always, at every source.
- Nodes inside a toplevel classified `secret` inherit `secret`.
- Class is computed by the join in S-05 §3 over: owner classify rules,
  role-derived (`password` → `secret`), app-declared raise, ancestor node
  class, and owning toplevel class. The join is a **maximum**, so ordering is
  irrelevant and no source can lower a class another source raised. Only
  owner policy sets a `public` base.
- Delivery: nodes above the agent's granted class are replaced by a stub
  `{id, role, sensitivity, states:{hidden}}` with no name/value/rect; the
  agent knows something is there and that it cannot see it (so it can ask
  via `grant.request`).

---

## 8. Terminal Extension

For `role=terminal` trees (P-04): `terminal_line` nodes in reading order
with `value.Text`, `ext.row`, `ext.is_prompt` (heuristic), `ext.scrollback`
(bool). Cursor exposed as `ext.cursor={row,col}` on the terminal node.
A TUI detector may `synthesize` panes/tables/menus over the grid.

---

## 9. Web Extension (P-08)

`ext.url`, `ext.frame` (iframe id), `ext.shadow_host`, `ext.aria` (raw
attributes), `ext.tag`. `role=document` per frame; cross-origin frames
carry `app_trust=untrusted` regardless of the browser's class.

---

## 10. Open Decisions

1. Default node budget (4,000 proposed) and whether it is per-agent
   configurable (proposed: yes, via grant constraint).
2. Whether `value` text changes should ever bump generation for specific
   roles (e.g., `textfield` before `activate` on a form). Proposed: yes for
   `textfield/textarea/combobox` when the agent's own action changed them.
3. Compact table encoding as default in MCP vs. JSON. Proposed: table
   default, JSON on request.

---

---

*End of Volume 1. Continues in Volume 2 with S-05 Sensitivity Classification.*
