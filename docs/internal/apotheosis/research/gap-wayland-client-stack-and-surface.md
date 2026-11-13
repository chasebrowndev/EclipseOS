# Gap dossier: Wayland client stack and surface mechanics

Subject: gap-wayland-client-stack-and-surface. Written 2026-11-12. Prior-art rule observed:
techniques are described in prose; only interface facts (protocol names, request names, config keys) are quoted.
Source clones live under `/home/risk/.cache/apotheosis-prior-art/` (foot, sctk, wayland-rs, wezterm, rio, wgpu).
Versions inspected: wayland-client 0.31.14, wayland-backend 0.3.15, wayland-sys 0.31.11, wayland-protocols 0.32.13,
smithay-client-toolkit (sctk) 0.21.1, wgpu 30.0.0, wayland-protocols (system) 1.50, libwayland 1.26.0.

## 1. Summary

Decisions this dossier supports (each is a row in the structured output):

1. **Use `wayland-client` with the `system` backend (libwayland through `wayland-sys`), not the pure-Rust backend.**
   This is forced, not a preference. wgpu's Vulkan backend creates its surface with `vkCreateWaylandSurfaceKHR`, which
   needs a real `wl_display*` and `wl_surface*` that libwayland-client owns. The pure-Rust backend has no such pointer, and
   `wayland-backend` only implements `HasDisplayHandle` when `client_system` and `rwh_06` are both on (source: wayland-backend/src/lib.rs
   "raw-window-handle integration", sys/mod.rs). The system backend is exactly the "only libwayland" allowance in the fixed decisions.
2. **Link libwayland-client by `dlopen` (the `wayland-backend/dlopen` feature), not at link time.** Rio's window layer and wezterm do the
   same family of thing. `wayland-sys` with `dlopen` skips `pkg-config` and loads `libwayland-client.so` at run time through `dlib`
   (which uses `libloading`, ISC). Benefits: no C toolchain or `-dev` package at build time, a clean error and a fallback (X11 is out of scope, so a
   clear "no Wayland" message) at run time, and one fewer link-time system dependency. Cost: a handful of function-pointer calls per request;
   unmeasurable here. The build then compiles no C at all. Caveat: `wayland-scanner` is a proc macro that parses protocol XML at build time. It is Rust, so it is allowed. The XML comes
   from the `wayland-protocols*` crates, not from a system package.
3. **Do not adopt smithay-client-toolkit as the architecture; reuse selected parts or copy the ideas.** sctk 0.21.1 is MIT and pulls only allowed
   crates, but it is built around a `Dispatch`-delegate trait web that couples every protocol to one state type, its xkb binding links
   `libxkbcommon` via `pkg-config` (allowed), and it has **no fractional-scale or viewporter module** (verified by grep: zero hits in `src/`).
   Both are on the critical path for a crisp wgpu grid. sctk does have `background_effect.rs`, `presentation_time.rs`, `xdg::window` with decoration modes, `dmabuf.rs`, and a
   `fallback_frame` CSD that draws with subsurfaces. Recommendation: depend on `wayland-client` + `wayland-protocols` directly, write a thin protocol-binding layer of our own (about 15 globals), and optionally
   take `calloop-wayland-source` or sctk's keyboard-repeat handling as a reference. Revisit full sctk adoption only if the thin layer exceeds roughly 3 kLOC.
4. **Hand-rolling the wire protocol (no wayland-client) is not worth it.** It would remove `wayland-sys`, but the Vulkan WSI in Mesa needs a libwayland
   `wl_display` anyway, so libwayland stays loaded. We would re-implement object maps, fd passing and proxy wrappers for no gain.
5. **Event loop: calloop 0.14 (MIT) owning the pty fds, the Wayland fd, the control socket, timers, and signals; one thread.** This matches the existing
   Abyss invariant (single-threaded calloop core) and Rio's window layer. Foot's epoll `fdm` and wezterm's mio loop are the other two proven shapes and are compared below. The
   Wayland fd must be read with the `prepare_read` / `read` / `dispatch_pending` protocol (what `calloop-wayland-source` does). A flush before every poll is mandatory.
6. **Frame pacing: wl_surface frame callbacks as the gate, presentation-time feedback as a measurement (and for pacing hints), `wp_fifo_v1` / `wp_commit_timing_v1` as optional upgrades, `wp_tearing_control_v1` for an opt-in low-latency mode.**
   Foot uses frame callbacks for gating and presentation-time only for opt-in statistics. wgpu hides the swapchain, so we must verify how the driver's WSI throttles (unverified for Mesa below).
7. **Scaling: `wp_fractional_scale_v1` + `wp_viewporter`, with the buffer scale fixed at 1 and the viewport destination set to round(px / scale).**
   Foot does exactly this. Wezterm only supports integer scale (blurry on fractional outputs). Rio's winit fork has `wp_fractional_scaling.rs` and `wp_viewporter.rs`.
