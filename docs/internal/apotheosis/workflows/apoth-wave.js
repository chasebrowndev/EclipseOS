// SPDX-License-Identifier: AGPL-3.0-only
// W3 — Apotheosis implementation wave (the workhorse). Orchestrator: Opus. Agents: Sonnet.
//
// Workflow({scriptPath, args: {
//   root: "<abs path of the worktree the wave works in>",
//   milestones: [{
//     id: "M2", title: "apoth-vt", spec: "T-01",          // spec: sections, e.g. "T-04, T-02 §1-§4"
//     units: [{ dir: "apotheosis/crates/apoth-vt", agent: "apoth-core", task: "what to build, scope, exit tests" }]
//   }, ...],
//   dryRun: false        // true: every agent only states what it would do (cheap wiring test)
// }})
//
// Rules of a wave: unit dirs are disjoint; at most 4 writers (up to 8 only for the M2-M7 pure-crate
// wave); nobody runs cargo before the Build phase; builds are serialized, one milestone at a time.
// The script never commits. The orchestrator reads the diff, integrates, gates, commits, opens the PR.
export const meta = {
  name: 'apoth-wave',
  description: 'Apotheosis implementation wave: rules -> parallel writers -> serialized build -> verify -> fix',
  phases: [
    { title: 'Rules', detail: 'spec-oracle extracts the binding rules per milestone' },
    { title: 'Write', detail: 'one agent per unit, parallel, disjoint dirs, no cargo', model: 'sonnet' },
    { title: 'Build', detail: 'serialized gate per milestone; fix inside own dirs', model: 'sonnet' },
    { title: 'Verify', detail: 'read-only reviewers and prober, parallel', model: 'sonnet' },
    { title: 'Fix', detail: 'serialized, at most two rounds', model: 'sonnet' },
  ],
}

const ROOT = args && args.root
const MS = (args && args.milestones) || []
const DRY = !!(args && args.dryRun)
if (!ROOT || !MS.length) throw new Error('args.root and args.milestones are required')
const dirs = MS.flatMap(m => m.units.map(u => u.dir))
if (new Set(dirs).size !== dirs.length) throw new Error('unit dirs must be disjoint: ' + dirs.join(', '))
if (dirs.length > 8) throw new Error('at most 8 writers in one wave')

const M = DRY ? 'haiku' : 'sonnet'
const DRYNOTE = DRY ? '\nDRY RUN: do not read, write or run anything. Reply with one sentence saying what you would do.\n' : ''
const SPEC = `${ROOT}/docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`
const BRIEF = `${ROOT}/docs/internal/apotheosis/BRIEF.md`
const CARGO = 'CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$HOME/.cache/apotheosis-target nice -n 19 cargo -j 2'
const HEAD = `Repository worktree: ${ROOT} (work only here; never cd to another checkout).
Read ${BRIEF} first; it binds you. Spec volume: ${SPEC}.${DRYNOTE}`

const FINDINGS = {
  type: 'object',
  properties: {
    findings: { type: 'array', items: { type: 'object', properties: {
      file: { type: 'string' }, severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
      problem: { type: 'string' }, fix: { type: 'string' }, cite: { type: 'string', description: 'T-section, matrix id or invariant' },
    }, required: ['file', 'severity', 'problem', 'fix'] } },
  },
  required: ['findings'],
}

// 1. Rules — one oracle per milestone, parallel.
phase('Rules')
const rules = await parallel(MS.map(m => () => agent(
  `${HEAD}\nFor Apotheosis milestone ${m.id} (${m.title}), governed by ${m.spec} of ${SPEC}: list every binding rule an
implementer must satisfy — behaviours, limits, defaults, named budgets, test-plan items due at this milestone, and the
feature-matrix row ids involved (${ROOT}/docs/internal/apotheosis/research/FEATURE-MATRIX.md). Also list anything the
spec leaves open that this milestone will hit. Return rules with citations, no excerpts. Units in scope:\n` +
  m.units.map(u => `- ${u.dir}: ${u.task}`).join('\n'),
  { label: `rules:${m.id}`, phase: 'Rules', agentType: 'spec-oracle', model: DRY ? 'haiku' : 'sonnet' })))

// 2. Write — every unit in parallel; barrier, because the build must not start until all are ready.
phase('Write')
const units = MS.flatMap((m, mi) => m.units.map(u => ({ ...u, m, rules: rules[mi] || '(oracle unavailable: read the section yourself)' })))
const written = await parallel(units.map(u => () => agent(
  `${HEAD}\nMilestone ${u.m.id} — ${u.m.title}. Governing spec: ${u.m.spec}.
YOUR DIRECTORY (touch nothing else): ${ROOT}/${u.dir}
TASK: ${u.task}
BINDING RULES from the spec oracle:\n${u.rules}\n
Write the code and its tests. DO NOT run cargo or any build — other agents are writing in the same workspace.
End with "READY TO BUILD" and: files changed, rows implemented, dependencies you need added to the workspace,
spec gaps found, anything left undone.`,
  { label: `write:${u.m.id}:${u.dir.split('/').pop()}`, phase: 'Write', agentType: u.agent, model: M })))

