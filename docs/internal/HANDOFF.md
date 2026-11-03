<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Session handoff

## State as of 2026-10-11 (`main` at `de7e0eb`, PR #113)

Earlier handoffs are archived under `docs/internal/handoff/` (`2026-09-23.md`
is the one this file replaced). `docs/STATUS.md` is the progress record; this
file is only "where are we, what next".

### Merged to `main` since 2026-09-23 (PRs #58–#113)

- **Workspace split** (#73): per-product workspaces `abyss/`, `shell/`,
  `fog/`, `installer/`, `oracle-eyes/`, `packaging/`; every crate renamed
  `ec-*`; `ec-abyss` split into `-config`, `-render`, `-wire`, `-wlcs`.
- **Phase 1**: 9e done (#62), per-device input (#61), explicit-sync and
  per-output render fixes, the animation system (ADR 0073, C-18; #84–#98).
- **Phase 2** (#72, #74, #76–#78, #104–#113): M11 agent socket and scene
  queries with the leakage suite; M12 audit spine and `eclipse-audit`;
  M13 agent seats; M14 batches, dedupe, generations, `wait_for`,
  `compat_lock`; M15 consent prompt, batch prompt, emergency panel, phrase,
  commit slot; M16 enforcement; M17 classification; M19 `brokerd` and
  `ec-secret`; M21 `ec-cataclysm-pub`; M22 `eclipse_semantic_v1` server;
  M25 `agentd`, the console, the inference router (ADR 0076) and
  `ec-claude-agent`.
- **DE**: wallpaper, Display page, settings tree and search, pinned taskbar
  apps, launcher frecency.

### Where to look next

1. **Close M13–M15.** `docs/PROPOSEDFEATURES.md` → "Phase 2 — closing
   M13–M15": the TCB hooks in `policy::enforce` (BATCH-01, GEN-01, the
   `seat.atomic`/`seat.compat_lock` caps), the compat-lock indicator, the
   panel from the capture indicator, SEAT-01, and the non-semantic halves of
   LOCK-01 and WAIT-01.
2. **Then M22's consumers** (SEM-01): node-level redaction, agent
   `get_tree`, `action` forwarding.
3. **Phase 1 leftovers**: ext-image-copy cursor capture, the calendar
   drawer, M9f subjects; the manual gates M3, M4, M6 and the 14-day run.
4. **Spec still unwritten**: S-12, S-13, P-03, P-06, P-08, P-09, Tiers 5–7.
   Code already runs ahead of I-01/I-02 (ADR 0076) and D-03/D-05/D-07.
