# 0007 — Agent seats are the default input model
Status: accepted
Date: 2026-09-04
Deciders: chase (owner), Claude (advisory)

## Context
Agents must synthesize input without stealing the human's keyboard focus or
racing the human's own typing (COMPOSITOR §4, COMP-04). Sharing the human seat
makes concurrency indistinguishable from a hijack, and makes audit attribution
guesswork.

## Options
1. Inject on the human seat — trivial; unattributable, races the human, unsafe.
2. One Wayland seat per agent — clean attribution and isolation; some clients
   assume a single seat.
3. libei / portal only — no per-surface targeting, no compositor-side policy hook.

## Decision
Each agent gets its own Wayland seat. Input from an agent is delivered on that
seat, so focus, attribution and policy checks are per-principal. For clients
that cannot cope with multiple seats, a per-app compatibility fallback runs
agent input on the human seat under a short exclusive lock, with human input
queued and delivered in order afterwards.

## Consequences
- Audit records name a seat, therefore a principal, with no inference.
- The human's focus is never disturbed by agent activity.
- Multi-seat support becomes load-bearing everywhere (focus arbitration,
  clipboard, IME, XWayland) and must be tested with concurrent typing.
- Compat lock needs a default timeout (open; proposed 2 s).

## Revisit when
A significant class of real applications proves unusable on a non-human seat.
