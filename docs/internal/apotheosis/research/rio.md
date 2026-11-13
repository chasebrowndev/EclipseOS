# Prior art: Rio (raphamorim/rio)

Researched 2026-11-12 against upstream `main` at commit 72e0564 (v0.5.30, released 2026-11-12), shallow clone in `~/.cache/apotheosis-prior-art/rio`. Licence MIT. Per the prior-art rule this document describes techniques in prose; nothing here is transcribed source. Escape-sequence syntax and config key names are interface facts.

## Summary

Rio is a Rust terminal whose renderer library, sugarloaf, started as a wgpu renderer and has since grown native backends: Metal on macOS, a hand-written `ash` Vulkan backend on Linux, wgpu only on Windows and wasm (and optional on Linux/macOS behind a cargo feature), plus a tiny-skia CPU fallback. This is the single most important finding for Apotheosis: the project that most resembles our plan ("Rust, wgpu, Wayland, glass UI") abandoned wgpu as the Linux default. The commit/doc trail says the reason was to skip the wgpu translation layer; the open issue list shows the native Vulkan path has real alpha/transparency bugs on Wayland that the wgpu path does not.

What Rio does well and we should study closely:

- A packed 8-byte cell with side tables for styles and extras, a persistent GPU cell buffer indexed by row and column, and per-row dirty tracking so only changed rows are re-emitted and uploaded.
- Event-driven rendering with frame pacing tied to the Wayland `wl_surface.frame` callback, plus an input-rate tracker that keeps presenting only when input is fast (to stop VRR displays downclocking).
- A fast VT core extracted as a standalone crate (`rio-vt`) with SIMD plain-text paths, flat lookup tables for width and grapheme breaking, and a documented PTY drain policy.
- Wide graphics protocol coverage written from scratch (kitty graphics including Unicode placeholders, sixel, iTerm2 inline images), with a unified byte budget and tiered eviction.
- A new "Glyph Protocol" (APC 25a1) that lets apps register vector glyphs for PUA codepoints at runtime. Rio authored it and is the reference implementation.
- Procedural drawing of box, block, braille, Powerline and legacy-computing glyphs so they tile exactly regardless of font.
- The Wayland client is a fork of winit (`rio-window`) on smithay-client-toolkit, with ext-background-effect-v1 blur, KWin blur, fractional scale, viewporter, xdg-activation, cursor-shape, text-input and pointer constraints.

What is weak, and what to avoid:

- No IPC or remote control at all (open issues ask for it); config is TOML, actions are a hard-coded enum parsed from strings, no event-to-action rules.
- Several build-time and runtime C/C++ dependencies that our rules forbid: `simdutf` (C++), `onig` (Oniguruma, C), `fontconfig-sys` (C), `glslc`/`glslangValidator` invoked by `build.rs` for SPIR-V.
- Wayland resize and hidden-window problems (issues 1921, 1707, 1986, 1795), transparency bugs on the native Vulkan path (1967, 2002), no damage reporting to the compositor (1570).
- The renderer is mid-rewrite (the grid module doc still says "Phase 0 scaffolding" in places while the Vulkan path ships), so internal docs lag the code.

## Architecture (how it is built)

Workspace crates (root `Cargo.toml`): `sugarloaf` (renderer), `rio-vt` (VT core, grid, parser, search, selection, kitty/sixel/iTerm2/glyph protocols), `rio-grid` (turns rows plus resolved styles into GPU `CellBg`/`CellText` primitives), `rio-unicode` and `rio-grapheme-width` (grapheme clustering and width tables), `rio-fonts` (Nerd Font scaling rules and the embedded symbols-only font), `rio-graphics` (glyph outline layout), `teletypewriter` (PTY), `corcovado` (fork of mio 0.6 trimmed to the PTY loop), `rio-window` (winit fork with Wayland/X11/macOS/Windows), `rio-notifier`, `rio-backend` (config, event types), `frontends/rioterm` (the app: input, bindings, tabs/splits, UI overlays), `librio`/`libsugarloaf` (C ABI embedding for Swift, used by a macOS app called Canario), `librio-wasm`.

### Process and thread model

Single main thread owns the window event loop and the renderer. Each pane has a PTY reader thread ("Machine") that polls the PTY with corcovado, reads in bounded chunks, parses into the pane's `Crosswords` grid under a fair mutex, and posts a lightweight `TerminalDamaged(route)` user event to the event loop. A flag on the terminal (`damage_event_in_flight`) coalesces wakeups so at most one is outstanding. The renderer, when it runs, locks the terminal once per pane per frame and takes a snapshot of visible rows (reusing row allocations) plus the per-row resolved styles, so no lock is held while building GPU data.

