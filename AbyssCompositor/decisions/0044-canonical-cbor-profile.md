# 0044 — One canonical CBOR profile, hand-rolled, for grants, audit and sockets
Status: accepted
Date: 2026-09-17
Deciders: chase (owner), Claude (advisory)

## Context
Three things in Phase 2 are CBOR and none of them agree on it in the spec.
S-04 §1 says audit records are "Canonical CBOR" without naming a profile.
S-01 §4 says grants are CBOR + COSE_Sign1 without naming an encoding. COMP-13
§3 says the `policyd` socket is "SEQPACKET, CBOR" with no canonicality
requirement at all. Two of the three are signed or hashed, so a byte that
re-encodes differently is a verification failure, not a cosmetic difference.

## Options
1. `ciborium` + `coset`. Mature, but ciborium neither emits nor validates
   deterministic encoding, so canonicality would still be ours to enforce on
   top — and it pulls serde into the enforcement path.
2. Hand-rolled encoder/decoder restricted to the profile.
3. Different profiles per surface, converted at the boundary.

## Decision
One profile, RFC 8949 §4.2.1 core deterministic encoding, governs all three
surfaces: shortest-form integer heads, definite lengths only, map keys sorted
by encoded bytes, no tags, no indefinite items, no floats. It is implemented
by hand in `policy-eval::cbor`, and the decoder **rejects** any input that is
not in the profile rather than accepting and normalising it.

Rejecting is the point. A decoder that accepts sloppy input and a hasher that
hashes the canonical form disagree about what a record says, and that gap is
where a chain-break forgery lives.

## Consequences
- The signed and hashed byte layout is legible in TCB source, not generated.
- No serde anywhere in the enforcement path; the profile is ~1 file to review.
- Floats are unrepresentable. Nothing in A-04 §6, S-01 §4 or S-04 §1 needs one;
  a future field that does is an amendment to this ADR, not a local cast.
- Third-party CBOR tooling reading a segment sees ordinary CBOR, since the
  profile is a subset.

## Revisit when
A spec'd payload needs a type outside the profile, or a fuzzer finds a byte
sequence the decoder accepts that re-encodes differently.
