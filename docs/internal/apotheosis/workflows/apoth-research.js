// SPDX-License-Identifier: AGPL-3.0-only
// W1 — Apotheosis research. Run: Workflow({scriptPath, args: {root: "<abs path to worktree>"}})
// Dossiers + topics in parallel -> per-category merge -> assemble matrix -> critic -> gap round -> patch.
export const meta = {
  name: 'apoth-research',
  description: 'Apotheosis web + prior-art research: per-terminal dossiers, topic studies, merged feature matrix',
  phases: [
    { title: 'Survey', detail: '13 terminal dossiers + 9 topic studies, parallel, no builds' },
    { title: 'Merge', detail: 'one merger per feature category, then assemble FEATURE-MATRIX.md' },
    { title: 'Critic', detail: 'find gaps and unverified claims' },
    { title: 'Gaps', detail: 'fill gaps, patch the matrix' },
  ],
}

const ROOT = (args && args.root) || '/home/risk/syncedprojects/EclipseOS/.claude/worktrees/apotheosis-spec'
const OUT = `${ROOT}/docs/internal/apotheosis/research`
const CACHE = '/home/risk/.cache/apotheosis-prior-art'

const CATEGORIES = [
  'vt-core', 'unicode', 'input', 'fonts', 'rendering', 'performance', 'graphics', 'media',
  'mux', 'scrollback-search', 'shell-integration', 'navigation', 'config', 'automation',
  'ui', 'integration', 'security', 'accessibility', 'remote', 'packaging',
]

const CONTEXT = `
You are researching for Apotheosis, a new first-party terminal emulator for EclipseOS (a Rust
Wayland desktop with its own compositor, Abyss). Fixed decisions you should judge features against:
- All our source is Rust; configuration is KDL. No C/C++ compiled by our build. Allowed system
  libraries: libwayland, libxkbcommon, Vulkan drivers, libva, PipeWire. crates.io only; licences
  allowed: MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, MPL-2.0, CC0. No GPL dependencies.
- Own Wayland client + own wgpu renderer for the grid AND the glass interface (tabs, splits,
  palette, search). Wayland-only. Any compositor; extra integration on Abyss.
- Every setting must be editable in a settings GUI and by hand in KDL. Control without an embedded
  scripting language: one Action vocabulary shared by keybinds, palette, rules (event -> actions),
  profiles, layouts and a JSON control socket + CLI.
- Video: kitty animation + mpv --vo=kitty path; a native media protocol with a sandboxed decoder
  helper (AV1 software, VA-API hardware) and PipeWire audio.
- Goal: the best features and the most control from every terminal, perfectly implemented.

Hard rules for you:
- PRIOR-ART RULE: you may read other terminals' source to learn HOW they do things. Your output
  must describe techniques, algorithms, data structures and trade-offs in prose. Never paste or
  paraphrase-transliterate source code. Escape-sequence syntax, protocol message formats and config
  key names are interface facts and may be quoted.
- Source clones: if you need source, shallow-clone ONLY your own subject with
  \`git clone --depth 1 <url> ${CACHE}/<name>\` (skip if it exists). Never build anything. Never
  run cargo, make, meson or zig.
- Write exactly one file, the path you are given. Touch nothing else in the EclipseOS tree.
- Cite sources: URLs for docs, and upstream path + function name for source findings.
- Prefer primary sources (official docs, protocol specs, source) over blog posts. Mark anything
  you could not verify as "unverified".
`

const ROW = {
  type: 'object',
  properties: {
    feature: { type: 'string', description: 'short name, e.g. "kitty keyboard protocol flag 8 (report all keys)"' },
    category: { type: 'string', enum: CATEGORIES },
    mechanism: { type: 'string', description: 'how it works / how the subject implements it, prose, 1-3 sentences' },
    control_surface: { type: 'string', description: 'config keys, actions, CLI, escape sequences that expose it' },
    adopt: { type: 'string', enum: ['must', 'should', 'could', 'no'] },
    rationale: { type: 'string' },
    source: { type: 'string', description: 'URL or upstream path#function' },
  },
  required: ['feature', 'category', 'mechanism', 'adopt', 'rationale', 'source'],
}

