# eclipse-secret-prompt — the one-field password prompt

Read the root `CLAUDE.md` first. Governing decision: ADR 0053.

- **Not TCB.** An ordinary xdg_toplevel. Its protection is the owner
  windowrule in `/etc/eclipse/policy.kdl` that classifies app-id
  `^eclipse-secret-prompt$` as `secret` — so the app-id is load-bearing and
  must not change.
- **The secret is never logged, stored or shown by `Debug`.** No `derive(Debug)`
  on anything that can hold it; every copy we own is overwritten when replaced
  or dropped. No tracing of what was typed (root invariant).
- Fixed size (min == max, not resizable): abyss floats such toplevels.
- No literal colour, radius or size: `eclipse_ui::tokens` only.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
