# 0002 — Custom compositor on Smithay, pinned `=0.7.0`
Status: accepted
Date: 2026-09-04
Deciders: chase (owner), Claude (advisory)

## Context
`helios` needs a Wayland compositor base that leaves the scene graph, input
routing and policy hooks under our control, because the agent protocol
(COMP-08) and the enforcement path (COMP-11) reach into all three. Smithay is
a library, not a compositor; wlroots is C with its own opinions; forking an
existing compositor (Hyprland, niri) inherits a codebase whose input and focus
model we would have to gut.

Smithay 0.7 is pre-1.0 and makes breaking changes between minors. Its docs.rs
pages lag; the registry source is the only reliable reference.

## Options
1. Fork Hyprland — free Phase 1 features, C++, hostile to a rewritten input path.
2. wlroots via Rust bindings — mature, but FFI and a C threading/ownership model.
3. Smithay as a library — Rust end to end, we own state and the loop; pre-1.0 churn.

## Decision
Build `helios` as a Rust compositor on Smithay used as a library, with the
dependency pinned to `=0.7.0` (`default-features = false`). Version bumps are a
deliberate, separate PR. When writing against Smithay, read the vendored source
at `~/.cargo/registry/src/index.crates.io-*/smithay-0.7.0/` rather than
guessing or recalling APIs.

## Consequences
- We own `HeliosState`, the event loop, focus and the scene graph — required by
  COMP-01 §3, COMP-04 and COMP-11.
- No free Phase 1 features; milestones 1–9 are all ours to write.
- Smithay upgrades are project events, not dependency-bot noise.
- Smithay is MIT — compatible with AGPL-3.0-only (F-05 §9).

## Revisit when
Smithay 1.0 ships, or an upstream change we need is only in a later minor.
