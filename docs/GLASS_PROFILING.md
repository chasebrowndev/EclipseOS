<!-- SPDX-License-Identifier: AGPL-3.0-only -->

# Glass profiling and headless glass captures (COMP-02, liquid-glass Phase 0)

Counters for the blur/glass backdrop chain, so every later phase can be
compared against a recorded baseline, plus the recipe for a headless glass
screenshot. Source: `ec-abyss-render/src/glass_profile.rs`.

`abyss/bench/` is a CPU micro-benchmark harness (no GL context), so it does not
host this; the baseline is captured from a headless compositor instead.

## What is counted

Per output (one `BlurStore`), summarised every 120 frames as one `info` line
`glass profile` in the journal when `EC_GLASS_PROFILE` is set (read once when
the store is created, so set it before starting abyss). The values are journal
fields (`journalctl --user -t <comm> -o json`, `F_REBUILDS` and so on):

| Field | Meaning |
|---|---|
| `frames`, `avg_surfaces` | Frames in the window; mean blurred surfaces per frame |
| `rebuilds`, `max_rebuilds_per_frame` | Full chain rebuilds (backdrop redraw + down/up passes) |
| `level0_clears` | Age-0 clears of the output-sized level 0 (one per rebuild today) |
| `crop_texels` | Backdrop texels redrawn, summed crop area of every rebuild |
| `cleared_texels` | Texels cleared in level 0 (whole output per rebuild) |
| `passes`, `max_passes_per_rebuild` | Down + up + final blits run (7 at 4 passes) |

GPU timer queries (`EXT_disjoint_timer_query`) are not included: smithay 0.7.0's
GLES bindings do not expose the extension, so it would take raw
`eglGetProcAddress` pointers and the crate's first `unsafe`. Use `--stats`
(p50/p99 frame CPU time) alongside the counters.

## Headless glass recipe

Headless only (never the live session). Runtime dir short (socket path limit),
`policy.kdl` allowing `grim`, `mode "de"` in `abyss.kdl`; see the headless
capture recipe for the rest. What is specific to glass:

1. **The client must ask for glass.** foot (1.28) declares an *empty*
   `ext_background_effect` blur region by default, which is an explicit opt-out:
   the window gets no backdrop whatever its alpha, a `blur` rule, or an
   `opacity` rule (only its titlebar strip blurs). The window then looks like
   a translucent tint over a crisp backdrop. Start it with
   `foot -o colors-dark.alpha=0.35 -o colors-dark.blur=yes` (the section is
   `colors-dark`, not `colors`) so it requests a blur over its whole surface.
   Without a client request, `windowrule "translucent-region top=<H> ..."`
   also takes the window out of the opt-out.
2. **Size the windows with rules**, they ignore foot's own size flags:
   `windowrule "size 800x600" { app-id "^glassy$"; }`, and start foot with
   `-a glassy`. `ec-ctl --json windows` reports the rect (the border is outside
   it, so 800 reads 798).
3. **A discriminating backdrop**: big opaque black and white blocks (a 4x3
   checker of 50-column, 20-row foot cells) under the glass window. A busy text
   backdrop averages to flat grey under the blur and shows nothing.
4. **Output size**: `--size 2880x1620`; `grim out.png` after about 5 seconds.
   `ec-ctl focus <handle>` raises the glass window over the backdrop.
5. `EC_GLASS_PROFILE=1` in the environment of the compositor gives the counters
   above for the same run.

Kill only the pids you spawned; remove the runtime dir afterwards.

## Capturing the baseline

Scenarios, each at 1, 5, 10 and 20 glass surfaces (the `CROWDED` throttle
starts past 10):

1. idle (nothing moving; expect zero rebuilds)
2. drag one glass window across others
3. resize one glass window
4. damage underneath (video or a repainting client behind the glass)

Record the resulting lines in the table below.

## Baseline

