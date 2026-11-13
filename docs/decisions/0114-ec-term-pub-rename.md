# 0114 — `ec-cataclysm-pub` becomes `ec-term-pub`; the C ABI is removed
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
`abyss/crates/ec-cataclysm-pub` (STATUS milestone 21) was written for a C terminal: a 15-function C ABI, a header, and C fixtures under ASan. With ADR 0109 the only consumer is Rust.

## Options
1. Keep the name and the C ABI. A C header and C tests in the tree for no consumer; conflicts with the language rule's spirit.
2. Rename to `ec-term-pub`, delete the C ABI, header and C fixtures, port the fixtures to Rust tests.

## Decision
Option 2. Line diffing, publish-rate policy and OSC 133 mapping stay. The node model changes in exactly two ways: fixed ROOT_ID/SLOT_BASE/GROUP_BASE become a per-surface node-id allocator with a disjoint range per pane, and the publisher attaches its `terminal` node under a caller-given parent (window → tablist → tab); line nodes, `group` command blocks and their ext keys are unchanged. P-04 §1, §8 FFI/ASan fixture and §9 item 1 are superseded, the fixtures re-expressed as Rust tests; the echo-off raise changes per ADR 0115. `apoth-eclipse` owns one publisher per pane (T-17 §4). The reference client in `ec-abyss` follows the rename.

## Consequences
A mechanical PR across `abyss/` (milestone M20), with the P-04 §8 fixtures re-expressed in Rust so coverage does not drop. Third-party terminals that wanted a C ABI have none; they can speak the Wayland protocol directly.

## Revisit when
A non-Rust terminal asks to publish semantics.
