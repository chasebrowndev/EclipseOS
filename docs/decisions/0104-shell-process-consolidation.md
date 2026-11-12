<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0104 — Shell pane memory: measure, trim, then decide on consolidation
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
Every DE pane is its own iced 0.14 process on `iced_layershell` 0.19.1 (vendored,
`shell/vendor/`, one `eclipse-patch` hook). Every pane crate enables iced's `wgpu`
feature and none enables `tiny-skia` (`shell/crates/*/Cargo.toml`), so each pane that has
a surface owns its own wgpu instance, device and driver stack.

**Measurement.** Live session on the owner's desktop (RTX 4060 Ti, driver 615.78.08, one
1920x1080 output, session up ~26.5 h), 2026-11-11, read-only: `ps`,
`/proc/<pid>/smaps_rollup`, a per-mapping PSS sum over `/proc/<pid>/smaps`, `nvidia-smi`.
MiB. "ps RSS" is the kernel's mm counter and runs higher than smaps `Rss` (it counts
driver-inserted pages smaps does not list); PSS is the honest per-process figure.

| process | ps RSS | PSS | anon | file | GPU-stack PSS¹ | fonts PSS | VRAM | CPU s | class |
|---|---|---|---|---|---|---|---|---|---|
| ec-abyss | 266 | 193 | 127 | 58 | 52 | 0 | 180 | 871 | compositor |
| ec-hyperion-bar | 242 | 175 | 61 | 114 | 74 | 32 | 40 | 391 | always-on |
| ec-wallpaper | 191 | 111 | 41 | 69 | 69 | 0 | 78 | 45 | always-on |
| ec-toasts | 12 | 9 | 2 | 6 | 0 | 0 | — | 30 | always-on |
| ec-desktop | not running | | | | | | | | always-on in `mode "de"` only |
| ec-portal, ec-pairing, ec-polkit-agent, ec-screensaver, ec-oracle-eyes, ec-glass-host | ≤5 each | ≤2 | 0 | ≤2 | 0 | 0 | — | | always-on, not iced |

¹ Mappings of `/dev/nvidia*`, `libnvidia-*`, `libcuda`, `libEGL*`, `libGLX_nvidia`,
`libvulkan*`, `libLLVM`, `libgallium`. Of that, `/dev/nvidiactl` alone is 29 MiB (bar) and
43 MiB (wallpaper): per-context driver memory, the part no knob removes.

The three iced panes together: 295 MiB PSS, of which 104 MiB anon, plus 118 MiB VRAM.
File-backed pages (driver libraries, fonts) are partly shared with abyss and Firefox and
are reclaimable under pressure, so the summed RSS (~445 MiB) overstates the hard cost;
anon + `/dev/nvidiactl` + VRAM is the part that is really per-process.

**Why toasts is 12 MiB.** Same renderer, same features. `ec-toasts` opens its layer
surface only while a card is shown and removes it with the last card
(`shell/crates/ec-toasts/src/app.rs`, `sync_surface`). The vendored runtime creates the
wgpu compositor on the first surface's refresh and drops it when the window manager is
empty (`shell/vendor/iced_layershell/src/multi_window.rs`, `remove_compositor`). Idle,
toasts has no device, no driver libraries mapped (0 of them in `/proc/<pid>/maps`), and no
font system. While a toast shows it presumably costs what the others cost (not measured).

**What the always-on panes draw.**
- **Bar**: text, SVG icons, canvas, `eclipse_glass_v1` cells. It wakes on compositor
  events or a 500 ms poll (`POLL`, `ec-hyperion-bar/src/app.rs`) and runs a frame clock
  (`tokens::motion::FRAME_MS`) only while folding, spinning or animating. 0.4 % of a core
  averaged. It also `dlopen`s NVML for its GPU readout (`ec-services/src/usage/gpu.rs`);
  `libcuda` in its maps is likely from that (unverified). Its 32 MiB of font PSS is 101
  system font files, CJK included, mapped by the text stack.
- **Wallpaper**: one `Layer::Background` surface per output showing a colour and an
  optional decoded image (`Handle::from_rgba`). No text, no chrome
  (`ec-wallpaper/CLAUDE.md`). It redraws on config, theme or output change only, plus three
  nudges at 60/300/1200 ms because iced_wgpu uploads images on a worker thread and draws
  nothing until it lands (`app.rs`, `NUDGES`). Static otherwise. 78 MiB of VRAM to show one
  1080p picture.
- **Toasts**: cards on demand; frame clock only while a card moves.

Both GPU panes map the Vulkan *and* the EGL/GL stacks plus lavapipe (`libvulkan_lvp`,
`libLLVM`): iced_wgpu defaults to `wgpu::Backends::all()` and
`PowerPreference::HighPerformance` unless `WGPU_BACKEND` / `WGPU_POWER_PREF` are set
(`iced_wgpu-0.14.0/src/settings.rs`, `src/window/compositor.rs`). No unit and no crate sets
any `WGPU_*` or `ICED_*` variable today. That Vulkan is the backend in use is inferred from
`libnvidia-glvkspirv` being mapped, not verified.

