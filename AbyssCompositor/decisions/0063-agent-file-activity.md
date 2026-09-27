# 0063 — `agentd` is the sole trusted emitter of agent file-activity events
Status: proposed
Date: 2026-09-24
Deciders: chase (owner), Claude (advisory)

## Context
Depends on `agentd` (VOL1 §6), the sandbox (S-03) and ADR 0048 (task records). The consumer is Fog, in the agentic profile's `fog-agentic` build: live "where are agents working" views (Map, Files, Graph, Detail) and replay.

EclipseOS has no file-level record of what agents touch. The S-04 audit log records decisions, launches, tasks, secrets, network and sandbox events, but no file reads or writes. `eclipse_semantic_v1` (COMP-09) is a per-toplevel UI tree, and `cataclysm-pub` (ADR 0034) carries terminal nodes only.

Fog wants to show, at a glance, which files agents are working on and where they have been. It needs events only: no file contents, diffs or snapshots.

Constraints from the existing spec:
- Agents are bwrap-sandboxed and reach the OS only via MCP over their per-agent `agentd` socket. They have no session-bus access and can't reach other sockets (VOL2 ~L1641). They cannot report their own activity, and must not be trusted to.
- Identity is the cgroup principal: `agents.slice/agent-<id>.slice/launch-<req_id>.scope`. Tasks carry `task_id`, principal, origin and `statement_hash` (ADR 0048).
- Semantic-channel ext keys are bounded and untrusted, so they are unsuitable for carrying file events.

## Options
1. **`agentd` emits, from MCP tools plus fanotify mount marks on the sandbox's grant mounts.** Attribution is exact by construction, and no new privileged process is needed. `agentd` must keep up with fanotify volume.
2. **Harness hooks (Claude Code PreToolUse/PostToolUse).** Rejected. They are self-reports from an untrusted party, and they don't fit the MCP-only sandbox model.
3. **Carrying events over `eclipse_semantic_v1` ext keys.** Rejected. Those keys are bounded and untrusted, and they describe UI rather than filesystem activity.
4. **Filesystem-wide fanotify or eBPF with PID/cgroup filtering.** Rejected. It observes all user activity and must filter it out, which carries a leak risk and costs more. Mount-scoped marks see only sandbox access by construction.
5. **Snapshots and diffs.** Out of scope. Fog is not a review tool. A future review tool can propose its own ADR.

## Decision
Option 1.

1. **`agentd` is the sole trusted emitter** of agent file-activity events. Agents never self-report, and no model or agent tokens are involved.

2. **Mediated access (exact).** `agentd` MCP file tools emit an event for every read, search and mutation they perform.

3. **Direct access via sandbox mount marks (exact).** A sandbox reaches host files only through its own bind-mount instances (`fs.read` / `fs.write` grants). Only that sandbox uses those mount instances, so any access through them belongs to that agent.
   - At sandbox setup, `agentd` places fanotify marks with `FAN_MARK_MOUNT` on each of the sandbox's grant mounts. It subscribes to `FAN_OPEN`, `FAN_ACCESS`, `FAN_MODIFY`, `FAN_CLOSE_WRITE` and `FAN_CLOSE_NOWRITE`.
   - Every such event is attributed exactly to the owning task. No PID filtering is needed, and user activity on the same files, which goes through the host mount, is never observed.
   - `agentd` reads the opening process's `comm` (e.g. `rg`, `cat`, `cargo`) and includes it as `tool`.
   - Requires `CAP_SYS_ADMIN` in `agentd`, which is already privileged to build sandboxes. No new privileged process is introduced.
   - Marks are removed when the sandbox is torn down.

4. **Directory-entry changes (inferred where needed).** If the target kernel does not deliver create/delete/rename events for mount marks (to be verified), those ops fall back to inference: changes under a granted path during the task's lifetime, detected by consumers, are marked `inferred`. Changes outside all grants are never attributed to an agent.

5. **Sweep aggregation.** To keep the stream bounded, `agentd` collapses read bursts before emitting them.
   - If one process opens ≥ 20 distinct files within a 1 s window, the burst becomes a single `Sweep`: its root is the deepest common ancestor, plus file count, duration and tool.
   - Smaller bursts emit individual `read` events.
   - For MCP search tools, the `Sweep` carries matched paths. For kernel-observed sweeps, matches are unknown and omitted.
   - Thresholds are configurable in policy.

6. **Event stream.** `agentd` exposes a user-session activity socket at `$XDG_RUNTIME_DIR/eclipse/agent-activity.sock`.
   - Subscribers are authenticated by peer credentials. Only processes in the user's session slice may subscribe; agent scopes are refused.
   - The wire format is length-prefixed and versioned. Schema v1:

     ```
     TaskStart  { task_id, principal, agent_id, origin, grants[], ts }
     FileEvent  { task_id, principal, op, path, attribution, tool?, ts }
                  op: read | create | modify | delete | rename{from} | chmod
                  attribution: exact | inferred
     Sweep      { task_id, principal, root, file_count, duration_ms,
                  tool?, matched[]?, ts }
     TaskEnd    { task_id, outcome, ts }
     ```

   - Paths are absolute host paths, never sandbox-internal paths.
   - Events carry no file content.

7. **Retention is the consumer's concern.** `agentd` streams events; consumers that want replay (Fog) keep their own event log.

8. **Redaction.** For paths under a secret or sensitive grant, consumers show that activity occurred but not the path (COMP-02 §7). The exact rule is to be confirmed against COMP-02.

## Consequences
- Attribution is exact for MCP-mediated access and for opens, reads and modifies through sandbox mounts. Only directory-entry ops may be inferred, pending kernel verification. Consumers must label the difference.
- `agentd` handles fanotify volume from build tools and greps. Sweep aggregation keeps the outgoing stream small, but `agentd` must read events promptly to avoid queue overflow (`FAN_Q_OVERFLOW` → emit a `Sweep` with an unknown count).
- The cost is small: one socket on `agentd`, fanotify marks per sandbox, and no content storage.
- Gives EclipseOS a general "where is this agent working" primitive that other tools can reuse.

## Open questions
- Verify on the target kernel whether `FAN_MARK_MOUNT` delivers `FAN_CREATE` / `FAN_DELETE` / `FAN_MOVED_*` (with `FAN_REPORT_DFID_NAME`). If not, decision 4's inference applies.
- Sweep thresholds: are ≥ 20 files within 1 s the right defaults?
- Redaction granularity for sensitive paths: hide the path, the filename only, or the whole event?
- The agent colour (VOL1 L4573) needs deciding before consumers ship presence visuals.
- Where should the schema crate live (`crates/agent-activity-proto`?) so that `agentd` and Fog share it?

## Revisit when
Any open question above is answered, in particular the kernel check on directory-entry events for mount marks, since it decides whether decision 4's inference is needed at all.
