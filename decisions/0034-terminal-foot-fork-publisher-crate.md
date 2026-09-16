# 0034 — Terminal emulator: keep the foot fork, extract the publisher

Status: accepted
Date: 2026-09-08
Deciders: owner, Claude (advisory)

## Context

P-04 §1 chose a foot fork as the terminal perception source; §9 Open
Decision 1 left the choice provisional. A from-scratch Rust emulator
("Cataclysm") was considered, motivated by speed, lightness,
customizability, and agent fit.

Speed and lightness do not survive scrutiny: foot is at or near the floor
for a Wayland terminal, and agents read the grid at P-04 §7's bounded
publish rate, not the draw loop. Every agent-facing capability in P-04
(OSC 133 blocks, echo-off→`secret`, untrusted line provenance, TUI
synthesis, scrollback ranges) is delivered by the patch, not by owning the
VT layer.

What is real: the emulator parses adversarial escape sequences while
holding secret bytes (P-04 §6.1), and a C fork must reimplement
`eclipse_semantic_v1` types, the node model, and diff/publish logic that
already exist in Rust for `abyss` — a duplication tax on every protocol
revision. Against that: Phase 1 is not past its exit gate, there is no CI,
and COMP-16 milestones 10-18 need re-sequencing before P-04 can be built
at all.

## Options

1. Fork foot, publisher in C. Fastest to a working terminal; permanent
   two-language duplication of the semantic protocol; C parses untrusted
   input in a secret-holding process.
2. Rewrite in Rust from scratch. Memory safety and one language; months of
   VT/font/shaping/mouse/IME work with no agent-visible gain; direct
   conflict with quality-over-speed sequencing.
3. Rust on `alacritty_terminal` or `termwiz`. Cuts the rewrite to a
   frontend plus publisher; still displaces Phase 1/2 work and still starts
   behind foot on maturity.
4. Fork foot; write the publisher as a Rust crate with a C ABI, linked by
   foot via FFI and by `abyss` natively.

## Decision

Option 4. `cataclysm` remains a foot fork for v1. The semantic publisher —
`eclipse_semantic_v1` types, node model, line diffing, and the P-04 §7
publish-rate policy — is built as `cataclysm-pub`, a Rust crate exposing a
C ABI, consumed by the fork through FFI and by the compositor directly.

The from-scratch emulator is deferred, not rejected: this reduces it to
replacing a frontend under a stable publisher rather than writing a
terminal.

Naming: `cataclysm` is both the codename and the binary name. The earlier
identifier `eclipse-term` is retired and must not appear in new documents.

## Consequences

- Easier: protocol revisions are one change in one language; `abyss` and
  the terminal cannot drift on node semantics.
- Harder: an FFI boundary in the terminal, and a Rust build dependency in a
  C fork's build.
- Forbidden: reimplementing any part of the node model or publish policy in
  C. If it is protocol-shaped, it lives in the crate.
- Owed: the crate's C header and lifetime/ownership rules; the P-04 §8
  throughput and echo-off fixtures run against the FFI path, not a Rust
  test harness only; P-04 §1/§8/§9 edited; the `eclipse-term` →
  `cataclysm` rename applied across the planning index, D-05, and STATUS.

## Revisit when

- An `eclipse_semantic_v1` revision costs more to port across the FFI
  boundary than the boundary saved, or
- a memory-safety CVE lands in foot's escape-sequence parser, or
- foot upstream diverges enough that the fork is a rewrite in practice.