PTY drain policy (rio-vt `performer/mod.rs`, `Machine::pty_read`): the read buffer is 1 MiB, each `read(2)` is capped at 64 KiB, and a single terminal lock lease parses at most 256 KiB (4 chunks) before releasing so the renderer can snapshot (documented as about 1 ms of parsing at full SGR density, inside a 120 Hz frame). The parser first tries a non-blocking unfair lock and keeps reading if it fails, falling back to a blocking lock only when the buffer is full or the read ended. The PTY fd is level-triggered and re-registered only when write interest changes. A short read is trusted to mean drained, except after child exit. The PTY is opened with a nonzero baud rate (230400) because BSD/XNU size the tty queue from it (changelog says baud 0 capped throughput on macOS).

### Terminal core (rio-vt)

- Parser: Paul Williams state machine forked from alacritty's vte, with inline OSC capacity of 2048 bytes (larger payloads such as OSC 52 spill to an overflow buffer) and a 4-intermediate, 16-param limit.
- Cell: a packed 64-bit `Square` (`crosswords/square.rs`). Bits hold a 21-bit codepoint, a 2-bit wide state (narrow, wide, spacer, leading spacer for a wide char split by soft wrap), 7 per-cell flag bits, a 2-bit content tag, a 16-bit style id and a 16-bit extras id. The content tag has a trick: cells that are only a coloured background (palette index or RGB) repurpose the style and extras bits to hold the colour directly, so selections, blank-after-clear regions and colour blocks need no style-table lookup at all.
- Style: a per-grid intern table (`crosswords/style.rs`) keyed by the tuple (fg, bg, underline colour, SGR flags), id 0 is always the default style, ids are `u16`. Underline kinds (single, double, curly, dotted, dashed) are one-hot in the flag word, plus slow and rapid blink. Variable-size data (zero-width combiners, hyperlinks, graphics references) lives in an extras table.
- Storage: ring buffer of rows (alacritty lineage) so scroll is an offset add. Rows carry a `dirty` bit and a `has_styles` hint. A scroll marks damage once rather than per row. Recycled rows reset only their occupied cells.
- Damage model: `TerminalDamage` is one of Noop, Full, Partial (consult per-row dirty bits) or CursorOnly. The consumed frame records the last cursor position so an idle drain fires no events (a bug fixed in a recent release: before that every PTY read woke the renderer even with no change).
- Width and graphemes: a flat 128 KiB `u8` width table over U+0000 to U+1FFFF built at first use from the `unicode-width`-derived `rio-unicode` crate, with scalar fallback above plane 1. Mode 2027 grapheme segmentation uses a flat class table over the same range and a transition table indexed by (previous class, next class, state) returning (break, next state), both built by driving the reference implementation so they cannot drift from the conformance-tested rules. Grapheme clustering is on by default since 0.5.20 (config key `grapheme-clustering`). The changelog notes that in legacy mode a variation selector no longer changes the preceding cell width (it did before, desyncing tmux redraws).
- SIMD: UTF-8 validation (own `simd_utf8.rs` shaped like the simdutf8 API), base64 via `simdutf` (C++) for OSC 52, iTerm2 images and kitty graphics chunks, and word-at-a-time scanning of printable runs and CSI params.
- Synchronized output (mode 2026) is modelled as a plain mode that only holds presentation: bytes are always parsed into the grid, the renderer keeps showing the last frame, and Rio's own UI (search, palette, cursor) still updates on top. An update ends on an end marker, after 1 second without a new begin, on full reset, or on a real resize. The legacy iTerm2 DCS form (`DCS = 1 s` / `DCS = 2 s`) is also honoured.
- Selection, search, vi mode: regex search uses the `regex-automata` lazy DFA with four DFAs (forward/reverse, left/right origin) as in alacritty. Vi mode and vi selection modes exist. Hints use Oniguruma (see "avoid").

### Rendering (sugarloaf)

Backends (`sugarloaf/src/context/`): an enum `ContextType` of Wgpu (feature-gated), Metal, Vulkan (Linux only) and Cpu. Each backend has its own grid renderer, general quad/geometry renderer, image renderer and context. Defaults: Metal on macOS, native Vulkan on Linux, wgpu elsewhere (`rio-backend/src/config/renderer.rs`). Config key `renderer.backend` accepts metal, vulkan, webgpu. `renderer.use-cpu` selects the tiny-skia CPU rasterizer (solid quads and glyphs only, experimental).