8. **CSD: single wgpu surface, titlebar and tabs drawn by us inside the main surface; negotiate `zxdg_decoration_manager_v1` and fall back to our own frame; shadow and resize margin via window geometry + input region.**
   Do not copy the foot / sctk approach of eight or more subsurfaces for borders. That design exists because they draw with wl_shm; with a GPU surface a single transparent-margin surface
   is cheaper and lets blur, shadow and rounded corners be one render pass.
9. **Glass on other compositors: `ext_background_effect_manager_v1` first, `org_kde_kwin_blur` second (KDE), else no blur and a documented opaque-tint fallback.** Foot already ships the ext path, wezterm has both ext and KDE, Rio has both managers.
   On Abyss the same protocol should be implemented by the compositor, so there is one client code path.

Note on the brief: Rio does not maintain an sctk fork. `rio-window` is Rio's own fork of **winit** (header: "The winit contributors") that depends on crates.io sctk 0.19.2 + `sctk-adwaita` 0.10. Findings below cite that.

## 2. Architecture: how each stack is built

### 2.1 wayland-rs crates

| Crate | Version | Licence | Role |
|---|---|---|---|
| wayland-sys | 0.31.11 | MIT | FFI to libwayland-client/server/cursor/egl. `build.rs` runs `pkg-config` for `wayland-client` unless feature `dlopen` is on (then it links nothing). |
| wayland-backend | 0.3.15 | MIT | Two interchangeable implementations behind one API: `rs` (pure Rust wire, socket, map) and `sys` (libwayland). `client_system` selects `sys`. |
| wayland-client | 0.31.14 | MIT | Typed proxies, `EventQueue<State>`, `Dispatch` trait, `Connection`, `GlobalList`. Feature `system` forwards to `wayland-backend/client_system`; `dlopen` forwards likewise. |
| wayland-protocols (+ `-wlr`, `-misc`, `-plasma`, `-experimental`) | 0.32.13 / 0.3.12 | MIT | Generated bindings, `client` feature. Staging and unstable are feature-gated. |
| wayland-scanner | 0.31 | MIT | Proc macro that generates bindings from XML at build time. |
| wayland-cursor | 0.31.14 | MIT | Cursor theme loading via wl_shm. Alternative: `wp_cursor_shape_v1` which needs no theme code. |
| dlib / libloading | 0.5.3 / 0.9.0 | MIT / ISC | The dlopen mechanism. |

Key behaviours:

- **Feature unification caveat** (wayland-backend docs): if any crate in the dependency graph enables `client_system`, every wayland-client user in the graph silently switches to the sys backend. This means a transitive dependency can flip us. Pin it explicitly and add a CI assertion that `wayland-backend` resolves with `client_system` and `dlopen`.
- **Pointer access** (wayland-backend/src/sys/client_impl): `Backend::display_ptr()`, `ObjectId::as_ptr()` and `Backend::from_foreign_display()` exist only in the sys backend. They are how a `wl_surface*` is obtained for Vulkan.
- **Polling model**: `Connection::prepare_read()` returns a guard; `guard.read()` reads from the socket; `EventQueue::dispatch_pending()` runs handlers; `flush()` writes. `Connection::roundtrip` and `EventQueue::blocking_dispatch` exist for startup. `ReadEventsGuard::connection_fd()` gives the fd to register in a poller (wezterm uses this).
- **Pure-Rust backend** (`rs`): MIT, no C, would satisfy "no libwayland" if we did not need Vulkan. It does not implement `HasDisplayHandle`, and a pointer-free display cannot be given to a driver. Dead end for this project.

### 2.2 smithay-client-toolkit 0.21.1 (MIT)

Modules seen in `src/`: activation, background_effect, compositor, data_device_manager, dmabuf, foreign_toplevel_list, output, presentation_time, primary_selection, registry, seat (keyboard, pointer, touch, input_method v1 and v3, pointer_constraints, relative_pointer), session_lock, shell (xdg window, popup, dialog, `fallback_frame`, layer), shm, subcompositor.
Dependencies: wayland-client, wayland-protocols (+ wlr, misc, experimental), wayland-csd-frame (MIT, the CSD frame trait), memmap2, rustix, xkbcommon (optional, default on, `build.rs` calls `pkg-config` for `xkbcommon`), xkeysym, calloop + calloop-wayland-source (default on, optional), cursor-icon, bitflags, thiserror.
Window decoration handling (shell/xdg/window): `WindowDecorations` = ServerDefault / RequestServer / RequestClient / ClientOnly / None-ish; resulting `DecorationMode` is Client when the compositor has no decoration manager or says client-side. A `FallbackFrame` implements `csd_frame::DecorationsFrame` with a title subsurface, border subsurfaces and min/max/close buttons drawn into wl_shm buffers.
**Absent**: fractional-scale, viewporter, tearing-control, fifo, commit-timing helpers; no wgpu or raw-window-handle glue (that lives in winit).
Maturity: it is the base of winit's Wayland backend, COSMIC, and many apps; but its API is churn-prone (0.19 to 0.21 in about 18 months, the `Dispatch2` rewrite is visible in `dispatch2.rs`).

