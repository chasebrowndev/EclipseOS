# topic-gpu-latency: GPU terminal renderers, Wayland frame pacing, latency budgets

Prior-art rule observed: techniques described in prose; no source transcribed. Items not confirmed are marked "unverified".

## Summary

- Terminal latency is dominated by pacing policy (when a frame is started and presented), not GPU speed. foot and kitty win by being event-driven with small bounded delays; Alacritty, WezTerm and Rio use plain wgpu/GL vsync paths with a 2-deep queue.
- On Wayland, `wl_surface.frame` callbacks are the baseline throttle; `wp_presentation` gives measurement; `fifo-v1` and `commit-timing-v1` are staging protocols with patchy compositor support (fifo: Hyprland, Jay, KWin 6.7, Mutter 51, Weston 15; commit-timing: Hyprland, Jay, Mutter 51, Weston 15; none in Sway, niri, COSMIC, labwc, wayfire, river per wayland.app table). Both must be optional and probed at bind.
- wgpu on Wayland: `Immediate` is unsupported, `Mailbox` only on Vulkan, `Fifo` universal. Surface config has `desired_maximum_frame_latency` (default 2; 1 cuts latency at the cost of serialising CPU and GPU). wgpu exposes no buffer-age and no present-with-damage in its public API (unverified for trunk; none found in the changelog). Version note: the wgpu changelog shows v29 (2026-03) and v30 (2026-07, v30.0.1 2026-08) as current, so "v27 era" facts are stale; pin by checking at implementation time.
- Recommended design: event-driven render thread, damage-tracked cell upload (not full redraw), Mailbox where offered else Fifo with frame latency 1, frame-callback gating, presentation-feedback driven telemetry, optional fifo/commit-timing, and a built-in latency probe so budgets are testable in CI.

## Architecture (how the references are built)

- foot (CPU renderer, reference for pacing): after client data arrives it arms a lower timer and re-arms on more data, capped by an upper timer, so non-atomic app updates (erase then redraw) do not show intermediate frames. It issues a `wl_surface.frame` request per rendered frame and refuses to start another until the callback fires (`frame_callback` in render.c; tracked via window frame_callback pointer). It binds `wp_presentation` and keeps statistics of how many refresh cycles each frame took (zero/one/two buckets) for debug. Config: `tweak.delayed-render-lower` / `delayed-render-upper` (nanoseconds; docs foot.ini(5)). Defaults recalled as 500 us lower and 16.6 ms upper (unverified, check the scd).
- kitty (OpenGL, own main loop): `input_delay` (default 3 ms) batches reads from the child, `repaint_delay` (default 10 ms) limits repaint rate, `sync_to_monitor` toggles vsync. The documented low-latency profile is `input_delay 0`, `repaint_delay 2`, `sync_to_monitor no`, `wayland_enable_ime no` (docs/performance.rst). Kitty ships a throughput benchmark kitten (`kitten __benchmark__`).
- Alacritty (OpenGL via glutin, damage-aware): vsync disabled on the GL surface; a two-frame damage history (`DamageTracker.frames[2]`, display/damage.rs) is combined and passed to `swap_buffers_with_damage` (display/mod.rs, `draw`), because with double buffering the back buffer is two frames stale. Damage is line-granular from the terminal core (`TermDamage`, `LineDamageBounds`). A debug option highlights damage rects.
- Ghostty (Metal/OpenGL, renderer thread): dedicated renderer thread with an async "draw now" wakeup and a timer-driven cursor blink; on macOS the vsync comes from a display link that triggers draws, otherwise the thread draws on demand (renderer/Thread.zig). Cell data is built into GPU buffers and uploaded; a generic renderer abstracts over API backends (renderer/generic.zig). Terminal state is shared with the IO thread under a mutex; the render thread snapshots rows it needs (design takeaway: minimise lock hold time).
- WezTerm (wgpu or GL via front_end): wgpu surface configured with `PresentMode::Fifo` and `desired_maximum_frame_latency: 2` (wezterm-gui/src/termwindow/webgpu.rs). Config `max_fps` caps animation; full-frame quad rebuild per paint with a texture-atlas glyph cache.
- Rio (sugarloaf, wgpu): same Fifo and latency-2 defaults (sugarloaf/src/context/webgpu.rs); also a native Vulkan context path (context/vulkan.rs), indicating wgpu overhead concerns.
- Windows Terminal AtlasEngine (D3D11/D2D with DirectWrite shaping): frame-latency waitable swap chain; per-row dirty tracking where scroll is handled by rotating row pointers and moving bitmap planes, then presenting with dirty rects and scroll rect via `Present1`; falls back to full present after settings changes (AtlasEngine.cpp `StartPaint`/`EndPaint`). Glyph atlas is a GPU texture with shader-side cell composition (only partially verified: fetch truncated).

## Feature inventory

Pacing: coalescing timers (foot), input/repaint delay (kitty), frame-callback gating, presentation feedback statistics, fifo barriers, commit timestamps, waitable swap chain (WT).
Damage: line-level damage from VT core, per-buffer damage history (needs buffer age), scroll rect optimisation, damage-rect debug overlay.
Present: Mailbox vs Fifo vs Immediate; frame-latency depth; vsync toggle; adaptive redraw-on-demand with zero idle wakeups.
Measurement: typometer (software), hardware photodiode/camera rigs, internal key-to-capture (Dan Luu), vtebench (throughput), kitty benchmark kitten, foot presentation stats.

## Control surface

