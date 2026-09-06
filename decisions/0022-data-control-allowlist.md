# 0022 — `wlr_data_control` behind a process-name allowlist
Status: accepted
Date: 2026-09-06
Deciders: chase (owner), Claude (advisory)

## Context
`zwlr_data_control_manager_v1` hands any client that binds it every clipboard
and primary-selection offer, forever, with no focus requirement and no user
gesture. That is ambient authority over the human's clipboard — exactly what
the root invariants forbid — but clipboard managers (`cliphist`,
`wl-clip-persist`) are load-bearing for the intended UX, so the protocol cannot
simply be omitted.

Smithay 0.7 builds the global with a per-client filter
(`DataControlState::new::<D, F>`, `F: Fn(&Client) -> bool`) that drives
`GlobalDispatch::can_view`, so a denied client never sees the global. It also
ships `ext_data_control` (the upstream successor); nothing on this machine
speaks it yet.

## Options
1. **Expose it unconditionally.** Any client, agent-spawned included, silently
   reads every secret ever copied. Rejected outright.
2. **Never expose it.** Safe, but breaks clipboard history, which the spec
   assumes exists.
3. **Config allowlist keyed on client identity, fail-closed.** Only names
   listed in `clipboard { data-control-allow ... }` see the global; everyone
   else, and every client whose identity cannot be established, is denied.

## Decision
Option 3, with identity derived from the client's socket credentials:
`Client::get_credentials()` → `pid` → `/proc/<pid>/comm`, falling back to the
basename of `/proc/<pid>/exe`. An empty allowlist (the default) denies
everyone. An unreadable or absent `/proc` entry denies. Denials log a warn with
the process name only.

`ext_data_control` is deliberately left unexposed until a client needs it; when
it lands it goes behind the same filter.

## Consequences
- Identity is a stopgap, not a capability. `SO_PEERCRED` pids can be reused and
  a binary can be renamed; anything that can already exec arbitrary binaries as
  the user can defeat this. It raises the bar from "any client" to "a client
  the user's config named", which is the point until COMP-05 app identity
  exists — `TODO(COMP-05)` in `protocols/standard/data_control.rs`.
- The filter closure captures the allowlist at `HeliosState::new`, so editing
  the list needs a restart. Hot-reload is M6 work.
- Clipboard *provenance* is separate and tracked independently: `new_selection`
  records the focused toplevel's `app_id` and the offered MIME type names.
  Contents are never read, logged or stored (root invariant).
