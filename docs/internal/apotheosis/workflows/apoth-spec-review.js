// SPDX-License-Identifier: AGPL-3.0-only
// W2 — Apotheosis spec review. Run: Workflow({scriptPath, args: {root: "<abs path to worktree>"}})
// Six lenses read the T-NN volume and try to refute it; each lens's findings are then
// attacked by an independent skeptic. Read-only: no agent edits anything.
export const meta = {
  name: 'apoth-spec-review',
  description: 'Adversarial six-lens review of the Apotheosis spec volume (read-only)',
  phases: [
    { title: 'Review', detail: 'six lenses, parallel, each told to refute the spec', model: 'opus' },
    { title: 'Verify', detail: 'one skeptic per lens tries to refute each finding', model: 'opus' },
  ],
}

const ROOT = (args && args.root) || '/home/risk/syncedprojects/EclipseOS/.claude/worktrees/apotheosis-spec'
const SPEC = `${ROOT}/docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`
const MATRIX = `${ROOT}/docs/internal/apotheosis/research/FEATURE-MATRIX.md`

const COMMON = `
You are reviewing a draft specification, not code. The spec volume is ${SPEC} (sections T-00..T-25,
about 2500 lines; read T-00 fully, then the sections your lens needs, with offset/limit). Its
normative annex is ${MATRIX} (requirement rows by id; search it with rg, do not read it whole).
New decision records are ${ROOT}/docs/decisions/0109-*.md .. 0125-*.md.
Rules: read-only. Do not edit, create or delete any file. Do not run cargo or any build.
Your job is to find where the spec is WRONG, CONTRADICTORY, UNIMPLEMENTABLE or SILENT on something
an implementer must know. Do not report style, wording or praise. Every finding must cite the
section (e.g. "T-12 §4") and say concretely what to change. Severity: blocker (implementing it as
written produces a defect or cannot be done), major (an implementer would have to guess or two
sections disagree), minor (worth fixing, low risk). Report at most 15 findings, most severe first.
`

const LENSES = [
  { key: 'vt', prompt: `LENS: VT / protocol correctness. Check T-01, T-03, T-04, T-05, T-14 §1, T-15, T-19 against
what you know of ECMA-48, DEC VT behaviour, xterm ctlseqs, the kitty keyboard and graphics protocol
specifications, sixel, OSC 8/52/133, DA/XTVERSION/XTGETTCAP reply formats. Look for wrong sequence
syntax, wrong reply formats, wrong default modes, mode numbers that do not exist, behaviours real
programs (vim, tmux, neovim, fish, notcurses, mpv) depend on that the spec gets wrong or omits.` },
  { key: 'security', prompt: `LENS: security and root invariants. Check T-18, T-12, T-17, T-16 §4-5, T-21, T-22 and the
repository invariants in ${ROOT}/CLAUDE.md. Look for: reply-injection paths, consent that can be
spoofed or bypassed, the control socket granting more than seat policy would, places content or
keystrokes could be logged or persisted, credential-state gaps, sandbox holes in the media helper,
fail-open behaviour, TOCTOU in peer identification, remote-pane rules that can be loosened.` },
  { key: 'perf', prompt: `LENS: performance. Check T-23 against T-01, T-02, T-06, T-07, T-00 §4.2. Are the budgets
measurable as defined (is the start and end event of each observable)? Are any mutually
inconsistent or physically implausible? Does the threading model (main / io per pane / render per
window / workers) actually allow LAT-KEY, LAT-ECHO, IDLE and the throughput budgets, or is there
a lock, wakeup or copy the spec implies that breaks one? Is anything on a hot path that allocates?` },
  { key: 'feasibility', prompt: `LENS: feasibility under the language rule. Check T-00 §3, T-24, T-06, T-07, T-09, T-16, T-20 and
ADRs 0110, 0118, 0119, 0125 against the facts in Appendix A of the matrix ("third-party facts").
For each third-party crate the spec relies on, does Appendix A support the claim (licence,
pure-Rust build, feature availability, version)? Is anything required that the allow-list forbids,
or allow-listed but unnecessary? Is the lang-rule CI check (T-24 §3) actually able to detect what
it claims? Does any milestone in T-00 §7 depend on something scheduled after it?` },
  { key: 'settings', prompt: `LENS: settings and control coverage. Check T-10, T-11, T-12, T-08 and every section that names a
config key (search the spec for backticked keys like \`window.opacity\`). Does every behaviour a user
would want to change have a named key or Action? Are key names consistent between sections (same
key spelled two ways, a key used but belonging to no top-level node listed in T-10 §3)? Is any
option impossible to express in a generated settings GUI? Do the capability classes in T-12 §4
cover every Action family listed in T-11 §1? Is anything reachable only by hand-editing?` },
  { key: 'conflicts', prompt: `LENS: conflicts with existing EclipseOS specs and code. Compare T-17, T-24 §5, T-10 §1, T-12 §4 and
ADRs 0113-0117 against ${ROOT}/docs/ECLIPSEOS_SPECS_v2_VOL1.md and VOL2 (COMP-06, COMP-09, COMP-11,
COMP-13, P-04, S-05, D-05; search, do not read whole), ${ROOT}/docs/decisions/0107-*.md,
${ROOT}/docs/STYLE.md, and the code paths the spec cites (verify each cited repo path exists and
says what the spec claims, e.g. abyss/crates/ec-abyss-config/src/binds.rs, general.rs,
abyss/crates/ec-policyd/src/default-policy.kdl, abyss/crates/ec-cataclysm-pub, ec-ipc get_theme,
shell/crates/ec-ui/src/tokens.rs, ext_background_effect support in abyss). Report every place the
new volume contradicts an existing spec without saying it amends it, or cites something that is
not there.` },
]

