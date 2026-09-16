# 0035 — Day-one CI: cloud-only gate, `cargo-deny` interim, mechanized review rules

Status: accepted
Date: 2026-09-08
Deciders: owner, Claude (advisory)

## Context

F-07 §3 requires CI on every push and names four day-one commands.
COMP-14 §5 requires benchmark regression enforcement in it. COMP-15 §2 as
amended requires ten security suites to block every push. None of this
existed: the gate was run by hand, which is why STATUS defect 10 classes
the security suites as *unenforceable* rather than merely unimplemented.

Standing up CI is cheap. Making it enforce what COMP-15 §2 requires is
weeks of suite-building. Conflating the two would have turned a one-day
task into a phase. This ADR records the boundary and three deviations from
F-07 as written.

## Decision

**1. Day-one CI is cloud-only, no GPU.** The self-hosted runner on the
reference machine is deferred until a benchmark harness exists to run on
it. COMP-14 §5 already places GPU benchmarks nightly rather than per push,
so nothing per-push needs a GPU today. This closes F-07 §7 open item 1
(CI competing with dev work for the GPU): it cannot compete for a resource
it does not use.

**2. `cargo deny` is adopted now as an interim supply-chain gate.** F-07 §3
lists `cargo audit` + `cargo vet`, added after S-12. `cargo-deny` covers
advisories, licenses, bans, and sources in one tool with one config, and
costs nothing to add today. `deny.toml` at the repo root is therefore a
policy artifact written before its owning document. **S-12 must consume it,
not re-decide it** — or supersede it explicitly and say so.

The license allowlist in `deny.toml` is permissive-only (Apache-2.0, MIT,
MPL-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0). Rationale: the workspace is
AGPLv3 (F-05); a copyleft dependency that is not ours is a licensing
decision, not a dependency decision, and must surface as a build failure
rather than as a discovery at release. Additions to the allowlist require a
dated justification in the PR body.

**3. F-07 §4 and §5 are mechanized.** Both were prose conventions with no
enforcement.

- §4 (owner reads every line of the enforcement path) becomes a CI job that
  **warns** on the PR when the diff touches a TCB path. It warns rather
  than blocks because with one reviewer a hard block is theatre. Its value
  is that the obligation is printed where it cannot be forgotten.
- §5 (every PR cites the spec section it implements) becomes a CI job that
  **blocks**. Required format: `(Implements|Fixes|Amends) <ID> §<n>`, where
  `<ID>` matches `(COMP|S|P|A|F|I|D|X)-[0-9]{2}`. This is the mechanism
  that keeps the spec→code trail auditable when work is delegated to an
  agent rather than typed by hand, so it is worth a hard failure.

Mechanizing §4 requires an authoritative list of TCB paths, which no
document previously contained. It is added to F-07 §4 as a table; the CI
regex derives from that table and not the reverse.

## Consequences

- STATUS defect 10 splits: the *unenforceable* half closes; the ten
  missing suites remain open and become COMP-16 input.
- STATUS defect 11 (no benchmark harness) is untouched and must not be made
  to look otherwise by the presence of CI.
- `rust-toolchain.toml` becomes mandatory: CI and dev must not drift, the
  same rule as the Smithay 0.7.0 pin in `CLAUDE.md`.
- The gate job must be set as a required status check in branch protection.
  Without that, the workflow is a report and not a gate, and this ADR is
  unimplemented regardless of what is merged.

## Revisit when

- S-12 is written (it owns supply chain and may supersede `deny.toml`).
- A benchmark harness exists (stand up the self-hosted runner; reopen
  F-07 §7 item 1 with real contention data).
- The security suites of COMP-15 §2 begin landing (each needs a CI slot and
  a tier assignment from COMP-15 §1).