### 2.3 Foot (`wayland.c`, `render.c`, `fdm.c`; MIT; C, libwayland direct)

- **Event loop**: own `fdm` (epoll wrapper, 496 lines). `fdm_wayl()` in wayland.c: on EPOLLIN calls `wl_display_read_events`, `wl_display_dispatch_pending`, then loops `wl_display_prepare_read` until it succeeds. On EPOLLHUP it deliberately does **not** call `wl_display_cancel_read` (comment cites foot issue 651: later roundtrip hangs). `wayl_flush()` loops on `wl_display_flush`, handling EINTR, and on EAGAIN polls POLLOUT; on EPIPE it reads events once to surface the protocol error. This flush-with-backpressure routine is the technique to copy.
- **Globals bound** (wayland.c registry handler): xdg_wm_base, zxdg_decoration_manager, wp_viewporter, wp_fractional_scale_manager, wp_presentation (+ `clock_id` event used to pick the clock for `clock_gettime`), wp_single_pixel_buffer_manager (used for solid overlays with a viewport), xdg_activation, ext_background_effect_manager_v1 (`HAVE_EXT_BACKGROUND_EFFECT` compile flag), primary selection, data device, text-input, cursor-shape, subcompositor, xdg-toplevel-icon, ext-foreign-toplevel, and others.
- **Decoration negotiation** (`xdg_toplevel_decoration_configure`): config `csd.preferred` (server | client | none). The handler maps the compositor reply to `CSD_YES` or `CSD_NO` and applies it on the next xdg configure ack. If there is no decoration manager, the window falls back to CSD (`wayl_win_init` around the `csd.preferred` branch).
- **CSD structure** (`csd_instantiate`): eight subsurfaces: title, left, right, top, bottom, plus minimize, maximize, close that are children of the title surface. Each is `wl_subsurface` in sync/desync mode with `quirk_weston_subsurface_desync_on/off` workarounds in render.c. Borders double as invisible resize handles and shadow.
- **Opaque and input regions**: `wayl_win_alpha_changed()` sets the opaque region to the full surface when alpha is 1.0 or fullscreen, clears it otherwise; when blur is enabled and the manager reports the BLUR capability, it sets the blur region to a whole-surface region only while translucent, and clears it when opaque. A one-time warning if blur is requested but unsupported. `wayl_win_subsurface_new_with_custom_parent` sets an **empty input region** on the helper surface so pointer events fall through to the main surface (wayland.c near line 2757).
- **Scaling** (`surface_scale_explicit_width_height`): if fractional scaling is active, buffer scale = 1 and `wp_viewport_set_destination(round(px/scale))`. The code asserts that the pixel size is an exact round-trip (`round(scale*round(px/scale)) == px`), i.e. the application chooses buffer sizes that are exactly representable. Otherwise integer `wl_surface.set_buffer_scale`. `preferred_buffer_scale` (wl_surface v6) is used when the compositor sends it. Fractional preferred scale arrives as a numerator over 120.
- **Frame pacing** (`render.c`): one `frame_callback` per window. While a callback is outstanding, new refresh requests are only recorded in `render.pending.*` and replayed from the callback. Output delay timers: `tweak.delayed-render-lower` default 500000 ns and `delayed-render-upper` default half a 60 Hz frame (config.c defaults); a burst of pty output is coalesced until input pauses for the lower bound or the upper bound elapses. Title updates are rate-limited to 8.333 ms by a timerfd. Synchronized updates (DEC mode 2026) suppress grid refresh but never CSD, search or url overlays.
- **Presentation-time**: with config `presentation_timings` and a bound `wp_presentation`, foot records the commit time in the compositor clock, requests feedback per commit and tallies how many frames were shown after zero, one, two refreshes (`presentation_statistics` zero/one/two, logged at exit). It is diagnostic, not a feedback controller.
- **Rendering**: wl_shm software (pixman) buffers, not GPU; irrelevant for the surface path except as a frame-pacing reference.

### 2.4 Wezterm (`window/src/os/wayland/`, MIT)

