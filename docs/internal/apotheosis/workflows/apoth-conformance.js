// SPDX-License-Identifier: AGPL-3.0-only
// W5 — Apotheosis conformance. Run after any wave touching parser, term, grid or renderer.
// Workflow({scriptPath, args: { root: "<abs worktree path>", scope: "what changed, e.g. 'M13 term v2'", dryRun: false }})
// Three agents IN SEQUENCE (they all build and run; one build slot). Read-only on source.
export const meta = {
  name: 'apoth-conformance',
  description: 'Apotheosis conformance: fixtures and suites, fuzz triage, benches against T-23 budgets (sequential, read-only)',
  phases: [
    { title: 'Suites', detail: 'golden corpus, differential oracles, vttest/esctest subset', model: 'sonnet' },
    { title: 'Fuzz', detail: 'fuzz smoke per target, triage crashes', model: 'sonnet' },
    { title: 'Budgets', detail: 'apoth-bench against T-23', model: 'sonnet' },
  ],
}
const ROOT = args && args.root
if (!ROOT) throw new Error('args.root required')
const DRY = !!(args && args.dryRun)
const M = DRY ? 'haiku' : 'sonnet'
const HEAD = `Worktree: ${ROOT}. Read ${ROOT}/docs/internal/apotheosis/BRIEF.md first. Scope of the change under test:
${(args && args.scope) || 'unspecified — test everything'}. You are cleared to build (you are the only builder now).
Spec: ${ROOT}/docs/ECLIPSEOS_SPECS_APOTHEOSIS.md.${DRY ? '\nDRY RUN: do nothing; reply with one sentence saying what you would do.' : ''}`
const R = { type: 'object', properties: {
  pass: { type: 'boolean' }, summary: { type: 'string' },
  failures: { type: 'array', items: { type: 'object', properties: { id: { type: 'string' }, expected: { type: 'string' }, observed: { type: 'string' }, repro: { type: 'string' }, cite: { type: 'string' } }, required: ['id', 'observed', 'repro'] } },
}, required: ['pass', 'summary', 'failures'] }

phase('Suites')
const suites = await agent(`${HEAD}\nRun per T-25 §1-§3: the full golden-fixture corpus (with chunking variants), the differential
oracle comparison, and the automatable vttest and esctest2 subsets. Also check that every T-03 conformance row cites a
fixture. Report each failing fixture or suite item with the bytes that reproduce it.`,
  { label: 'suites', phase: 'Suites', agentType: 'apoth-prober', schema: R, model: M })
phase('Fuzz')
const fuzz = await agent(`${HEAD}\nRun the fuzz smoke (T-18 §11): 60 s per target in ${ROOT}/apotheosis/fuzz, plus the structure-aware
property tests. For every crash, hang or limit breach: minimise it, name the target and the T-section rule broken, and
give the reproducing input as hex. Do not fix anything.`,
  { label: 'fuzz', phase: 'Fuzz', agentType: 'apoth-prober', schema: R, model: M })
phase('Budgets')
const bench = await agent(`${HEAD}\nRun apoth-bench for every T-23 §2 budget whose gate milestone has been reached (headless set
always; end-to-end set under the nested headless session). Compare with the committed baseline in
${ROOT}/apotheosis/bench/results/. A regression over 5 % or a missed absolute ceiling is a failure: report budget id,
target, measured value and the run conditions. Class-M budgets: say they need the owner's reference machine.`,
  { label: 'budgets', phase: 'Budgets', agentType: 'apoth-prober', schema: R, model: M })
return { pass: !!(suites && suites.pass && fuzz && fuzz.pass && bench && bench.pass), suites, fuzz, bench }
