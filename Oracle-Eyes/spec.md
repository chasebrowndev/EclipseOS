# Oracle-Eyes — System Specification

**Status:** Draft v0.5
**One-line:** A compositor-native overlay system that detects questions/topics on screen and renders short, contextual answers — Kiroshi optics for your monitor instead of your eyes.

---

## 1. Concept

Cyberpunk 2077's Kiroshi optics scan whatever you look at and surface a short annotation. Oracle-Eyes does the same for a desktop session: it watches (or is pointed at) pixels on screen, decides whether there's a question or a topic worth annotating, and renders a small, short-lived overlay with an answer or a one-line primer. Target use case: research/reading sessions where you're hitting a lot of new information and want a fast, low-friction gut-check without alt-tabbing to a browser or chat window.

Because this runs alongside a compositor you wrote — AbyssCompositor, Rust,
Smithay — Oracle-Eyes isn't a userspace overlay hack layered on top of someone
else's window manager. It is an **addon to EclipseOS**, a sibling directory to
`AbyssCompositor/` in the same repository, and the compositor grows first-class
support for what it needs rather than Oracle-Eyes working around what it lacks.
Abyss stays *just* the compositor; Oracle-Eyes is a separate program.

**Dependency direction:** the compositor must build, run, and be fully usable
with zero Oracle-Eyes code present. Oracle-Eyes requires the compositor, never
the reverse.

**Corrected in v0.5 — the secure layer does not exist.** v0.4 asserted that the
"secure layer (§3.5) already exists in the compositor" and that Phase 0 was a
lookup. It is not there. The concept it was reaching for is **Trusted UI**
(ADR 0009, COMP-10), and the only part of Trusted UI implemented today is the
48×8 capture indicator; there is no `trusted_ui/` module, no phrase, no prompt,
and no text renderer of any kind in the compositor. `docs/STATUS.md` records
this. Phase 0 is therefore new compositor work, not documentation.

**And Oracle-Eyes must not render into Trusted UI even once it exists.** Trusted
UI carries the owner's personal secret phrase and is the system's anti-spoof
anchor; COMP-10 §3.2 requires a test asserting that untrusted text cannot render
in the trusted position. Oracle-Eyes renders model output derived from OCR'd
screen pixels — the most attacker-influenced input in the system. Instead the
compositor grows a separate, explicitly **untrusted annotation pass** (COMP-18,
ADR 0040), which gives Oracle-Eyes the two properties it actually needs —
invisible to other clients' captures, invisible to its own — without diluting
the phrase. See §3.5.

## 2. Modes

### 2.1 Select Mode
- User presses a hotkey, gets a `grim`-style region selector (click-drag box) rendered directly by the compositor — no external tool dependency.
- On release, the selected region is captured once and run through the pipeline (§3), **skipping the classifier (§3.3) entirely** — a manual selection is an explicit request to answer, not a candidate to be screened out. OCR still runs (needed to produce the query text).
- The result is rendered as an overlay anchored near the selection.
- The overlay persists indefinitely until a dismiss hotkey is pressed. No timer.
- One capture per invocation — this is explicitly a manual, single-shot tool, not a live feed of that region.
- **Failure is not silent here** (contrast §6): if OCR finds no text, or the answer session errors, show a brief, unmistakable failure indicator (e.g. the overlay flashes red border + "no answer" for ~1.5s then dismisses itself) rather than nothing happening. A user who explicitly acted needs to know the action didn't produce a result; automatic mode's silent-skip policy (§6) does not apply here.

