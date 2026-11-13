# Prior-art dossier: Alacritty

Subject: https://github.com/alacritty/alacritty (shallow clone at commit aa5d942, workspace `alacritty` 0.18.0-dev, `alacritty_terminal` 0.26.1-dev, read 2026-11-12). Licence Apache-2.0 (README; `alacritty_terminal/Cargo.toml`). Companion benchmark tool: https://github.com/alacritty/vtebench (MIT or Apache-2.0 dual files).

Rule reminder: this document describes techniques in prose. No source is reproduced. Interface facts (config key names, IPC message names, action names) are quoted.

## Summary

Alacritty is the minimalist reference for "a fast VT core plus a thin GPU renderer". It deliberately ships no tabs, no splits, no GUI settings editor, no ligature shaping, no graphics protocols (README FAQ). What it does extremely well is worth copying as engineering discipline:

- A cleanly separable terminal library crate (`alacritty_terminal`): grid, parser driver, selection, vi mode, regex search, damage, PTY event loop. No windowing or rendering types inside.
- A very simple, cheap-per-frame renderer: a glyph atlas, one instanced draw per batch, two passes (backgrounds, then glyphs), dual-source blending for subpixel text.
- Line-granular damage tracking that is passed all the way to the compositor (`swap_buffers_with_damage`).
- A lock discipline between the PTY reader thread and the render/UI thread (fair mutex, bounded locked read, 1 MiB read buffer, unfair try-lock) that keeps throughput high without starving rendering.
- Vi mode, regex search with lazily built DFAs run directly on the grid, and "hints" (regex to label to action) as an unusually small, composable keyboard-driven feature set.
- A tiny control surface: a Unix socket with newline-delimited JSON, three messages (`CreateWindow`, `Config`, `GetConfig`), plus per-window runtime config overrides.

What Apotheosis should not copy: the config format (TOML with a hand-rolled derive and merge), the OpenGL stack, FreeType/fontconfig via `crossfont` (C libraries), the absence of ligature/graphics/mux, and the per-codepoint-only glyph cache.

## Architecture (how it is built)

### Crates

- `alacritty_terminal`: `grid/` (storage ring, rows, resize/reflow), `term/` (Term state machine, `cell.rs`, `color.rs`, `search.rs`), `selection.rs`, `vi_mode.rs`, `event_loop.rs` (PTY thread), `sync.rs` (FairMutex), `tty/` (unix pty, windows conpty). Parsing is delegated to the separate `vte` crate (state machine plus an `ansi` Processor with synchronized-update support).
- `alacritty`: winit + glutin windowing, `display/` (Display, damage, hints, content iterator, cursor, bell), `renderer/` (text, rects, shaders), `input/` (mouse, keyboard, bindings), `config/` (TOML schema), `polling/` (config file monitor, IPC socket, signals), `scheduler.rs` (timer wheel for cursor blink, bell, etc.), `window_context.rs`.
- `alacritty_config`, `alacritty_config_derive`: derive macro that deserialises TOML while tolerating unknown/invalid fields (per-field warnings rather than total failure) and supports "merge" semantics for imports and runtime overrides.

### Grid

- `Grid<Cell>` holds rows in a `Storage` ring buffer (`grid/storage.rs`). Scrolling the whole buffer is a modular add on a `zero` offset rather than a memmove; indexing is reimplemented to account for the rotating origin. Visible lines plus scrollback live in one structure; shrinking `len` reduces history without freeing the raw allocation; a cache cap of 1000 rows avoids unbounded retention.
- Scrollback is capped at `MAX_SCROLLBACK_LINES = 100_000` (`config/scrolling.rs`), default 10_000 (unverified for default; the cap is confirmed in source).
- `Row` tracks an `occ` upper bound, the highest column modified since last reset, so clear/reset and "is empty" checks skip the untouched tail.
- `Cell` is `{ c: char, fg, bg, flags, extra: Option<Arc<CellExtra>> }`. Rare data (zero-width combining chars up to a small fixed array, underline colour, hyperlink) lives behind the optional shared `extra`, keeping the common cell small and cheap to clone. Flags are a 16-bit bitset: inverse, bold, italic, underline family (single, double, undercurl, dotted, dashed), wrapline, wide char, wide-char spacer, leading wide-char spacer, dim, hidden, strikeout.
- Wrapped lines are marked by a `WRAPLINE` flag on the last cell rather than a separate line table; resize reflows by walking those flags (`grid/resize.rs`).
- Wide characters occupy a cell plus a spacer cell; a "leading spacer" exists for the case where a wide char does not fit at end of line and the line wraps.
- Selection is anchor-based with sides (`Side::Left/Right`), types Simple, Block, Semantic, Lines (`selection.rs`). Anchors rotate with scrolling so selections survive output.