- Cargo features: `smithay-client-toolkit` (default-features off), `wayland-backend` with `client_system` + `rwh_06`, `wayland-egl`, `wayland-protocols-plasma` (for `org_kde_kwin_blur`), `mio` for the loop, and `xkbcommon`.
- **Event loop** (`connection.rs: run_message_loop_impl`): mio `Poll` with two tokens: the Wayland fd (obtained from `prepare_read().connection_fd()`) and an internal spawn queue fd. Each iteration: run the spawn queue, `dispatch_pending`, `flush`, `poll`, then `prepare_read` + `read` for the Wayland token. This is a hand-rolled version of what calloop-wayland-source does and is also the shape where a pty fd would be added.
- **Decorations** (`window.rs`): uses sctk `FallbackFrame<WaylandState>`, `request_decoration_mode`; initial decorations depend on config `window_decorations` (title bit present or not). Frame is redrawn on `refresh_decorations`.
- **Blur** (`update_window_background_blur`): if `ext_background_effect_manager` is bound, create a per-surface effect object on demand, honour the capability flag (`ext_background_effect_can_blur`), destroy it when blur is disabled; the KDE path uses `org_kde_kwin_blur`. Config keys `wayland_window_background_blur` and `kde_window_background_blur`.
- **Scaling**: integer only. Pixel dimensions are floored to a multiple of the integer buffer scale (comment "Align pixel dimensions to the integer buffer scale factor"). No `wp_fractional_scale` or viewporter usage was found in `window.rs`. This is a negative example.
- **Frame callback**: a `frame_callback: Option<WlCallback>`; repaint requests are deferred while it is set and a NeedRepaint is emitted from the callback (window.rs ~634).
- **Surface handles**: implements raw-window-handle `HasWindowHandle` by building a `WaylandWindowHandle` from the proxy pointer, and `HasDisplayHandle` through wayland-backend's `rwh_06` impl; rendering is wgpu or EGL.

### 2.5 Rio (`rio-window`, MIT; own winit fork)

- Cargo: features `wayland`, `wayland-dlopen` (= `wayland-backend/dlopen`), `wayland-csd-adwaita` (sctk-adwaita with `ab_glyph`); sctk 0.19.2 with calloop; `calloop` 0.13; wgpu 30.0.2; raw-window-handle 0.6.2. Default features of the `rioterm` crate include `wayland-dlopen`, confirming the dlopen route works in a shipped terminal.
- **Event loop** (`event_loop/mod.rs`): `calloop::EventLoop<WinitState>`; the Wayland queue is wrapped in `calloop_wayland_source::WaylandSource` registered through a `Dispatcher`; user events through `calloop::channel`; wake-ups through `calloop::ping`. A code comment says flush should ideally be the last action before polling and is left to calloop.
- **Handles** (`event_loop/mod.rs`, `window/mod.rs`): builds `WaylandDisplayHandle` and `WaylandWindowHandle` from raw pointers for raw-window-handle; sugarloaf (`sugarloaf/src/context/webgpu.rs`) creates the wgpu surface from the window, enumerates `surface_caps.alpha_modes` and prefers `PreMultiplied` / `PostMultiplied` for transparent windows, clearing with a premultiplied colour accordingly.
- **Features present in the fork**: `types/wp_fractional_scaling.rs`, `types/wp_viewporter.rs`, `types/ext_background_effect.rs`, `types/kwin_blur.rs`. CSD is `sctk_adwaita::AdwaitaFrame` created lazily when the configure says Client mode and a subcompositor exists; a `csd_fails` flag permanently disables retry after the first failure (window/state.rs). Decoration mode is taken from the configure, so xdg-decoration is delegated to sctk.
- The licence is MIT; its fork of winit is Apache-2.0 in origin (winit); verify per file before copying anything (we copy ideas only).

### 2.6 wgpu (MIT OR Apache-2.0, 30.0.0) as the consumer

- `wgpu-hal/src/vulkan/instance.rs`: enables `VK_KHR_wayland_surface` when available (extension list near line 335); `create_surface_from_wayland(display, surface)` builds `VkWaylandSurfaceCreateInfoKHR` with the two pointers taken from `RawDisplayHandle::Wayland` and `RawWindowHandle::Wayland` (line ~1109).
- Therefore the contract is: both pointers must be live libwayland objects for the whole life of the swapchain; the surface must outlive the `wgpu::Surface`; the window handle must not be destroyed before it. We must own that ordering (drop the swapchain, then the role objects, then the wl_surface).
- Present modes: Fifo and FifoRelaxed go through the same branch (swapchain/native.rs ~289). Presentation behaviour on Wayland (who waits for the frame callback) is implemented by the driver WSI, not wgpu. (Mesa implements FIFO throttling with its own event queue and a frame callback on a proxy wrapper of our display; this is documented behaviour I could not verify against Mesa source in this session: **unverified**.)
- Alpha: the surface's `alpha_modes` decide whether per-pixel transparency is possible. Glass needs `PreMultiplied` (or `PostMultiplied`); we must read the capability, never assume. (Rio does this; Mesa advertising PRE_MULTIPLIED on Wayland: **unverified** here.)

### 2.7 Hand-rolled client

Writing wire encoding, fd passing (SCM_RIGHTS), an object ID allocator and event decoding is a few thousand lines plus the generated protocol code; the generator would be our own XML-to-Rust build step (feasible since we want KDL and no C, but `wayland-scanner` already is that). The decisive point is the Vulkan WSI: Mesa needs libwayland's `wl_display` and `wl_proxy` machinery in-process. A hand-rolled client would have to either hand Mesa a libwayland connection created from our fd (`wl_display_connect_to_fd`) and then run two clients on one socket, or give up Vulkan. The `from_foreign_display` path in wayland-backend shows the supported way to share. Verdict: no.

## 3. Feature inventory (grouped)

