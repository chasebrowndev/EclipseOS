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

GPU time is a separate log line group on the same summary (`gpu`, `gpu_*_us`),
see "GPU baseline (step 0)" below. Use `--stats` (p50/p99 frame CPU time)
alongside the counters.

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

## After Phase 1a (depth tiers)

Branch `comp02-glass-pyramid`, same machine and harness as the baseline above.
Phase 1a replaces the per-window blur chain with one shared pyramid per
*depth tier* of an output (`ec-abyss-render/src/groups.rs`, `blur.rs`
`group_refresh` / `group_element`, `lib.rs` `insert_blur`).

- **Tier of a window.** 0 if no window below it in z-order overlaps its own
  region (the frame rectangle, never its blur crop), otherwise one more than the
  highest tier among the lower windows that do. Every window counts, glass or
  not. Computed back to front into store-owned scratch vectors.
- **One pyramid per tier, built lowest first.** Tier 0 is built from what lies
  below every window (wallpaper, bottom layers). Tier `d` is built from that
  plus the windows of tier `< d` below the tier's topmost member, composited in
  z-order together with their own glass, so a window sees the lower windows that
  overlap it and never its own content or anything above it. Tiled or
  non-overlapping windows all share tier 0 whatever their blur margins reach.
- **Cap.** `MAX_TIERS` = 4 stacked layers (tiers 0..3). A window deeper than
  that clamps to tier 3 and samples that pyramid, which is built like the rest
  from windows of true tier `< 3`: a clamped window does not see a lower window
  that is itself clamped. If a pyramid cannot be built its members take the
  per-window path for that frame (`BlurStore::element`, unchanged).
