// SPDX-License-Identifier: AGPL-3.0-only
// W4 — Apotheosis glass-interface pass, per surface. Orchestrator: Opus. Agents: Sonnet.
//
// Workflow({scriptPath, args: {
//   root: "<abs worktree path>",
//   shots: "<abs dir for final screenshots>",
//   surfaces: [{ name: "tab-strip", spec: "T-08 §4, T-13 §4", task: "what to build or change" }],
//   dryRun: false
// }})
// Surfaces run one after another (they share apoth-ui and one build slot). Per surface:
// build -> headless screenshots -> STYLE.md critic -> iterate (max 3) -> prober clicks every control.
// Afterwards the orchestrator sends the files in `finals` to the owner (SendUserFile).
export const meta = {
  name: 'apoth-ui-pass',
  description: 'Apotheosis glass surface: build, screenshot headlessly, critique against STYLE.md, iterate, probe',
  phases: [
    { title: 'Build', detail: 'apoth-frontend builds the surface and screenshots it', model: 'sonnet' },
    { title: 'Critique', detail: 'independent STYLE.md critic reads the screenshots', model: 'sonnet' },
    { title: 'Probe', detail: 'apoth-prober clicks and tabs to every control', model: 'sonnet' },
  ],
}
const ROOT = args && args.root, SHOTS = args && args.shots
const SURFACES = (args && args.surfaces) || []
const DRY = !!(args && args.dryRun)
if (!ROOT || !SHOTS || !SURFACES.length) throw new Error('args.root, args.shots, args.surfaces required')
const M = DRY ? 'haiku' : 'sonnet'
const DRYNOTE = DRY ? '\nDRY RUN: do nothing; reply with one sentence saying what you would do.\n' : ''
const HEAD = `Worktree: ${ROOT}. Read ${ROOT}/docs/internal/apotheosis/BRIEF.md first.${DRYNOTE}`
const CRIT = { type: 'object', properties: {
  ship: { type: 'boolean', description: 'true only if you would put this in front of the owner as finished' },
  issues: { type: 'array', items: { type: 'object', properties: { where: { type: 'string' }, problem: { type: 'string' }, rule: { type: 'string', description: 'STYLE.md or T-08 rule broken' } }, required: ['where', 'problem'] } },
}, required: ['ship', 'issues'] }

const out = []
for (const s of SURFACES) {
  const dir = `${SHOTS}/${s.name}`
  let feedback = '', verdict = null, rounds = 0
  for (let r = 1; r <= 3; r++) {
    rounds = r
    await agent(`${HEAD}\nSURFACE: ${s.name} — governed by ${s.spec}.\nTASK: ${s.task}\n` +
      (feedback ? `ROUND ${r}. The critic rejected the last round. Fix every item:\n${feedback}\n` : '') +
      `You are cleared to build (you are the only builder). Render the surface in the hidden headless nested session with
real blur, in every state it has (rest, hover, pressed, keyboard focus, disabled, open/closed), dark and light, scale 1 and 2.
Save PNGs to ${dir}/ with descriptive names and list them.`,
      { label: `build:${s.name}:r${r}`, phase: 'Build', agentType: 'apoth-frontend', model: M })
    verdict = await agent(`${HEAD}\nYou are a design critic, read-only. Read ${ROOT}/docs/STYLE.md fully and T-08 in
${ROOT}/docs/ECLIPSEOS_SPECS_APOTHEOSIS.md. Then Read every PNG in ${dir}/ and judge the "${s.name}" surface. It must be
real glass: translucent layers over blur, rounded corners, crisp rim, soft depth; no gradients, no glow or halo; focus and
selection are a brighter fill plus a crisp rim; every state distinguishable; text contrast adequate; spacing on the token
scale; nothing generic or flat. Be hard to please: the owner has rejected ugly UI before. List concrete issues.`,
      { label: `critic:${s.name}:r${r}`, phase: 'Critique', schema: CRIT, model: M })
    if (DRY || !verdict || verdict.ship) break
    feedback = verdict.issues.map(i => `- ${i.where}: ${i.problem}${i.rule ? ` (${i.rule})` : ''}`).join('\n')
  }
  const probe = await agent(`${HEAD}\nProbe the "${s.name}" surface (${s.spec}) in the headless session: click every control,
reach every control by keyboard, dismiss with Escape, and assert an observable effect for each. Report controls that do
nothing, cannot be reached, or lack an accessibility node.`,
    { label: `probe:${s.name}`, phase: 'Probe', agentType: 'apoth-prober', model: M })
  out.push({ surface: s.name, rounds, shipped: !!(verdict && verdict.ship), remainingIssues: verdict && !verdict.ship ? verdict.issues : [], probe, finals: dir })
}
return { surfaces: out, next: 'Orchestrator: send the PNGs under each `finals` dir to the owner; do not claim a surface done that did not ship.' }
