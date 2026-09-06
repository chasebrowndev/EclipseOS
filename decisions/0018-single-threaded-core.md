# 0018 — Single-threaded core owning `HeliosState`; handle-based state
Status: accepted
Date: 2026-09-04
Deciders: chase (owner), Claude (advisory)

## Context
COMP-01 §3. The compositor's hot paths — input delivery and the policy check —
must not allocate or block, and the state must be snapshottable for crash
resilience. A shared-ownership graph (`Rc<RefCell<_>>`, or locks across
threads) makes both hard and makes borrow errors a daily tax.

## Options
1. Multi-threaded state behind locks — throughput we do not need, lock ordering
   bugs we cannot afford in the TCB, unbounded hot-path latency.
2. `Rc<RefCell<_>>` scene graph — single-threaded but panics at runtime and
   resists snapshotting.
3. Single `calloop` loop owning a plain-struct tree, children by handle.

## Decision
One `calloop` event loop owns `HeliosState` exclusively. State is a tree of
plain structs; children are referenced by `u64` handle or index, never by
pointer. No locks on the hot path. Concurrency is used only where it pays: a
render thread per GPU, and blocking work (config parse, screenshot encode,
audit serialization) on a small worker pool that communicates back into the
loop by channel.

## Consequences
- State snapshotting for crash resilience is trivial.
- Handles are stable ids for the session, which is exactly what the agent
  protocol already exposes — one concept, not two.
- Anything that wants to touch state from another thread must post a message
  instead. That constraint is deliberate; do not work around it.
- No allocation is permitted in input delivery or policy check; both are
  benchmarked.

## Revisit when
A profile on the reference machine shows the single loop is the bottleneck for
a target in COMP-14.
