# Contributing to EclipseOS

## Before your first pull request: the CLA

EclipseOS is dual-licensed: AGPL-3.0-only for everyone, and a commercial
licence for organisations that cannot comply with the AGPL. To offer that, the
project needs the right to relicense contributions, so a short Contributor
Licence Agreement (CLA) comes with your first pull request. We tell you what it
means before you sign (F-05 §6).

- **You keep your copyright.** You grant the project a perpetual, irrevocable
  copyright and patent licence, including the right to relicense. It is an
  Apache ICLA derivative.
- **It is one signature,** once, through a bot on your first pull request.
  Contributing on company time? Your employer signs a corporate CCLA.
- **Your contribution may be used in the commercial offering.** That is the
  trade, stated plainly.
- A `Signed-off-by` line is welcome but is not a substitute: DCO alone grants no
  relicensing rights, so it cannot back dual licensing.

If that trade is not for you, that is a reasonable position. Fork under the AGPL
instead. Nothing in the free version is held back to sell the paid one (F-05 §7).

Questions about the CLA are welcome in an issue before you start.

## Workflow

Trunk-based development. No long-running feature branches — with one reviewer,
they rot.

1. Branch from `main`, named for the milestone it implements:
   `comp16-m03-multi-output`, `s02-evaluator`.
2. Keep it short-lived. Merge on green CI.
3. Open a PR. Small fixes and drive-by improvements are welcome as they are.

## Commits

Conventional commits, scoped by crate:

```
feat(abyss): multi-output hotplug with layout persistence
fix(policyd): deny on missing table entry instead of falling through
docs: record focus-follows-mouse default as ADR 0019
```

## Who merges

**Anyone may open a pull request. Only the project owner merges them.**
Contributors and their commits are welcome as they are; nobody else has merge
rights, and a green CI run does not merge anything by itself. `CODEOWNERS`
routes every change to the owner for review.

Commits you author stay under your name. The owner's own commits, and those of
their agents, are authored by the owner. A signed CLA is needed before your
first PR is merged.

## Pull requests

A short description of what changed and why is enough. A screenshot helps for
anything visual (dark and light, if both are affected).

- **A spec citation is optional.** If your change implements a section, say so
  (`Implements COMP-08 §4`), because it keeps the spec-to-code trail visible.
  The `spec-trail` check only posts a notice when there is none.
- **If you change behavior a doc specifies, update the doc in the same PR**, or
  explain why not.
- CI runs on every PR. `gate.yml` and its companions:
  - `gate` — `cargo fmt --check`, `cargo clippy -D warnings`, `cargo build`,
    `cargo test`. Blocking.
  - `supply-chain` — `cargo deny check advisories bans licenses sources`. Blocking.
  - `tcb-review` — owner review on TCB paths (below). Blocking for those paths.
  - `attribution` — commit messages carry no agent trailers. Blocking. Any
    human author is fine.
  - `conformance` (wlcs) is advisory: it runs and reports but never blocks.
  - `gui-coverage`, `doc-refs` and the rest report their results; the owner
    decides what to do with a red one.
  - `spec-trail` — a notice, never a failure.

## Review policy

| Area | Review |
|---|---|
| `abyss` enforcement path, `policyd`, `ec-policy-eval`, `sandbox` | Owner reads every line. No exceptions. These are the TCB. |
| Everything else | Merge on green CI; owner reviews at leisure |

## Architecture decisions

Anything that constrains future work gets an ADR in `decisions/`, in the format
of `decisions/TEMPLATE.md`. ADRs are never edited after acceptance except to
change status; superseding one means writing a new one.

## Code requirements

- Every source file carries an SPDX header:
  `// SPDX-License-Identifier: AGPL-3.0-only` for system crates,
  `Apache-2.0` for protocol crates and SDKs.
- The invariants in [CLAUDE.md](CLAUDE.md) are not negotiable. If a change
  seems to require violating one, stop and open an issue instead.
- No GPLv2-only dependencies — they are incompatible with AGPL-3.0.
  `cargo deny` enforces this.
