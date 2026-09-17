# 0046 — The S-04 §4 audit store is the task/counter journal, built at milestone 10
Status: accepted
Date: 2026-09-17
Deciders: chase (owner)

## Context
A-04 §6 requires task counters to survive a `policyd` restart, which needs a
durable journal. S-04 §4 separately specifies a hash-chained, append-only audit
store written by `policyd` alone, and A-04 §6 already routes the journal over
"the same `SOCK_SEQPACKET` path as audit, with the same backpressure". COMP-16
sequences the audit store in milestone 12, after the task store in 10.

The advisory recommendation was a small private journal at 10, replaced at 12.
The owner chose to build the real store at 10 instead.

## Decision
Milestone 10 builds the S-04 §4 store, and policyd's task/counter journal is a
`task`-kind record in it (S-04 §1.1, amendments A2-07/A2-08). There is one
durable path in the TCB, not two.

Milestone 10 defines the full record envelope, the canonical-CBOR encoding, the
chain computation, segment rotation, fsync policy and anchors. It defines the
`task` body only; the other seventeen `kind` bodies stay milestone 12's work,
as does `eclipse-audit verify` and retention.

## Consequences
- Milestones 10 and 12 are no longer independent. 12 becomes "emit the
  remaining record kinds and ship the verifier" against a store that exists.
- Counter durability is proven by the same chain that proves audit integrity;
  a restart test that replays the journal also exercises the chain.
- A store-format mistake is paid for twice as early, which is the trade the
  owner took knowingly.
- Spec silences resolved here, none of which S-04 states: `<date>` is UTC
  `YYYYMMDD` and `<n>` a zero-padded per-day counter; the 64 MiB rotation
  threshold counts **uncompressed** record bytes, so rotation is predictable
  from what was written rather than from how well it compressed; zstd frames
  one per segment; segments and the store directory are `0600`/`0700` owned by
  policyd's user; a truncated tail segment is repaired at startup by
  discarding the trailing partial record and reopening, and a tail whose chain
  does not verify is sealed aside rather than appended to.

## Revisit when
Milestone 12's remaining record kinds need an envelope field that isn't there,
or the 250 ms fsync proves too costly on the target disk.