### PTY thread and the lock

`event_loop.rs`: a dedicated thread uses `mio` to poll the PTY and a channel. It reads into a 1 MiB buffer (`READ_BUFFER_SIZE = 0x10_0000`). It parses bytes while holding the terminal lock, but bounds each locked parse to about 64 KiB (`MAX_LOCKED_READ = u16::MAX`) before releasing so the renderer can interleave. It first attempts an unfair try-lock; if that fails and the unparsed backlog reaches the buffer size it forces a blocking unfair lock. A `FairMutex` (`sync.rs`) reserves the "next" slot for the PTY reader so a hot render thread cannot starve parsing. Synchronized updates (mode 2026) are handled in the parser: bytes inside a sync block do not queue a redraw, and a timeout wakes the loop to force-stop a stuck sync (`sync_timeout` path). Wakeup events to the UI are coalesced: a redraw is queued only if processed bytes were not all inside a synchronized region.

### Damage

`Term::damage()` returns either Full or Partial line-bound ranges (`LineDamageBounds {line, left, right}`), tracked by `damage_point` / `damage_line` at every mutation site, and reset after the frame. `display/damage.rs` keeps two frames of damage (current and next, swapped each frame) because of buffer age, adds extra rectangles for the vi cursor, selection changes, hint highlights, message bar, converts terminal lines to GL rects (bottom-left origin, with overdamage padding), merges overlapping rects, then calls `swap_buffers_with_damage`. A debug option highlights damage rectangles.

### Renderer

- Two backends: GLSL 3.3 and GLES2 (fallback), selected at start. All GL, no wgpu.
- Glyph atlas (`renderer/text/atlas.rs`): 1024x1024 textures; shelf packing in rows. Row height is the tallest glyph in the row; when the next glyph does not fit horizontally a new row starts above the previous; when the atlas is full a new atlas texture is appended (`Vec<Atlas>`, `current_atlas`). On font change or size change all atlases are cleared and glyphs reloaded.
- Glyph cache (`glyph_cache.rs`): a hash map keyed by (character, font key including bold/italic variant, size), "naive" by the code's own admission: keyed by `char`, so it cannot hold alternative shapings of one codepoint (hence no ligatures). All ASCII is preloaded for each style at startup. Missing glyphs are cached once under a sentinel key.
- Built-in box and block drawing (`builtin_font.rs`, about 1000 lines): box-drawing, block elements, braille-like and powerline glyphs are drawn procedurally onto a small canvas so they tile perfectly regardless of font, and are optional via `font.builtin_box_drawing`.
- Instanced drawing: per-glyph instance data is packed (u16 column and row, i16 glyph offset and size, f32 UV rect, u8 RGB foreground, plus background RGBA and a flags/alpha byte). A batch holds up to `BATCH_MAX = 0x10000` instances, flushed when full or when the active atlas texture changes. A static quad (via EBO) is instanced.
- Two passes per batch: pass 0 draws cell backgrounds (skipped when alpha 0, premultiplied), pass 1 draws glyphs. Subpixel text uses dual-source blending (`SRC1_COLOR`, `ONE_MINUS_SRC1_COLOR`) with the per-channel coverage mask as the second output, giving correct LCD blending over arbitrary backgrounds in a single pass; colour glyphs (emoji) are flagged and output with alpha as the mask.
- Rects (`rects.rs`): underlines, strikeouts, cursor shapes, bell flash, visual bell and selection-adjacent decorations drawn as a separate rect pass; underline variants (curl, dotted, dashed) are generated geometrically.
- Font rasterisation goes through `crossfont` (FreeType plus fontconfig on Linux): a C-library dependency Apotheosis must not take.