### 3.1 Library and linkage
- wayland-client sys backend + dlopen; pure-Rust backend rejected (above).
- wayland-protocols crates for typed XML; protocol XML for experimental/staging comes from crates, not system packages. System `wayland-protocols 1.50-1` here already contains: alpha-modifier, color-management, commit-timing, content-type, cursor-shape, ext-background-effect, ext-data-control, ext-idle-notify, ext-image-copy-capture, ext-session-lock, ext-workspace, fifo, fractional-scale, linux-drm-syncobj, pointer-warp, security-context, single-pixel-buffer, tearing-control, xdg-activation, xdg-dialog, xdg-session-management, xdg-system-bell, xdg-toplevel-drag, xdg-toplevel-icon, xdg-toplevel-tag. (`ls /usr/share/wayland-protocols/staging` on the dev host.)
- Licences: all of wayland-rs MIT; sctk MIT; calloop MIT; raw-window-handle MIT OR Apache-2.0 OR Zlib; xkbcommon crate MIT; xkeysym MIT OR Apache-2.0 OR Zlib; libloading ISC; mio MIT; polling Apache-2.0 OR MIT; ash MIT OR Apache-2.0; wgpu MIT OR Apache-2.0; kbvm MIT OR Apache-2.0. All inside the allowed list (crates.io API, queried 2026-11-12). The `wayland-protocols` XML itself carries MIT licence text per file (copyright holders vary); the generated crates are MIT.
- The only non-Rust code loaded at run time: libwayland-client, libxkbcommon (or kbvm to remove it; kbvm is a pure-Rust xkb implementation, maturity **unverified**), libvulkan + driver.

### 3.2 Surface and window management
- xdg_wm_base ping/pong; xdg_surface configure/ack sequence; initial commit-without-buffer then first configure.
- xdg_toplevel states relevant to CSD: maximized, fullscreen, resizing, tiled_left/right/top/bottom (since 2), suspended (since 6), constrained_* (since 7). `wm_capabilities` event (since 5) tells which of window_menu, maximize, fullscreen, minimize are available. `configure_bounds` (since 4). Requests: move, resize (edge), show_window_menu, set_minimized/maximized/fullscreen, set_min_size/max_size.
- CSD must drop shadows and rounded corners when tiled or maximized (tiled_* states) and when fullscreen.
- Window geometry: `xdg_surface.set_window_geometry` excludes a transparent shadow margin from the window's logical extent so snapping and tiling treat the visible window as the frame.

### 3.3 Decoration negotiation
- `zxdg_decoration_manager_v1.get_toplevel_decoration`, `set_mode(client|server)`, `unset_mode`, `configure(mode)` event. The reply is authoritative; the client must treat the mode as undefined until the first configure after the request. Foot, sctk and Rio all treat the compositor's answer as final and re-layout on mode change.
- If the global is absent, GNOME/Mutter style compositors expect CSD; wlroots-based and KDE have SSD. Abyss as our own compositor can promise `client` mode for Apotheosis (trusted glass chrome drawn by the app) or `server` mode when the user asks for the native frame.
- Policy: honour a config key (`decorations "auto|client|server|none"`) the way foot's `csd.preferred` does; `none` means a borderless window.

### 3.4 Input region, opaque region, shadows
- `wl_surface.set_opaque_region`: empty region lets the compositor treat the whole buffer as translucent (required for glass; mis-setting it makes the compositor skip blending and renders black or stale). Foot sets it to the full surface only when alpha is 1 or the window is fullscreen. Our wgpu surface must set it per frame state: opaque region = union of rectangles known to be fully opaque (e.g. title bar when not translucent), else empty.
- `wl_surface.set_input_region`: restrict to the visible window plus a resize margin so the transparent shadow outside the geometry does not eat clicks meant for windows beneath; helper surfaces (foot) use an empty input region.
- Shadows: either (a) transparent margin inside the main buffer, with window geometry set to the inner rectangle, resizing by `xdg_toplevel.resize` driven by our own hit test in the margin; or (b) separate subsurface (foot/sctk). (a) is simpler for a GPU renderer and costs margin pixels of fill; (b) lets shadow be omitted when tiled with no change to the main buffer size. Recommendation (a); margin pixel cost at 12 px shadow on a 1920x1080 window is about 3.5 percent extra fill (computed, not measured).
- Blur region is a `wl_region`, therefore a union of axis-aligned rectangles. Rounded corners cannot be exact: the blur region will be slightly larger than the rounded glass at corners (stair-step approximation or accept a few-pixel overhang). This is an interface fact from the XML ("The blur region is specified in the surface-local coordinates, and clipped by the compositor to the surface size"; "The blur algorithm is subject to compositor policies").