const REPORT = {
  type: 'object',
  properties: {
    file_written: { type: 'string' },
    rows: { type: 'array', items: ROW },
    facts: {
      type: 'array',
      description: 'verifiable third-party facts relevant to our stack (crate licences, maturity, API support)',
      items: {
        type: 'object',
        properties: {
          claim: { type: 'string' },
          verdict: { type: 'string', enum: ['true', 'false', 'partial', 'unverified'] },
          source: { type: 'string' },
        },
        required: ['claim', 'verdict', 'source'],
      },
    },
    budgets: {
      type: 'array',
      description: 'published performance numbers (latency, throughput, startup, memory) with method',
      items: {
        type: 'object',
        properties: { metric: { type: 'string' }, value: { type: 'string' }, method: { type: 'string' }, source: { type: 'string' } },
        required: ['metric', 'value', 'source'],
      },
    },
  },
  required: ['file_written', 'rows'],
}

const DOSSIERS = [
  ['kitty', 'https://github.com/kovidgoyal/kitty', 'GPL-3.0: study its PUBLISHED protocol docs (sw.kovidgoyal.net/kitty) for graphics, keyboard, text sizing (OSC 66), pointer shapes, notifications (OSC 99), remote control, kittens, hints, layouts, sessions, shell integration, choose-files, the render pipeline and its latency knobs (input_delay, repaint_delay, sync_to_monitor). Read source only to understand techniques; record nothing that resembles code.'],
  ['wezterm', 'https://github.com/wezterm/wezterm', 'Lua config surface (what users script and why — every one of those needs a KDL/Action/rule equivalent), multiplexer domains (local, unix, ssh, tls), mux protocol, termwiz, quick select, copy mode, launcher, workspaces, image protocols, font fallback and shaping.'],
  ['ghostty', 'https://github.com/ghostty-org/ghostty', 'Terminal core design (page-based scrollback, pins, style interning), SIMD parser fast paths, renderer, grapheme handling, config keybind syntax (sequences, key tables), shell integration injection, quick terminal, performance claims and how they were measured. Note it is Zig: we learn ideas only.'],
  ['foot', 'https://codeberg.org/dnkl/foot', 'Latency and damage tracking on a CPU renderer (pixman), server/client mode, scrollback, URL mode, sixel implementation, Wayland protocol coverage, startup speed, memory footprint, its benchmarks. It is the incumbent Apotheosis replaces; list every foot behaviour a foot user would miss.'],
  ['alacritty', 'https://github.com/alacritty/alacritty', 'alacritty_terminal grid and vi mode, hints, regex search, GPU renderer (glyph atlas, instancing), vtebench, config live reload, IPC (alacritty msg).'],
  ['rio', 'https://github.com/raphamorim/rio', 'Rust + wgpu terminal: renderer architecture (sugarloaf), how it uses wgpu on Wayland, frame pacing, image support, its known performance problems.'],
  ['contour', 'https://github.com/contour-terminal/contour', 'VT conformance breadth, DEC modes, vi input mode, status line, grapheme clustering mode 2027 (it originated it), good-image protocol, input bindings model.'],
  ['iterm2', 'https://iterm2.com/documentation.html', 'Feature leadership: triggers, smart selection, shell integration marks, captured output, inline images (OSC 1337), tmux -CC control-mode integration, broadcast input, instant replay, profiles switching by host/path, password manager. Docs only (macOS app).'],
  ['xterm', 'https://invisible-island.net/xterm/ctlseqs/ctlseqs.html', 'The authoritative control-sequence reference: enumerate every sequence family, which are obsolete, which modern terminals implement, the security-relevant ones (title reporting, font ops, window ops), sixel and ReGIS, modifyOtherKeys, XTGETTCAP, XTVERSION. Also vttest and esctest2 (https://github.com/ThomasDickey/esctest2) as conformance resources.'],
  ['windows-terminal', 'https://github.com/microsoft/terminal', 'Command palette, actions/keybinding JSON model, profiles, panes, quake mode, settings UI that edits a JSON file (directly relevant: GUI and text config in sync), Atlas renderer design, shell integration marks.'],
  ['vte-konsole', 'https://gitlab.gnome.org/GNOME/vte', 'VTE (GNOME) and Konsole (https://invent.kde.org/utilities/konsole): accessibility via AT-SPI, bidi support, OSC 777, hyperlinks spec (egmontkob gist), Konsole split views and profiles. Accessibility is the key output.'],
  ['tmux-zellij', 'https://github.com/zellij-org/zellij', 'zellij (Rust) and tmux (https://github.com/tmux/tmux): session persistence, detach/attach, layouts files, floating panes, stacked panes, plugin model (zellij WASM), resurrect. What a built-in mux must offer so users do not need tmux.'],
  ['warp-wave', 'https://docs.warp.dev', 'Warp and Wave Terminal (https://docs.waveterm.dev): blocks (command+output units), input editor, workflows, AI features (record only, adopt=no unless agent-perception relevant), inline previews. UX ideas only.'],
]

