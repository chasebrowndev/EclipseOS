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

| Scenario | Surfaces | rebuilds/120f | max/frame | crop_texels | passes | Frame p99 |
|---|---|---|---|---|---|---|
| idle, 800 px glass window, static backdrop (2880x1620, headless) | 1 | 0 | 0 | 0 | 0 | (not recorded) |
| first frames after map (same scene) | 1 | 10 then 4 | n/a | 2.8M then 0.9M | 70 then 28 | (not recorded) |

The rest is not yet recorded.

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
