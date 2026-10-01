# policyd — task store, grant issuer, audit writer

Read the root `CLAUDE.md` first.

- **TCB, and the only writer of the audit store.** Owner-reviewed line by line;
  never delegated to `eclipse-backend` (F-07 §4).
- Governing spec: A-04 (task objects, §§2, 4–6, 9), S-01 §4 and §6 (grants),
  S-04 §1 and §4 (record envelope, store), COMP-13 §3 (socket). ADRs 0044,
  0045, 0046, 0047 (the `policy-eval` API surface), 0048 (a task statement is
  journalled as a hash, and does not survive a restart).
- **Journal before answer.** A task transition or counter change is durable
  before the caller is told it happened (A-04 §5). Answering first and
  journalling after is the bug this daemon exists to not have.
- **One active-or-paused-or-draining task per principal** (A-04 §4). The second
  is refused, not queued.
- A grant is refused at issue if its `expires` exceeds its task's `deadline`
  (S-01 §4). Closing a task revokes every grant naming it in **one** operation
  — a partial revocation is a grant that outlived its task.
- policyd holds the only signing key. It signs; it never verifies a grant it is
  handed and never treats a caller-supplied grant as authority (S-01 §6).
- Deny is the answer on every error path, including a full disk and a failed
  fsync. An audit write that cannot be made durable stalls the agent that
  caused it (S-04 §4 backpressure); it never proceeds unjournalled.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