- kitty keys: `input_delay`, `repaint_delay`, `sync_to_monitor`. foot: `tweak.delayed-render-lower/upper`, `tweak.render-timer` (log/overlay frame time). wezterm: `max_fps`, `animation_fps`, `front_end`, `webgpu_power_preference`. Alacritty: `debug.highlight_damage`, `debug.render_timer`.
- Proposed Apotheosis KDL (names are proposals): `render { present-mode "auto|mailbox|fifo"; frame-latency 1; coalesce-lower "0.5ms"; coalesce-upper "8ms"; use-fifo-protocol #true; use-commit-timing #false; damage-overlay #false; frame-stats-overlay #false }`. Actions: `toggle-frame-stats`, `toggle-damage-overlay`, `dump-frame-timings`. Control socket: `render.stats` returning percentile frame/key-to-present times, `render.probe-latency`.

## Performance (numbers and method)

- Dan Luu "Terminal latency" (2017, MacBook Pro 2014, macOS, internal keypress to screen capture, 10k samples): idle median / 99.9th ms: alacritty 31/36, terminal.app 6/25, st 25/63, iterm2 44/60, hyper 32/49, emacs-eshell 5/17. Loaded 99.9th: alacritty 56, st 111. Lesson: tails matter; throughput does not predict latency. Old hardware, partial path (https://danluu.com/term-latency/).
- kitty issue 2701: kitty author reports typometer 7 ms (kitty) vs 30 ms (Alacritty) (2020); thume.ca hardware test ~35 ms kitty vs ~50 ms Alacritty on macOS (second-hand via issue page). The Linux typometer table in the linked comment could not be retrieved: unverified.
- kitty documents that its default input_delay (3 ms) is included in its numbers.
- Display floor: one 60 Hz refresh is 16.7 ms; 144 Hz is 6.9 ms. With Fifo and 2-deep queue the worst-case added latency is about 2 refreshes.
- No published wgpu Wayland latency numbers found; Apotheosis must measure.

## What Apotheosis should take

1. Event-driven render loop with zero idle wakeups; cursor blink via timer only.
2. foot-style two-timer coalescing, but defaults tiny (lower 0.5 ms, upper 8 ms) and bypassed for keystroke echo (input-triggered output renders immediately).
3. Frame-callback gating: at most one frame in flight per surface.
4. Present mode policy: Mailbox if reported (Vulkan Wayland), else Fifo; `desired_maximum_frame_latency` 1; user override.
5. Damage tracking from the VT core at line level, GPU buffer update of only changed rows; keep a 2-3 deep damage history if buffer age ever becomes available, otherwise rely on a retained offscreen grid texture and compose glass UI on top.
6. `wp_presentation` feedback to compute real key-to-photon (commit-to-presented) and surface drops; expose in stats overlay.
7. Optional fifo-v1 when advertised (animations/smooth scroll), commit-timing for smooth-scroll and video frame pacing; capability-probed with fallback.
8. Built-in latency probe (synthetic key via control socket to presented timestamp) and CI budget gate; record p50/p99/p99.9.
9. Separate render thread with short-lived snapshot of dirty rows to avoid holding the VT lock.
10. Abyss integration: on Abyss, advertise/consume direct-scanout hints (`zero_copy` flag) and request unredirected presentation for fullscreen terminal.

## What it should avoid

- Fifo with latency 2 as an unexamined default (WezTerm/Rio behaviour).
- Fixed repaint timers that add milliseconds to every keystroke (kitty 10 ms default repaint_delay).
- Full-grid rebuild per frame; unconditional redraw on every wl_callback.
- Relying on `Immediate` (unsupported on Wayland in wgpu) or on any single staging protocol.
- Assuming wgpu exposes buffer age/present damage; dropping to `wgpu::hal` Vulkan (`set_next_present_chain`, `VK_KHR_incremental_present`, `VK_EXT_present_timing`) is possible but costs portability: unverified.
- Treating median numbers as the budget; tails drive perceived lag.

## Sources

- https://wayland.app/protocols/fifo-v1 ; https://wayland.app/protocols/commit-timing-v1 ; https://wayland.app/protocols/presentation-time
- https://docs.rs/wgpu/latest/wgpu/enum.PresentMode.html
- https://raw.githubusercontent.com/gfx-rs/wgpu/trunk/CHANGELOG.md (desired_maximum_frame_latency introduced with default 2; dmabuf import `texture_from_dmabuf_fd` in v30; `set_next_present_chain` and queue-family ownership transfer unreleased; Vulkan external-memory feature flags removed in unreleased)
- https://danluu.com/term-latency/ ; https://github.com/kovidgoyal/kitty/issues/2701 ; https://thume.ca/2020/05/20/making-a-latency-tester/ (cited by kitty docs) ; https://pavelfatin.com/typometer/
- Local clones (/home/risk/.cache/apotheosis-prior-art): kitty/docs/performance.rst; kitty/kitty/options/definition.py (input_delay, repaint_delay, sync_to_monitor); foot/render.c (frame_callback, presentation_context, delayed render timers); foot/doc/foot.ini.5.scd (delayed-render-lower/upper); alacritty/alacritty/src/display/damage.rs and display/mod.rs (DamageTracker, swap_buffers_with_damage); ghostty/src/renderer/Thread.zig and generic.zig; wezterm/wezterm-gui/src/termwindow/webgpu.rs; rio/sugarloaf/src/context/webgpu.rs
- https://github.com/microsoft/terminal/blob/main/src/renderer/atlas/AtlasEngine.cpp (partial)
