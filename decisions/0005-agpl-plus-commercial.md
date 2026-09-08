# 0005 — AGPLv3 for system code, Apache-2.0 for protocol and SDKs, CLA, commercial dual licence
Status: accepted
Date: 2026-09-05
Deciders: chase (owner), Claude (advisory)

## Context
F-05. The project needs a revenue mechanism that does not degrade the free
version, and a trust story that requires the whole TCB to be auditable. An
agent linking a copyleft SDK would be forced to AGPL, which would kill
third-party agent development — the exact ecosystem the project needs.

## Options
1. MIT/Apache throughout — no revenue mechanism; closed forks permitted.
2. GPLv3 — internal modification without publication; weak paid path.
3. Open core — closing the classifier or policy rules undercuts the trust story.
4. BSL/FSL source-available — not OSI open source; the label fight costs more
   than the revenue.
5. AGPLv3 + CLA + commercial dual licence.

## Decision
All first-party system code (`abyss`, `policyd`, `policy-eval`, `sandbox`,
`agentd`, `registryd`, packaging) is **AGPL-3.0-only**. Protocol definitions
and the agent SDKs are **Apache-2.0** (patent grant; lets agents be any
licence). Documentation is CC BY-SA 4.0. A CLA — Apache ICLA-derived, with a
corporate CCLA — is required before the first external contribution, and its
purpose (the project sells commercial licences of contributed code) is stated
plainly in CONTRIBUTING.md. Name and logo are not licensed by the AGPL.

## Consequences
- Line to hold: anything that runs as part of the system is AGPL; anything an
  external agent links against is Apache-2.0.
- SPDX headers required on every source file.
- GPLv2-only dependencies are forbidden; `cargo deny` gates this in CI.
- Owes: `LICENSE`, `LICENSE-APACHE`, `NOTICE`, `CONTRIBUTING.md`,
  `TRADEMARK.md`, `deny.toml`. CLA-bot before external PRs are accepted.
- Legal review and a professional trademark clearance are required before
  taking organisational money. This ADR is not legal advice.

## Revisit when
A lawyer reviews the texts, a first commercial customer appears, or contributed
audit data is used to train shipped weights.