**Constraints.**
- ADR 0052 (amended by 0070 for names): one crate and one package per swappable
  component. Its rules: "A swappable component never depends on another one. Shared code
  goes into `ec-ui` (visual) or `ec-services` (data and D-Bus)." "Only `eclipseos-hyperion`
  depends on `eclipseos-abyss`. The others are plain layer-shell clients and must keep
  working under any compositor that speaks wlr-layer-shell." "Cross-component launches use
  the binary name on `PATH` … so a replacement binary of the same name slots in." Its stated
  motive is install granularity: toasts without our bar. It says nothing about process
  count; it constrains packages, dependencies and binary names.
- ADR 0066 / 0069: optional pieces are add-ons (manifest + hooks that stay off). None of
  these panes is an add-on, and none of the options below needs one.
- Not TCB. Trusted UI is compositor-drawn, never a layer-shell client (root
  `CLAUDE.md`; each pane's `CLAUDE.md` restates it). Merging or moving these panes changes
  no trust boundary. ADR 0090 already puts DE chrome (titlebars) in the compositor outside
  `trusted_ui/`, "It confers no trust": the precedent for option 4.
- Glass is compositor-drawn. STYLE.md: "The compositor draws the material (blur,
  saturation, hairline rim, shadow) under the surface (COMP-02 §9, C-14); a client only
  lays the light tint on top". ADR 0092's adaptive tint is computed in the compositor's
  glass composite from the blurred backdrop. `eclipse_glass_v1` attaches to the
  `wl_surface` (`ec-ui/src/glass.rs`, vendor hook), independent of the client renderer.
  No pane uses a wgpu-only widget (`shader`); `rg` finds none.
- Crash isolation. `ec-hyperion-bar`, `ec-wallpaper`, `ec-toasts`, `ec-desktop` units are
  each `Restart=on-failure`, `RestartSec=2`, `PartOf=graphical-session.target`; the first
  three are `WantedBy=abyss-session.target`, `ec-desktop` has no `[Install]` and is started
  by abyss in `mode "de"` (`packaging/*.service`). A bar panic today costs the bar for 2 s
  and nothing else.
- On-demand panes (`ec-launcher`, `ec-center` spawned by name per ADR 0052; `ec-settings`,
  `ec-policy-viewer`, `ec-secret-prompt` by desktop entry or request) were not running at
  measurement time and are out of scope. `ec-greeter` is hosted by abyss in the greeter
  session (ADR 0087), not the user session. There is no `ec-lock` crate.

## Options
Savings are **estimates** against the table above, system-wide (sum of PSS over all
processes, plus VRAM), unless marked measured.

1. **Status quo.** ~295 MiB PSS, ~118 MiB VRAM, two GPU contexts besides abyss. Zero work,
   full isolation.
2. **tiny-skia for low-motion panes.** Add iced's `tiny-skia` feature; with both renderers
   built, iced picks at runtime by `ICED_BACKEND` (`iced_renderer-0.14.0/src/fallback.rs`),
   so the unit chooses with one `Environment=` line and can flip back. tiny-skia draws on the
   CPU into `wl_shm` (softbuffer): no device, no driver stack, no VRAM.
   - Wallpaper: est. −60 to −70 MiB PSS and −70 MiB VRAM, +16 to +24 MiB anon for shm
     buffers per 1080p output. Draws are rare, so CPU cost is a one-off scale per
     configure. The `NUDGES` workaround becomes unnecessary (upload is synchronous).
   - Bar: est. −50 to −70 MiB PSS, −35 MiB VRAM. The fold and spinner animations
     re-rasterise on the CPU each frame and the compositor uploads each damaged shm buffer;
     cost unknown.
   - Lost: nothing of the glass material (compositor-drawn). Possibly text and image
     filtering quality differs from wgpu; must be screenshot-compared against STYLE.md.
   - Confirm: build with both features, run each pane headless under `ICED_BACKEND` each
     way, compare PSS/VRAM at idle and CPU time over a scripted minute of bar animation.
3. **One `ec-shell` host** for bar, wallpaper, toasts (and desktop in `de`), on-demand panes
   stay separate. Panes become library crates behind cargo features or runtime-selected
   modules; one iced daemon, one wgpu device. Est. −70 to −110 MiB PSS and −30 to −50 MiB
   VRAM (one context and one driver-private set instead of two or three). Cost: the largest
   change here (one `Message`/`App` sum over three programs, shared per-output reconcile),
   one event loop so a slow bar update stalls toasts, one panic takes all of them down
   with them. ADR 0052's dependency rule survives if the host is its own crate depending on
   each pane's lib, but "one package per component" becomes "plus a host package that
   bundles them", and the swap story becomes a feature/config flag rather than a binary on
   `PATH`. Confirm: a throwaway host showing bar + wallpaper, measured as above.
