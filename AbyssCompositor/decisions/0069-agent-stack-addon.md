# 0069 — The agent stack is one add-on
Status: accepted
Date: 2026-09-30
Deciders: chase (owner), Claude (advisory)

## Context
Agents are the reason EclipseOS exists, but most of the people who install it
on a given day will not run one. Today `policyd` is in the floor: it ships in
`eclipseos-policyd` under `eclipseos-meta`, its unit is wanted by
`abyss-session.target`, and it starts every session for everyone (D-07 §4.4,
"`policyd` is in the floor"). COMP-01 §6 has abyss open the privileged socket
unconditionally and draw a persistent "agents disabled" indicator whenever
`policyd` is absent, so a system that never wanted agents would read as
permanently degraded.

The rest of Phase 2 (`agentd`, `brokerd`, the egress proxy, `registryd`, the
inference router, the MCP surface, `cataclysm`) has no code yet, so where it
ships is cheap to decide now and expensive later.

ADR 0066 already defines what an optional piece is: a package with a manifest
that switches on host hooks that stay off otherwise. Build variants and plugins
are ruled out.

## Options
1. **Keep `policyd` in the floor; the rest optional.** Every system pays for a
   daemon only agents use, and the "agents disabled" indicator is always on
   for people who chose no agents.
2. **Agents as a build variant of abyss.** Ruled out by ADR 0066, and it would
   put the enforcement point in a binary most CI runs never build.
3. **The whole agent stack is one add-on; the compositor keeps its half built
   in and dormant.** The split is by "does a person without agents get
   anything from this", not by what touches the network.

## Decision
Option 3.

**The add-on.** One package, `eclipseos-agents`, ships every agent-only
process: `policyd`, `agentd`, `brokerd`, the egress proxy, `registryd`, the
inference router, the MCP surface, and the `cataclysm` terminal when it exists.
Its manifest enables the `agents` hook. `eclipseos-policyd` folds into it and
leaves `eclipseos-meta`. The Agentic install profile (ADR 0060, D-07 §4.4)
installs it; no other profile does.

**What stays in the base.** abyss keeps its side compiled in and dormant: the
privileged socket and agent globals (COMP-08), grant verification and scope
filtering (S-01 §6), the enforcement table (COMP-11), and the
`eclipse_semantic_v1` server (COMP-09). The enforcement point has to live in
the compositor, and ADR 0066 lets an add-on switch host behaviour on, never
supply code. Also in the base, because they serve people without agents:
the window model, sensitivity classes and redaction, the capture policy
(`policy.kdl`), and `trusted_ui/`.

**The `agents` hook (abyss).** When it is on, abyss does what COMP-01 §6 and
COMP-08 describe. When it is off:
- no `abyss-agent-N` socket is created and no agent global is advertised on
  any socket;
- abyss makes no attempt to reach `policyd` and shows no "agents disabled"
  indicator. No add-on is the normal state, not degraded mode;
- `eclipse_semantic_v1` is not advertised, so clients spend nothing
  publishing trees that nothing reads;
- the agent lifecycle methods (`get_agents`, `pause_agent`, `resume_agent`,
  `terminate_agent`, `revoke_grants`) are refused like any hook-bound method
  (ADR 0066 §Authority).

The `agent-override` (Super+Escape) and `agent-attention` binds stay with the
hook off. The override chord is the trusted-UI escape and always works
(COMP-04 §6). Attention opens phrase entry and the pending decision queue
(COMP-10 §3.10), which command-widget approvals use without any agent.

Turning the hook off at runtime (the add-on is removed) behaves like
"`agentd` dies" in COMP-01 §6: every agent object is destroyed and its seats
and workspaces are torn down, then the socket is removed.

**Other add-ons.** `fog-activity` (ADR 0066) depends on `eclipseos-agents`,
because `fog-activityd` subscribes to `agentd`. Oracle Eyes stays independent
until it acts through the agent protocol.

## Consequences
- **Spec.** VOL1 Appendix F records it: COMP-01 §6 gains the hook-off row,
  D-07 §4.4 moves `policyd` out of the floor, and ADR 0066's hook table gains
  `agents`.
- **Packaging (first Phase 2 PR).** Fold `eclipseos-policyd` into
  `eclipseos-agents` with an `addon.kdl` naming `agents`. Drop it from
  `eclipseos-meta` and from the base `abyss-session.target.wants/`. The setup
  plan's Agentic profile adds the package.
- **abyss.** Add a `Hook::Agents` variant, checked before the privileged socket
  binds, before any agent or semantic global is created, and in the gate table.
- **CI.** The dormant code is still TCB and still tested on every push. The
  COMP-15 §2 suites run with the hook on. One test asserts that with the hook
  off, no agent global is reachable on any socket and no socket file exists.
- **Enabling later.** Installing a package needs root, and Settings' Add-ons
  pane is read-only (ADR 0066). A "set up agents" button waits on the trusted
  admin prompt (COMP-10), the same blocker as the boot-splash picker. Until
  then: `pacman -S eclipseos-agents`, then log out and back in.

## Revisit when
- A non-agent feature needs `policyd` (it would move back to the floor, or the
  feature gets its own daemon).
- The trusted admin prompt lands, and with it the in-session install path.