// 3. Build — serialized, one milestone at a time.
phase('Build')
const builds = []
for (const m of MS) {
  const mine = units.map((u, i) => ({ u, w: written[i] })).filter(x => x.u.m === m)
  builds.push(await agent(
    `${HEAD}\nGO BUILD — milestone ${m.id} (${m.title}). You own: ${m.units.map(u => u.dir).join(', ')}.
Writers reported:\n${mine.map(x => `## ${x.u.dir}\n${x.w || '(no report)'}`).join('\n')}\n
From ${ROOT}/apotheosis, touch the changed files, then run in order, fixing failures only inside the owned dirs:
  ${CARGO} fmt --all --check   (cargo fmt needs no -j; drop it there)
  ${CARGO} clippy --workspace --all-targets --all-features -- -D warnings
  ${CARGO} build --workspace --all-targets
  ${CARGO} test --workspace
If a failure needs a change outside the owned dirs (workspace Cargo.toml, another crate), do not make it: report it.
Return: each command's final status and the last 30 lines of any failure, files you changed, and what the orchestrator must wire up.`,
    { label: `build:${m.id}`, phase: 'Build', agentType: m.units[0].agent, model: M }))
}

// 4. Verify — parallel, read-only.
phase('Verify')
const scope = `Milestones: ${MS.map(m => `${m.id} (${m.spec})`).join(', ')}. Changed dirs: ${dirs.join(', ')}. Inspect with
git -C ${ROOT} status --short and git -C ${ROOT} diff. Read-only: change nothing.`
const lenses = [
  { key: 'invariants', type: 'invariant-review', p: `Review the uncommitted diff against the repository root invariants and the Apotheosis rules in ${BRIEF}
(language rule, SPDX headers, no content in logs, unsafe confinement, no allocation on named hot paths).` },
  { key: 'spec', type: 'general-purpose', p: `Spec conformance: for each rule the governing sections state for this milestone, find the code and the test that
proves it. Report rules with no implementation, behaviour that contradicts the section, and tests that assert the wrong thing.` },
  { key: 'correctness', type: 'general-purpose', p: `Adversarial correctness: hunt for real bugs — off-by-one at grid edges, wide-cell seams, integer overflow on
parameters, unbounded growth, lock order, chunk-boundary dependence in parsers, panics on hostile input. Each finding needs a concrete input.` },
  { key: 'probe', type: 'apoth-prober', p: `Exercise what was built: run the crate tests and fixtures for the changed dirs (use the cargo prefix in ${BRIEF};
you are cleared to build, alone, now) and, if a binary exists, drive it headlessly. Report anything not proven working.` },
]
const reviews = DRY ? [] : await parallel(lenses.map(l => () => agent(`${HEAD}\n${scope}\n${l.p}`,
  { label: `verify:${l.key}`, phase: 'Verify', agentType: l.type, schema: FINDINGS, model: 'sonnet' })))
let open = reviews.filter(Boolean).flatMap((r, i) => r.findings.map(f => ({ lens: lenses[i].key, ...f })))
log(`${open.length} findings from verification`)

// 5. Fix — serialized, at most two rounds; minors are left for the orchestrator.
phase('Fix')
const fixes = []
for (let round = 1; round <= 2; round++) {
  const todo = open.filter(f => f.severity !== 'minor')
  if (!todo.length) break
  const report = await agent(
    `${HEAD}\nFix round ${round} for ${MS.map(m => m.id).join(', ')}. You may edit only: ${dirs.join(', ')}. You are cleared to build (alone).
Findings to fix, or to refute with evidence:\n${todo.map((f, i) => `[${i}] ${f.severity} ${f.file} (${f.lens}; ${f.cite || ''})\n${f.problem}\nFIX: ${f.fix}`).join('\n\n')}\n
After fixing, re-run the four gate commands from ${ROOT}/apotheosis with the prefix "${CARGO}". Return per finding:
fixed / refuted (why) / not fixed (why), plus final gate status.`,
    { label: `fix:${round}`, phase: 'Fix', agentType: MS[0].units[0].agent, model: 'sonnet', schema: {
      type: 'object', properties: {
        results: { type: 'array', items: { type: 'object', properties: { index: { type: 'integer' }, outcome: { type: 'string', enum: ['fixed', 'refuted', 'not-fixed'] }, note: { type: 'string' } }, required: ['index', 'outcome'] } },
        gate: { type: 'string' },
      }, required: ['results', 'gate'] } })
  fixes.push(report)
  const left = new Set(((report && report.results) || []).filter(r => r.outcome === 'not-fixed').map(r => r.index))
  open = todo.filter((_, i) => left.has(i) || !report).concat(open.filter(f => f.severity === 'minor'))
  if (![...left].length) break
}

return {
  milestones: MS.map(m => m.id),
  rules, written, builds, fixes,
  openFindings: open,
  prBodyDraft: `Implements ${MS.map(m => m.spec).join('; ')}\n\nMilestones: ${MS.map(m => `${m.id} ${m.title}`).join(', ')}.\n` +
    `Open findings carried: ${open.length}.`,
  next: 'Orchestrator: read the diff, wire workspace Cargo.toml/deny/docs, run the gate skill, commit as owner, hand to pr-driver. Do not merge.',
}
