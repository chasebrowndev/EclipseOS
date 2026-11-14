<!-- SPDX-License-Identifier: Apache-2.0 -->
# ec-term-pub — terminal grid publisher

Read the root `CLAUDE.md` first.

- **Not TCB.** It holds terminal text but enforces nothing; S-05's raise-only
  rule means nothing here can lower a class (F-07 §4).
- Governing spec: ADR 0114 (rename of `ec-cataclysm-pub`, C ABI removed, node
  ids and parent), T-17 §4, P-04 §2 (node model), §3 (OSC 133 blocks), §6.1
  (echo-off), §7 (publish rate, line diffing); COMP-09 §2 (the requests a
  batch is made of).
- **Apache-2.0**, not the workspace AGPL: protocol-shaped (F-05 §3 permissive
  tier). Every file's SPDX line says so.
- **Pure library.** No Wayland, no I/O, no clock, no threads, no logging, no
  dependencies. Time comes in as a monotonic `now_ns` argument. Rust only:
  there is no C ABI (ADR 0114).
- Node ids come from the caller's per-surface [`NodeIds`]: each publisher owns
  one disjoint [`IdRange`] for the life of the surface, ids are never reused
  within it, and its `terminal` node attaches under a parent the caller owns.
- Echo-off invariants: the raise is the next batch after `set_echo(false)`,
  ahead of every rate limit, and carries nothing but `ext.echo=false` (plus the
  terminal node itself if it was never published); no batch at all leaves
  while raised; the grace cannot be configured shorter. What drives
  `set_echo` (the credential predicate, ADR 0115) is the caller's.
- Grid text is never logged. Line/ext strings are sanitised (no NUL or
  controls, valid UTF-8, bounded) because the bytes come from whatever runs
  in the terminal.
- The gate is `tests/fixtures.rs`: the P-04 §8 throughput, echo-off and
  OSC 133 fixtures applied to a mirror tree that enforces COMP-09 request
  validity (ADR 0114 replaces the C/ASan fixture with it).
- Every `.rs` starts with `// SPDX-License-Identifier: Apache-2.0`.