- **Only a window's own backdrop is tiered.** Layers, popups and shaped
  backdrops, and panes (which keep reusing their window's backdrop), stay on the
  per-surface path. Panes of a lower window are not part of a higher tier's
  composite.
- Each member draws the same `BlurElement` as before with its region as a UV
  offset into the shared half-resolution result; `px_uv` is one physical pixel of
  the union crop of the tier's members. One damage scan per pyramid. The
  crowded throttle is per pyramid. Rebuilds are still whole-pyramid and
  full-crop (1b and 1c are not done).
- The capture pass (`still.rs`, which `grim` and the built-in screenshots use)
  records its windows the same way and takes the same path, so a capture and the
  screen agree. A redaction card pushed between windows rides with the window
  below it.

### Pixel checks (headless, 2880x1620, checker backdrop, full frame)

All through `grim`, Phase 0 binary against this branch:

| Scene | Differing samples | Mean abs diff / max (of 255) |
|---|---|---|
| one glass window over the checker window | 0 of 14.0 M | 0 / 0 |
| five glass windows apart (several share tier 1) | 2548 of 14.0 M | 0.0002 / 1 (UV rounding) |
| five glass windows cascaded (overlap chain) | 0 of 14.0 M | 0 / 0 |
| eight glass windows cascaded (chain deeper than the cap, clamped) | 807 564 of 14.0 M | 0.234 / 26 |
| glass window 14 px beside a lower white window of its own tier, plain `blur` mode | 42 618 of 14.0 M | 0.042 / 50 |
| same, glass mode (adaptive tint darkens both) | 0 | 0 / 0 |
| glass window 18 and 46 px beside a lower glass window | 0 | 0 / 0 |

The two non-zero rows are the accepted differences. A lower window that does not
overlap the glass window no longer pulls into its blur through the kernel margin
(the 14 px case: the white neighbour's bleed at the glass window's edge is gone,
up to 50 levels in plain blur mode and nothing visible in glass mode); and a
stack deeper than four stops seeing the clamped window directly beneath it.
The overlap chains up to the cap and the pair (upper window's backdrop contains
the lower window's content and not its own) match Phase 0 exactly.

### Cascade layout (same matrix as the baseline)

Rebuilds per frame and render times, Phase 0 to tiers. Rows marked * were
re-measured today with the Phase 0 binary (this machine ran about 3 percent
faster today than in the baseline session); the rest compare against the
baseline table above. Render p50 / p99 in us, range over the 5 s reports.

| Scenario | N | rebuilds/frame | max/frame | render p50 | render p99 |
|---|---|---|---|---|---|
| idle | 1 | 0 to 0 | 0 to 0 | 808-829 to 222-291 | 1706-1772 to 900-1144 |
| idle | 5* | 0 to 0 | 0 to 0 | 334-479 to 322-433 | 1128-1641 to 993-1300 |
| idle | 10 | 0 to 0 | 0 to 0 | 1021-1441 to 430-624 | 3519-3898 to 1508-2418 |
| idle | 20* | 0 to 0 | 0 to 0 | 827-1227 to 681-893 | 2390-2416 to 2340-2487 |
| drag | 1 | 0.066 to 0.072 | 1 to 2 | | |
| drag | 5* | 0.072 to 0.162 | 1 to 4 | 326-489 to 360-481 | 1349-2087 to 1947-2213 |
| drag | 10 | 0.066 to 0.100 | 1 to 4 | | |
| drag | 20* | 0.067 to 0.099 | 1 to 4 | 733-1163 to 589-819 | 2007-2774 to 1988-2475 |
| resize | 1 | 0.013 to 0.013 | 2 to 2 | | |
| resize | 5 | 0.017 to 0.007 | 6 to 4 | | |
| resize | 10 | 0.014 to 0.007 | 11 to 4 | | |
| resize | 20 | 0.004 to 0.004 | 1 to 1 | | |
| damage under | 1 | 0.564 to 0.549 | 1 to 1 | | |
| damage under | 5* | 2.868 to 1.692 | 5 to 3 | 458-2168 to 505-750 | 3947-4054 to 3053-3104 |
| damage under | 10* | 1.667 to 0.500 | 9 to 3 | 793-1359 to 647-1146 | 7369-7662 to 3090-3111 |
| damage under | 20* | 3.333 to 0.500 | 11 to 2 | 1654-2515 to 1184-1458 | 10162-10632 to 3210-3339 |

Idle with 20 overlapping glass windows is about a quarter cheaper at p50
(1110 to 838 us median of the reports), with zero rebuilds either way; under
damage the cost no longer grows with N (p99 render time 10.2 ms to 3.3 ms at
N = 20). Drag is worse: a dragged window changes the tier's union crop on every
step, so the whole pyramid rebuilds, with a larger crop (1.0-1.75 Mtexels
against 0.6). That is the 1c problem (the drag margin), not fixed here.

### Spread layout (windows 960 x 810 px apart), same day

| Scenario | N | rebuilds/frame | max/frame | crop Mtex/rebuild | render p50 | render p99 |
|---|---|---|---|---|---|---|
| idle | 3 | 0 to 0 | 0 to 0 | n/a | 281-352 to 293-350 | 949-1032 to 1021-1195 |
| idle | 5 | 0 to 0 | 0 to 0 | n/a | 379-436 to 336-434 | 1208-1367 to 1104-1307 |
| damage under | 3 | 1.114 to 0.551 | 2 to 1 | 0.51 to 1.08 | 425-720 to 439-713 | 2640-2663 to 2267-2380 |
| damage under | 5 | 2.278 to 0.554 | 4 to 1 | 0.54 to 2.46 | 473-727 to 487-1250 | 3631-4524 to 2278-5022 |
| drag | 3 | 0.058 to 0.111 | 1 to 2 | 0.59 to 1.75 | 353-439 to 294-379 | 866-1744 to 1549-1934 |
| drag | 5 | 0.067 to 0.059 | 1 to 2 | 0.60 to 2.77 | 317-424 to 318-405 | 977-3996 to 1615-1856 |

Under damage the rebuilds drop by 2.1x (N = 3) and 4.1x (N = 5), and cleared
texels per frame from 10.6 M to 2.6 M at N = 5. Texels redrawn into level 0 per
frame are about 11 percent higher at N = 5 (1.36 M against 1.23 M): fewer,
larger rebuilds. Drag at N = 3 doubles its rebuild count, as above.

### Tiled layout (`mode "wm"`, N glass windows tiled, no overlap), same day

Every window is tier 0, so there is one pyramid, built from the wallpaper only.
In the damage scenario the first and the last tile are opaque windows
repainting random blocks every ~30 ms (a window's content changes, the wallpaper
does not); the other tiles are static glass windows. `avg_surfaces` counts the
glass windows (2 fewer than N in that scenario).

| Scenario | N | rebuilds/frame | max/frame | render p50 | render p99 |
|---|---|---|---|---|---|
| idle | 4 | 0 to 0 | 0 to 0 | 211-271 to 241-308 | 1626-3005 to 1262-2944 |
| idle | 9 | 0 to 0 | 0 to 0 | 427-527 to 387-467 | 2384-2832 to 1157-2098 |
| idle | 20 | 0 to 0 | 0 to 0 | 836-1103 to 587-739 | 5234-5594 to 1560-1872 |
| window content changes | 4 | 1.131 to 0 | 2 to 0 | 314-1269 to 273-947 | 3510-4145 to 3251-3951 |
| window content changes | 9 | 4.151 to 0 | 7 to 0 | 788-2728 to 551-1226 | 4475-4578 to 2859-3481 |
| window content changes | 20 | 3.000 to 0.167 | 18 to 1 | 1524-1816 to 1199-1493 | 7516-7991 to 3307-3626 |

A tiled layout needs no rebuild when a window's content changes: the one
pyramid only depends on the wallpaper and the bottom layers.

The 120 rebuilds in 12 s at N = 20 (one every 6 frames, the crowded throttle)
are real damage, not a bug: the harness's spiral tiling at N = 20 gives windows
of 6 x 16 px, which the terminal cannot shrink to, so the smallest tiles overlap
one another and the repainting one sits under others. Those are tier 1 windows,
whose pyramid contains the repainting window. N = 12, whose tiles are large enough not to overlap, has 0
rebuilds.

Not measured / unavailable: GPU time (no timer queries).

## Phase 1c: the drag margin

A tier's pyramid is built over its members' union crop plus a margin
(`crop_margin`, an eighth of the longer side of the output clamped to 128..512
px: 360 px at 2880 wide), its size rounded up to a 512 px bucket
(`CROP_BUCKET`) and kept on the output and the chain grid (`padded_crop`). A
member that moves or grows inside that region changes only its UV offset: no
rebuild, no new chain key. It rebuilds when a member leaves the region, when
membership or tier changes, on a parameter change, and on damage anywhere in the
region (damage in the margin, outside every member's crop, rebuilds too, or a
member moving onto it would show stale pixels). Chains are keyed by the bucketed
size, so a drag reuses the same few chains (at most eight distinct sizes
through the whole output, tested). 360 px: a rebuild per about 360 px of travel
rather than per step; each rebuild is a few times a window's own crop (the margin
is paid in texels on every rebuild, which is why it is not larger).

Cost: a damage rebuild now covers the padded region, so under damage the crop
per rebuild is 2.6 Mtexels against 1.1 (spread N = 3) and 4.2 against 2.5
(N = 5); the render p99 under damage at N = 5 rose from 2.6 ms to 5.3 ms in one
run. Pixels: single and cascaded windows differ slightly from Phase 0 (one
glass window: 8805 samples, max 10 of 255; five cascaded: mean 0.11, max 17).
Cause, bisected with a debug override of the built region on the one-window
scene (not shipped): with the region equal to the old crop the output is
identical to Phase 0 again (0 of 14.0 M), so it is the padding. It is not the
finite-crop edge effect of the old crop (a build over the whole output differs
from Phase 0 by only 9314 samples of at most 3 levels, so Phase 0 is already
close to the unbounded result, while the padded builds are *further* from it,
up to 10). Growing the old crop by 128 px in width, or by 16 or 64 px in both,
still matches Phase 0; by 256 px in width the window's left 50 px band changes by
up to 10 levels; any growth in height changes its top band (y 40 to 92) by at
most 3. The differences therefore sit at the window's own left and top edge and
switch on at a size threshold, rather than shrinking with a larger margin: a
crop-size dependence in the sampling, not yet pinned to a shader or a grid
phase. Margin alone and bucketing alone each differ (14759 and 9985 samples).

Rebuilds per frame while dragging, Phase 0 / tiers (before 1c) / tiers with the
margin. `drag` is `ec-ctl drag --steps 120` (a burst, about 12 frames of motion,
x3 out and back, so a lower bound on a real drag). `paced` is 40 drags of 20 px
(4 steps each), one per ec-ctl round trip, over 800 px out and back three times
(about 480 frames): the window follows the pointer throughout.

| Layout | N | burst Phase 0 / tiers / 1c | paced Phase 0 / tiers / 1c |
|---|---|---|---|
| cascade | 1 | 0.066 / 0.072 / 0.017 | 0.214 / 0.160 / 0.017 |
| cascade | 5 | 0.072 / 0.162 / 0.039 | 0.163 / 0.265 / 0.038 |
| cascade | 10 | 0.066 / 0.100 / 0.043 | 0.135 / 0.148 / 0.033 |
| cascade | 20 | 0.067 / 0.099 / 0.042 | 0.072 / 0.014 / 0.011 |
| spread | 3 | 0.058 / 0.109 / 0.002 | 0.025 / 0.033 / 0.014 |
| spread | 5 | 0.067 / 0.059 / 0.010 | 0.133 / 0.035 / 0.117 |
| tiled | 4, 9, 12, 20 | not a drag | 0.067, 0.150, 0.092, 0.158 / (n/a) / 0, 0, 0, 0.008 |

Caveats, honestly: the 1c rebuilds that remain are the ones at the margin edge
(about one per 360 px) and the first build of each pass; it is not ~0 mid-drag
for a window that travels farther than the margin. In the spread layout the
paced drag only moved the window 80 px (it was dragged by a window that sits
against the output edge), and the N = 5 paced rows there are dominated by tier
changes as the dragged window crosses others (membership change = rebuild), not
by the margin. In `mode "wm"` a drag does not move a tile; the Phase 0 rebuilds
in the tiled row are crop invalidation as the layout reacts, and 1c has none.

Damage and idle (1c), rebuilds per frame: cascade damage 1 / 5 / 10 / 20 =
0.546 / 1.675 / 0.500 / 0.500 (tiers before 1c: 0.549 / 1.692 / 0.500 / 0.500);
spread damage 3 / 5 = 0.550 / 0.572; tiled damage 4 / 9 / 12 / 20 = 0 / 0 / 0 /
0.167. Idle is 0 everywhere. Render p50/p99 are within run-to-run noise of the
tiers numbers apart from the damage p99 above.

## After 1b (damage-limited rebuilds)

Branch `comp02-glass-damage` (on top of the tier and margin work). A tier
rebuild no longer clears the whole output-sized level 0 and re-blurs its whole
region for a small damage rect.

- **Persistent per tier.** Each tier owns its level 0 (output-sized, drawn over
  the built region only), its down levels `1..=passes`, its up levels
  `2..passes` (kept apart from the down levels, which the up passes used to
  overwrite), its result texture and a render `OutputDamageTracker`. About 25 MB
  per tier at 2880 x 1620 (the output-sized level 0 is most of it), allocated on
  first use and dropped with the tier.
- **Damage since the last build.** A partial rebuild renders the backdrop with
  `age = 1` into the tier's own level 0, so the tracker's diff against its last
  build is exactly the damage since then, however many frames ago (throttle and
  `pending` are unchanged: a deferred rebuild simply sees more damage). Elements
  are still cropped to the built region, so damage outside it is ignored.
- **Per-level rectangles** (`ec-abyss-render/src/partial.rs`). Damage is turned
  into texture space (flipped under `Flipped180`), then each pass is drawn only
  over the destination texels whose taps can reach a changed source texel: the
  downsample reaches 0.5 x offset source texels, the upsample offset texels, each
  plus the bilinear footprint, with the real size ratio of the level (not 2:
  levels of an odd size are slightly off), plus one texel each side for float
  rounding. The final write into the result is flipped back when the output is
  mirrored. Passes are drawn with the GLES frame's damage rectangles (instanced
  quads), so the sampling coordinates of every texel are the full pass's.
