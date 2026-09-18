# 0047 — The `policy-eval` API surface: what the two processes are allowed to share
Status: accepted
Date: 2026-09-17
Deciders: chase (owner)

## Context
ADR 0008 puts one evaluator in one crate and links it into two processes,
`abyss` and `policyd`, so a decision cannot drift between the process that
mints authority and the process that enforces it. That only holds if the crate
exposes the *same* surface to both. The failure mode is quiet: an API added for
one caller's convenience — a mutable grant setter for policyd, a "just parse
it, skip the signature" shortcut for abyss — and the two processes stop being
two users of one evaluator and become two evaluators that happen to share a
file. The root `policy-eval/CLAUDE.md` already states the rule ("do not add an
API that only one of them can call"); milestone 10 built the first real surface,
so this records what it is and why each piece is shaped the way it is.

## Decision
`policy-eval` exposes three modules and nothing else.

**`cbor`** — `Writer`, `MapBuilder`, `enc`, `Reader`, `Error`. The canonical
profile of ADR 0044, written by hand with no serde so a reviewer can read the
exact bytes that get signed and hashed. `Reader` **rejects** non-canonical
input instead of normalising it: two encodings of one grant would be two
different signed objects, and a verifier that accepts both accepts a grant its
issuer never signed. `MapBuilder` sorts keys on `finish()` rather than trusting
the caller to insert in order — the ordering is a correctness property of the
format, not a style rule, so it is not left to call sites.

**`grant`** — `Grant`, `Capability`, `Constraints`, `Rate`, `VerifyError`,
`Grant::{encode, decode, verify, is_valid_at, allows}`, and the COSE_Sign1
primitives `key_id`, `protected_header`, `sig_structure`, `cose_sign1`
(ADR 0045). The split is the important part: `sig_structure` produces the bytes
to be signed and `Grant::verify` consumes a full COSE object, so **policyd
signs with the same byte layout abyss verifies**, by construction and not by
two parallel implementations agreeing. There is no `Grant::new`, no setter, and
no public mutation: a grant is immutable after issue (S-01 §4), so the type is
built by decoding signed bytes or by the issuer filling the struct literal
before it signs, and nothing in between can amend one.

`Grant::allows` is the hot path and allocates nothing — it compares borrowed
strings. Anything needing a `Vec` belongs in compilation, per the crate's own
no-allocation rule.

Expiry lives in `is_valid_at`, separate from `verify`'s signature check, and is
exact: no grace window, and `CLOCK_SKEW_MS` tolerance applies only to
`issued_ms` in the future — a clock that runs slow may not resurrect a dead
grant. The caller checks validity *at request time*, which is why it is its own
call and not folded irreversibly into decode.

**`task`** — `Task`, `TaskState`, `TaskEvent`, `CloseReason`, `Origin`,
`Counters`, `RateRing`, `Ulid`. The A-04 state machine lives here, not in
policyd, even though today only policyd drives it: abyss must be able to say
what state a task is in to answer a request against it, and a second copy of
the transition table is exactly the drift ADR 0008 exists to prevent.
`TaskState::transition` is a total function returning `Result`, so an illegal
transition is a value the caller must handle rather than a panic or a silently
ignored event, and `Closed` is terminal in the table itself.

## Consequences
- Every public item here has two callers or is on its way to having two. An
  item that only ever serves one process is a bug this ADR names.
- `TaskState::as_str()` is deliberately lossy — it collapses `Closed(reason)`
  to `"closed"` because it is a display/interop name, not a serialization.
  Anything persisting a closed task must journal the reason alongside it;
  policyd's task record does exactly that.
- Adding a record kind or a capability field touches one crate and both
  processes recompile against it, which is the point.
- The crate has no policy *language* yet — S-02 compilation and the
  enforcement table arrive at milestone 16 and will extend this surface with a
  compiled table plus a `check()` that is a lookup. This ADR is the shape they
  must fit into, not a final inventory.

## Revisit when
Milestone 16 lands `check()` and the compiled table, or a caller needs a
`policy-eval` item that genuinely serves only one of the two processes — in
which case the honest answer is a new crate, not an exception here.
