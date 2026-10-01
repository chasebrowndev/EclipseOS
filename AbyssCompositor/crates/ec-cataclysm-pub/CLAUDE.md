<!-- SPDX-License-Identifier: Apache-2.0 -->
# ec-cataclysm-pub — terminal grid publisher, C ABI

Read the root `CLAUDE.md` first.

- **Not TCB.** It holds terminal text but enforces nothing; S-05's raise-only
  rule means nothing here can lower a class (F-07 §4).
- Governing spec: ADR 0034, P-04 §2 (node model), §3 (OSC 133 blocks), §6.1
  (echo-off), §7 (publish rate, line diffing); COMP-09 §2 (the requests a
  batch is made of); COMP-16 milestone 21 (gate).
- **Apache-2.0**, not the workspace AGPL: linked into the MIT foot fork, and
  protocol-shaped (F-05 §3 permissive tier). Every file's SPDX line says so.
- **Pure library.** No Wayland, no I/O, no clock, no threads, no logging, no
  dependencies. Time comes in as a monotonic `now_ns` argument.
- ADR 0034: nothing protocol-shaped goes into the C fork. Node ids, roles,
  ext keys, diffing and the rate policy all live here; C only forwards ops.
- The C header `include/ec_cataclysm_pub.h` is hand-written and is the
  contract, including its ownership/lifetime section. Changing `src/ffi.rs`
  means changing the header in the same diff (and `ECPUB_ABI_VERSION` if
  incompatible).
- Echo-off invariants: the raise is the next batch after `set_echo(false)`,
  ahead of every rate limit, and carries nothing but `ext.echo=false`; no
  batch at all leaves while raised; the grace cannot be configured shorter.
- Grid text is never logged. Line/ext strings are sanitised (no NUL or
  controls, valid UTF-8, bounded) because the bytes come from whatever runs
  in the terminal.
- The gate is `tests/ffi.rs`: it compiles `tests/c/fixtures.c` against the
  header and the staticlib and runs it under ASan/LSan/UBSan. Rust unit tests
  are extra, not a substitute (COMP-16 M21).
- Every `.rs` starts with `// SPDX-License-Identifier: Apache-2.0`; C files
  with `/* SPDX-License-Identifier: Apache-2.0 */`.
