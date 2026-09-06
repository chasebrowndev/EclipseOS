# 0016 — Configuration format is KDL
Status: accepted
Date: 2026-09-04
Deciders: chase (owner), Claude (advisory)

## Context
COMP-13 needs a human-edited config with nested blocks (outputs, binds, rules,
policy references), hot reload, and machine-writable edits from the IPC layer.
The human UX is Hyprland-shaped, so the config should read that way.

## Options
1. hyprlang — familiar, but reimplementing someone else's parser and quirks.
2. TOML — ubiquitous; nested block structure gets ugly fast.
3. KDL — node/block structure that looks like hyprlang, a real Rust parser,
   spans for good error messages.

## Decision
KDL, with a Hyprland-like block structure. hyprlang is not reimplemented and
Hyprland-config compatibility is an explicit non-goal.

## Consequences
- Parse/validate/hot-reload lives in `crates/helios/src/config/`.
- Errors report file, line and column; an invalid config never takes down a
  running session — the last good config stays live.
- Migrating a Hyprland user is a documentation job, not a compatibility shim.

## Revisit when
KDL's Rust ecosystem stalls, or the config outgrows what nodes express.
