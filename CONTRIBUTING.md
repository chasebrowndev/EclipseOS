# Contributing to EclipseOS

## Read this first: the CLA and why it exists

EclipseOS is dual-licensed: AGPL-3.0-only for everyone, and a commercial
licence for organisations that will not or cannot comply with the AGPL.

**Selling commercial licences requires holding the rights to the code, so
contributions require a signed Contributor Licence Agreement — and that means
the project may sell commercial licences of code you contributed.** You are
told this plainly, before you sign, on purpose (F-05 §6).

- The CLA is Apache ICLA-derived: you grant the project a perpetual,
  irrevocable copyright and patent licence with the right to relicense. **You
  keep your copyright.**
- Contributing on company time? Your employer signs a corporate CCLA.
- A CLA bot checks pull requests. No signature, no merge.
- DCO-only sign-off was considered and rejected: it grants no relicensing
  rights, which would forfeit the dual-licensing model above. A `Signed-off-by`
  line is welcome but is not a substitute for the CLA.

If that trade is not acceptable to you, that is a completely reasonable
position — fork under the AGPL instead. Nothing about the free version is
degraded to sell the paid one, and that is a commitment (F-05 §7).

## Workflow

Trunk-based development. No long-running feature branches — with one reviewer,
they rot.

1. Branch from `main`, named for the milestone it implements:
   `comp16-m03-multi-output`, `s02-evaluator`.
2. Keep it short-lived. Merge on green CI.
3. Open a PR.

## Commits

Conventional commits, scoped by crate:

```
feat(helios): multi-output hotplug with layout persistence
fix(policyd): deny on missing table entry instead of falling through
docs: record focus-follows-mouse default as ADR 0019
```

## Pull requests

- **Cite the spec section you implement** in the PR body:
  `Implements COMP-08 §4`. This is what keeps the spec→code trail auditable
  when work is delegated.
- **If you change behavior a doc specifies, update the doc in the same PR**, or
  explain why not.
- CI must be green: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo build`, `cargo test`, `cargo deny check`.

## Review policy

| Area | Review |
|---|---|
| `helios` enforcement path, `policyd`, `policy-eval`, `sandbox` | Owner reads every line. No exceptions. These are the TCB. |
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