### Search and hints

- `term/search.rs`: uses `regex-automata` lazy ("hybrid") DFAs. For each pattern it builds four DFAs: forward and reverse variants for each of left and right search directions. A match is found by running the forward DFA from the origin to find the end, then the reverse DFA back from that end to find the start, which gives leftmost-longest semantics on a grid without extracting text. The DFA is stepped per UTF-8 byte of each cell's char, handles wide-char spacers (skipped), and follows wrapped lines across the `WRAPLINE` flag without copying. Search wraps around the buffer. Lazy DFA caches are retained between searches (the pattern is compiled once per search term).
- `RegexIter` yields successive matches; `visible_regex_match_iter` bounds the scan to the viewport plus a limit of 100 wrapped lines outside it (`MAX_SEARCH_LINES`).
- Other helpers in the same file: bracket matching (pairs `()`, `[]`, `{}`, `<>`), semantic word search left/right (using configurable separator chars), inline (`f`/`t` style) search, line-start/line-end search.
- Hints (`display/hint.rs`, `config/ui_config.rs`): each hint = regex, optional hyperlink matching, action (built-ins Copy, Paste, Select, MoveViModeCursor, or an external command with args), `post_processing`, `persist`, optional mouse binding, optional key binding, per-hint colours. When a hint starts, visible matches are collected, each assigned a short label from a configurable alphabet (default `jfkdls;ahgurieowpq`, every char must have width 1, at least two) by an incrementing counter with a split ratio (`HINT_SPLIT_PERCENTAGE = 0.5`) so the last character uses only part of the alphabet to reduce prefix collisions. Typed keys narrow candidates; a unique full label triggers the action. Hyperlinks (OSC 8) take precedence over regex matches. Matches are revalidated at action time (the regex is re-run at the stored point) so scrolling between display and activation cannot paste stale text. Post-processing trims trailing punctuation and unbalanced brackets from URLs. A default URL hint ships (schemes: ipfs, ipns, magnet, mailto, gemini, gopher, https, http, news, file, git, ssh, ftp) bound to Ctrl+Shift+O and mouse-clickable, launching `xdg-open`.

### Vi mode

`vi_mode.rs`: a `ViModeCursor` that is only a point; every motion is a pure function `(term, cursor, motion) -> cursor`. Motions: Up, Down, Left, Right, First, Last, FirstOccupied, High, Middle, Low, SemanticLeft/Right (and End variants), WordLeft/Right (whitespace words, and End variants), Bracket, ParagraphUp/Down. The cursor is clamped to the grid including scrollback; the viewport follows the cursor (scroll-to-cursor). Selection from vi mode uses the same Selection machinery as the mouse (simple, semantic, line, block toggles). Search is integrated (`/`, `?`, plus `SearchStart`/`SearchEnd` actions to jump to match ends). Motions are bindable actions, so users remap them in config.

### Config, reload, IPC

