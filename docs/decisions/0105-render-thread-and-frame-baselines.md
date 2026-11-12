<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0105 — Render thread: measure first, then a numeric rule for leaving the loop
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context

### The documents disagree with the code
Three documents say the compositor has a render thread per GPU:

- COMP-01 §3 (VOL1 l.2199): "Concurrency where it pays: a render thread per GPU
  (COMP-02) and blocking work … on a small `rayon`-style pool".
- ADR 0018, Decision: "Concurrency is used only where it pays: a render thread per
  GPU, and blocking work … on a small worker pool".
- `docs/ARCHITECTURE.md` l.74, under "Core model".

The code has none. Every `thread::spawn` / `thread::Builder` in `ec-abyss` and
`ec-abyss-render` (grep, 2026-11-11; no `rayon`, no `tokio` in either crate):

| Where | Thread | Renders? |
|---|---|---|
| `session.rs:60` | `session-ordered`, spawns session helpers in order | no |
| `xwayland/mod.rs:360` | one-shot, writes X `RESOURCE_MANAGER` | no |
| `render/screenshot.rs:121` | one per clipboard reader, writes PNG to a pipe | no |
| `render/screenshot.rs:280` | one per screenshot, PNG encode | no (pixels already read back) |
| `trusted_ui/broker.rs:415` | `ec-brokerd-ui`, blocking socket to brokerd | no |
| `backend/headless.rs:762` | inside `#[cfg(test)]` | no |

`ec-abyss-config/src/approvals.rs:208` (`abyss-approvals`, file writer) is the only
other one linked into the compositor. There is no pool either: blocking work gets a
dedicated or per-job thread. Whether config parse and audit serialization run on the
loop today was not checked (**unverified**).

