# 0033 — an explicit render-device that does not resolve refuses to start

Status: accepted
Date: 2026-09-07
Deciders: chase (owner), Claude (advisory)

## Context

COMP-01 §4 gives the render device an override chain: the `--render-device`
CLI flag, then `ECLIPSE_RENDER_DEVICE`, then `misc { render-device … }` in
the config, with smithay's `primary_gpu()` as the auto default. Vol 1 §12
Open Decision 1 left unresolved what happens when the named device does not
exist — refuse to start, or warn and auto-select.

## Options

1. **Refuse to start.** A typo, a stale PCI address after a hardware change,
   or a card that moved node numbers is reported immediately at the one place
   that can explain it.
2. **Warn and fall back to auto.** The compositor always comes up, but on a
   multi-GPU box it may come up on the wrong GPU with the warning buried in
   the journal — the failure then looks like a performance or capture bug
   rather than a config error.

## Decision

Option 1, as the spec proposed. `resolve_render_device` in
`backend/drm.rs` returns an error for an explicit request that names a
missing path, a PCI address with no `drm/` directory in sysfs, or a PCI
address exposing no card node. Absent or `"auto"` keeps the auto query and is
the only path that may fall back. `misc { render-device "auto" }` is accepted
as an explicit way to spell the default, so a config can override an
inherited value back to auto.

Multi-GPU is out of scope for v1 (COMP-01 §4), so the resolver deliberately
does no ranking; it resolves one name or fails.