- **Full rebuilds** stay for: the first build, a member, tier, parameter or
  region change (`fresh`), a resized built region, GL errors, and damage whose
  level 1 rectangles cover at least half of level 1 (`FULL_SHARE`): past that the
  grown rectangles reach most of the level anyway and the per-rectangle draws
  cost more than they save. More than eight rectangles per level fold into
  their bounding box.
- **Nothing changed means no rebuild:** if the tracker reports no damage inside
  the region the members are left alone (no commit bump, no redraw).
- `EC_GLASS_FULL_REBUILD=1` forces every tier rebuild full (the reference for
  the pixel comparison below); the profile line gains `pass_texels` (texels
  written by all passes) and `partial_rebuilds`.

### Correctness

- `partial::tests::a_pass_changes_nothing_outside_its_spread` runs the real
  taps (5 down, 8 up, bilinear, edge clamp) of 400 random cases (offsets 1 to 16,
  source sizes that are not twice the destination's, sub-rectangle origins) and
  asserts that every destination texel a perturbed damage rectangle changes lies
  inside the rectangle `spread` reports (more than 10 000 changed texels
  checked); further tests cover the whole-chain plan, the mirrored flip, the
  fold into a bounding box, and the full-rebuild threshold.
- Pixels, partial against `EC_GLASS_FULL_REBUILD`, 2880 x 1620 `grim`, after a
  deterministic repaint sequence (70 coloured blocks, 0.1 s apart, over a
  checker, under glass windows): **0 differing samples of 14 000 000** in each of
  three scenes (3 windows at the default blur; 6 windows at size 12, 3 passes;
  4 windows at size 6, 5 passes), with 10 to 63 of the rebuilds partial in each
  run. The mirrored (`Flipped180`, winit) case is covered by the unit test only,
  since no nested window may be opened.

### Numbers (headless, this machine)

Per frame, 1c against 1b, same day. Cleared and drawn are Mtexels of level 0;
pass is texels written by all blits (not measured for 1c: a full pass writes
about 0.66 x the region). Rebuilds per frame are unchanged by 1b.
`damage` is the harness's repainting window (the whole backdrop changes each
frame, so most rebuilds are full); `small` is a 12-cell block at a random place
every 30 ms.