Grid pipeline (`sugarloaf/src/grid/`): two instanced passes per pane. A background pass reads a flat `bg_cells` buffer (4 bytes per cell, premultiplied RGBA, alpha 0 meaning "default background shows through") indexed by row times columns. A text pass draws one 32-byte `CellText` instance per glyph or decoration: atlas position and size, bearings, grid position, premultiplied colour, a mono-versus-colour atlas flag, flag bits (no-min-contrast, is-cursor-glyph) and an atlas page index. Decorations (underline kinds, strikethrough, overline, hyperlink underline) are separate instances, not bits on the base glyph. Selection, search and inverse tinting are folded into the colour on the CPU, there are no selection bits in the shader. The uniform block (144 bytes) carries projection, padding, cursor colours and position (so the shader inverts the glyph under a block cursor), cell size, grid size, a WCAG minimum-contrast ratio, a flag word (display-P3 tag, linear blending), a padding-extend bitfield (edge cells extend into the window margin) and an input colourspace selector (sRGB, DisplayP3, Rec.2020). Only dirty rows are rewritten into the persistent GPU buffers.

Glyph atlas: glyphs are rasterized by swash into R8 (and colour) bitmaps keyed by (font id, glyph id, size bucket), where the bucket quantizes size to a quarter physical pixel so a resize animation reuses glyphs. The Vulkan backend uses a list of 2048 by 2048 pages, up to 16, with a shelf allocator (guillotiere for some paths, and a shelf allocator allowing about 10% waste per shelf in the image cache), and buckets cells by page to bind the right descriptor set per draw. Shaping uses swash with an N-way associative text-run cache (256 buckets by 8 entries, wyhash keys, move-to-end LRU inside a bucket) holding shaped glyph arrays per run.

Procedural "sprites" (`sugarloaf/src/sprite/`): box drawing (U+2500 to 259F), braille (2800 to 28FF), legacy computing symbols (1FB00 to 1FB3B, 1CD00 to 1CDE5 and a few more), Powerline (E0B0 to E0BF and a few), geometric triangles. Each is CPU-rasterized once to an exact-cell R8 bitmap and uploaded like a glyph. A single comparison `cp < 0x2500` short-circuits the hot loop. Nerd Font glyph scaling and alignment follow the Nerd Fonts patcher rules (table derived by way of ghostty), with a symbols-only Nerd Font embedded behind a feature flag. Font discovery on Linux is fontconfig via direct FFI for per-codepoint fallback (`FcFontSort` with charset), plus `fontconfig-parser` for configuration.

