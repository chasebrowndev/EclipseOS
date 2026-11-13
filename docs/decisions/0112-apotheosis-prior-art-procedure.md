# 0112 — Prior art is studied, never copied
Status: accepted
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
The owner permits reading kitty, wezterm, foot, ghostty and others: "read and understand logic, not functions directly; how they do things, not their exact implementations." kitty is GPL-3.0; several others are MIT/Apache. Agents that read foreign source while writing tend to transliterate it.

## Options
1. Forbid reading prior art. Loses years of solved edge cases.
2. Allow reading with a written reminder not to copy. Unenforceable with agents.
3. Separate the reader from the writer: a read-only agent answers in prose and logs each lookup; implementers never open foreign source.

## Decision
Option 3 (T-00 §3.4). The `apoth-prior-art` agent reads clones under `~/.cache/apotheosis-prior-art/`, answers with approach, invariants and edge cases — no code — and appends to `docs/internal/apotheosis/PRIOR-ART.md`. Protocols are implemented from published specifications; where the only reference implementation is GPL (kitty graphics and keyboard), its source is not consulted for that crate. Unicode tables are generated from the UCD.

## Consequences
A provenance log exists for every borrowed idea. Slower than copying; that is the point. Reviewers treat structural similarity to a named prior-art function as a defect.

## Revisit when
A licence question arises about a specific module, or the log shows the procedure being bypassed.