| Layout | Scenario | N | rebuilds/f | partial | cleared 1c to 1b | drawn 1c to 1b | pass (1b) | render p50 / p99 us 1c to 1b |
|---|---|---|---|---|---|---|---|---|
| cascade | damage | 5 | 1.78 | 0 of 1068 | 8.28 to 2.69 | 3.26 to 3.27 | 2.16 | 1264 / 6018 to 1290 / 5504 |
| cascade | damage | 20 | 0.50 | 120 of 360 | 2.33 to 0.77 | 1.08 to 0.79 | 0.62 | 2450 / 5610 to 2251 / 5873 |
| cascade | small | 5 | 1.78 | 1266 of 1266 | 8.30 to 0.31 | 3.26 to 0.31 | 0.65 | 1055 / 4479 to 1096 / 3962 |
| cascade | small | 20 | 0.50 | 298 of 300 | 2.33 to 0.09 | 1.08 to 0.09 | 0.23 | 1468 / 4183 to 1660 / 4433 |
| spread | damage | 3 | 0.57 | 0 of 340 | 2.63 to 0.89 | 1.48 to 1.49 | 0.98 | 1119 / 4308 to 1090 / 4281 |
| spread | damage | 5 | 0.57 | 411 of 411 | 2.66 to 0.90 | 2.36 to 0.90 | 0.83 | 1231 / 4602 to 1265 / 4136 |
| spread | small | 3 | 0.57 | 408 of 408 | 2.66 to 0 | 1.49 to 0 | 0.05 | 930 / 3044 to 1049 / 3330 |
| spread | small | 5 | 0.57 | 410 of 410 | 2.68 to 0 | 2.38 to 0 | 0.05 | 1047 / 3689 to 980 / 3602 |
| tiled | damage | 20 | 0.17 | 120 of 120 | 0.78 to 0 | 0.04 to 0 | 0 | 2855 / 5863 to 2799 / 6153 |

