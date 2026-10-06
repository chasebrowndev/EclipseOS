# Phase 2 backend: what is left, and in what order

Status as of 2026-10-06 (branch `claude/gifted-pasteur-ob6uoi`). Milestones
are VOL1's COMP-16 table; `docs/STATUS.md` has the detail.

```mermaid
flowchart TD
  classDef done fill:#1f3d1f,stroke:#5c5,color:#cfc
  classDef part fill:#3d3a1f,stroke:#cc5,color:#ffc
  classDef todo fill:#1f2a3d,stroke:#58c,color:#cdf
  classDef blocked fill:#3d1f1f,stroke:#c55,color:#fcc

  M10[M10 policyd tasks and grants]:::done
  M11[M11 agent socket, scene queries]:::done
  M12[M12 audit spine]:::done
  M13[M13 agent seats, injection, override]:::part
  M14[M14 atomic batches, wait_for, dedupe, generations]:::todo
  M15[M15 trusted UI: prompt, panel, phrase]:::part
  M16[M16 table enforcement, prompt, defer]:::part
  M17[M17 policy-driven classes, races]:::part
  M18[M18 provenance chain, irreversible matcher]:::blocked
  M19[M19 brokerd secrets]:::todo
  M20[M20 egress proxy]:::todo
  M21[M21 cataclysm-pub library]:::done
  M22[M22 eclipse_semantic_v1 server]:::todo
  M23[M23 cataclysm: foot fork]:::blocked
  M24[M24 launcher, agent workspaces, virtual outputs]:::blocked
  M25[M25 MCP in agentd, SDK, reference agent]:::todo
  PD[policyd: mint, defer checks, open_task link]:::part
  C19[COMP-19 protected surfaces, commit slot]:::todo
  A08[A-08 console socket in agentd, conversation channel]:::todo
  CON[agent console pane, ported from the design prototype]:::todo
  EXIT{{PHASE 2 EXIT: open, type, click, read via the protocol}}

  M10 --> M11 --> M13
  M12 --> M13
  M13 --> M14
  M16 --> M14
  M15 --> M16
  M16 --> M17
  M17 --> M18
  M21 --> M22
  M22 --> M23
  M14 --> M25
  M22 --> M25
  PD --> M25
  M24 --> M25
  M25 --> EXIT
  M18 --> EXIT
  M19 -.parallel.-> EXIT
  M20 -.parallel.-> EXIT

  C19 --> CON
  A08 --> CON
  PD --> C19
  M15 --> C19
  CON --> EXIT

  F03[[F-03: stamper enum undecided]]:::blocked --> M18
  C17[[C-00 §17 item 1: layout model undecided]]:::blocked --> M24
```

## Work order

| Wave | Item | Who | Why now |
|---|---|---|---|
| A1 | M14: generations, dedupe, atomic batches, `wait_for`, `compat_lock` | backend agent, TCB parts main | unblocks M25; finishes the seat protocol |
| A2 | M16 remainder: golden decision suite (both evaluators), fault-injection no-mutation suite, ≤50 µs bench | main (TCB) | the M16 exit gate |
| A3 | M17: `classify`/`trust` compiled into the table, class recompute on title/url change, race suite; retire the manual flag | main (TCB) | default policy cannot classify anything until this lands |
| A4 | M13/M15 suite gaps: X11 posture, trusted-UI suite (agent seat cannot answer a prompt) | main | exit gates of done-ish milestones |
| B1 ∥ | ~~M21 `cataclysm-pub`~~: already in the tree since the workspace split; STATUS had it as not started | — | done; needs owner review |
| B2 ∥ | M19 `brokerd` (S-08): sealed store, injection modes; TPM mocked here | backend agent | separable; real TPM is a hardware check |
| B3 ∥ | M20 egress proxy (S-09) | backend agent, if the container can create netns | separable; may need the dev host |
| C1 | `policyd`: mint from prompt answers, deterministic defer checks, `open_task`/`close_task` on the link | main (TCB) | done except the chain-reading defer predicates (M18); a task CLI was built and removed, since A-08 §7 forbids one |
| C2 | M22 `eclipse_semantic_v1` server | backend agent | after M21 |
| C3 | M25 MCP surface in `agentd`, reference agent | backend agent | after M14 and C1; the Phase 2 exit demo |
| D1 | COMP-19 `eclipse_protected_surface_v1`: protected panes, commit slot (preview → arm → physical Enter → `create_task`), capture exclusion, human-only input | backend agent for protocol plumbing; slot, capture and policyd preview are main (TCB) | the only legal way a task is created (A-08 §5.2); closes TASK-01 |
| D2 | A-08 §6–7: `console.sock` in `agentd`, `conversation/<task_id>` channel, `list_tasks`, `subscribe`, `cancel_task` drain/immediate | backend agent; policyd task feed is main | the console's data source |
| D3 | Agent console pane (`shell/crates/ec-console`), ported from the design prototype | frontend agent | after D1 and D2 |
| — | M18, M23, M24 | blocked | spec decisions (F-03, C-00 §17) and a C fork |

The agent console prototype is in (scratchpad; not committed, 17 MB bundle).
It matches A-08 and COMP-19: the composer, the decision queue and the resume
card are compositor-drawn holes, and the console itself talks only to
`agentd`'s `console.sock`.
