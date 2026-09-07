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

## Amendment — 2026-09-07
The resolution order is inverted: the basename of `/proc/<pid>/exe` is tried
first, and `/proc/<pid>/comm` is now only the fallback. `comm` is capped at 15
bytes, so every `xdg-desktop-portal-*` backend reports `xdg-desktop-por` and a
single allowlist entry would have admitted all of them. `exe` is maintained by
the kernel, is not writable by the process, and is not truncated. The identity
is still containment rather than authentication: a binary copied under an
allowlisted name still passes.

## Amendment — 2026-09-07 (hot reload)
The "editing the list needs a restart" consequence above is retired. Both
allowlists are now a `config::Allowlist` — an `Arc<RwLock<Vec<String>>>` handle
held by `HeliosState` and *shared with* the global's bind filter, rather than a
`Vec<String>` snapshot moved into the closure. `config::watch::reload_now`
writes the new names through the handle, so the next bind sees them; clients
that already hold the global keep it, because a visibility filter is only
consulted when a client asks what globals exist. Revoking a live grant is still
restart-only and is COMP-11 work.

The lock is not a hot-path violation: it is touched at global-bind time and on
config reload, never in input delivery or a policy check. It exists to satisfy
the `Send + Sync` bound wayland-server puts on filter closures, not to
coordinate anything — the core stays single-threaded. Read failures (a poisoned
lock) report "empty" and "not contained", i.e. they deny.