Drag, burst and paced (`pdrag`), rebuilds per frame unchanged (cascade N = 5:
0.042 to 0.039 and 0.038 to 0.038; spread N = 5 paced 0.085 to 0.083); of those
rebuilds, 18 of 42 (burst) and 6 of 18 (paced) are partial at cascade N = 5, 32
of 40 at spread N = 5 paced, and the cleared texels per frame fall by 30 to 85
percent. Idle is 0 everywhere.

Reading it honestly:

- The texels cleared and written fall sharply where the damage is small
  (cascade N = 5 small damage: 8.3 to 0.31 Mtexels per frame cleared, and about
  2.2 to 0.65 written by the passes, if a full pass writes 0.66 x the region;
  spread: 2.7 to 0 cleared, about 1.0 to 0.05 written) and less where it is large:
  the repainting-window scenario damages most of the built region each frame, so
  the threshold sends its rebuilds to the full path (cascade N = 5: 0 of 1068
  partial), and what is saved there is only the whole-output clear (8.28 to 2.69:
  a rebuild that is not partial still draws and clears only its damage).
- Render p50 / p99 here is CPU time to build the frame (`--stats`): it does not
  move within noise, because the work 1b removes is GPU fill, not CPU. There is
  no GPU timer available (see the top of this file), so the GPU saving is
  inferred from the texel counts, not measured.