UI chrome (tabs "island", command palette, search bar, assistant/error overlays, scrollbar, confirm-quit, trail cursor) is drawn immediate-mode through sugarloaf quad, rounded-rect and text-overlay APIs: nothing is cached, "not drawing equals not visible". Layout of panes uses taffy (flexbox/grid) with draggable borders. Animation uses a small spring helper (tab island) and an exponential ease-out for the cursor trail whose update composes exactly across irregular frame gaps (model taken from kitty's cursor trail).

CRT/shader filters: librashader (RetroArch slang presets) runs only on the wgpu backend; a filter chain's final pass overwrites the alpha channel, so combining filters with `window.opacity` just warns once.

### Wayland and frame pacing

`rio-window` is a fork of winit on smithay-client-toolkit with sctk-adwaita CSD fallback (feature gated). Protocols used include xdg-shell, fractional scale, viewporter, xdg-activation (startup notify), cursor-shape, text-input-v3, pointer constraints, relative pointer, ext-background-effect-v1 (generic blur, added recently) and the KDE blur protocol.

Frame pacing (rio-window `wayland/event_loop/mod.rs` and `window/state.rs`): a per-window frame-callback state machine with states None, Requested, Received. A redraw is only delivered to the app when a previous callback has not been left pending; the callback is requested at present time (not speculatively, because a requested callback with no commit never fires and would wedge the loop). The loop also redraws when the callback was received and the input-rate tracker says present-after-input is on. The input-rate tracker (`platform_impl/input_rate.rs`, modelled on Zed's) extends a sustain window only when input arrives at 60 or more events per second inside a 100 ms rolling window, specifically to keep VRR displays from downclocking while dragging, without forcing 60 redraws after one keystroke (which saturated the Metal drawable pool).

App-side pacing (`frontends/rioterm/src/router/mod.rs`): the vblank interval is derived from the monitor refresh in millihertz. On `RenderRoute` the pane is marked dirty and, if less than a vblank has passed since the last render timestamp, a one-shot timer (a copy of alacritty's scheduler) is armed for the next frame boundary, otherwise redraw is requested at once. Strategy config `renderer.strategy` is `events` (default) or `game` (continuous). `renderer.disable-unfocused-render` and `renderer.disable-occluded-render` suppress redraws; a one-shot flag forces a render after un-occlusion.

Present path on Vulkan (`context/vulkan.rs`): present mode is hard-coded FIFO, with a code comment that MAILBOX on X11 Mesa runs a driver polling thread that pegged Xorg under sustained input. Triple-buffer request (at least 3 swapchain images, clamped). Format preference B8G8R8A8_UNORM with sRGB non-linear colour space, so blending happens in gamma space and shaders emit sRGB-encoded output. Composite alpha chosen by a priority guess: post-multiplied, then pre-multiplied, then inherit, then opaque. Frames-in-flight use per-frame semaphore, fence and command pool arrays. `VkShared` is an `Arc` that owns device, instance and entry in drop order so any leaf resource (buffer, atlas, per-pane grid renderer) can outlive the context safely (fix for upstream issue 1568, a use-after-destroy at shutdown).

On the wgpu path: present mode is Fifo, power preference HighPerformance, and when transparency is requested the adapter list is enumerated and ranked so that only adapters whose surface offers pre- or post-multiplied alpha are considered (highest device class first, never a software rasterizer), with fallback to the default adapter if device creation fails.

### Graphics protocols

- Kitty graphics (`rio-vt/src/ansi/kitty_graphics_protocol.rs`, `kitty_virtual.rs`): chunked transmission with a per-terminal accumulator keyed by image id, 10 second stale-chunk timeout, max 10,000 pixels per axis and 400 MiB decoded payload (matching upstream kitty), auto-assigned ids in the upper half of the u32 range, overlay placements keyed by (image id, placement id), and virtual placements via the Unicode placeholder U+10EEEE with row/column diacritics and image id in the foreground colour.
- Sixel and iTerm2 OSC 1337 inline images go through an atlas path with DEC "grid plane" semantics (private palette, cursor-to-the-right modes).
- A unified byte budget (default 320 MB "per kitty spec") with three-tier eviction: inactive-screen images first, then active-screen images with no live placement, then visible images as last resort, oldest first within a tier. A weak-reference scheme tracks whether a graphic still has placements, giving O(1) liveness checks.
- A GPU-side image texture cache with explicit evict-by-key and evict-by-route hooks so freed grid graphics release textures.

### Glyph Protocol

Rio authored a spec (`specs/glyph-protocol.md`, dated 2026-11-08; blog post at rapha.land) carried over APC with identifier `25a1`. Verbs: `s` (support and format negotiation), `q` (query codepoint coverage: system, glossary, both, none), `r` (register a glyph for a PUA codepoint), `c` (clear one or all). Payload formats: `glyf` (single monochrome OpenType simple-glyph outline), `colrv0` (up to 16 flat-colour layers) and `colrv1` (paint graph with solid and gradient paints, no transforms or sweeps in v1). Registrations are limited to the three Unicode private-use areas so the protocol cannot restyle real text, payloads are capped at 64 KiB after base64 decode, and registrations are per session (per pane) with no cross-session state.

## Feature inventory (grouped by category)

VT and protocol: DEC/xterm baseline; 256 and truecolor; OSC 0/2 title, 4/104 palette, 7 cwd (accepts a `kitty-shell-cwd://` flavour, checks the host name), 8 hyperlinks (with `id`), 9 notifications and 9;4 progress (ConEmu), 9;9 cwd (ConEmu), 10/11/12 dynamic colours with queries and 110 to 112 resets, 22 mouse cursor shape, 50 cursor shape, 52 clipboard, 133 semantic prompts, 1337 SetUserVar and inline images; XTVERSION; XTGETTCAP via DCS; kitty keyboard protocol (disambiguate, event types, alternate keys, report all keys, associated text; fixed in a recent release so repeats are repeats and releases are delivered); kitty graphics; sixel; iTerm2 images; mode 2026 and 2027; focus reporting; bracketed paste; all mouse modes with SGR and UTF-8 encodings and shift to bypass; alternate scroll; Glyph Protocol; underline styles and coloured underlines.

Unicode and text: grapheme clustering mode 2027 on by default; wide, emoji and ZWJ handling with a "leading spacer" for wide characters at soft-wrap; IME with preedit drawn as a cursor-coloured block laid out per grapheme cluster (slides left at the right edge, never wraps), text-input-v3 on Wayland; dead keys and CJK commit text reach the palette and rename field.

Fonts: per-style family config, symbol maps (codepoint range to font), additional font directories, hinting toggle, OpenType feature strings, `use-drawable-chars` to turn procedural box/braille drawing off, bold-with-light-colours, Nerd Font scaling and embedded symbols font, COLR colour glyph rasterization (`colr_raster.rs`), system emoji fallback via fontconfig. Known gap: variable-font named instances fail to load via fontconfig's encoded face index (issue 1913); symbol maps slow startup (1765).

Rendering and visual: window opacity and blur (`window.blur` true uses system blur, which on Wayland is the KDE protocol or ext-background-effect-v1); colourspace selection (srgb, display-p3, rec2020); minimum contrast enforcement per cell in the shader; trail cursor; custom mouse cursor; CRT and librashader filters (wgpu only); scrollbar; adaptive light/dark theme following the system on Wayland and X11; `draw-bold-text-with-light-colors`; padding-extend for edge cell colours.

Multiplexing and navigation: tabs ("island" navigation mode, or Plain or native tabs on macOS), splits right and down, move dividers, select next or previous split, close split or tab, move tab, close other tabs, quake-style drop-down window with a system-wide global hotkey (the `global-hotkey` crate; on Wayland this has known problems, issue 1936), `hold` option, OSC 9;4 progress shown as a bar on the tab strip with a 15 second timeout, spawn new instance, new window.

Scrollback, search, selection: configurable scrollback limit, vi mode with normal, line, block and semantic selection, regex search forward and backward with the lazy DFA, scroll to previous or next OSC 133 prompt, scroll by page, half page, top, bottom, clear history, copy-on-select, select all, URL-hint mode with an alphabet of labels and per-rule regex, hyperlinks, post-processing and a mouse binding (alacritty-style hints).

Shell integration: embedded scripts for zsh, fish and PowerShell, injected at spawn through `ZDOTDIR` (zsh), an `XDG_DATA_DIRS` prepend (fish `vendor_conf.d`) or an `-EncodedCommand` rewrite (PowerShell); they report cwd after each prompt; bash is not integrated (needs `--posix` argv surgery); `RIO_SHELL_INTEGRATION` exports the script directory for manual sourcing; scripts are written once per version to the cache directory and old directories are left alone on purpose.

Integration: desktop notifications over D-Bus `org.freedesktop.Notifications` on a background thread; bell with config; file watcher (`notify`) for live config reload; embeddable libraries (`rio-vt` Rust crate, `librio` C ABI with a pull-model render-state API, `libsugarloaf`, wasm).

## Control surface (config, actions, IPC)

Config: single TOML file (`~/.config/rio/config.toml`), with theme files, platform override blocks (`overwrite_based_on_platform`), live reload through a file watcher, and `--write-config` to emit defaults. Top-level keys visible in `rio-backend/src/config/mod.rs`: cursor, navigation, window, shell, platform, keyboard, title, working-dir, line-height, theme, scroll, adaptive-theme, fonts, editor, margin, panel, env-vars, option-as-alt, colors, bindings, developer (fps counter, log level, log file), renderer, hints, bell, effects, scrollback-history-limit, grapheme-clustering, copy-on-select, hide-cursor-when-typing, confirm-before-quit, hold, shell-integration, enable-scroll-bar. Panel keys include margin, padding, row-gap, column-gap, border-width, border-radius. Renderer keys: backend, strategy, use-cpu, disable-unfocused-render, disable-occluded-render, filters.

Bindings: `[bindings] keys = [ { key, with, action | esc, mode } ]`. A binding either runs a named action or writes an escape string, optionally gated by terminal mode names such as appcursor. Actions are a fixed Rust enum (`frontends/rioterm/src/bindings/mod.rs`) parsed from lowercase names: Esc, Run(program), Scroll(n), Hint, ViMotion, Vi, Mouse, Paste, Copy, CopySelection, PasteSelection, font size increase, decrease, reset, scroll page, half page, top, bottom, ClearHistory, Hide, Minimize, Quit, ClearLogNotice, SpawnNewInstance, WindowCreateNew, ConfigEditor, TabCreateNew, MoveCurrentTabToPrev and Next, SelectNextTab, SelectPrevTab, SelectTab(n), SelectLastTab, TabCloseCurrent, CloseCurrentSplitOrTab, TabCloseUnfocused, ToggleFullscreen, ToggleMaximized, ClearSelection, SelectAll, ToggleQuake, ScrollToPrevPrompt, ScrollToNextPrompt, ToggleViMode, ToggleAppearanceTheme, Search(..), SearchForward, SearchBackward, SplitRight, SplitDown, SelectNextSplit, SelectPrevSplit, the "or tab" variants, MoveDivider up, down, left, right, OpenCommandPalette, ReceiveChar, None. About 60 actions, no arguments except for a few variants, no event-triggered rules, no profiles or layouts.

CLI (clap): `-e` command, `--working-dir`, `--write-config`, `--enable-log-file`, `--title-placeholder`, `--title`, `--hold`, `--app-id` (Wayland app_id or X11 class). Flags are re-applied on config reload through an `Overrides` struct so a reload does not discard them.

IPC: none. `TerminalOptions` derives serde, which hints at an intended IPC path (the struct comments say "options which can be passed via IPC"), but no socket exists in the tree; issues 868 (open windows from an existing instance), 1796 (daemon mode) and 1656 (detach tab) are open. Settings GUI: none (a `ConfigEditor` action opens the file in `editor`). Command palette exists and lists actions with shortcuts.

## Performance (numbers and method)

All numbers below are self-reported by the project (changelog at https://rioterm.com/changelog), not independently reproduced. Treat them as order-of-magnitude signals.

- VT parse throughput (Rust criterion suite `rio-vt/benches/vt_input.rs`, plain text versus ANSI-mixed streams, 120x40 grid): plain ASCII 865 to 2340 MiB/s (about 2.7x), scroll-dominated 266 to 772 MiB/s, CJK and emoji 243 to 726 MiB/s (about 3x), colour-change-heavy 1.6x, full-screen TUI redraw 1.6x. Method: headless `Crosswords` fed through the `Processor`, no PTY, no renderer.
- PTY drain: nonzero baud fix described as a "massive" gain for truecolor full-screen repaint on macOS/BSD; dropping the confirmation read raised bulk output from 23 to 26 MiB/s with per-wakeup latency down 18% on macOS (about 150 bytes per wakeup there). Linux was not the measured platform for these.
- Wasm build versus xterm.js with WebGL on byte-identical 120x40 workloads: about 3x faster plain-text parse and paint, about 1.8x faster cold init, 6x faster headless parse.
- Erase-line repair for wide-character seams on a 320-column grid: 61 ns versus 42 ns with no repair.
- Frame budget design numbers: lock lease of at most 256 KiB parse (about 1 ms at full SGR density) per frame at 120 Hz; the CellText instance is 32 bytes and CellBg 4 bytes; the Square is 8 bytes (was 24 plus per-cell heap extras).
- Input rate sustain: 60 events per second over a 100 ms window.
- No published latency (keypress-to-photon) numbers and no frame-time histograms were found. Unverified: any claim about its memory use. Open issue 1961 reports memory growth on Windows from 0.4.7 to 0.5.28.

Test method worth copying: a damage-pipeline harness that replays the renderer's exact frame-consumption and row-rebuild decisions against a painted mirror, driven by randomized editor-shaped traffic (region scrolls, insert/delete line, alt-screen flips, resizes, racing redraws) over 200 seeds, asserting three invariants (scroll offset never exceeds history, every content change has a wakeup, painted equals grid). Plus property tests that bare continuation codepoints arriving in their own damage window still mark the row.

### Known performance and correctness problems (open GitHub issues, state at 2026-11-12)

- 1921: window resize on native Wayland is laggy and does not follow the cursor (NVIDIA RTX 4060, KDE Wayland); XWayland fine. 1707: text corrupted when resizing quickly on KDE Wayland with wobbly windows.
- 1986: a Wayland window mapped hidden (wlroots-based compositor, off-screen tag) never answers terminal queries; fish waits 10 s for DA1 and warns. Cause is plausibly that rendering and event handling are tied to frame callbacks, which a hidden surface never receives (inference, unverified).
- 1795: closing on Sway freezes the window until a resize shows the confirm-quit prompt; the prompt is drawn only on the next redraw.
- 1967: native Vulkan with `window.opacity` of 0.35 shows a washed-out white light background on Intel Mesa under niri; wgpu backend correct. 2002: Windows GPU renderer ignores opacity (CPU path correct). 1761: Vulkan draws only one image when two image layers are on screen. Cause for 1967 is unverified; the composite-alpha guess order (post-multiplied preferred over pre-multiplied) while shaders emit premultiplied colour is a candidate.
- 1570: Rio presents the whole window every frame; proposal to use `VK_KHR_incremental_present` for damage. Upstream has per-row damage internally but does not forward it to the compositor.
- 1620: Wayland selection is not exported to the PRIMARY selection. 1622: Wayland CSD title drops non-Cantarell glyphs (no font fallback in the client-side decoration). 1889: hint click modifier differs from docs on Wayland.
- 1936: ToggleQuake on Wayland cannot be closed and falls behind (Wayland has no global hotkeys or arbitrary window placement without layer-shell).
- 1603: terminal stops responding to resize requests while a process runs silently.
- 1969: Ctrl+L does not clear when scrolled up. 1619: URL hit region misaligned after wide glyphs.
- Resolved in changelog (worth the lesson): stale glyphs when a combining mark attaches to an already painted cell (no damage recorded); scroll offset drift from region scrolls corrupting the viewport; idle terminals redrawing at PTY rate; pty fds leaking into child shells because they lacked close-on-exec.

## What Apotheosis should take

1. Do not copy the "wgpu is a translation layer, go native Vulkan" conclusion without data, because our rules fix wgpu. Do copy the transparency discipline: pick an adapter and a surface alpha mode explicitly, test premultiplied output end to end, and treat translucent windows as a first-class path with a regression screenshot test, since both Rio's native Vulkan and DX12-via-wgpu paths shipped transparency bugs.
2. Cell and data-layout design: 8-byte packed cell, style intern table with id 0 as default, extras side table, bg-only cell encoding, ring-buffer rows, per-row dirty bit and a Noop/Full/Partial/CursorOnly damage enum. Persistent GPU cell buffers (4-byte bg, 32-byte text instance) with only dirty rows uploaded.
3. Fold selection, search highlight, inverse and dim into cell colours on the CPU, keep the shader free of selection logic; keep the shader-side pieces that are cheap and useful: cursor inversion of the glyph underneath, WCAG minimum-contrast, padding-extend.
4. Render-on-damage with a wake coalescing flag, frame-callback driven pacing, and the input-rate sustain tracker for VRR panels. Add one improvement: continue answering terminal queries and servicing the PTY when the surface is hidden or has no frame callbacks (issue 1986), since parsing and presentation must be decoupled.
5. PTY drain policy: 64 KiB read chunks, bounded lock lease (about 4 chunks), try-lock-then-continue reading, coalesced wake events, level-triggered fd. Our compositor-facing main loop should never wait on the pane lock beyond one bounded lease.
6. Synchronized output (2026) modelled as a presentation hold with a 1 second safety timeout and a resize release, while our own glass UI keeps animating over the held frame. Also accept the legacy DCS form.
7. Procedural sprites for box, block, braille, legacy computing and Powerline so cell tiling is exact; one-comparison fast reject; embed a symbols-only Nerd Font and apply patcher scaling rules (the `rio-fonts` idea; the font file is an asset, check its licence).
8. Flat lookup tables for width and for grapheme break transitions built from a conformance-tested reference at startup (or build time) so hot-path segmentation is one table index.
9. Graphics: kitty graphics with the same hard limits (10,000 px axis, 400 MiB payload, 10 s chunk timeout, auto-id range separation), Unicode placeholder virtual placements, a single memory budget over kitty, sixel and iTerm2 images with tiered eviction (inactive screen, unreferenced, visible), and liveness tracking so textures free when the last placement dies.
10. Consider Glyph Protocol (APC 25a1): PUA-only, per-session, 64 KiB cap, glyf/COLRv0/COLRv1. It fits our "perfectly implemented" bar and is a good capability to surface in the settings GUI and security model (it never restyles real text). It is a one-vendor proposal, mark as optional until other terminals adopt it.
11. Test harnesses: the randomized damage-pipeline mirror test and property tests for zero-width attach damage. Adopt both before writing the renderer, they found many bugs upstream.
12. Shell integration by environment injection (ZDOTDIR, XDG_DATA_DIRS, versioned cache dir, never delete old dirs, manual-source env var), but go further than Rio: also bash via `--rcfile` or `ENV`, OSC 133 marks, and use the marks for prompt-to-prompt navigation and command output selection.
13. Cursor trail with kitty's composable exponential ease-out, which stays correct under irregular, event-driven frame timing, as an optional effect. Fits the glass aesthetic.
14. Action vocabulary breadth as a floor: Rio's roughly 60 actions (split, divider moves, prompt navigation, quake, palette, vi mode, theme toggle) are the minimum our enum should cover, but with arguments, since Rio's are mostly argument-less.
15. Immediate-mode glass UI drawn through the same renderer (palette, tabs, search), layout via taffy (MIT) and a spring helper. Rio proves the approach is viable on one renderer; note its UI is flat quads with rounded corners and no real glass, so the blur and refraction layer is ours to invent.
16. Background blur on Wayland via ext-background-effect-v1 with KDE blur protocol fallback, degrade with a single warning.
17. Config reload preserving launch overrides (`Overrides` pattern), and platform override blocks.

## What it should avoid

1. Building or linking C/C++: `simdutf` (C++), `onig`/`onig_sys` (Oniguruma), `fontconfig-sys`, `glslc` or `glslangValidator` in `build.rs`. Replacements to evaluate: pure-Rust base64 decoders with SIMD (the `base64` crate's engines or `base64-simd`), `regex` or `fancy-regex` for hint patterns (note Rio's default URL regex needs lookbehind, so we must design the hint matcher without lookbehind or with a hand-written scanner), `fontconfig-parser` plus our own charset matching or `fontdb`, and `naga` for in-process GLSL or WGSL to SPIR-V (our wgpu path uses WGSL anyway).
2. Hard-coding FIFO present, one swapchain policy and no damage forwarding. Prefer wgpu's present modes plus `wp_presentation` timing, and evaluate presenting damage to the compositor (Rio issue 1570 shows demand).
3. Drawing the confirm-quit prompt or answering PTY queries only on redraw (issues 1795, 1986): UI state must advance on its own events, not on frame callbacks.
4. A fixed Rust action enum parsed from strings with no arguments and no IPC. Our single Action vocabulary must be data (name plus typed arguments) so config, palette, rules and the socket share it.
5. Shipping a half-migrated renderer (rich-text path plus new grid path plus three backends). Pick one GPU path (wgpu) and keep the CPU fallback only if it is cheap to test; Rio's `use-cpu` lacks images, filters, advanced underlines and corner radii.
6. Global hotkey crates on Wayland for quake mode. Use compositor integration (Abyss) or xdg-activation and layer-shell where available; on other compositors, document the limitation.
7. A winit fork. Rio carries its own winit fork to get Wayland features; our plan (own Wayland client on `wayland-client` and sctk-class crates, own protocol handling) avoids inheriting winit's frame-callback and resize behaviour, which is the origin of several Rio Wayland issues (1921, 1707).
8. Per-cell legacy zero-width handling that mutates cells without recording damage; and any width change from variation selectors outside mode 2027.
9. Treating TOML-in-code defaults as the config contract; Rio's settings are not discoverable (no GUI), and some options are macOS-only or backend-only without schema flags. Our KDL schema must carry metadata for the settings GUI.
10. Dependence on a single maintainer's custom protocols for core features (Glyph Protocol, trail cursor timing) without a fallback path.

## Sources

Source (upstream paths, all under https://github.com/raphamorim/rio, commit 72e0564):

- `sugarloaf/src/context/mod.rs`, `context/vulkan.rs` (present mode comment, swapchain format, composite alpha guess, `VkShared`), `context/webgpu.rs` (adapter ranking for alpha-capable surfaces, `PresentMode::Fifo`)
- `sugarloaf/src/grid/cell.rs` (`CellBg`, `CellText`, `GridUniforms`), `grid/atlas.rs` (`GlyphKey`, `AtlasSlot`), `grid/vulkan.rs` (page size, max pages), `grid/mod.rs`
- `sugarloaf/src/sprite/mod.rs` and siblings (`is_drawable`, box_drawing, braille, powerline, legacy)
- `sugarloaf/src/font/text_run_cache.rs` (N-way associative cache), `font/linux.rs` (fontconfig FFI), `font/fonts.rs` (`SugarloafFonts`), `renderer/image_cache/atlas.rs` (`AtlasAllocator`), `build.rs` (glslc SPIR-V step)
- `rio-vt/src/performer/mod.rs` (`Machine::pty_read`, `notify_damage`, constants), `crosswords/square.rs` (packed cell), `crosswords/style.rs` (`StyleSet`), `crosswords/grid/storage.rs` (ring buffer), `crosswords/search.rs` (lazy DFA), `codepoint_width.rs`, `grapheme_lut.rs`, `simd_utf8.rs`, `simd_base64.rs`, `event/mod.rs` (`TerminalDamage`), `event/sync.rs` (`FairMutex`), `ansi/kitty_graphics_protocol.rs`, `ansi/kitty_virtual.rs`, `ansi/graphics.rs` (`evict_images`), `ansi/glyph_protocol.rs`, `ansi/sixel.rs`, `ansi/iterm2_image_protocol.rs`, `ansi/mode.rs`, `performer/handler.rs` (OSC and DCS dispatch), `performer/osc.rs`, `benches/vt_input.rs`
- `rio-window/src/platform_impl/linux/wayland/event_loop/mod.rs` (frame callback handling), `window/state.rs` (`request_frame_callback`), `types/ext_background_effect.rs`, `types/kwin_blur.rs`, `platform_impl/input_rate.rs` (`InputRateTracker`)
- `frontends/rioterm/src/router/mod.rs` (`update_vblank_interval`, `wait_until`, `request_overlay_redraw`), `application.rs` (`user_event` render handling), `scheduler.rs`, `bindings/mod.rs` (`Action`), `bindings/kitty_keyboard.rs` (`build_key_sequence`), `shell_integration/mod.rs`, `renderer/trail_cursor.rs`, `renderer/island.rs`, `renderer/command_palette.rs`, `layout/mod.rs` (taffy), `cli.rs`, `context/renderable.rs`
- `rio-backend/src/config/mod.rs`, `renderer.rs`, `window.rs`, `effects.rs`, `hints.rs`, `bindings.rs`, `navigation.rs`, `layout.rs`
- `specs/glyph-protocol.md`, `specs/librio.md`, `rio-vt/README.md`, `rio-grid/README.md`, `corcovado/README.md`
- `Cargo.toml`, `frontends/rioterm/Cargo.toml`, `sugarloaf/Cargo.toml` (dependency and feature facts)

Web:

- Changelog and performance claims: https://rioterm.com/changelog
- Docs: https://rioterm.com/docs/config
- Open issues (fetched with `gh issue view` on 2026-11-12): https://github.com/raphamorim/rio/issues/1986, /1967, /1921, /1570, /1795, /1707, /1761, /1620, /1622, /1936, /868, /1796, /1913, /1961, /2002
- Glyph Protocol blog post (referenced in the spec, not fetched): https://rapha.land/introducing-glyph-protocol-for-terminals/ (unverified)
- Releases: https://github.com/raphamorim/rio/releases (v0.5.30 on 2026-11-12)

Unverified: the exact reason Rio moved Linux from wgpu to native Vulkan (inferred from `sugarloaf/Cargo.toml` comment "native backends skip the wgpu translation layer" and the changelog line "vulkan native support"); the root cause of issue 1967; any end-to-end latency or memory figures.