const TOPICS = [
  ['escape-registry', 'A complete registry of escape sequences a modern terminal must handle: C0/C1, ESC, CSI (incl. SGR with colon sub-params, underline styles/colours), DCS (DECRQSS, XTGETTCAP, sixel, tmux passthrough), OSC (0/1/2, 4, 7, 8, 9, 9;4, 10-19, 22, 52, 66, 99, 104-119, 133, 777, 1337, 5522, 3008), APC (kitty graphics), DEC private modes incl. 2026, 2027, 2031, 2048, 1004, 1006, 1016, 2004. For each: purpose, who implements it, security notes. Use xterm ctlseqs, kitty docs, terminal-wg specs (https://gitlab.freedesktop.org/terminal-wg/specifications) and contour/ghostty docs.'],
  ['keyboard', 'Keyboard input: legacy encodings, modifyOtherKeys levels, the kitty keyboard protocol (all flags, release events, alternate keys, associated text), fixterms, xkb keysym handling on Wayland, IME with text-input-v3 (preedit rendering), dead keys, compose. Also evaluate pure-Rust xkb replacements (kbvm crate: licence, maturity, xkbcommon parity).'],
  ['graphics', 'Image protocols: kitty graphics (every action, transmission medium, unicode placeholders, animation), sixel (decoding edge cases, colour registers, scrolling mode), iTerm2 OSC 1337, the terminal-wg "good image protocol" proposals. Security limits used in practice. Rust crates for decoding PNG/JPEG/GIF/WebP under our licence list.'],
  ['media', 'Video and audio inside a terminal: mpv --vo=kitty and --vo=sixel (shm path, performance), timg/chafa/notcurses approaches, any native video protocols. Rust decode options under our rules: rav1d (crates.io status, licence, nasm/asm build needs), dav1d bindings (C, disallowed), cros-codecs and cros-libva (licence, maturity, VA-API coverage), pure-Rust demuxers (matroska/webm/mp4 crates), Opus/Vorbis/AAC pure-Rust decoders (symphonia: licence MPL-2.0?), PipeWire Rust bindings (pipewire-rs licence and maturity), A/V sync technique. Recommend a concrete crate list.'],
  ['fonts', 'Font stack in Rust: fontdb, harfrust vs rustybuzz, swash (hinting, variable fonts, COLRv1, emoji), skrifa/fontations, ligatures in a grid, fallback and emoji presentation, box-drawing/powerline/sextant/octant procedural rendering (kitty, ghostty, wezterm, foot techniques), font metrics and cell sizing, gamma/linear blending, subpixel positioning. Licences of each crate.'],
  ['gpu-latency', 'GPU terminal renderer designs (kitty, alacritty, ghostty, wezterm, rio, Windows Terminal Atlas) and frame pacing on Wayland: wl_surface.frame, presentation-time, fifo-v1, commit-timing-v1, buffer damage. wgpu specifics: Wayland surface present modes, whether wgpu (v27 era) can export dmabufs or report buffer age/damage, startup cost. Latency measurement methodology (typometer, Is It Snappy, kitty/foot/ghostty published numbers, vtebench, termbench). Produce proposed budget numbers with sources.'],
  ['scrollback-search', 'Scrollback storage (ring buffers, ghostty pages, kitty history buffer, foot), compression, disk spill, reflow on resize, search (regex over wrapped lines, incremental, time budgets), copy mode / vi mode, selection semantics (word chars, smart selection, rectangular), semantic zones from OSC 133.'],
  ['mux-remote', 'Built-in multiplexing and remote: wezterm mux protocol and domains, kitty ssh kitten (terminfo + shell integration push), tmux control mode, zellij session model, session save/restore. Persistence and its security implications. Rust crates for ssh (russh licence and crypto deps) vs shelling out to system ssh.'],
  ['config-automation', 'Configuration and automation surfaces across terminals: kitty.conf + remote control, wezterm Lua, ghostty config, Windows Terminal settings.json + settings UI, iTerm2 triggers, Alacritty TOML. Catalogue what users actually automate (GitHub dotfiles, issue trackers) and map each need to a declarative KDL equivalent (actions, key tables, rules, profiles, templates) or to the control socket. Also: accessibility APIs (AccessKit unix adapter licence and deps) and how terminals expose the grid to screen readers.'],
]

