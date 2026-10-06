<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ec-agentd — the agent daemon

Governing spec: A-01, COMP-08 §1-§3, C-00 §8.1. **Not TCB**: abyss verifies
every grant and filters every answer (`abyss/src/policy/`); agentd holds no
authority beyond the signed grants `policyd` gives it.

- Lib plus daemon. No arguments runs the daemon: `policyd` link (C2),
  `console.sock` (C5, A-08 §7), per-task `mcp.sock` (C6), launcher. See
  `docs/internal/console-plan.md`. One core thread owns all state; socket,
  link and reaper threads only pass `Msg`s by channel.
- It cannot create a task. Only `PauseTask`, `CancelTask` and `Exited` are ever
  sent to `policyd`; `tests/no_task_creation.rs` greps the source for it.
- The M11 admission client (`admit.rs`, `ec-agentd [--list] <grant.cose>`:
  dials `ec-agent.sock`, `ECLIPSE_AGENT_SOCKET` overrides) stays: it becomes
  the per-task bridge.
- Message text is never logged, audited or put in a `channel` record; only
  ids, sizes and a chain hash.
- The `dev-unsandboxed` feature runs agents as plain children for tests. It
  must never appear in the PKGBUILD.
- Ships only in the `eclipseos-agents` add-on (ADR 0069).
- SPDX `AGPL-3.0-only` on every file: it is a system daemon, not an SDK.
- Never logs window titles or other scene content to the journal; `--list`
  prints to stdout for the human who ran it.