### 3.5 Blur, glass and fallback
- `ext_background_effect_manager_v1`: capabilities event sent at bind and on change (only BLUR is defined today); `get_background_effect(surface)` (error `background_effect_exists` if already bound); `ext_background_effect_surface_v1.set_blur_region(region|null)`, double-buffered, applied on commit; becomes inert when the surface is destroyed. Effects disappear when the capability goes away.
- Compositor support: reported for KWin (Plasma 6.7), Mutter (GNOME 51 per Phoronix, 2026-07-04) and niri 26.04; Hyprland and Sway not confirmed. These come from secondary news sources (Phoronix, Neowin tag page); treat as **unverified** until checked against release notes. Dev host protocol list proves the XML ships in wayland-protocols 1.50.
- Fallback ladder: (1) ext-background-effect with capability BLUR; (2) `org_kde_kwin_blur` from `wayland-protocols-plasma` (wezterm and Rio do this); (3) no blur: raise tint alpha automatically so text stays legible, or switch to opaque (config `blur-fallback "opaque|tint|none"`). Foot only warns once.
- Trigger rule from foot: enable blur only while the surface is actually translucent; clear the region when opaque (fullscreen) to save compositor work.
- Capability changes at run time must be handled by re-evaluating the ladder (wezterm tracks `ext_background_effect_can_blur`).

### 3.6 Scaling
- `wp_fractional_scale_v1.preferred_scale` (numerator over 120). `wp_viewporter`: `set_destination` (logical size) with `set_buffer_scale(1)`. `wl_surface.preferred_buffer_scale` (v6) for the integer case.
- Pitfall (foot asserts it): buffer pixel size must equal round(scale * round(px/scale)), otherwise the viewport resamples. Choose logical size first, then buffer = round(logical * scale), and only then compute the cell grid so cell metrics are integer physical pixels.
- Subsurface content (tab strip overlays) needs its own viewport if it is a separate surface; one more reason for the single-surface CSD.
- Scale changes per output: fractional scale arrives per surface (not per output), so no `wl_output.enter` tracking is needed for DPI.

### 3.7 Frame pacing and presentation
- `wl_surface.frame` callback: the baseline throttle; coalesce all damage until `done`; keep a pending flag and re-render from the callback (foot, wezterm). Never commit twice per callback unless latency mode permits.
- `wp_presentation`: `clock_id` event (use that clock for local timestamps), `feedback(surface)` per commit returns `sync_output`, then `presented` (tv_sec hi/lo, tv_nsec, refresh in nanoseconds, seq hi/lo, flags) or `discarded`. Flags: vsync, hw_clock, hw_completion, zero_copy. refresh = 0 means unknown (variable refresh).
- Use: (a) measure commit-to-photon and missed deadlines; (b) estimate refresh period for animation timing (glass transitions); (c) drive predictive scheduling: start rendering at (next_vblank - estimated_render_time - margin). Foot only counts frames. sctk wraps this in `presentation_time.rs` (`PresentationTimeState::feedback`, handler `presented` with `PresentTime`, `refresh`, `seq`, `flags`).
- `wp_fifo_v1` (`set_barrier`, `wait_barrier`) provides FIFO commit semantics at the compositor; `wp_commit_timing_v1` (`get_timer`, `set_timestamp`) allows target presentation times. Both are staging protocols that Vulkan WSI can use; whether Mesa uses them automatically for FIFO present is **unverified**. For a wgpu client we mostly benefit passively.
- `wp_tearing_control_v1`: `set_presentation_hint(vsync|async)`; opt-in for lowest latency (requires `PresentMode::Immediate` or Mailbox support in the driver and compositor honouring the hint). Do not default to it.
- VT output bursts (cat of a large file): foot's delayed-render window (0.5 ms lower bound, half a frame upper bound) bounds latency while coalescing. We should adopt the same two-timer coalescer in the core loop, keyed off the measured refresh period rather than a fixed 60 Hz.

### 3.7a Wayland-specific client globals the grid and UI need
xdg_activation (focus and urgency), cursor_shape (avoid cursor theme code; wayland-cursor remains the fallback), pointer-constraints / relative-pointer (mouse reporting while captured), text_input v3 (IME; the sctk `input_method` modules are for compositor-side use, text-input is the client side), primary selection, data device and ext-data-control, content-type, xdg-toplevel-icon, xdg-system-bell, xdg-toplevel-drag (dragging a tab out into a new window), security-context (sandbox flatpak). Row list covers the ones that affect the UI rows; the rest belong to INTEGRATION-10.

### 3.8 Event loop comparison

| Design | Used by | Strengths | Weaknesses |
|---|---|---|---|
| calloop (MIT, 0.14.5) + calloop-wayland-source (0.4.1) | Rio window layer, sctk, Abyss core | Typed sources (`Generic` fd, `Timer`, `channel`, `ping`, signals); same crate as the compositor, single-threaded; sctk uses it | callback-style; state must be a single `&mut State` (matches our handle-based invariant) |
| Hand-rolled epoll (`fdm`) | Foot | Oneshot control, tiny, no deps | We would write timers and signal handling; Rust unsafe |
| mio + hand-written Wayland read loop | wezterm | Simple, cross-platform | No timers, spawn queue and wake-ups ad hoc |
| polling (Apache-2.0 OR MIT) | calloop's backend | Fine as a building block | Too low level |