function surveyPrompt(slug, url, focus) {
  return `${CONTEXT}
Subject: ${slug} (${url}).
Focus: ${focus}

Write a dossier to ${OUT}/${slug}.md with sections: Summary; Architecture (how it is built); Feature
inventory (grouped by category); Control surface (config, actions, IPC); Performance (numbers +
method); What Apotheosis should take; What it should avoid; Sources. Then return structured rows:
one row per distinct feature or technique worth deciding on (aim for 25-60), plus facts and budgets.
Every row's category must be one of: ${CATEGORIES.join(', ')}.`
}

phase('Survey')
const surveys = [
  ...DOSSIERS.map(([s, u, f]) => ({ kind: 'dossier', slug: s, prompt: surveyPrompt(s, u, f) })),
  ...TOPICS.map(([s, f]) => ({ kind: 'topic', slug: `topic-${s}`, prompt: surveyPrompt(`topic-${s}`, 'see focus', f) })),
]
const reports = (await parallel(surveys.map(s => () =>
  agent(s.prompt, { label: `${s.kind}:${s.slug}`, phase: 'Survey', schema: REPORT, model: 'sonnet', agentType: 'general-purpose' })
    .then(r => r && { ...r, slug: s.slug })
))).filter(Boolean)
const missing = surveys.filter(s => !reports.find(r => r.slug === s.slug)).map(s => s.slug)
if (missing.length) log(`Survey agents that returned nothing: ${missing.join(', ')}`)

// Barrier is justified: each category merge needs rows from every survey.
const rows = reports.flatMap(r => (r.rows || []).map(x => ({ ...x, from: r.slug })))
const facts = reports.flatMap(r => (r.facts || []).map(x => ({ ...x, from: r.slug })))
const budgets = reports.flatMap(r => (r.budgets || []).map(x => ({ ...x, from: r.slug })))
log(`${rows.length} rows, ${facts.length} facts, ${budgets.length} budget figures from ${reports.length} surveys`)