- Config: TOML (`alacritty.toml`) with `[general] import = [...]` (recursive imports with a depth limit; `config/mod.rs`), merge semantics. Per-field error tolerance with warnings shown in an in-terminal message bar.
- Live reload: a watcher thread (`polling`... `config/monitor.rs`) uses the `notify` crate, watches the parent directories of the config file and each imported file (so atomic-rename saves are caught), canonicalises symlinks while keeping the base path, debounces 10 ms, falls back to 1 s polling, hashes the path list to know when imports changed and the monitor must restart. On change a `ConfigReload` event reloads, replaces the shared `Rc<UiConfig>` and each window context applies it (font, colours, bindings, terminal config such as scrollback and kitty-keyboard flag). Reload failure keeps the old config and surfaces a message-bar error. CLI `-o key=value` overrides are re-applied after reload.
- IPC (`polling/ipc.rs`, `cli.rs`): Unix stream socket, path exported in `ALACRITTY_SOCKET` (printed as an export line in `--daemon` mode). Wire format: one JSON document per connection (`serde_json` of the `SocketMessage` enum), sender writes then half-closes (`shutdown(Write)`), the server replies with one JSON line for requests that have replies. Messages: `CreateWindow(WindowOptions)` (working dir, command, title, class, and `-o` option overrides), `Config(IpcConfig)` (list of `-o` options, optional `--window-id`, `-1` meaning all windows, `--reset` to clear runtime changes), `GetConfig(IpcGetConfig)` (returns the effective config of a window or the global one as JSON). The window id is also taken from the `ALACRITTY_WINDOW_ID` env var so a shell inside a window targets its own window by default. `alacritty msg` is the clap subcommand front end. There is no authentication beyond socket file permissions (unverified: path default under `$XDG_RUNTIME_DIR`).
- `alacritty migrate`: converts an old YAML config to TOML, handling imports and renamed fields; flags `--dry-run`, `--skip-imports`, `--skip-renames`, `--silent`.

## Feature inventory (by category)

VT core and input
- Kitty keyboard protocol (flags disambiguate, report event types, alternate keys, all keys as escape codes, associated text), with a per-screen mode stack (main and alt screens swap stacks), toggled in config (`kitty_keyboard`). `TermMode` bitflags enumerate DEC modes: app cursor, app keypad, mouse click/drag/motion, SGR and UTF-8 mouse, bracketed paste, focus in/out, alternate scroll, origin, insert, linefeed-newline, urgency hints.
- OSC 52 with four policies (Disabled, OnlyCopy default, OnlyPaste, CopyPaste), explicit security trade-off in the doc comment.
- OSC 8 hyperlinks (stored in `CellExtra`), underline colour (SGR 58), undercurl and dotted/dashed/double underline.
- Synchronized updates (DEC 2026) with timeout.
- Sixel, kitty graphics, inline images: not supported.
- Tabs/splits: none (`SelectNextTab` etc. exist only for macOS native window tabbing).

Selection and clipboard
- Click-drag, double-click semantic, triple-click line, right-click expand, Ctrl for block; primary selection (`PasteSelection`, `CopySelection`) and clipboard; `selection.save_to_clipboard`, `semantic_escape_chars`.

Search/scrollback
- Forward/backward regex search with live match highlight and wraparound; vi-mode search; `ClearHistory`; scroll actions (page, half page, line, top, bottom); `scrolling.multiplier`; history cap 100000.

Hints and links
- Regex hints, OSC 8 hyperlink hints, mouse-activated URL opening with required modifiers; hint colours in `colors.hints`.

Fonts and rendering
- Normal/bold/italic/bold-italic families; font size actions; `builtin_box_drawing`; glyph offset and cell offset adjustments; subpixel AA on; no ligatures; fallback via fontconfig.
- Transparency (`window.opacity`), blur request on Wayland, decorations choices, dynamic padding, startup mode (maximized, fullscreen), cursor styles with vi-mode cursor style and blinking with timeouts, visual bell with animation and duration, bell command.

Config and automation
- Imports, live reload, `-o` CLI overrides, IPC runtime overrides, `msg create-window` (multi-window in one process and daemon mode), `migrate`.
- Key bindings with modes (`~AppCursor`, `Alt`, `Vi`, `Search`, `~Search` conditions) and actions: `Esc`/`Command`/`Hint`/`ViMotion`/`Vi(...)`/`Search(...)`/`Mouse(...)` plus Paste, Copy, CopySelection, PasteSelection, font size, scroll, ClearHistory, Hide, Minimize, Quit, ClearLogNotice, SpawnNewInstance, CreateNewWindow, ToggleFullscreen, tab selection (macOS).

## Control surface