- The 1c margin made damage rebuilds larger (spread N = 5: 4.2 against 2.5
  Mtexels of crop per rebuild); with 1b a small damage costs a few thousand
  texels again, whatever the margin, and a damage that covers most of the region
  costs what it did in 1c.

## Chromatic aberration

Branch `comp02-glass-ca`. `decoration.blur.glass.dispersion` (0..1, default 0) now
draws a visible colour fringe at the glass edge. The old dispersion moved red and
blue by 5 percent of a bend of at most 4 px (under 0.2 px) and read the heavily
blurred result, so it was invisible.

- **Fringe.** In the glass program, red is read `dispersion * DISPERSION_PX * u`
  physical px along the inward edge normal and blue the same distance outward,
  on top of the existing bend; `u` is the bevel falloff (1 at the outer edge, 0
  on the plateau). `DISPERSION_PX` is 8 logical px at dispersion 1 (scaled by the
  output scale), `effects::fringe_px`. The offsets are in real pixels: the
  program multiplies by `px_uv`, which is one physical pixel of the sampled
  texture, so the fringe does not depend on the output or the tier's union size
  (tested).
- **Sharper source, no second sampler.** smithay 0.7.0's GLES path binds one
  texture per draw and has no sampler uniform, so the lightly blurred level
  (down level 1, `/2`, five taps) is packed beside the blurred result in one
  texture: twice as wide, blurred left, level 1 right, only while
  `dispersion > 0` in glass mode (`blur::packs`). Every channel moves from the
  blurred result toward the packed sharp half by `u`, so the plateau stays pure
  blur and nothing marks where the effect starts; green takes the same
  sharpening (with red and blue sharp against blurred green, a sharp edge
  turned into a wide green band in the first version). `px_uv.x` and the clamps
  are halved for the packed width (`dual_tex` uniform).
- **Rebuilds.** A partial rebuild copies the damaged rectangles of down level 1
  into the right half (the plan's level 1 rectangles, flipped as the result's);
  toggling dispersion at runtime changes the tier's parameters, which forces one
  full rebuild and allocates or drops the packed texture (tested; and checked
  live over the socket: 0 to 1 and back, 0 differing samples against a static
  start in each state).
- **Reach.** `glass_reach(refraction, dispersion, scale)` is the bend (with the
  old 5 percent margin, kept so a build without dispersion keeps its region) plus
  the widest fringe, and invalidation uses the same number, so the red and blue
  taps stay inside the built region (tested).
- **Panes** stay at 0 (their backdrop is not packed; the shader only fringes a
  packed texture). A plain-blur or frost member of a packed tier samples the
  left half as before; its clamp can reach the first texel of the right half at
  an output edge.
- **Cost.** Off: nothing changes (0 of 14.0 M samples differ from the 1b build in
  both configs below). On: five more texture taps per fragment (red and blue read both halves, green the sharp half) inside the
  bevel only (the shader returns before them outside it and skips them at
  `dispersion` 0), one more full-region blit per tier rebuild (level 1 into the
  right half, plus its rectangles on a partial rebuild), and a result texture
  twice as wide (about one third of the crop's texels more, per tier). GPU time
  is not measured.
- **What it looks like.** A window frame's glass is only its border ring (1 px
  by default), so with the default config the fringe is a faint line; the
  plain bevel (`general.border-size 0`, 16 px) shows it clearly. At 8 px a
  white-to-black edge under the window edge gets a blue fringe of about 4 px on
  its outer side and a dark red/black one inside, no halo, no bloom, no
  banding; `DISPERSION_PX` 3 to 16 were tried (3 reads as a thin tint, 12 and 16
  as a broad blue band).

