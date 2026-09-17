# 0045 — Grants are COSE_Sign1 over canonical CBOR, Ed25519, embedded payload
Status: accepted
Date: 2026-09-17
Deciders: chase (owner), Claude (advisory)

## Context
S-01 §4 states grants are CBOR + COSE_Sign1 with KDL as the human-readable
projection, and S-01 §8 still lists that as a *proposed* resolution of open
decision 1. S-01 §6 requires the compositor to verify against policyd's public
key, provisioned at startup over trusted IPC, such that `agentd` can neither
fabricate nor extend a grant. Neither volume names an algorithm, a header
split, a `kid` scheme, or whether the payload is detached. `external_aad` does
not appear in either volume.

## Decision
Accept CBOR + COSE_Sign1, and fix the parameters the spec leaves open:

- **alg `-8` (EdDSA) over Ed25519.** 64-byte signatures, no curve agility, no
  algorithm negotiation — the verifier accepts exactly one `alg` and refuses
  every other value rather than dispatching on it.
- **Protected header carries `alg` and `kid`; the unprotected header is empty**
  and a non-empty one is refused. Everything that governs verification is
  inside the signed bytes.
- **`kid` is BLAKE3-256 of the raw 32-byte public key.** No names, no counters,
  no issuer-chosen string — key identity is derived from the key.
- **Payload embedded, not detached.** A grant travels as one self-contained
  blob through `create_agent(id, grant_blob)` (VOL1 §3527); a detached payload
  is a second thing to lose.
- **`external_aad` is the empty string.** Every field that binds a grant to its
  task, principal and expiry is in the payload, where a verifier cannot forget
  to supply it.
- The `Sig_structure` is canonical CBOR per ADR 0044, as is the payload.

## Consequences
- Grant verification in `abyss` is: parse, check `alg`/`kid`, one Ed25519
  verify, then compare `expires` against the clock. No allocation beyond the
  parse, no key lookup by name.
- Rotating policyd's key changes every live grant's `kid`; grants are
  short-lived and bounded by their task deadline (S-01 §4), so rotation is a
  restart concern and not a revocation mechanism.
- We are not COSE-generic and cannot read a third party's COSE object. That is
  deliberate: the only issuer is policyd.

## Revisit when
A hardware-backed key store forces a different `alg`, or a second issuer
appears — which would itself be an S-01 §6 violation and should be argued
before it is implemented.