- Config: single TOML file; sections `general`, `window`, `scrolling`, `font`, `colors`, `bell`, `selection`, `cursor`, `terminal` (`osc52`, shell), `mouse`, `hints`, `keyboard`, `debug`. No GUI. Reload automatic (`general.live_config_reload`).
- Actions: closed enum in `config/bindings.rs`; bindable to keys, mouse, and hints. Actions with arguments are limited to `Esc(string)` (send bytes), `Command(program, args)`, `ViMotion`, `Hint`. There is no event-to-action rule system.
- CLI: `alacritty [-o key=value]... [--class] [--title] [-e cmd] [--working-directory] [--daemon]`; `alacritty msg create-window|config|get-config`; `alacritty migrate`.
- IPC: see above; JSON, one request per connection, JSON-line reply, no event stream or subscription, no query for terminal content or state.

## Performance (numbers + method)

- Alacritty's own claim is qualitative (README FAQ): "consistently score better than the competition" on vtebench; it explicitly says latency and frame consistency are harder to quantify and that some terminals intentionally throttle. No numbers are published in the repo (verified by reading README and CHANGELOG; grep found none).
- vtebench method (vtebench README and `benchmarks/`): each benchmark is a directory with an executable `benchmark` that writes its payload to stdout and an optional `setup`; the tool repeats the output to reach a minimum sample size, feeds it through the terminal's PTY and measures wall time to consume it. It reports per-benchmark samples and supports `.dat` output plus gnuplot SVG scripts. Default suite: `cursor_motion`, `dense_cells`, `light_cells`, `medium_cells`, `scrolling`, `scrolling_bottom_region`, `scrolling_bottom_small_region`, `scrolling_fullscreen`, `scrolling_top_region`, `scrolling_top_small_region`, `sync_medium_cells`, `unicode`. The README disclaimer: it measures only PTY read throughput, not frame rate or latency.
- Structural budget constants worth recording: PTY read buffer 1 MiB; max locked parse slice about 64 KiB; renderer batch cap 65 536 instances; atlas texture 1024x1024; scrollback cap 100 000 lines; config debounce 10 ms; watcher poll fallback 1 s; hint matches scanned within viewport plus 100 wrapped lines.
- Techniques credited for speed: ring-buffer scrolling, `occ`-bounded row ops, line-granular damage, parse under lock with bounded slices, no per-frame allocations in the draw loop (batch reuse), instanced quads, glyph atlas, damage-limited swap.

## What Apotheosis should take

1. Keep the terminal core a separable crate with no GUI types; drive it from a PTY thread and render from a snapshot or short lock.
2. Ring-buffer row storage with a rotating origin, and `occ` high-water mark per row.
3. Small cells with rarely-used data behind an optional shared extra block; WRAPLINE-flag-based reflow.
4. Bounded parse slices, a big read buffer, fair lock or equivalent hand-off so rendering never starves; coalesce wakeups, skip redraw inside synchronized updates.
5. Line-range damage from the core to the renderer to the compositor (wgpu: use Wayland `wl_surface.damage_buffer` and track buffer age or per-frame full redraw with scissor).
6. Lazy-DFA regex search straight on the grid with forward-plus-reverse DFA match location; cap scan area for highlighting.
7. Hints as first-class: regex + label alphabet + action list; revalidate at activation; OSC 8 precedence; URL post-processing. Make hint definitions pure KDL data with Actions.
8. Vi-style copy mode as pure motion functions bound to actions, sharing the selection machinery.
9. Procedural box-drawing and powerline glyphs for pixel-perfect tiling.
10. Dual-source-blend (or equivalent) subpixel text path; two-pass bg then glyph; instance struct packing.
11. Config watcher design: watch parent dirs, canonicalise symlinks, debounce, restart on import change, keep old config on error, show errors in UI.
12. IPC ideas: per-window targeting via env var, `GetConfig` introspection, `Config` runtime overrides with `reset`, window/instance creation over socket.
13. Per-field error tolerance in config with in-UI warnings.
14. OSC 52 policy modes, default copy-only.
15. vtebench as the throughput regression suite, adding frame-time and latency tests it admits it lacks.

## What it should avoid