## GPU baseline (step 0)

GPU time is measured with raw GL timer queries (`gpu_timer.rs`, the crate's one
`unsafe` module): `GL_TIME_ELAPSED_EXT` spans around the backdrop composite,
the down passes, the up passes (with the final write), the packed copy and the
glass draws, read back a few frames later without stalling. It runs only with
`EC_GLASS_PROFILE` set and when the driver advertises
`GL_EXT_disjoint_timer_query`; the summary line then carries `gpu`
(`timer`, `no-extension` or `off`), `gpu_frames`, `gpu_{backdrop,down,up,pack,glass}_us`
(mean per timed frame), `gpu_total_max_us`, `gpu_glass_max_us`, `gpu_lost`,
`gpu_disjoint` and `gpu_skipped`. A frame is the interval between two
glass-store frame starts, so measure with one output.

Machine: AMD Ryzen AI 7 350 w/ Radeon 860M (integrated, Mesa radeonsi), Linux
7.2.15, headless backend at 2880x1620, 10 glass windows, 12 s idle/damage and
three paced-drag passes. The extension was present; no frame was lost or
disjoint. Binary: step 0 (after the occlusion cull, allocation-free uniforms
and the preset scaffold). Times are ms per frame.

| layout | scenario | backdrop | down | up | pack | glass draws | total mean | total max | CPU p50 us | CPU p99 us |
|---|---|---|---|---|---|---|---|---|---|---|
| cascade | idle | 0 | 0 | 0 | 0 | 0.71 | 0.71 | 1.10 | 436 | 1491 |
| cascade | paced drag | 0.01 | 0.01 | 0 | 0 | 0.77 | 0.79 | 2.86 | 469 | 1716 |
| cascade | damage | 0.09 | 0.11 | 0.06 | 0 | 0.84 | 1.09 | 2.35 | 1023 | 2996 |
| spread | idle | 0 | 0 | 0 | 0 | 0.62 | 0.62 | 1.00 | 387 | 1459 |
| spread | paced drag | 0 | 0 | 0 | 0 | 0.60 | 0.60 | 0.89 | 392 | 1591 |
| spread | damage | 0.10 | 0.10 | 0.08 | 0 | 0.59 | 0.87 | 1.38 | 767 | 2642 |
| tiled | idle | 0 | 0 | 0 | 0 | 1.72 | 1.72 | 2.56 | 427 | 1490 |
| tiled | paced drag | 0 | 0 | 0 | 0 | 1.75 | 1.75 | 2.55 | 362 | 2292 |
| tiled | damage | 0 | 0 | 0 | 0 | 1.26 | 1.26 | 1.64 | 986 | 3064 |

Reading it: the blur chain is nearly free on this GPU (a tenth of a ms even
under damage, thanks to 1a/1b); the glass draws dominate, and they cost most when
windows tile (1.7 ms, large glass areas). The pack column is 0 because
dispersion is 0 in this config. Idle still draws glass every frame in headless
(the headless backend repaints at age 0); on a real output with damage tracking
the idle draw is skipped. A bigger iGPU number than a discrete card would give;
no NVIDIA-desktop number was taken.

## Step A (lens optics, cheaper shading)

What changed: the glass program has three compile-time presets (`GLASS_LENS`,
`GLASS_REGULAR`, `GLASS_FROST`). Shaped layer glass and glass panes are Lens, a
window with a glass bezel is Regular, any other window body is Frost. Lens and
Regular bend the rim with a convex profile (`strength * (1-t)^2.5`, pulled from
outside the shape), read the lightly blurred half of a packed texture there,
and fringe per channel (red `1-k`, blue `1+k`, `k = dispersion * 0.12`). The
plateau is one tap. Lens tiers/chains blur with 2 passes, Regular 3, Frost the
configured count; passes and packing are part of the tier's params, so a preset
change is exactly one full rebuild. Reach is per member
(`effects::preset_reach`). Config: `decoration.blur.glass.lens.{refraction,
bevel, dispersion, rim}` (defaults 40 / 40 / 0.8 / 0.7); Regular uses the
top-level keys (rim displacement = `refraction * 5`).

