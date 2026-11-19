# Apotheosis — hand-off

Running state for the orchestrating session. Update at the end of every session.

## Where things are
| What | Where |
|---|---|
| Spec (contract) | `docs/ECLIPSEOS_SPECS_APOTHEOSIS.md` — T-00 first, always |
| Requirements annex | `docs/internal/apotheosis/research/FEATURE-MATRIX.md` (946 rows; tiers in T-00 §6) |
| Decisions | `docs/decisions/0109-…` to `0125-…` |
| Milestone contract / progress | T-00 §7 / `docs/STATUS.md` "Apotheosis" |
| Agent brief | `docs/internal/apotheosis/BRIEF.md` |
| Agent definitions | `docs/internal/apotheosis/agents/*.md` — copy into `.claude/agents/` on each machine (`.claude/` is not tracked) |
| Workflows | `docs/internal/apotheosis/workflows/*.js` |
| Spec review record | `docs/internal/apotheosis/SPEC-REVIEW.md` |
| Prior-art log | `docs/internal/apotheosis/PRIOR-ART.md` |

## How to run a milestone (orchestrator = Opus, agents = Sonnet)
1. Read T-00 §7 for the milestone's sections and dependencies; read those sections.
2. Create the worktree and branch yourself (`apoth-mNN-<slug>`), relative worktree
   paths. Check your own cwd first: agents inherit it.
3. Run **W3** `apoth-wave.js` with `args` as documented at the top of the script. One
   wave = the milestones that can run together (T-00 §7 "Needs"). M2–M7 is the one
   six-writer wave; every other wave is ≤ 4 writers.
4. W3 returns gate output, changed files, open findings and a PR body draft. **You**
   read the diff, fix integration (workspace `Cargo.toml`, deny, docs), run the gate
   (`gate` skill, `apotheosis` workspace), commit as the owner (no agent trailers), and
   hand to `pr-driver`. Never merge.
5. After any wave touching parser, term, grid or renderer: **W5** `apoth-conformance.js`.
   Any glass surface: **W4** `apoth-ui-pass.js`, then send the final screenshots to the
   owner. Before M31: **W6** `apoth-parity-audit.js` must come back clean.
6. Update `docs/STATUS.md` and this file.

## Workflow wiring
All four build workflows (W3–W6) were dry-run once on 2026-11-12 (`dryRun: true`, one
item each) and returned through every phase. The dry run skips W3's Verify and Fix
phases and every real build, so the first real wave is still the first real test.
On a machine where `.claude/agents/apoth-*.md` is missing, copy them from
`docs/internal/apotheosis/agents/` and start a new session before running anything.

## Standing rules that have bitten before
- Builds are serialized (`CARGO_TARGET_DIR=~/.cache/apotheosis-target`,
  `CARGO_INCREMENTAL=0`, `nice -n 19`, `-j 2`); the disk is small.
- Headless only; kill by captured pid only.
- Owner decisions go through the question tool, not a prose list with a silent default.
- X-milestones (X1–X4) change `abyss/` or `shell/`: use `eclipse-backend` /
  `eclipse-frontend`, and TCB paths (policy data, P-04 signal) stay with the main thread
  and owner review.
- ec-settings pages for the terminal are `eclipse-frontend` work (iced); the terminal's
  own glass interface is `apoth-frontend` work (our renderer).

## State
- 2026-11-18: the spec PR is merged (#311). Done on main: M1-M6, M8-M11, M14-M16, M19-M21, M23,
  M25, M26, M32, M34, M35, M37, M38 and X1-X5. Partial: M7, M12, M17, M22, M24, M27-M30,
  M33. Not started: M31, M36, M39. Unknown: M13, M18. The table with evidence is in
  `docs/STATUS.md` ("Apotheosis").
- Next gaps: the consent bar (M24), `apoth notify`/`fonts`/`completions` (M28/M29),
  `layout.*` and `--layout` (M22), undo close, session save and the quick terminal (M33),
  lazy reflow and cold-page compression (M12), baselines and budgets (M17), the 83
  unhandled actions, and the M18 dogfood gate that blocks M31. foot stays the default
  terminal until M31.

## Open owner decisions
Collected from each section's "Open Decisions"; none blocks M1–M13. Ask at the milestone
that needs it: PGO in packaging and the absolute budget column (M17); daemon as default
(M35); `-git` package channel (before M31).