### 2.2 Automatic Mode
- Runs continuously, globally, across the whole screen — no per-application scoping (decided: not worth the complexity right now).
- Detects *new* candidate content (§3.3) and pushes a result without user action.
- Overlay display time: `clamp(min_display_ms + k * word_count, min_display_ms, max_display_ms)`. Defaults (configurable, §5): `min_display_ms = 4000`, `k = 150ms/word`, `max_display_ms = 20000`.
- Multiple simultaneous overlays are allowed, uncapped in principle — if several regions trigger close together, all of them get an overlay, with placement handled by collision avoidance (§3.5) rather than a queue or a cap. **In practice, screen space is finite:** if a new overlay cannot find a non-overlapping position after collision avoidance runs, evict the oldest live overlay to make room (dismiss it immediately, don't wait out its timer) rather than stacking or dropping the new one.

## 3. Pipeline

```
[Output buffer / damage region]
        │
        ▼
[3.1 Trigger / capture]
        │
        ▼
[3.2 OCR] (local)
        │
        ▼
[3.3 Candidate detection / classifier] (local, automatic mode only — select mode bypasses)
        │ (not worth answering) ──► idle
        │ (worth answering)
        ▼
[3.4 Answer — Claude Code CLI, one-shot per query] (text only)
        │
        ▼
[3.5 Render + lifecycle]
        │
        ▼ (optional, user-triggered)
[3.6 Expand]
```

Everything in §3.2–§3.4 runs in the Oracle-Eyes daemon, out of process from the compositor. See §4.

### 3.1 Trigger / capture
- **Select mode:** one-shot capture of the selected region via the compositor's own buffer access (no need for a `wlr-screencopy`-style protocol round-trip since the plugin is *inside* the compositor process — read the composited frame directly).
- **Automatic mode:** do **not** run this on every frame. Hook into the compositor's existing damage tracking — reuse the damage-region signal already computed for rendering. Debounce: wait for damage in a region to go quiet for `settle_ms` (default 600ms, configurable §5) before treating it as "settled" and worth analyzing. This turns "scanning the whole screen constantly" into "react when something changed and stopped changing."
- **Secure-layer exclusion (both modes):** buffer reads for capture must exclude Oracle-Eyes' own secure-layer surfaces, and damage originating from those surfaces must never be treated as trigger input. Without this, an overlay appearing causes damage, which triggers a capture, which can OCR the overlay's own text and re-trigger — a feedback loop. This exclusion is a hard requirement for Phase 3+ (automatic mode), not an optimization.

### 3.2 OCR (local)
- Text extraction happens locally, before anything leaves the machine. This is a hard requirement, not an optimization: since answers come from a cloud-backed session, sending extracted *text* instead of raw screen images is both the privacy-sane choice (a screen can contain anything — credentials, private messages, whatever else is on screen) and the cheaper one (text tokens vs. image tokens on every automatic-mode trigger).
- **Redaction pass runs on OCR output before anything downstream sees it** (classifier or answerer): regex-based scrubbing for common secret/credential shapes (API key patterns, JWTs, long hex/base64 tokens, `password:`/`token:`-style key-value lines, credit-card-like digit runs). This is a floor, not a guarantee — dropping per-app scoping (§2.2) means there's no allow/deny list to lean on, so redaction is the remaining backstop and should be treated as best-effort, not a privacy guarantee to advertise.
- Engine choice is open (§7) — Tesseract as baseline; bake-off against 1-2 alternatives before locking in (§8 Phase 0), since OCR quality caps answer quality with no vision fallback.
- Runs on the same machine as the compositor — CPU-bound, not GPU-bound (GPU stays free for other workloads).

### 3.3 Candidate detection / classifier (local, automatic mode only)
Before spending a query, cheaply decide if the OCR'd text is worth sending out:
- Skip regions with no text-like structure at the capture stage (edge density / connected-component heuristic) before even running OCR.
- Skip regions matching a **seen-and-unchanged hash** — an in-memory set of recently-answered region hashes with a TTL (default 5 min, configurable), not a persistent cache or answer history. This avoids re-answering the same paragraph every time its window regains focus, without contradicting the "no session history/cache" decision in §7 (that decision is about not storing *answers*; a short-lived dedup hash of *inputs* is a different thing and is kept).
- **Fast-path rule, narrowed:** the original "any `?` skips the classifier" rule over-fires on URLs, code, and forum markup. Revised: fast-path only text that looks sentence-shaped — a `?` preceded by at least N (default 3) space-separated words with at least one lowercase letter, no more than one `?`/`!` in the segment, and not inside what looks like a URL or code token (contains `/`, `{`, `;`, or a `://` substring nearby). Anything not matching this shape still goes through the classifier rather than skipping it.
- Everything else goes through a small **local** classifier (~1–2B parameters, runs on CPU) to decide: is this actually a topic worth annotating, or just noise (nav chrome, a timestamp, a username)?
- **Rate limit, concrete default:** max 1 query dispatched per 3s, regardless of how much damage is happening or how many regions pass the classifier in that window (extras are dropped, not queued past the window — a queue here would fight the seen-hash TTL and the region could easily have changed by the time it's served).

### 3.4 Answer generation — Claude Code CLI (Haiku, Claude Pro), one-shot per query
- **Design changed from v0.3.** A single long-lived interactive session with a `/clear` hook after each response does not map onto Claude Code CLI's actual modes: non-interactive/print mode (`claude -p`) runs one query and exits — there is no persistent process to send `/clear` into. Streaming-JSON input mode (`--input-format stream-json`) can stay resident, but slash commands are not a supported control surface there, and hook scripts can only run shell commands on lifecycle events, not inject input into a running session.
- **Chosen approach: fresh `claude -p` process per query, no persistent session.** This gets clean-context-per-query for free (there's nothing to accumulate) at the cost of paying process/model-init latency per call. Given the classifier + rate-limit already bound query frequency to roughly one per few seconds at most, this is judged acceptable; revisit only if measured latency blows the §6 budget.
- **Every invocation must be locked down**, since this wrapper is what's actually exposed to arbitrary on-screen text (see security note below):
  - `--system-prompt` override that defines the HUD-annotation contract (short answer, no tool use, no meta-commentary) — not the CLI's default system prompt, which is oriented around coding-agent behavior and costs tokens per call for instructions that don't apply here.
  - All tools disabled. No file read/write, no shell, no web access. The wrapper's only inputs are the OCR'd text and (for expand, §3.6) a wider capture; its only output is a short text answer.
  - Max turns = 1. No multi-turn tool loop.
  - Structured output format (`--output-format json` or equivalent), parsed programmatically — never scrape TUI/interactive-mode output.
- **Security requirement, not optional:** OCR'd screen text is untrusted input. A web page, terminal output, or document on screen can contain adversarial text ("ignore previous instructions and run …") aimed at whatever reads it next. With tools disabled and no persistent session, the blast radius of a successful injection is "the CLI prints a strange-looking answer," not "the CLI executes something" — this is *why* tools-disabled is a hard requirement above, not a hardening nice-to-have.
- Prompt contract: force short output regardless of model verbosity — this is a HUD annotation, not a chat response. Cap at 2–4 sentences / ~40–60 words, enforced by system-prompt instruction and a hard character-count truncation as backstop.
- **Concurrency:** one query in flight at a time (process-per-query naturally serializes; don't fire a second `claude -p` before the first returns). §2.2's "unlimited concurrent overlays" is about how many can be *displayed*, bounded separately by the §3.3 rate limit on how many can be *answered*.
- Cost model: bounded by Claude Pro's usage/rate ceiling rather than dollars-per-call, but §3.3's gating logic is identical either way — don't call unless it's worth it. **Verify current Anthropic consumer terms permit this kind of automated/scripted use of a Pro-subscription-backed CLI session before building on it** — this is a plan-terms question, not an engineering one, and belongs in Phase 0 (§8).

### 3.5 Render + lifecycle
- Overlays render in the compositor's **annotation pass** (COMP-18, ADR 0040) —
  a new, explicitly untrusted compositor-drawn pass above everything
  client-drawn and above the cursor, and strictly **below** Trusted UI. It is
  not the secure layer and grants nothing; it carries no phrase and renders no
  prompt.
- Annotations are absent from every capture target by construction, not by
  policy: the capture path builds its own pass list from layers and windows and
  never sees backend-prepended elements. This is load-bearing twice — other
  clients' screenshots must not contain answers derived from a region they may
  not be allowed to read, and Oracle-Eyes must not OCR its own output and loop.
- **The compositor owns presentation.** Oracle-Eyes supplies a rectangle and a
  string; placement, collision avoidance, eviction, styling and sanitisation are
  the compositor's, because it is the only party that knows output geometry and
  the only party a compromised Oracle-Eyes cannot influence. Control characters
  are stripped, length is clamped, and no markup is interpreted (COMP-18 §2).
  Oracle-Eyes can affect nothing but the glyphs.
- **Select mode:** anchored to the selected region, persists until dismiss hotkey. Failure state per §2.1.
- **Automatic mode:** anchored near the source region when practical; falls back to a fixed HUD zone (e.g. screen edge) when the source region is too small/crowded to anchor to directly. Timed dismissal per §2.2's display-time formula. Collision avoidance: each new overlay checks bounding boxes against currently-live overlays and nudges/offsets until it doesn't intersect any of them; if no non-intersecting position exists, evict the oldest live overlay (§2.2) rather than stacking or silently dropping the new one.
- Visual language: minimal, translucent, monospace/HUD-styled — legible over arbitrary content, not a modal dialog. Never intercepts clicks/input (click-through except on its own dismiss/expand target).

### 3.6 Expand (manual escalation)
- If an auto-generated (or select-mode) answer is too shallow, hover the cursor over the live overlay and press the expand hotkey.
- **Resolved (was open in v0.3):** given §3.4's per-query-process design (no persistent session, nothing to race a `/clear` hook against), expand always issues a fresh `claude -p` call with more context stuffed in — the original OCR text plus a wider capture region around it, run through the same locked-down invocation as §3.4. There is no "same session before `/clear`" variant to choose between anymore; that branch of the v0.3 open question is moot given the design change.
- The overlay updates in place with the expanded answer rather than spawning a second one.
- Only reachable while the overlay is live and the cursor is over it — no separate "re-ask" UI needed.

## 4. Integration Architecture

**Changed in v0.5. The `dlopen` plugin ABI of v0.4 is rejected** — see ADR 0041.

v0.4 proposed loading Oracle-Eyes into the compositor as a shared object talking
through a hand-rolled `#[repr(C)] HostVtable` of five `extern "C"` function
pointers. That collides with the first invariant in `AbyssCompositor/CLAUDE.md`:
"No ambient authority. Every operation requires a capability check." A vtable
handed to a loaded object *is* ambient authority — once the pointers are in its
hands there is no gate left, nothing to audit per call, and nothing the owner can
revoke short of deleting the `.so`. It also puts third-party code inside the
address space of a single-threaded core that owns all compositor state, where a
segfault or a blocking read is a compositor freeze, and it invents a third
unversioned ABI beside two stable IPC surfaces that already exist. v0.4's own
§4a had already reduced the in-process half to a forwarder; v0.5 deletes it.

**Oracle-Eyes is an ordinary out-of-process daemon** holding two independently
revocable capabilities:

- **Control — the COMP-13 control socket.** Line-delimited JSON-RPC 2.0 at
  `$XDG_RUNTIME_DIR/eclipse/abyss.sock`, mode 0600, owner-uid checked via
  `SO_PEERCRED`, every method classified in a static fail-closed table where a
  name that is absent simply does not exist. Oracle-Eyes may call
  `annotation_create` / `annotation_update` / `annotation_destroy` /
  `annotation_clear`, and subscribe to the `keybind` event kind (and later
  `damage`). Nothing else.
- **Pixels — `ext-image-copy-capture-v1`**, behind the existing fail-closed
  capture gate, enabled by one line in `policy.kdl`:

  ```kdl
  capture { allow "oracle-eyes" }
  ```

  Identity is the peer pid's executable basename. Revoking is deleting that
  line; it hot-reloads.

Capture is deliberately *not* a control-socket capability — pixels travel the
Wayland path so the two grants stay separable. "May draw but may no longer read"
is a coherent, reachable state, which is the point.

Consequences worth stating plainly:

- Oracle-Eyes runs its own threads and async freely. Abyss's
  single-threaded-core invariant is Abyss's, not its. A crash, a hang, or a
  runaway allocation in OCR or the model call cannot stall the compositor.
- Cost is a round trip plus a pixel copy per query, which the model call
  dominates by orders of magnitude.
- Hotkeys stay compositor-owned. Abyss gains annotation actions bound in KDL
  like any other; triggering one emits a `keybind` event rather than acting
  directly, so Oracle-Eyes never touches the seat. The region selector is a
  compositor-drawn modal (the overscan calibration overlay is the precedent) and
  returns its rectangle in that event.
- Damage-driven capture is harder from outside: DRM's damage lives inside
  smithay's `DrmCompositor` rather than an `OutputDamageTracker`. Automatic mode
  therefore ships first on a fixed poll, and the `damage` event comes last.
- The compositor must never special-case "is Oracle-Eyes running". Nothing in
  COMP-18 names it; the allowlist entry is config.

## 5. Configuration
- **All hotkeys are user-configurable** — select-mode trigger, dismiss, automatic-mode toggle, and expand (§3.6) all route through the compositor's existing keybind config system rather than being hardcoded. No hotkey is assumed to exist at a fixed binding; defaults ship in config, not code.
- Per-output config still worth keeping (in case automatic mode should only run on one monitor) even though app-level scoping was dropped.
- **Numeric defaults are all configurable, not hardcoded** — table below is the starting point, not a spec of fixed constants:

| Setting | Default |
|---|---|
| Damage settle debounce (§3.1) | 600ms |
| Seen-hash TTL (§3.3) | 5 min |
| Query rate limit (§3.3) | 1 / 3s |
| Fast-path min words before `?` (§3.3) | 3 |
| Overlay min display time (§2.2) | 4000ms |
| Overlay display scaling factor `k` (§2.2) | 150ms/word |
| Overlay max display time (§2.2) | 20000ms |
| Select-mode failure indicator duration (§2.1) | 1500ms |

## 6. Non-Functional Requirements
| Area | Requirement |
|---|---|
| Privacy / isolation | OCR is local; only extracted, redacted text (not screen images) leaves the machine (§3.2's regex redaction pass is best-effort, not a guarantee — there is no per-app allow/deny list backstopping it). Overlays render in the compositor's annotation pass — absent from other clients' captures and from Oracle-Eyes' own reads by construction (§3.5), which both protects observers and prevents feedback loops |
| Security | OCR'd screen text is untrusted input reaching a model with no tool access (§3.4) — this is a prompt-injection surface by construction, mitigated (not eliminated) by disabling all tools/file/shell/web access on every CLI invocation |
| Performance | Automatic mode must be idle-cost near-zero when screen is static; no continuous polling. All work (OCR, classifier, CLI process) runs out of process (§4), never on the compositor's thread |
| Usage budget | No per-call dollar cost (Claude Pro subscription, not metered API), but bounded by Pro's usage/rate limits — §3.3's local gating exists to stay under that ceiling, same logic as a cost control even without one. **Confirm this usage pattern is within Anthropic's consumer terms before building (§8 Phase 0)** |
| Latency | Select mode: best-effort, no hard ceiling (user explicitly wants no timer pressure here), but a failure surfaces visibly (§2.1) rather than hanging silently. Automatic mode: felt-latency budget of <3s from "settled" to overlay appearing, on top of per-query CLI process startup (§3.4) — this startup cost is a known risk to this budget and worth measuring early (§8 Phase 1) |
| Resource budget | OCR + local classifier run in the daemon (§4), CPU-bound, coexisting with normal desktop use without competing for GPU |
| Failure mode | **Automatic mode:** session unreachable / errored / rate-limited → overlay silently skipped, never a visible error state cluttering the HUD. **Select mode:** the opposite — failure must surface visibly (§2.1), since the user took an explicit action and needs to know it didn't produce a result |
| Toggleability | Automatic mode, and Oracle-Eyes as a whole, must be off-able without side effects: stop the daemon, and revoke either capability independently by editing `policy.kdl` (§4) |

## 7. Open Decisions
Narrower now, but still real forks (the plugin ABI is no longer among them — rejected in v0.5, §4; the annotation pass is specified in COMP-18):
1. **OCR engine** (§3.2) — Tesseract as baseline; bake-off against alternatives in Phase 0 (§8), since it's the accuracy ceiling for everything downstream.
2. **Local classifier model** (§3.3) — specific 1–2B model to run, once there's a corpus of real captured text to test candidate-detection accuracy against (and to sanity-check the narrowed `?`-fast-path in §3.3 doesn't over- or under-fire). Needs Phase 1 running first to gather that corpus.
3. ~~**IPC mechanism between plugin and daemon**~~ — resolved in v0.5: the existing COMP-13 control socket for control, `ext-image-copy-capture-v1` for pixels (§4). Not a fork.

Resolved (see §4): plugin ABI boundary is a hand-rolled `extern "C"` vtable, not `abi_stable` — single consumer, a small fixed set of capabilities, no data on the boundary that needs `abi_stable`'s richer types.

Resolved: overlay stacking is unlimited-with-collision-avoidance-and-eviction (§2.2); scoping is global (no app allow/deny list); no *answer* history/cache is needed, though a short-lived input dedup hash is (§3.3); manual escalation is the hover+hotkey expand flow, always as a fresh query (§3.6); integration is out-of-process over gated IPC and capture (§4), each capability separately revocable; answer generation is one-shot `claude -p` calls per query, not a persistent session (§3.4, changed from v0.3); the whole of Oracle-Eyes is a separate daemon process, not in the compositor (§4, changed in v0.5).

## 8. Phased Build Plan
0. **Paperwork and scaffolding** — before any pipeline code. ADR 0040
   (annotation pass) and ADR 0041 (out-of-process, two capabilities); COMP-18 in
   Volume 1, which is what compositor PRs cite for the `spec-trail` CI job; this
   spec's v0.5 amendments; `Oracle-Eyes/CLAUDE.md`, agent definitions, and a CI
   job. Note that v0.4's "look up the secure layer" step is void — see §1.
   (Anthropic consumer terms for the automated CLI usage in §3.4, and a latency
   smoke-test for `claude -p`, are set aside per owner direction — both must be
   closed before automatic mode ships.)
1. **Select mode, manual pipeline** — region capture → local OCR → (classifier skipped, §2.1/§3.3) → one-shot `claude -p` call (§3.4) → overlay rendered in the annotation pass → dismiss hotkey → visible failure indicator on error (§2.1). No damage tracking, no automatic mode, no expand yet. Proves the IPC and capture capabilities (§4), the annotation pass (§3.5), and the CLI wrapper (§3.4) end to end. **Done when:** a user can select a region containing a question, get a rendered answer within the CLI's natural latency, dismiss it, and get a visible (not silent) indicator if OCR or the CLI call fails.
2. **Expand flow** — add hover+hotkey escalation (§3.6) on top of select mode before moving to automatic, since it's easiest to validate on single, user-triggered overlays. **Done when:** hovering a live overlay and pressing expand replaces its content with a fresh, more detailed answer using a wider capture region.
3. **Automatic mode, naive trigger** — same pipeline, but on a fixed poll interval instead of damage-tracking, to validate candidate detection (§3.3, including the narrowed fast-path and redaction pass), the rate limiter, and multi-overlay collision avoidance + eviction (§3.5) without building the damage-tracking hook yet. **Done when:** leaving automatic mode running against a mixed screen (code, prose, chat) for 15+ minutes produces overlays only on genuinely question/topic-shaped content, at or under the configured rate limit, with no overlapping overlays and no feedback loop from its own overlays.
4. **Automatic mode, damage-tracked trigger** — replace polling with the `damage` event kind once the rest of the pipeline is proven, including the annotation-pass exclusion (§3.1). This is where the perf requirements in §6 actually get tested. **Done when:** automatic mode is measurably idle (no CPU/GPU cost) against a static screen, and the settle-to-overlay latency budget in §6 holds under normal use.