Cheaper shading: shaped glass takes its normal from an analytic gradient (one
evaluation per box instead of five), the adaptive tint reads 5 taps (centre plus
the cross) instead of 9, `pow` is `x*sqrt(x)` for one layer, and the tint is
skipped at `smoke` 0, the rim math outside its 1.25 px band.

GPU ms, tiled idle, 10 windows, interleaved runs on one machine state (the GPU
clock drifts between sessions: the same step-0 binary measured 1.72 ms in the
step 0 table and 2.6-2.8 ms later; compare rows within one table only):

| build | glass draws, ms (3 runs) | mean |
|---|---|---|
| step 0 | 2.60 2.80 2.59 | 2.66 |
| A1 (presets, lens optics; tiled windows have a bezel so they are Regular) | 2.74 2.87 2.89 | 2.83 |
| A1 + analytic shape gradient (no effect on unshaped) | 2.62 2.81 2.81 | 2.75 |
| + 5-tap adaptive | 2.40 2.56 2.10 | 2.35 |
| + pow/skips | 2.32 2.31 2.51 | 2.38 |

Full step-A matrix (10 windows, ms/frame, a later, faster GPU state, so not
comparable with the step 0 table above): cascade idle 0.61, drag 0.64, damage
0.86; spread 0.48, 0.47, 0.70; tiled 1.45, 1.48, 0.96.

Pixel difference vs step 0 at default config (Frost windows, adaptive on,
dispersion 0 and 0.5): 63147 of 3.5M samples
(1.8 percent) differ, max 9/255, mean 0.022; full image, every pixel: 252535
of 14.0M, max 10, mean 0.022. Frost with the 9-tap adaptive restored is
byte-identical to step 0 (tested before the tap change). The analytic shape
gradient vs central differences on an L-shaped glass: 847 of 1.27M samples
differ, max 5, mean 0.001.

Not done: a per-member luma value computed once per rebuild. The glass shader
has one sampler, the luma depends on the member's own rect (tap step and inset),
and reading a value back to a uniform would stall; the 5-tap form gets part of
the 0.7 ms the adaptive tint costs (measured: adaptive off, 0.96 ms vs 1.67 ms).

## Smooth adaptive tint, and the one-tap experiment

The adaptive tint's switch between the one-tap read and the 5-tap read was a hard
threshold (arm length 2.2 px, a surface about 23 px across). It is now a
smoothstep over arm lengths 0.55 to 8.8 px (`adaptive_tap_weight`, mirrored in
the shader's `w`); surfaces over about 93 px are unchanged. Worst case in the
tint model (c = 0.9 against a worst tap of 0.2): the old threshold jumped the tint
by 0.8 to 0.95 in one step; the blend moves it at most 0.06 to 0.08 per physical
pixel of resize. `smoke` 0, `adapt` 0, the 1.25 px rim band and the lens plateau
all fade to the value they skip, so they have no step; a unit test covers the
blend. Measured on a real window resized 12 to 96 px over vertical stripes and a
checker (dark scheme), the centre pixel's luma changes by at most 1.1 of 255
between adjacent sizes in both the old and the new build: that scene is blurred
to flat grey, so it cannot show the old pop. The 12 px window is mostly rim and is
the same in both builds.

One-tap experiment (adaptive tint from the pixel's own fetched colour, V2; V3 adds
a 30 percent squash toward the surface tone), measured on the same machine:

| | tiled idle ms (3 runs, mean) | cascade damage ms (mean) |
|---|---|---|
| V1 5 taps (kept) | 2.30 2.06 2.64 (2.33) | 0.96 1.15 0.98 (1.03) |
| V2 one tap | 1.60 2.08 1.81 (1.83) | 0.71 0.89 0.81 (0.80) |
| V3 one tap + squash | 1.68 1.89 2.07 (1.88) | 0.80 0.74 0.81 (0.78) |

Text contrast (WCAG, text colour against the glass along blank rows, worst over the
row and within 35 px of a bright-left / dark-right backdrop boundary) differs by
under 1.5 between variants in every case; the lowest is V2, lens, light scheme,
5.2 against V1 6.4 (all above 4.5). The side-by-side crops under `cap/look/adapt/`
show V2 and V3 drawing a dark contour along the boundary and stripe-like
modulation inside the lens interior (the tint follows the pixel's own luma), which V1
smooths. V1 was kept.