- TOML, the bespoke derive, and absence of a settings GUI; Apotheosis uses KDL and must expose every setting in the GUI.
- OpenGL, glutin, winit, and `crossfont` (FreeType/fontconfig C libraries); Apotheosis has its own Wayland client and wgpu.
- A glyph cache keyed only by `char`: it blocks ligatures, grapheme clusters, and variable shaping. Key by (font face, glyph id, size, subpixel offset, style) after shaping.
- One-request-per-connection IPC without events, subscriptions, content queries or an action-dispatch message; Apotheosis needs the shared Action vocabulary over the socket.
- A closed action enum with only string/command escape hatches; actions need typed arguments shared with rules/profiles/layouts.
- No mux: Apotheosis owns tabs and splits.
- Unauthenticated socket assumptions; add peer credential checks (`SO_PEERCRED`) and scoped capabilities.
- Per-glyph per-instance data sized for GL2-era constraints; with wgpu, use storage buffers and a cell-grid shader if it simplifies, but profile first.
- Do not claim speed from vtebench alone; its authors say it measures only PTY read throughput.

## Sources

Docs
- README (FAQ on tabs, GUI, benchmarking, licence): https://github.com/alacritty/alacritty/blob/master/README.md
- Feature guide (vi mode, search, hints, selection expansion, multi-window): https://github.com/alacritty/alacritty/blob/master/docs/features.md
- Escape sequence man page: https://github.com/alacritty/alacritty/blob/master/extra/man/alacritty-escapes.7.scd
- vtebench README and benchmarks: https://github.com/alacritty/vtebench

Source (upstream path and function or type)
- `alacritty_terminal/src/grid/storage.rs` (Storage ring, rotate, MAX_CACHE_SIZE), `grid/row.rs` (Row occ), `grid/resize.rs`
- `alacritty_terminal/src/term/cell.rs` (Cell, CellExtra, Flags), `term/mod.rs` (TermMode, Osc52, keyboard_mode_stack, damage, reset_damage, LineDamageBounds, TermDamageIterator)
- `alacritty_terminal/src/event_loop.rs` (READ_BUFFER_SIZE, MAX_LOCKED_READ, pty_read, sync timeout handling), `sync.rs` (FairMutex)
- `alacritty_terminal/src/term/search.rs` (RegexSearch, LazyDfa, regex_search_left/right, regex_search, bracket_search, semantic_search_*, inline_search_*, RegexIter)
- `alacritty_terminal/src/vi_mode.rs` (ViMotion, ViModeCursor::motion), `selection.rs` (SelectionType, Selection)
- `alacritty/src/display/hint.rs` (HintState, HintLabels increment, visible_regex_match_iter, HintPostProcessor, regex_match_at), `display/damage.rs` (DamageTracker, shape_frame_damage, RectDamageIterator, merge_rects), `display/mod.rs` (swap_buffers_with_damage)
- `alacritty/src/renderer/text/atlas.rs` (Atlas::insert, ATLAS_SIZE), `glyph_cache.rs` (GlyphCache::get), `glsl3.rs` (InstanceData, BATCH_MAX, blend func), `builtin_font.rs`, `res/glsl3/text.f.glsl`, `res/glsl3/text.v.glsl`, `renderer/rects.rs`
- `alacritty/src/config/monitor.rs` (ConfigMonitor::new, DEBOUNCE_DELAY, FALLBACK_POLLING_TIMEOUT), `config/mod.rs` (load, reload, load_imports), `config/ui_config.rs` (Hints, URL_REGEX, HintsAlphabet, HintInternalAction), `config/bindings.rs` (Action), `config/scrolling.rs` (MAX_SCROLLBACK_LINES)
- `alacritty/src/polling/ipc.rs` (IpcListener::process_message, send_message, handle_reply), `cli.rs` (SocketMessage, IpcConfig, IpcGetConfig, WindowOptions, MigrateOptions), `event.rs` (ConfigReload handling)

Unverified
- Default scrollback value (10 000) and default socket path location.
- Exact synchronized-update timeout value (lives in the `vte` crate, not read).
- Any numeric vtebench results (none published in repo).