**Verdict.** The code matches the root invariant ("One `calloop` loop owns
`AbyssState`"). The three documents state the render thread as built; it was never
built. `ARCHITECTURE.md` is wrong as a description of the tree and is corrected below.
COMP-01 §3 and ADR 0018 are wrong in the same way, but they are the spec and an
accepted ADR: this ADR does not silently pick the code. It proposes the COMP-01 §3
amendment in Consequences, for the owner to accept or reject. ADR 0018's own revisit
trigger ("a profile on the reference machine shows the single loop is the bottleneck
for a target in COMP-14") has never been evaluated, because no such profile exists.

### What the spec requires (sections opened for this ADR)
- COMP-01 §3: single-threaded core; frame scheduling per output, driven by DRM
  page-flip completion; no allocation in input delivery or the policy check, both
  benchmarked in `bench/`.
- COMP-02 §2: per output, `page_flip_done → schedule_repaint(deadline = next_vblank −
  render_estimate)`, `render_estimate` an EWMA of recent render times; target "human
  input → frame submitted within one frame". §5: direct scanout off whenever effects,
  trusted UI or overlay content are present. §10 restates COMP-14: ≤1 ms CPU / ≤2 ms
  GPU typical, ≤4 ms GPU with blur on one surface, >95 % scanout hit rate for
  fullscreen video, <0.1 % missed frames at 144 Hz. §11: damage-correctness property
  test by headless pixel compare.
- COMP-14 §2.1: input → frame submitted ≤1 frame (hard fail >2); input event processing
  (libinput → client dispatch) ≤200 µs p99 (hard fail >1 ms); policy `check()` ≤50 µs
  p99. §3: no allocation in the per-frame render path; no IPC in rendering or input.
  §4.1: headless frame benchmarks (1/10/50 windows, with and without effects) and GPU
  benchmarks on the reference machine. §5: headless benches every push, >10 %
  regression against the rolling baseline fails; GPU benches nightly on a self-hosted
  runner; baselines stored per commit. §6: shedding order (blur, shadows, animations
  first; policy, audit, trusted UI and redaction never). §7: a hung `policyd` must not
  affect frame time or input latency.
- COMP-16 milestone 9f: every COMP-14 §2 target produces a number; headless benches
  block on >10 % regression; GPU benches report nightly. Milestone 4's exit gate depends
  on it ("COMP-14 §2 budgets collected by the 9f harness and met").

### What the render path does today (`backend/drm.rs`)
- **Scheduling.** `on_vblank` (l.1481) completes the flip, sends `wp_presentation`
  feedback with the kernel timestamp, and re-renders only if `needs_render` is set.
  `schedule_render` (l.1951) marks every output dirty and renders the idle ones
  **immediately, inline**. There is no deadline and no `render_estimate`: grep finds no
  EWMA or deadline in `backend/`. **Second spec/code gap**: COMP-02 §2's late-render
  schedule is not built; a frame renders as soon as an output is idle, so input
  arriving mid-frame waits for the next VBlank and its frame flips one VBlank later.
- **Coupling to input.** The libinput source (l.1776–1820) calls
  `state.process_input_event(event); schedule_render(state);` per event. The first
  event of a dispatch therefore composites every idle output inside the input
  callback, before later queued events are processed, and clients are flushed only
  after the dispatch round (comment at l.2551). Input-to-client delivery currently
  includes a full CPU-side composite of all idle outputs, serially.
- **Per frame** (`render_output`, l.2053): shell/trusted-UI ticks; element collection
  with the renderer borrowed (cursor, trusted UI, capture indicator, region selector,
  window menu, overview, drop guides, windows/layers with blur backdrops, annotations);
  overscan; direct-scanout and VRR decision; `DrmCompositor::render_frame`, or the
  ADR 0084 copy path (`render_secondary`: primary renders, secondary blits);
  `queue_frame`; `stats.record`; COMP-14 §6 shedding (`drm.shed.observe`); frame
  callbacks; `space.refresh`. The element `Vec` is built per frame; `blur::crop_behind`
  collects a `Vec` per refreshed backdrop. COMP-14 §3 forbids per-frame allocation
  (**noted, not in this ADR's scope**).
- **Direct scanout.** `FrameFlags::DEFAULT` unless the candidate is sensitive, overscan
  is non-zero, or `render.direct_scanout` is off (l.2317); smithay assigns the plane.
  A scanned-out frame still runs element collection and `render_frame` on the loop;
  it skips the GPU composite, not the CPU work.
- **Blur** (`ec-abyss-render/src/blur.rs`). Dual-Kawase, `passes` clamped 1..=6
  (default 4, COMP-02 §9). Per blurred surface whose backdrop is dirty: render the
  cropped backdrop at full resolution into level 0, `passes` downsamples, `passes − 1`
  upsamples; the kept result is level 1 (half resolution), stretched bilinearly. With
  the default that is one backdrop render plus 8 blits. Cached per surface; refreshed
  only when damage behind it intersects region + kernel radius (+ glass reach). Above
  `CROWDED = 10` blurred surfaces a damage-only refresh runs at most every
  `CROWDED_INTERVAL = 6` frames and only `CROWDED_STARTS = 2` new backdrops start per
  frame. Shaders compile lazily on first use (`ensure_programs`), on the loop.
- **Multi-GPU** (ADR 0084/0086). One primary `GlesRenderer` plus an optional renderer
  per secondary device, all in `DrmData` on the loop. A copy frame uses two renderers in
  one call. Capture (`service_captures`, l.1971) takes the primary renderer out of
  `AbyssState` and calls `render::capture::service(state, renderer, prompt)`: capture
  shares the renderer and reads redaction state directly.
- **Instrumentation** (`ec-abyss-render/src/stats.rs`). One ring of 1024 samples for
  the whole compositor, not per output (COMP-14 §4.2 says per output). CPU time only
  (`render_frame`, `queue_frame`); no GPU timing, no input timestamps. Exposed through
  `get_metrics` (p50/p99) and logged with `--stats`.

### What smithay 0.7.0 allows (read in `abyss/vendor/smithay/src`)
- `GlesRenderer` is `!Send`: `_not_send: PhantomData<*mut ()>`
  (`backend/renderer/gles/mod.rs:314`). It cannot be moved to another thread.
- `EGLContext` is `unsafe impl Send` while not current (`backend/egl/context.rs:84`),
  and `GlesRenderer::new(context)` is an `unsafe fn` taking it (`gles/mod.rs:468`). A
  render thread therefore creates its own renderer from a context sent to it.
- `GlesTexture` is `Arc<GlesTextureInternal>`, which is `unsafe impl Send + Sync`
  (`gles/texture.rs:135`). Textures may cross threads.
- Surface state is behind a `Mutex` (`wayland/compositor/tree.rs:101,166`), and
  `RendererSurfaceState` is `unsafe impl Send + Sync` (`renderer/utils/wayland.rs:56`).
  Smithay tolerates reading surfaces off the main thread, at the price of a lock per
  surface access: a lock on the hot path.
- Element construction needs the renderer: abyss builds every element with
  `&mut GlesRenderer` (client buffer import, trusted-UI and overlay textures). A render
  thread cannot just be handed today's element list; import has to move with it.
- `DrmCompositor` gets the render fence into `IN_FENCE_FD` when the driver supports
  it; support is disabled on NVIDIA < 560.35.03 (`drm/compositor/mod.rs:1181–1204`).
  The comment at l.770 says the non-fencing path "force[s] a wait". A blocking
  `sync.wait()` was located in `drm/surface/gbm.rs:363,369`, not in the atomic path.
  Whether the loop blocks on GPU completion on the reference NVIDIA stack is
  **unverified**: the baselines below measure it.

### Other compositors (recollection, not verified against source here)
- niri: renders on its main event-loop thread with smithay's renderer; no render
  thread.
- cosmic-comp: moved KMS rendering to a thread per output surface, with the shell
  behind `Arc<RwLock<…>>` shared with those threads.
- wlroots compositors (sway, Hyprland): single-threaded rendering.
Single-threaded is the common smithay/wlroots shape; cosmic-comp's thread-per-output
came with shared, locked shell state, which our invariants forbid.

### `abyss/bench/` today
A hand-rolled harness (ADR 0043): `measure()` keeps every sample, `budgets.rs` encodes
COMP-14 §2.1/§2.1b, a counting allocator enforces `forbid_alloc`. The runner measures
only `timer-overhead`. `INPUT_DISPATCH` and the other budgets exist as data with no
bench behind them. Two dev binaries: `shader-compile` and `shader-preview` (surfaceless
EGL through the real `ec-abyss-render` path, prints average GPU time per frame). No
frame benchmark, no recorded baselines, no CI job (`gate.yml` has none, despite its
comment "GPU work is nightly"), no golden-image render tests (only
`ec-policyd/tests/golden.rs`, which is not rendering). The headless backend
(`backend/headless.rs`) renders for real into an offscreen texture on a hardware
render node or Mesa's software device, on a fixed 16.667 ms timer, with one virtual
output and no scanout.

## Options

### A. Measurement (prerequisite to any option in B)
1. **Do nothing; keep the docs claim.** Rejected: the docs describe a component that
   does not exist, and milestones 4 and 9b gate on numbers no one collects.
2. **Profile ad hoc when it feels slow.** Cheap, unrepeatable, no regression gate;
   contradicts COMP-14 §5.
3. **Define a baseline set, record it in-repo, gate on it.** Chosen; specified in
   Decision.

### B. If the numbers say rendering must leave the loop
1. **In-loop scheduling (input priority).** Stop rendering inside the input callback:
   input sets `needs_render`; one composite per dispatch round (idle or zero-timeout
   source), after all queued input is processed and flushed; then the COMP-02 §2
   deadline (`next_vblank − render_estimate`). Cost: none against the invariants: one
   thread, no locks, capture unchanged. Limit: a long composite still blocks the next
   input dispatch; it only stops input queuing *behind* it inside one round. Also
   closes the second spec/code gap.
2. **One thread; move expensive work off the critical path.** Precompile every program
   (blur down/up, glass, frost, transition shaders) at renderer init or config load,
   not on the first frame that needs them. Keep the backdrop cache and the `CROWDED`
   throttle; move the blur chain to quarter resolution after level 1 when GPU time is
   the limit. Move shm upload off frame-critical work where smithay allows (needs a
   shared EGL context; smithay warns that shared contexts on GLES2 insert `glFinish`,
   `gles/mod.rs:490`). PNG encode is already off-loop. Cost: low; no invariant touched.
   Limit: CPU element build and GL submission stay on the loop.
3. **Render thread per GPU, fed a snapshot.** Each GPU (not each output: copy frames
   use the primary and a secondary renderer together, ADR 0084) gets a thread that
   owns its `GlesRenderer`, built there from a sent `EGLContext`, its
   `DrmCompositor`s and damage trackers, and its own calloop for DRM VBlank events.
   The loop builds a plain-data scene snapshot per output per frame: buffer handles,
   geometry, effect params, the scanout candidate, and **the redaction and scanout
   decisions already made**. It sends the snapshot over a bounded channel; the thread
   imports, draws, commits, and returns presentation feedback, buffer releases and
   timings by channel. Costs:
   - Single-threaded core: `AbyssState` stays loop-owned, but the snapshot becomes a
     second representation of the scene to keep in step, and must be built without
     allocation (preallocated, reused). Element construction is rewritten: every
     element builder that takes `&mut GlesRenderer` today splits into "describe" (loop)
     and "import/draw" (thread).
   - No locks: smithay surface access from the thread is a `Mutex` per surface, so the
     thread must not touch `WlSurface` state. Everything it needs is copied into the
     snapshot; buffers cross as dmabuf/shm handles plus a commit counter. Channel
     internals are the same kind of synchronisation COMP-01 §3 already accepts for the
     pool.
   - Capture / redaction (TCB): `render/capture.rs` reads `AbyssState` and shares the
     primary renderer. It must become a job on the primary GPU's thread rendering
     **from the same snapshot** as that frame, with `classify` and the sensitive set
     resolved on the loop into the snapshot. Otherwise capture and scanout can see
     different frame decisions: a window redacted in one and not the other, or a
     scanout of a sensitive surface. Owner review of `capture.rs` (F-07 §4).
   - Trusted UI: the TrustedUI pass must stay last in every snapshot and must never be
     dropped by a bounded channel; a prompt frame needs a reserved slot.
   - Latency: one extra frame of pipelining unless the loop hands the snapshot over
     before the deadline. Input → frame submitted can get worse before it gets better.
   - Effort: large. Touches `backend/drm.rs`, `backend/headless.rs`, every element
     builder in `ec-abyss-render`, and `capture.rs`.
4. **Thread per output** (cosmic-comp's shape). Same as 3, but the copy path shares
   the primary renderer across output threads, which needs a lock or a second hop.
   Rejected on the no-locks invariant unless ADR 0084's copy path goes.

## Decision

**Decided by Claude on the owner's delegation (2026-11-11; owner: "whichever is
more efficient/better mechanically").** The single event loop is the design.
There is no render thread, and COMP-01 §3, ADR 0018 and `docs/ARCHITECTURE.md`
are amended to say so (Consequences). Mechanically: smithay 0.7.0's
`GlesRenderer` is `!Send` and every element builder needs `&mut renderer`, so a
render thread costs a describe/import split of every builder plus locks or a
second scene representation, for no measured gain. **B1 is done now as a spec
fix** (render once per dispatch round, after queued input is processed and
flushed, with the COMP-02 §2 `next_vblank − render_estimate` deadline), not
held for numbers: it closes a spec/code gap and is a precondition for reading
any latency number. The render-thread question reopens only if Step B's numeric
thresholds below are crossed.

**Baselines (owner, 2026-11-12).** CI runs the CPU-side, software-rendered numbers on
every PR. GPU numbers come from a manual TTY runbook, with no self-hosted runner.
Baselines are JSON committed in the repo.

**Step A: measure first.** Nothing about threading changes until the baseline set
below exists and has been run on the reference machine.

*Scenes* (each runs 60 s after a 5 s warm-up, same wallpaper, `decoration.blur` at
defaults, 4 passes, `blur` mode unless noted):

| Id | Scene | Outputs |
|---|---|---|
| S0 | idle desktop, bar only | all connected |
| S1 | 1 translucent window, blur, damaged every frame | primary |
| S4 | 4 translucent windows, blur, one animating | primary |
| S16 | 16 translucent windows, blur (above `CROWDED`) | primary |
| SG | S4 in `glass` mode, adaptive on | primary |
| SO | overview open/close loop, S4 behind | primary |
| SV | fullscreen video (mpv, dmabuf), direct scanout eligible | primary |
| SM | S4 on a 3840×2160 primary plus S1 on a second output (and the ADR 0084 copy route if the box has two GPUs) | 2 |
| SP | S4 with a `capture_output` stream at the grant FPS cap | primary |

*Metrics per scene*, all as p50 / p99 / max from retained samples (ADR 0043):
- CPU frame time per output: element build, `render_frame`, `queue_frame`, separately.
- Main-thread busy time per dispatch round that rendered (all outputs together).
- GPU frame time per output (GL timer query if the driver exposes one, **unverified**;
  else `glFinish` wall time in a bench-only build, labelled as such).
- Missed frames (flip later than its target VBlank), scanout hit rate (SV), frames
  rendered (S0: must be 0, COMP-14 §7).
- **L1** input event processing: libinput event read → client flush that carries it
  (COMP-14 §2.1, ≤200 µs p99), in S0 and S16+SO, with injected pointer motion at 1 kHz.
- **L2** input → frame submitted: event read → `queue_frame` of the first frame that
  reflects it (≤1 frame; hard fail >2).
- **L3** input → presented: event read → the page-flip timestamp of that frame
  (`DrmEventMetadata`). True photon latency needs external hardware (camera or
  photodiode) and is out of scope; L3 is the closest in-compositor proxy.

*Instrumentation owed* (backend work, not TCB): per-output stats rings (COMP-14 §4.2);
an input timestamp carried from the libinput read to the client flush and to the frame
that reflects it, recorded without allocation; scene drivers (synthetic clients
submitting translucent shm and dmabuf buffers at a set damage rate) shared between
headless and DRM runs; `ec-abyss-bench` subcommands that start the compositor, drive a
scene, read `get_metrics` and emit a record.

*Where results live.* `abyss/bench/baselines/<host>/<scene>.json`, one record per run:
commit, date, kernel, GPU and driver version, mode and refresh per output, config hash,
and the metrics above. `<host>` is `headless-ci` or a short reference-machine id.
Committed in the same PR as the change that moved them (COMP-14 §5: a deliberate
regression updates the baseline in the PR with justification).

*Regression threshold.* COMP-14 §5: >10 % on any p99 against the rolling median of
the last 10 recorded runs (COMP-14 §8 item 1, proposed there) fails. An absolute COMP-14
budget breach fails regardless of the baseline.

*How it runs.*
- **Headless, CI, every push.** S0, S1, S4, S16, SG, SO, SP on `backend/headless.rs`.
  These numbers are **CPU-side only**. On a cloud runner the "GPU" is Mesa's software
  device, so `render_frame` includes rasterisation on the CPU. There is one virtual
  output, a fixed 16.667 ms timer instead of VBlank, and no scanout. Only relative
  regressions on the same runner class mean anything. SV, SM, L3 and missed frames are
  not measurable headless.
- **Hardware, manual runbook, until a self-hosted runner exists.** All scenes on the
  reference machine from a real TTY (DRM backend; `CLAUDE.md`: DRM is tested on a real
  TTY, never nested). Run from a second VT or over ssh, with the live session stopped.
  The runbook is owed as a section in `abyss/bench/README.md`.

**Step B: the rule.** Evaluated on the reference machine at the shortest refresh it
drives (144 Hz, period 6.94 ms):

- **Stay single-threaded** while all hold: L1 p99 ≤ 200 µs; L2 ≤ 1 frame at p99;
  main-thread busy time per rendering dispatch p99 ≤ 25 % of the period (≤ 1.7 ms);
  missed frames < 0.1 % in S4, SO, SM.
- **B1 (in-loop input priority + COMP-02 §2 deadline) is applied now, unconditionally**
  (see Decision); it is not gated on these numbers. The measurements then show
  whether anything beyond it is needed. It is a precondition for every later option: until it lands, input
  latency is measured against an artefact of scheduling, not of threading.
- **Apply B2** if, after B1, first-use stalls (shader compile, chain or texture
  allocation) show in `max` but not in p99, or GPU time, not CPU time, is what breaches
  the budget. A render thread cannot fix GPU-bound frames.
- **Open a B3 ADR** only if, after B1 and B2, any of these holds in S4, SO or SM:
  main-thread busy time per rendering dispatch p99 > 50 % of the period (> 3.5 ms at
  144 Hz); L1 p99 > 1 ms (COMP-14 hard fail); L2 > 2 frames; or the loop is shown to
  block on GPU completion (the non-`IN_FENCE_FD` path) for > 1 ms p99. That ADR must
  name the snapshot format, the capture-on-snapshot design and the trusted-UI slot.
- B4 is not adopted.

## Consequences
- `docs/ARCHITECTURE.md` l.74–76 is corrected in a follow-up docs PR (not here: sibling
  ADRs are in flight in this tree). Current text:
  > - Concurrency only where it pays: a render thread per GPU, and blocking work
  >   (config parse, screenshot encode, audit serialization) on a small pool that
  >   talks back to the loop over channels.

  Replacement:
  > - Rendering runs on the loop thread: every output's composite, blur and capture
  >   happen inside the `calloop` loop that owns `AbyssState`. There is no render
  >   thread; ADR 0105 sets the measured threshold at which one would be considered.
  >   Blocking work runs on dedicated threads that talk back to the loop over
  >   channels (screenshot PNG encode, clipboard serving, the brokerd prompt socket,
  >   ordered session spawns, approval writes). There is no shared pool.
- COMP-01 §3 is amended now (decided 2026-11-11, see Decision), in the PR that
  carries the spec fix. Text:
  *(amended, ADR 0105)* "a render thread per GPU" is not built; rendering runs on the
  loop until ADR 0105's rule is met; blocking work uses dedicated threads, not a pool.
  ADR 0018's Decision carries the same sentence; mark it "as amended by 0105" rather
  than rewrite an accepted ADR.
- COMP-02 §2's EWMA deadline schedule is recorded as not built: a second spec/code gap
  surfaced here, fixed by B1 now (decided 2026-11-11), not ignored.
- Owed, in order: per-output stats + input timestamps; scene drivers; headless frame
  benches in `gate.yml`; hardware runbook and first reference baselines;
  `abyss/bench/README.md` updated ("What runs today"). The bench subjects and
  `gate.yml` change are one PR with the matching local gate line (root `CLAUDE.md`:
  local gate and CI must not differ).
- Milestones 4 and 9b ("frame budget still met per 9f") become checkable for the first
  time.
- Forbidden until a B3 ADR is accepted: any render work off the loop thread, any
  shared or locked access to `AbyssState` or smithay surface state from another thread.

## Open owner questions
1. ~~COMP-01 §3: amend now, or leave the render thread as a spec target?~~
   **Decided 2026-11-11 (Claude, on the owner's delegation):** amend now; also
   ADR 0018 ("as amended by 0105") and ARCHITECTURE.md.
2. Reference refresh for the rule: 144 Hz (strictest, per COMP-14 §2.2), **or** the
   panel actually on the reference machine?
3. ~~Hardware baselines: manual runbook, or self-hosted nightly runner?~~
   **Decided 2026-11-12 (owner):** GPU numbers come from a manual TTY runbook. No
   self-hosted runner.
4. ~~B1 (render once per dispatch round + EWMA deadline): now, or only once
   numbers show the need?~~ **Decided 2026-11-11 (Claude, on the owner's
   delegation):** implement now as a spec conformance fix.
5. ~~Headless CI runner: Mesa software rendering, or a runner with a render node?~~
   **Decided 2026-11-12 (owner):** CI runs CPU-side software-rendered numbers on every
   PR.
6. ~~Baseline record location?~~ **Decided 2026-11-12 (owner):** baselines are JSON
   committed in the repo (`abyss/bench/baselines/`).

## Unverified in this ADR
- Whether config parse and audit serialization run on the loop today.
- Whether the atomic DRM path blocks the loop on GPU completion without `IN_FENCE_FD`.
- GPU timer-query availability on the reference driver.
- niri / cosmic-comp / wlroots threading (recollection only).

## Revisit when
The first reference-machine baseline is recorded (evaluate Step B), or a COMP-14 §2.1 /
§2.2 target is breached in any recorded run, or smithay's renderer becomes `Send`
(a pin bump, its own ADR).