phase('Merge')
const MERGED = {
  type: 'object',
  properties: { category: { type: 'string' }, file_written: { type: 'string' }, must: { type: 'integer' }, should: { type: 'integer' }, could: { type: 'integer' }, no: { type: 'integer' } },
  required: ['category', 'file_written'],
}
const merged = (await parallel(CATEGORIES.map(cat => () => {
  const mine = rows.filter(r => r.category === cat)
  if (!mine.length) return Promise.resolve(null)
  return agent(`${CONTEXT}
You are merging research rows for category "${cat}" into one deduplicated decision table.
Input rows (JSON, each tagged with the survey it came from):
${JSON.stringify(mine)}

Write ${OUT}/matrix/${cat}.md (create the directory if needed) containing:
- "## ${cat}" heading, then a table: | ID | Feature | Mechanism we adopt | Control surface | Adopt | Seen in | Rationale |
  IDs are ${cat.toUpperCase().replace(/-/g, '')}-NN. Merge duplicates; when sources disagree on how
  to do it, pick the best technique and say why in Rationale. Adopt is must/should/could/no; a
  feature that two or more mature terminals ship and users rely on is at least "should".
- A short "Open questions" list for anything the rows could not settle.
Return counts per adopt level.`, { label: `merge:${cat}`, phase: 'Merge', schema: MERGED, model: 'sonnet', agentType: 'general-purpose' })
}))).filter(Boolean)

const assembled = await agent(`Assemble ${OUT}/FEATURE-MATRIX.md from the per-category files in ${OUT}/matrix/
(${merged.map(m => m.category).join(', ')}). Order sections as listed. Start the file with
"<!-- SPDX-License-Identifier: AGPL-3.0-only -->", a title "# Apotheosis feature matrix", one paragraph on
method, and a summary table of counts per category and adopt level. Then append two appendices built
from the JSON below: "Appendix A — third-party facts" (table: claim | verdict | source | from), and
"Appendix B — published performance figures" (table: metric | value | method | source | from).
Facts: ${JSON.stringify(facts)}
Budgets: ${JSON.stringify(budgets)}
Copy section content verbatim; do not re-decide anything. Return the path written.`,
  { label: 'assemble', phase: 'Merge', model: 'sonnet', agentType: 'general-purpose' })

phase('Critic')
const GAPS = {
  type: 'object',
  properties: {
    gaps: {
      type: 'array', maxItems: 6,
      items: { type: 'object', properties: { slug: { type: 'string' }, question: { type: 'string' }, why: { type: 'string' } }, required: ['slug', 'question', 'why'] },
    },
  },
  required: ['gaps'],
}
const critic = await agent(`${CONTEXT}
Read ${OUT}/FEATURE-MATRIX.md and skim the dossiers in ${OUT}/. You are the completeness critic.
Find at most 6 of the most important gaps: a terminal feature area nobody covered, a "must" row with
no mechanism, a third-party fact left unverified that the architecture depends on (vte APC support,
wgpu dmabuf export, rav1d build needs, cros-codecs licence, pipewire-rs licence, kbvm parity,
accesskit_unix deps), or a contradiction between sections. Each gap becomes one research question
for a fresh agent. Return them with a short kebab-case slug each.`, { label: 'critic', phase: 'Critic', schema: GAPS, model: 'sonnet', agentType: 'general-purpose' })

phase('Gaps')
const gapList = (critic && critic.gaps) || []
log(`${gapList.length} gaps to fill`)
const filled = (await parallel(gapList.map(g => () =>
  agent(surveyPrompt(`gap-${g.slug}`, 'see focus', `${g.question}\nWhy it matters: ${g.why}`),
    { label: `gap:${g.slug}`, phase: 'Gaps', schema: REPORT, model: 'sonnet', agentType: 'general-purpose' })
    .then(r => r && { ...r, slug: `gap-${g.slug}` })
))).filter(Boolean)

const patched = filled.length ? await agent(`${CONTEXT}
Patch ${OUT}/FEATURE-MATRIX.md (and the matching ${OUT}/matrix/<category>.md files) with these gap
findings. Add new rows with the next free IDs in their category, update rows whose mechanism or
adopt level changes, add facts to Appendix A and figures to Appendix B, and refresh the summary
counts. Keep everything else unchanged.
Gap reports: ${JSON.stringify(filled)}
Return a short list of what changed.`, { label: 'patch', phase: 'Gaps', model: 'sonnet', agentType: 'general-purpose' }) : 'no gaps filled'

return {
  surveys: reports.map(r => r.slug),
  missing,
  counts: merged,
  matrix: assembled,
  gaps: gapList.map(g => g.slug),
  gaps_filled: filled.map(f => f.slug),
  patch: patched,
}