Design for Apotheosis: one calloop `EventLoop<AppState>`:
- Source 1: Wayland queue via `WaylandSource` (prepare_read, read, dispatch, flush before sleeping).
- Sources 2..N: one `Generic` source per pty master fd, edge-triggered with a per-tick byte budget so a flood cannot starve the UI (foot achieves the same with its delayed-render timers; we copy the budget idea).
- Control socket: a listener `Generic` plus per-client streams; JSON lines decoded on the loop thread; every reply is an Action result.
- Timers: frame coalescer, cursor blink, key repeat (compositor does not repeat: `wl_keyboard.repeat_info` gives rate and delay, the client must generate repeats; sctk has repeat support through calloop timers).
- Cross-thread: font rasterization, image decode and the media helper communicate by `calloop::channel` and wake with `ping` (decoded in sandboxed helper processes, fds passed via SCM_RIGHTS).
- Signals: `calloop::signals` or a signalfd for SIGCHLD/SIGTERM.
The invariant from EclipseOS ("No locks on the hot path; blocking work goes to a pool and returns by channel") carries over unchanged.

## 4. Control surface (config, actions, IPC)

Settings that should exist (KDL keys are proposals, not existing interface; each also needs a settings-GUI field):

```
window {
    decorations "auto"          // auto | client | server | none
    csd { shadow 12; corner-radius 14; titlebar-tabs true; resize-margin 8 }
    glass { blur "auto"; blur-fallback "tint"; tint-alpha 0.78 }   // auto|on|off ; tint|opaque|none
    scale "auto"                // auto | 1.0 | 1.25 ... (override of fractional-scale)
}
render {
    frame-pacing "frame-callback"   // frame-callback | presentation | latency
    present-mode "fifo"             // fifo | mailbox | immediate (tearing-control)
    coalesce-lower "0.5ms"; coalesce-upper "auto"   // foot delayed-render analogue; auto = half refresh
    presentation-stats false
}
```

Actions (shared vocabulary): `window.toggle-decorations`, `window.set-blur on|off|toggle`, `window.toggle-fullscreen`, `window.show-menu`, `window.drag-tab-out`, `debug.dump-wayland-caps` (lists bound globals, versions, decoration mode, blur capability, scale, presentation clock), `debug.presentation-stats`. Events for rules: `wayland.decoration-mode-changed`, `wayland.blur-capability-changed`, `wayland.scale-changed`, `wayland.suspended`, `output.changed`.
IPC: control-socket method `window.info` returns decoration mode, scale numerator, blur status, present mode, last N presentation latencies; `debug.wayland_caps` as above. The compositor-specific extras on Abyss (trusted-UI hooks) ride the same socket vocabulary.

## 5. Performance (numbers and method)

Published or locally verified numbers are scarce; none are measured by me on hardware.
- Foot coalescing defaults: lower 500000 ns, upper = 16666666/2 ns (config.c defaults, verified in source). Title throttle 8333 us (render.c). Method: source read.
- Foot presentation statistics buckets (zero, one, two refreshes) are the only built-in latency metric; no numeric benchmark in source.
- Shadow margin fill overhead: 12 px margin on 1920x1080 is (1944*1104 - 1920*1080)/(1920*1080) = 3.5 percent of pixels; computed, not measured.- Wayland dispatch cost: unmeasured; calloop-wayland-source overhead expected negligible. **unverified**.
- Proposed budgets for Apotheosis: input event to commit under 1 frame period at p99 on the glass path; zero protocol errors under 100 resize events/s; startup to first buffer committed under 100 ms excluding GPU init. These are targets, not published numbers.
Method to establish real numbers: use presentation-time `presented` timestamps minus our commit timestamp (same `clock_id`), and a `typing latency` harness with a camera or an input-injection protocol on Abyss.

## 6. What Apotheosis should take

1. wayland-client `system` + `dlopen` (the only option compatible with a wgpu Vulkan surface and "only libwayland").
2. Pin feature unification and assert it in CI.
3. A thin first-party binding layer over wayland-client instead of sctk wholesale; borrow sctk's xdg configure/ack ordering, keyboard repeat handling and `PresentationTimeState` shape as reference designs.
4. calloop single-threaded loop with `WaylandSource`, pty `Generic` sources with byte budgets, control socket, timers.
5. Foot's `wayl_flush` backpressure routine and the EPIPE-surfaces-protocol-error trick; also the "do not cancel_read on hangup" lesson.
6. Fractional scale + viewporter with exact round-trip buffer sizes (foot), buffer_scale 1.
7. xdg-decoration negotiation with `auto/client/server/none`, treating the configure as authoritative (foot, sctk, Rio).
8. Single-surface GPU CSD: titlebar and tabs drawn in the wgpu pass, shadow as transparent margin, `set_window_geometry` to the inner rect, input region trimmed to geometry plus resize margin, tiled/maximized states remove shadow and radius.
9. Blur ladder: ext-background-effect, KDE blur, tint/opaque fallback; capability tracking; blur only while translucent (foot).
10. Opaque-region discipline: empty when translucent, full when opaque (foot).
11. Frame callback gating with pending flags (foot/wezterm), plus presentation-time measurement; optional wp_fifo / commit_timing; tearing-control opt-in.
12. Delayed-render coalescer keyed on measured refresh period.
13. Read `surface_caps.alpha_modes` for the wgpu surface and choose PreMultiplied (Rio sugarloaf).
14. Handle both `RawDisplayHandle::Wayland` from wayland-backend (`rwh_06`) and build `WaylandWindowHandle` from `ObjectId::as_ptr()`; own the drop order.
15. A permanent `debug.dump-wayland-caps` action for support.

