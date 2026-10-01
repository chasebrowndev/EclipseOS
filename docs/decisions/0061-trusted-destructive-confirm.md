# 0061 — Destructive-action confirmation and its request channel
Status: accepted
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
D-07 §6 has a root `ec-setup-helper` that must not repartition a disk on the
say-so of a client: a typed disk name proves nothing against a `liveuser` process
that sends the right string. It therefore asks the compositor for a human-seat
allow/deny. The spec had no channel for that. The human control socket is gated on
`SO_PEERCRED` owner-uid (ADR 0028) and the helper is root. On the live medium the
compositor, the wizard and every other client share one uid, and the socket lives
in a directory that uid controls, so a client could unlink and rebind the path and
answer "allow" to a requester that trusts the path.

## Options
1. **A method on the human control socket.** Reuses `gate.rs`, but the peer rule is
   owner-uid, which is the wrong party, and a Privileged method there weakens the
   table's meaning.
2. **A separate compositor socket, root-only peers, requester authenticates the
   compositor by pid and exe.** The prompt cannot be raised or answered by a
   same-uid process that merely connects; a rebound path fails the requester's check.
3. **A root-owned listening socket; the compositor connects.** Fixes path hijack,
   but the listener can no longer tell the compositor from any other liveuser
   process, since both share the uid.
4. **Keep the typed name only.** Rejected by D-07 §6.

## Decision
Option 2, specified as COMP-10 §3.10. `$XDG_RUNTIME_DIR/eclipse/trusted.sock`, peers
uid 0 only, one request per connection, deny on every failure, the answer produced
only from human-seat keys (default Deny, Escape Deny, Allow needs Tab then Enter,
injected input dropped). The helper checks the peer's pid and `/proc/<pid>/exe`
against the session's compositor before sending. The live medium sets
`kernel.yama.ptrace_scope = 1`.

## Consequences
- New TCB code in `abyss/crates/ec-abyss/src/trusted_ui/`; owner review line by line.
- ADR 0053's closed list of COMP-10 §3 surfaces grows by one; COMP-10 §3.10 and
  Appendix D DA-05 record it.
- The stated limit stands: a same-uid process able to ptrace or inject into the
  compositor defeats this, as it defeats the control socket (ADR 0028). What it
  stops is a process that connects, runs pkexec or types a string.
- The prompt shows the requester's claims (model, size, by-id) unverified, and on
  the live medium shows the "phrase unset" warning form (DA-03).
- We owe: anti-spoof tests (z-order in all three backends, injected input cannot
  answer, non-root peer refused), and the helper's pid/exe check (track H).

## Revisit when
The phrase can be set before install (then show it), when a second destructive
action wants the surface, or when S-03 gives per-process identity that replaces the
pid/exe check.
