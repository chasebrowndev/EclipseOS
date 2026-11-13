// SPDX-License-Identifier: AGPL-3.0-only
// W6 — Apotheosis parity audit: the gate for M31 (default terminal).
// Workflow({scriptPath, args: { root: "<abs worktree path>", categories: [...optional subset...], dryRun: false }})
// Every R1 `must` row of the feature matrix (tier rule: T-00 §6) is exercised against the real
// binary and marked proven / partial / missing; every non-proven verdict is re-checked by a second
// agent. Auditors run ONE AT A TIME (each drives the headless session and may build); ~20 + rechecks.
export const meta = {
  name: 'apoth-parity-audit',
  description: 'Apotheosis M31 gate: prove every R1 must-row of the feature matrix against the running terminal',
  phases: [
    { title: 'Audit', detail: 'one prober per matrix category, sequential', model: 'sonnet' },
    { title: 'Recheck', detail: 'second opinion on every partial/missing row', model: 'sonnet' },
  ],
}
const ROOT = args && args.root
if (!ROOT) throw new Error('args.root required')
const DRY = !!(args && args.dryRun)
const M = DRY ? 'haiku' : 'sonnet'
const ALL = ['vt-core', 'unicode', 'input', 'fonts', 'rendering', 'performance', 'graphics', 'media', 'mux',
  'scrollback-search', 'shell-integration', 'navigation', 'config', 'automation', 'ui', 'integration', 'security',
  'accessibility', 'remote', 'packaging']
const CATS = (args && args.categories) || ALL
const RES = `${ROOT}/docs/internal/apotheosis/research`
const HEAD = `Worktree: ${ROOT}. Read ${ROOT}/docs/internal/apotheosis/BRIEF.md first. You are cleared to build (alone).
Spec: ${ROOT}/docs/ECLIPSEOS_SPECS_APOTHEOSIS.md. Read-only on source.${DRY ? '\nDRY RUN: do nothing; return an empty rows list.' : ''}`
const ROWS = { type: 'object', properties: { rows: { type: 'array', items: { type: 'object', properties: {
  id: { type: 'string' }, tier: { type: 'string', enum: ['R1', 'R2', 'R3'] },
  verdict: { type: 'string', enum: ['proven', 'partial', 'missing', 'not-applicable'] },
  evidence: { type: 'string', description: 'the test, fixture id or headless procedure and what was observed' },
}, required: ['id', 'tier', 'verdict', 'evidence'] } } }, required: ['rows'] }

const audited = []
for (const c of CATS) {
  phase('Audit')
  const a = await agent(`${HEAD}\nCATEGORY: ${c}. Open ${RES}/matrix/${c}.md. For every row whose Adopt column is "must": decide its
tier with the rule and exception table in T-00 §6 (R1 unless excepted). For each R1 row, PROVE it against the built
terminal: an existing passing test or fixture that asserts exactly this behaviour counts only if you ran it; otherwise
drive the real binary in the headless session and observe. "proven" needs evidence you produced now; a code path that
merely exists is "partial". R2/R3 rows: verdict not-applicable, evidence "tier". Return every must-row.`,
    { label: `audit:${c}`, phase: 'Audit', agentType: 'apoth-prober', schema: ROWS, model: M })
  const rows = (a && a.rows) || []
  const doubt = rows.filter(r => r.tier === 'R1' && (r.verdict === 'partial' || r.verdict === 'missing'))
  if (doubt.length && !DRY) {
    phase('Recheck')
    const b = await agent(`${HEAD}\nSecond opinion for category ${c}. Another auditor marked these R1 rows not proven. For each, read the
matrix row in ${RES}/matrix/${c}.md and try independently to prove it works (the auditor may have used the wrong key,
sequence or config). Also confirm the tier is really R1 under T-00 §6. Return a verdict for each id.\n` +
      doubt.map(r => `- ${r.id}: ${r.verdict} — ${r.evidence}`).join('\n'),
      { label: `recheck:${c}`, phase: 'Recheck', agentType: 'apoth-prober', schema: ROWS, model: M })
    const by = new Map(((b && b.rows) || []).map(r => [r.id, r]))
    for (const r of rows) if (by.has(r.id)) { const s = by.get(r.id); r.recheck = s.verdict; r.recheckEvidence = s.evidence; if (s.verdict === 'proven' || s.verdict === 'not-applicable') { r.verdict = s.verdict; r.tier = s.tier } }
  }
  if (!a) log(`category ${c}: auditor returned nothing — NOT audited`)
  audited.push({ category: c, audited: !!a, rows })
}
const r1 = audited.flatMap(c => c.rows.filter(r => r.tier === 'R1').map(r => ({ category: c.category, ...r })))
const open = r1.filter(r => r.verdict !== 'proven')
const unaudited = audited.filter(c => !c.audited).map(c => c.category)
log(`${r1.length} R1 must-rows, ${open.length} not proven, ${unaudited.length} categories unaudited`)
return { clean: !DRY && open.length === 0 && unaudited.length === 0, r1Count: r1.length, open, unaudited,
  next: 'M31 may proceed only when clean is true. Write the table into docs/STATUS.md with the evidence.' }
