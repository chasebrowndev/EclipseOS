# 0117 — `apotheosis` joins the terminal app class in the default policy
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
`abyss/crates/ec-policyd/src/default-policy.kdl` applies `untrusted-origin-into-shell` to the terminals it names. A new default terminal that is absent from that class would silently lose the rule.

## Options
1. Leave it; rely on the terminal's own paste guard (T-18 §4).
2. Add `org.eclipseos.Apotheosis` to the class in the same PR that switches the default.

## Decision
Option 2, landed with the default switch (T-24 §5). There is no app class: the change adds the glob "org.eclipseos.Apotheosis*" to target_app_id of untrusted-origin-into-shell in abyss/crates/ec-policyd/src/default-policy.kdl, regenerates abyss/crates/ec-policyd/tests/golden/default-policy.tsv, and updates the VOL1 example; app id overrides on Abyss must keep the org.eclipseos.Apotheosis prefix (T-17 §7, T-09 §4). Owner review (TCB data). The terminal's own paste guard remains and behaves the same off Abyss; the two are independent layers.

## Consequences
A one-line TCB data change with tests, owner-reviewed. No user ever needs to edit `policy.kdl` for a terminal feature.

## Revisit when
The policy language gains a role-based (rather than app-id-based) way to name terminals.