4. **Wallpaper drawn by the compositor.** abyss already owns the config (`wallpaper { path
   … }` in `abyss.kdl`, read by the pane through `get_config`), already has a GPU context,
   and already links `png` (screenshots). Decode on a pool thread and return by channel
   (single-threaded core), upload once, draw under the background layer, outside
   `trusted_ui/` as ADR 0090 does chrome. Est. −100 to −111 MiB PSS, −65 to −75 MiB VRAM,
   +8 to +12 MiB in abyss (one texture). Cost: image decoding moves into the compositor
   process (pure-Rust decoders, user-chosen file; still a parser in a privileged-adjacent
   process); the blur pipeline's wallpaper-surface cases in `ec-abyss-render/src/blur.rs`
   need a compositor-element equivalent; `eclipseos-wallpaper` either retires or stays for
   non-abyss hosts (ADR 0052 requires it to work there). A third-party layer-shell
   wallpaper still works when the compositor's is off. Confirm: prototype, compare abyss
   PSS/VRAM before and after with the pane stopped.
5. **Keep processes, trim each.** Unit `Environment=` only, no code:
   `WGPU_BACKEND=vulkan` (skip the EGL/GL enumeration), `VK_LOADER_DRIVERS_SELECT` to the
   vendor ICD (skip lavapipe/LLVM), `WGPU_POWER_PREF=low` (matters on hybrid laptops, not
   here). Code-level: give the bar's text stack only the bundled faces plus needed fallbacks
   instead of every system font. Present mode is the default `AutoVsync`. Bar and toasts
   set `.antialiasing(true)` (`main.rs`), i.e. MSAA targets for geometry; turning it off
   saves an est. few MiB of VRAM at a visible cost to canvas edges, so it is not proposed;
   wallpaper leaves it at the vendored default (off). Est. −20 to
   −50 MiB system-wide; much of it is clean, shared file pages, so the hard saving is the
   low end. `/dev/nvidiactl` and VRAM stay. Confirm: set the variables on one unit in a
   headless nested session, diff `smaps` per mapping.

## Decision
Proposed, staged, cheapest and most reversible first:
- **Stage 0, measure properly.** A script that sums PSS over every process and reads
  `nvidia-smi` and per-mapping classes, run before and after each stage, plus one reading
  of toasts while a card is visible. Report system-wide PSS and VRAM, not RSS.
- **Stage 1, option 5's environment knobs** in the three pane units. Config only,
  one-line revert.
- **Stage 2, the wallpaper**: option 4 or option 2, by owner decision (Q2). Either removes
  most of a GPU context from the always-on set for a pane that draws one picture.
- **Stage 3, the bar on tiny-skia** only if stage 0's CPU measurement of its animations is
  acceptable and screenshots match STYLE.md; otherwise it stays on wgpu.
- **Option 3 is deferred**, not rejected. After stages 1–3 the always-on set holds at most
  one GPU context besides abyss, which leaves little for a host process to share, and the
  host costs crash isolation and ADR 0052's swap story.

## Consequences
- Stages 1–3 keep every crate, package, binary name and unit; ADR 0052 is untouched.
- Option 4 adds an image decoder and a draw path to abyss and owes a `docs/` update (CONFIG,
  COMPOSITION) and a decision on `eclipseos-wallpaper`'s future.
- Option 2 adds a second renderer to the build and a screenshot comparison per pane that
  uses it; the headless screenshot loop must exercise the renderer the unit selects.
- The measurement script becomes the yardstick for later pane work.

## Open owner decisions
1. Is ADR 0052's one-package-per-component worth more than an estimated 70–110 MiB from a
   shared host? **Keep 0052 strictly** (option 3 stays off the table) **or allow a bundling
   host package**.
2. Wallpaper: **compositor-drawn** (option 4, largest saving, abyss grows) **or a
   tiny-skia client** (option 2, stays a portable layer-shell package).
3. Bar: **tiny-skia allowed if the CPU and screenshot checks pass**, **or wgpu regardless**.
4. If Q1 is "allow": **a bar crash may take toasts and wallpaper down for 2 s**, **or crash
   isolation is a hard requirement**.
5. Target metric: **system-wide PSS + VRAM** **or summed RSS** (the figure first quoted).

## Revisit when
A low-memory target (live ISO, 4–8 GB laptop) shows the always-on set mattering; the
on-demand panes become resident; iced or `iced_layershell` gains a shared-device
multi-program mode; or stage 0 shows the estimates above off by more than half.

Unverified: Vulkan as the backend in use; `libcuda` coming from NVML; toasts' cost while
visible; every figure marked est.
