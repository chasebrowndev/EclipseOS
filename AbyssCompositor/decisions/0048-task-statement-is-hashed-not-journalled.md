# 0048 — A task's statement is journalled as a hash, and does not survive a restart
Status: accepted
Date: 2026-09-17
Deciders: chase (owner)

## Context
S-04 §1.1 (and A2-07, the amendment that introduced the kind) fix the `task`
record body as `{task_id, principal, origin, origin_ref, parent_task_id, state,
statement_hash, counters_snapshot, reason}`. The field is `statement_hash`, not
`statement`: the audit log records *which* statement a task ran under, not the
text of it.

The first cut of `policyd`'s task journal wrote the statement verbatim, because
`policyd` rebuilds its task objects from those records and `Task::statement` is
a field like any other. That is a real conflict with the spec, and the spec is
right: a task statement is human text about human work, and the audit log is
the one file in the system most likely to be read by someone who was never
meant to see it.

What the spec does not say is where the text lives instead. A-04 §9 lists what
survives a `policyd` restart — grants, chain roots, audit-persisted links — and
the statement is in neither that list nor the list of things that don't.

## Decision
The `task` record carries `statement_hash` (BLAKE3-256 of the trusted
statement) and never the statement text.

**A task's statement does not survive a `policyd` restart.** `read_task_body`
reconstructs a task with an empty `statement`; it reads the journalled hash
only to check its shape, and discards it.

This is allowed to be lossy because nothing in `policyd` reads `statement`.
The statement is rendered by trusted UI at prompt time (A-04 §11, COMP-10
§3.2), in the live process that was handed it; a `policyd` that has restarted
is not that process, and any prompt it drives afterwards is a new one carrying
its own text. An auditor recovering what a task was *for* reads the `prompt`
records, whose body is the prompt text as shown (S-04 §1.1) — the hash is what
ties those records back to the task.

The alternative — a side table of statement texts keyed by hash, durable across
restarts — was rejected. It reintroduces exactly the file the spec declined to
write, with none of the audit chain's integrity guarantees protecting it, to
serve a reader that does not exist.

## Consequences
- `Task::statement` is empty on a replayed task. Anything that grows a use for
  the statement inside `policyd` is a change to this decision, not a bug to
  patch around by re-adding the field to the record.
- `Task::statement_hash()` derives from `statement`, so on a replayed task it
  hashes the empty string and will not match the journalled hash. Nothing
  compares them today; a future comparison needs the hash stored on the task,
  which is the point at which this ADR gets revisited.
- The rule generalises: the task journal records what `policyd` needs to
  decide, plus what S-04 §1.1 names. Human text is neither.

## Revisit when
`eclipse-audit trace` (milestone 12) needs to show a statement beside a task
and the `prompt` records turn out not to cover the case — for instance a task
that was opened and closed without ever prompting.