const FINDINGS = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          section: { type: 'string' },
          severity: { type: 'string', enum: ['blocker', 'major', 'minor'] },
          problem: { type: 'string', description: 'what is wrong, with the evidence' },
          fix: { type: 'string', description: 'the concrete change to the spec text' },
        },
        required: ['section', 'severity', 'problem', 'fix'],
      },
    },
  },
  required: ['findings'],
}

const VERDICTS = {
  type: 'object',
  properties: {
    verdicts: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          index: { type: 'integer' },
          holds: { type: 'boolean', description: 'true only if you checked the spec text and the claim and the finding stands' },
          note: { type: 'string', description: 'why it holds or is refuted; a better fix if you have one' },
        },
        required: ['index', 'holds', 'note'],
      },
    },
  },
  required: ['verdicts'],
}

const results = await pipeline(
  LENSES,
  l => agent(COMMON + l.prompt, { label: `review:${l.key}`, phase: 'Review', schema: FINDINGS, model: 'opus' }),
  (r, l) => {
    const fs = (r && r.findings) || []
    if (!fs.length) return { lens: l.key, findings: [] }
    const list = fs.map((f, i) => `[${i}] ${f.section} (${f.severity})\nPROBLEM: ${f.problem}\nFIX: ${f.fix}`).join('\n\n')
    return agent(
      `You are a skeptic checking another reviewer's findings about a draft spec at ${SPEC}
(annex: ${MATRIX}; repo root ${ROOT}). Read-only; no edits, no builds. For EACH finding below, open
the cited section and any cited external fact, and try to REFUTE it: the reviewer may have misread
the spec, missed a sentence elsewhere that already covers it, or be wrong about the protocol or the
code. holds=true only if after checking it still stands. Return one verdict per index.\n\n${list}`,
      { label: `verify:${l.key}`, phase: 'Verify', schema: VERDICTS, model: 'opus' },
    ).then(v => {
      const by = new Map(((v && v.verdicts) || []).map(x => [x.index, x]))
      return {
        lens: l.key,
        findings: fs.map((f, i) => ({ ...f, holds: by.has(i) ? by.get(i).holds : null, note: by.has(i) ? by.get(i).note : 'unverified' })),
      }
    })
  },
)

const all = results.filter(Boolean).flatMap(r => r.findings.map(f => ({ lens: r.lens, ...f })))
const kept = all.filter(f => f.holds !== false)
log(`${all.length} findings, ${kept.length} survive verification`)
return { kept, refuted: all.filter(f => f.holds === false) }