Measured with the recipe above on commit `2d549482` (the px_uv fix and retune,
`glass.refraction 1.5`), debug profile of the dev build (`target/debug`, which
the workspace builds with optimisation), headless backend on the Framework
laptop's AMD Ryzen AI 7 350 with its Radeon 860M (`radeonsi`, `/dev/dri/renderD128`),
output 2880x1620 at scale 1, `--stats` on. This is not the NVIDIA desktop; do
not compare these numbers with that machine's.

Setup: one opaque 1600x1000 backdrop foot (static 4x3 black/white checker, or
for "damage" a loop repainting random blocks every ~30 ms), N translucent
glass foots of 600x400 cascaded over it, default blur (size 8, 4 passes, so a
128 px kernel margin), glass mode. Each row is the sum of the `glass profile`
lines emitted during the scenario (120 frames each; the whole window is 6
lines idle, 8-9 for drag and resize). Windows were opened one by one, then
settled for 4 s before the window starts; the first-frame build cost is
therefore not in these rows.

"surfaces" is the `avg_surfaces` field and reads N + 1: the N glass windows
plus one more blur request that I did not identify (most likely the backdrop
foot's titlebar strip). N = 10 therefore reads 11, which is past `CROWDED`
(more than 10) and so is throttled to one damage-driven refresh per 6 frames
per surface; N = 20 is throttled too. Every
rebuild runs 7 passes (4 down, 2 up, 1 final) and clears the whole output
(4.67 Mtexels at 2880x1620) whatever the crop, so cleared texels per frame are
`rebuilds/frame x 4.67 M`.

| Scenario | N | surfaces | frames | rebuilds | rebuilds/frame | max/frame | level0 clears | crop Mtex/rebuild | render p50 / p99 (us, range over the 5 s reports) |
|---|---|---|---|---|---|---|---|---|---|
| idle | 1 | 2 | 840 | 0 | 0 | 0 | 0 | n/a | 808-829 / 1706-1772 |
| idle | 5 | 6 | 720 | 0 | 0 | 0 | 0 | n/a | 980-1098 / 2406-2597 |
| idle | 10 | 11 | 720 | 0 | 0 | 0 | 0 | n/a | 1021-1441 / 3519-3898 |
| idle | 20 | 21 | 720 | 0 | 0 | 0 | 0 | n/a | 1599-2236 / 4292-4378 |
| drag | 1 | 2 | 1080 | 71 | 0.066 | 1 | 71 | 0.57 | 775-864 / 1994-2231 |
| drag | 5 | 6 | 1080 | 72 | 0.067 | 1 | 72 | 0.60 | 660-1039 / 2853-3433 |
| drag | 10 | 11 | 1080 | 71 | 0.066 | 1 | 71 | 0.60 | 1073-1423 / 3066-4710 |
| drag | 20 | 21 | 960 | 65 | 0.068 | 1 | 65 | 0.60 | 1730-2434 / 4555-5707 |
| resize | 1 | 2 | 1080 | 14 | 0.013 | 2 | 14 | 0.49 | 671-759 / 2321-3580 |
| resize | 5 | 6 | 1080 | 18 | 0.017 | 6 | 18 | 0.54 | 848-1080 / 2797-4843 |
| resize | 10 | 11 | 1080 | 15 | 0.014 | 11 | 15 | 0.67 | 1074-1485 / 3444-4520 |
| resize | 20 | 21 | 1080 | 4 | 0.004 | 1 | 4 | 1.02 | 1675-2455 / 4654-5830 |
| damage under | 1 | 2 | 720 | 406 | 0.564 | 1 | 406 | 0.45 | 774-1403 / 5826-5944 |
| damage under | 5 | 6 | 600 | 1950 | 3.25 | 5 | 1950 | 0.54 | 1294-1707 / 13062 |
| damage under | 10 | 11 | 600 | 1000 | 1.667 | 9 | 1000 | 0.57 | 1772-2559 / 15990-17174 |
| damage under | 20 | 21 | 600 | 2000 | 3.33 | 11 | 2000 | 0.58 | 3128-5691 / 15991-17264 |

All counters and render percentiles are measured. `render` is the CPU time in
`render_frame` from `--stats`; `fps` was 56 (N = 1) falling to 51 (N = 20)
idle and drag, and 52 / 46 / 46 / 41 for damage. Idle at N = 20 costs 1.6-2.2 ms
p50 per frame with zero rebuilds, so most of the N-scaling there is element
collection, not the blur chain.

Not available: GPU time (no timer queries, see above), so "frame time" here is
CPU render time only; per-pass timings; and any NVIDIA-desktop number.

Caveats on how the scenarios were scripted:
- **Drag** and **resize** are `ec-ctl drag ... --steps 120` (scripted input is
  injected as a burst, not paced per frame), three out-and-back repeats with a
  3 s wait after each. The compositor therefore renders only about a dozen
  frames of motion per drag (about 12 rebuilds per drag), not 120; the
  rebuilds column is per scenario window (about 18 s), idle stretches included.
  Treat them as a lower bound on per-frame drag cost, and compare phases
  against each other rather than against a real-time drag. The dragged window
  is the topmost glass window; it moves from its cascade slot to (1800, 714)
  and back. A 16 px snap of the crop origin is what makes consecutive drag
  frames rebuild.
- **Resize** grabs the bottom-right corner 6 px outside the frame of the last
  glass window and drags it +500/+400 and back; the net window size changed
  by 6 px in width and none in height in every run, so a resize did take hold,
  but the N = 20 row (4 rebuilds) looks anomalously low next to N = 10 and may
  have grabbed a different window or been absorbed by the `CROWDED`
  throttle; do not read it as a trend.
- **Damage under** repaints random 50x10-cell blocks of the backdrop (so
  every glass window under it is dirty on most frames). At N = 5 every glass
  window rebuilds nearly every frame (3.25 per frame); at N >= 10 the
  `CROWDED` throttle (one refresh per surface per 6 frames) caps it, which is
  why N = 10 reads lower than N = 5. This is the scenario the shared backdrop
  pyramid is meant to flatten: cost scales with N there, and p99 render
  time goes from about 6 ms to 16 ms.

## The `px_uv` correction and the retune

The glass/frost element samples the blur result texture, which covers the
chain's crop (window plus kernel margin), not the whole output. One physical
pixel is therefore `1/crop` in that texture's UV, but `px_uv` was `1/output`.
The blur store now points `px_uv` at the crop (`effects::set_px_uv`).

For an 800 px window on a 2880 px output (default size 8, 4 passes: crop about
1070 px wide, k = 1070/2880 = 0.37) the bend and the adaptive taps were drawn at
about 0.37 of their configured physical-pixel size. The bevel width was never
affected (it comes from the SDF). To keep that case looking as it did, the
defaults carry the factor:

| | Old | New |
|---|---|---|
| `decoration.blur.glass.refraction` | 4 | 1.5 (4 x 0.37) |
| adaptive `ADAPT_STEP` / `ADAPT_SPAN` | 48 / 0.5 | 18 / 0.19 |
| adaptive `ADAPT_INSET` / `ADAPT_MIN_STEP` | 24 / 6 | 9 / 2.2 |

`glass.refraction` is now a float (0..64). Whole numbers in existing configs
still parse; they are now true physical-pixel bends (an old `refraction 4` is
2.7x stronger than it looked). `glass_reach` takes the float and rounds the
reserved margin up to whole pixels.

The old look was crop/output dependent, so only the reference case matches
exactly. Headless old-vs-new captures with the checker backdrop (glass window
800x600 over a 1600x1000 backdrop, 2880x1620): inside the glass window the mean
absolute difference is 0.35 of 255 (max 13) at 800 px, and 1.1 (max 18) for a
300x200 window, where the old bug was weaker (crop/output about 0.2) so the new
bend is slightly stronger than the old picture.