## 7. What it should avoid

- Pure-Rust Wayland backend (cannot give Vulkan a `wl_display`).
- Hand-rolled wire protocol.
- Eight-subsurface CSD with wl_shm (foot, sctk `FallbackFrame`, `sctk-adwaita`): right for a CPU renderer, wasteful and flicker-prone for GPU glass; also needs Weston subsurface desync quirks.
- Integer-only scaling (wezterm): blurry at 125 percent and 150 percent.
- Depending on `winit` (Rio's `rio-window` shows the maintenance burden of forking it) because winit hides the frame callback, the blur protocols and the decoration policy.
- Assuming blur exists; assuming Mesa alpha modes; assuming the compositor honours `wp_tearing_control`.
- `csd_fails` permanent-disable pattern as seen in Rio's window state (a single transient failure disables decoration for the session); retry on next configure instead.
- Treating `refresh = 0` in presentation feedback as an error (variable refresh).
- sctk 0.21 as a hard dependency before its API settles (0.19 to 0.21 breakage); if adopted, pin exact versions like the Smithay `=0.7.0` rule.

## 8. Sources

Primary source (path and function; interface names only quoted):
- foot `wayland.c`: `fdm_wayl`, `wayl_flush`, `xdg_toplevel_decoration_configure`, `csd_instantiate`, `fractional_scale_preferred_scale`, `wayl_win_alpha_changed`, `ext_background_capabilities`, `wayl_win_subsurface_new_with_custom_parent`; `render.c`: `surface_scale_explicit_width_height`, frame callback handling and `presentation_feedback_listener`; `config.c`: tweak delayed-render defaults. Repo https://codeberg.org/dnkl/foot
- wezterm `window/src/os/wayland/connection.rs: run_message_loop_impl`; `window.rs: update_window_background_blur`, `refresh_frame`, handle impls; `window/Cargo.toml`. https://github.com/wezterm/wezterm
- rio `rio-window/Cargo.toml`, `src/platform_impl/linux/wayland/event_loop/mod.rs`, `window/state.rs`, `types/ext_background_effect.rs`, `types/wp_fractional_scaling.rs`, `types/wp_viewporter.rs`; `sugarloaf/src/context/webgpu.rs`. https://github.com/raphamorim/rio
- smithay-client-toolkit `src/{background_effect,presentation_time}.rs`, `shell/xdg/window/mod.rs`, `shell/xdg/fallback_frame.rs`, `Cargo.toml`, `build.rs`. https://github.com/Smithay/client-toolkit
- wayland-rs `wayland-backend/src/lib.rs` (docs), `sys/mod.rs` (rwh_06 impl), `sys/client_impl/mod.rs` (`display_ptr`, `from_foreign_display`, `poll_fd`); `wayland-sys/build.rs`, `src/lib.rs` (`ffi_dispatch`); `wayland-client/src/{conn,event_queue}.rs`. https://github.com/Smithay/wayland-rs
- wgpu `wgpu-hal/src/vulkan/instance.rs: create_surface_from_wayland`; `swapchain/native.rs`. https://github.com/gfx-rs/wgpu
- Protocol XML on dev host: `/usr/share/wayland-protocols/{stable/xdg-shell,stable/presentation-time,stable/viewporter,unstable/xdg-decoration,staging/{ext-background-effect,fractional-scale,fifo,commit-timing,tearing-control}}`. Docs mirrors: https://wayland.app/protocols/ext-background-effect-v1, https://wayland.app/protocols/xdg-shell.
- Licences and versions: crates.io API https://crates.io/api/v1/crates/{calloop,calloop-wayland-source,dlib,raw-window-handle,xkbcommon,xkeysym,sctk-adwaita,wayland-csd-frame,libloading,polling,mio,kbvm,wayland-cursor,ash} queried 2026-11-12.
- Secondary (compositor support for ext-background-effect, unverified): https://phoronix.com/news/GNOME-Mutter-Background-Blur, https://www.webpronews.com/gnome-finally-delivers-native-background-blur-to-wayland-apps/, https://www.neowin.net/news/tags/niri/

Unverified items collected: Mesa WSI queue and frame-callback behaviour; Mesa alpha-mode advertisement on Wayland; Hyprland/Sway ext-background-effect status; kbvm maturity; whether Vulkan WSI uses wp_fifo/commit_timing on its own; dispatch overhead numbers.
