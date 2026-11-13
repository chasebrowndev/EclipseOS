# 0116 — The terminal's control socket never bypasses seat policy
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
Apotheosis exposes a JSON socket that can type into panes and read their contents (T-12). Any same-uid process can connect — including an agent whose seat requests the compositor would deny. Without a rule, the socket is an ambient-authority side door around `check()`.

## Options
1. No input or content methods on the socket. Loses the automation that is a headline goal.
2. Same-uid means trusted. Violates "no ambient authority".
3. Capability classes per client with consent, plus: requests from an agent peer (pidfd-derived cgroup under `agents.slice/`, on every host, independent of Abyss detection) get only `observe` by default; input, read-content, clipboard, spawn, layout and appearance are deferred to policyd via the T-12 §4.1 class→capability table; config-write and keys are always refused; an unreachable or unauthenticated policy service is a deny.

## Decision
Option 3 (T-12 §4). Classes: observe, layout, appearance, spawn, read-content, input, clipboard, keys, config-write. The user's own shell (a descendant of the target pane) acts on its own pane without prompts. The `keys` stream is config-only opt-in and suppressed in the credential state.

## Consequences
Apotheosis needs a way to ask the policy service about a peer; until that query exists (COMP-11 X milestone), every agent-origin request outside `observe` is denied. Peer identity is taken once at hello via SO_PEERPIDFD (fallback pidfd_open plus SO_PEERCRED/start-time recheck) and bound to the connection; `socket.grants` and `socket.key-stream` are never writable over the socket. The R2 server attach socket and `apoth proxy` (T-21 §3, T-22 §5) are under the same rule: attach requires read-content + input per pane with seat-policy deference for agent peers; only the installed GUI binary is pre-granted.

## Revisit when
The agent gateway gains a first-class terminal capability that makes this deference redundant.
