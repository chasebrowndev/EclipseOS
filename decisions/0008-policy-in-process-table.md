# 0008 — Policy enforced in-process from a compiled table; `defer` is tighten-only
Status: accepted
Date: 2026-09-04
Deciders: chase (owner), Claude (advisory)

## Context
Every agent request crosses a policy check on the compositor's hot path, on a
10th-gen i5 that also runs four daemons (F-04 §4). An IPC round trip to
`policyd` per request would put a scheduler on the frame path. But some
decisions genuinely need slow-path context.

## Options
1. IPC to `policyd` per check — one policy implementation; unacceptable latency.
2. In-process compiled table — table lookup, no allocation; two code paths to
   keep in agreement.
3. In-process only, no slow path — cannot express context-dependent rules.

## Decision
`policyd` compiles rules into an enforcement table that `abyss` evaluates
in-process, as a table lookup with no allocation. Only requests explicitly
marked deferrable take the `policyd` slow path, and a deferred answer may only
**tighten** the table's outcome — never turn a deny or prompt into an allow
(ratchet rule). Everything is fail-closed: unknown request, missing entry, or
an unavailable `policyd` denies.

## Consequences
- Hot-path check is benchmarked in `bench/` and stays allocation-free.
- Two evaluators exist, so a golden decision suite proving `policy-eval` ≡
  `abyss` is a CI gate once S-02 lands.
- `policyd` being down degrades to deny, never to allow.

## Revisit when
The golden suite proves impossible to keep green, or table compilation cannot
express a rule class we need.
