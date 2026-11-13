# Prior art: Ghostty

Subject: https://github.com/ghostty-org/ghostty (MIT; Zig; tip checked out at commit cf3a8a6, `build.zig.zon` version 1.4.1-dev, min Zig 0.17.0). Source read from a shallow clone at `/home/risk/.cache/apotheosis-prior-art/ghostty`. Paths below are upstream-relative. Techniques only; no code reproduced. Anything not verified is marked "unverified".

## Summary

Ghostty is a terminal split into a renderer-agnostic, GUI-agnostic core (`src/terminal`, shipped as libghostty-vt) and per-platform app runtimes (Swift/AppKit on macOS, GTK4 + libadwaita on Linux). The core's distinguishing ideas, all directly relevant to Apotheosis:

1. Scrollback is a linked list of fixed-size, mmap-backed, offset-addressed "pages" that are self-contained and relocatable. Cells are 64-bit, styles are interned per page with reference counts, and graphemes, hyperlinks and strings live in per-page side tables. Cold pages are LZ4-compressed and their physical memory is released while the virtual mapping is kept.
2. "Pins" (node + row + col) are the stable reference type for viewport, cursor, selection, search hits and prompt marks. Tracked pins are fixed up when pages are pruned, reflowed or moved.
3. The VT hot path is batch-oriented. Ground-state text is decoded in bulk (SIMD UTF-8 to codepoints), split into printable runs, and written to the grid as runs per width class. OSC and APC payloads are bulk-scanned. The byte-at-a-time table-driven state machine is the slow path.
4. A render-state snapshot layer (`terminal/render.zig`) copies only what the renderer needs, under a short lock, and tracks per-row dirty flags so the GPU cell buffers are rebuilt row-wise.
5. A uniform "binding action" vocabulary drives keybinds, the command palette, menus, chained actions, key sequences and key tables, and a handful of one-shot CLI/D-Bus IPC calls. There is no scripting language. This is the closest existing design to what Apotheosis wants.
6. Ghostty's perf claims are qualitative. There is no published benchmark table. The in-repo benchmark harness is rigorous in method, though.

Caveat: Ghostty's tip has moved well beyond what is commonly described in older write-ups (scrollback compression, snapshots, tmux control-mode viewer, Glyph Protocol, kitty text sizing/DnD/clipboard, search thread, undo, key tables, command palette). Everything here was read from the tip source, not from blog posts.

## Architecture (how it is built)

### Layering
- `src/terminal/`: VT core, no renderer or GUI dependency. Key files: `Parser.zig` + `parse_table.zig` (vt100.net DEC ANSI state machine, table generated at comptime, with `:` allowed in CSI params for SGR sub-params), `stream.zig` (parser plus handler dispatch plus fast paths), `Terminal.zig` (print/erase/scroll semantics), `Screen.zig`, `ScreenSet.zig` (primary/alternate), `PageList.zig`, `page.zig`, `style.zig`, `ref_counted_set.zig`, `bitmap_allocator.zig`, `Selection.zig`, `highlight.zig`, `render.zig`, `search/`, `kitty/`, `tmux/`, `snapshot/`, `osc/parsers/`.
- `src/termio/`: pty, subprocess (`Exec.zig`), shell-integration injection, and the IO thread; a separate writer thread (`Thread.zig`) handles pty writes, resize coalescing (25 ms), synchronized-output timeout (1000 ms), selection auto-scroll tick (15 ms).
- `src/renderer/`: generic renderer parameterised over a graphics API (Metal, OpenGL). `cell.zig` holds the GPU-facing cell contents. Shaders in `renderer/shaders/glsl` and `.metal`. Custom shadertoy-style user shaders are supported.
- `src/font/`: `Collection`, `CodepointResolver`, `SharedGrid` (shared across surfaces, with glyph and codepoint caches and two atlases, grayscale and color), shapers (HarfBuzz, CoreText), faces (FreeType, CoreText), a procedural `sprite/` face (box drawing, powerline, etc.), and a generated Nerd Font constraint table.
- `src/apprt/`: runtime abstraction. GTK runtime handles windows/tabs/splits, layer-shell quick terminal, KDE blur and slide protocols, `ext_background_effect`, xdg-activation, D-Bus IPC (`apprt/gtk/ipc/`).
- `src/simd/`: C++ (Highway + simdutf) for UTF-8 decode, index_of, codepoint width, base64. Newer paths in `stream.zig` use Zig's portable `@Vector` instead. This is the part we cannot copy literally since our build forbids C++.

### Threads
Per terminal surface: IO (read) thread owns parser and mutates the terminal under a mutex; a renderer thread builds frames; a termio writer thread; an optional search thread. Shared state is guarded by one terminal mutex. The render path takes it only for `RenderState.beginUpdate`, then finishes with `endUpdate` on memory it owns, so the IO thread is unblocked. Source: `terminal/render.zig` header comments; `termio/Thread.zig`; `renderer/generic.zig` (draw mutex, swap chain).

### Page-based scrollback
- A `Page` is one contiguous page-aligned zeroed mmap block containing: rows array, cells array (row-contiguous, column-ordered, cache-line aligned), a grapheme bitmap allocator and map, a string bitmap allocator, a style set, hyperlink set and map. All internal references are offsets from the block start, so the whole block can be memcpy'd, serialized, compressed or relocated without pointer fix-ups. (`terminal/page.zig`)
- `Cell` is a packed 64 bits: 2-bit content tag (codepoint, codepoint-in-grapheme-cluster, bg-only palette color, bg-only RGB), 24-bit content, style id, wide/spacer state (2 bits), protected flag, hyperlink flag, semantic-content (prompt/input/output). Background-only cells do not consume style-table entries.
- `Row` is a packed 64 bits: offset to its cells, wrap flag, wrap-continuation flag, grapheme flag, "styled" flag, hyperlink flag, semantic-prompt state. The styled, grapheme and hyperlink flags are one-sided hints that allow false positives but never false negatives. Their purpose is to give erase/insert/delete line operations a fast path (comment cites around 4x for unstyled rows) and to speed jump-to-prompt.
- Standard page capacity is 215 columns by 215 rows with 128 styles and 8 KiB grapheme bytes, and uses a pooled allocator. Pages needing more of any dimension get a bigger heap/mmap page. `grow` carries forward a ring of hints about what the last several full pages needed, so a new page matches recent density instead of just the last page (`PageList.growCapacity`, `grow_hints`). Trimming rows to fit a pool item is bounded to keep at least 90 percent of the rows-per-byte of an unconstrained page; the comment records that using heap pages instead was about 17 percent slower in one case.
- Scrollback limits: `scrollback-limit-bytes` (default 50,000,000, measured on the uncompressed logical size, includes the active area) and `scrollback-limit-lines`; whichever is hit first prunes whole historical pages. A page overlapping the active area is never split to satisfy a limit.
- Compression: `terminal/compress/lz4.zig` is a hand-written, allocation-free raw LZ4 block codec. `compress/Page.zig` stores the full `Page` metadata value plus the encoded block, keeps the virtual mapping, and asks the OS to discard physical pages (documented as `MADV_DONTNEED` on Linux). Restore recommits the same address range and decodes in place, so restoration cannot fail on allocation. `PageList` runs an incremental, idle-driven compression pass over complete historical pages that do not intersect the viewport; it repeats until a full verification pass compresses nothing, and re-arms from an activity serial counter. Config doc claims text-heavy history compresses to about 10 to 30 percent of raw page size. Design notes in `compress/AGENTS.md`: priorities are ratio on real page dumps, then decode speed, then encode speed; wide copies are the only SIMD that paid off; vectorised compares measured as net losses because matches are short.
- Pins: `Pin{node, y, x, garbage}`. `PageList.tracked_pins` is a set of pins that every mutating operation (prune, reflow, erase, clone) fixes up; `garbage` is set when the pinned page was pruned and no sensible relocation exists. Pins back the viewport, the cursor, selections, search matches, and prompt regions. Nodes carry a serial so a pin can be validated in O(1) even when the allocator reuses an address. Resize with reflow keeps tracked pins and can blank from a pin to the bottom (used for prompt redraw on resize).
- Reflow on column resize recycles one source node for the next destination page to avoid the decommit/recommit syscall pair per page (`recycle_node`).

### Style interning
`terminal/style.zig`: a style is a packed 128-bit value (three 32-bit tagged colors: none/palette/rgb, plus 16 bits of flags incl. underline kind). Zero bits are the default style, id 0, so default cells need no lookup. Styles live in a per-page `RefCountedSet` (`ref_counted_set.zig`): open-addressed Robin Hood hash table with linear probing at max load 0.8125, over a flat item array; ids are indices into the item array so refcount updates avoid re-hashing; dead items are reclaimed lazily when their id or slot is needed; id 0 reserved. When a page's set fills, the page capacity is grown (page is cloned larger), which is rare. Hyperlinks use the same structure plus a cell-offset to link-id map; strings (OSC 8 id/uri) use a 32-byte-chunk bitmap allocator; graphemes use a 4-codepoint (16-byte) chunk bitmap allocator with a hard cap of 64 codepoints per cluster.

### SIMD and bulk parsing
- Ground-state fast path (`stream.zig: nextSliceCapped`): finish any in-flight scalar UTF-8 or escape state first, then loop: bulk-decode UTF-8 up to the next ESC (0x1B) into a u32 buffer (4096 entries per chunk), or, if every byte was ASCII, skip the decode and read the input bytes directly. The result carries `consumed`, `decoded` and an `ascii` flag. A partial trailing UTF-8 sequence is handed to a scalar DFA decoder (Hoehrmann style, with W3C maximal-subpart U+FFFD replacement, `UTF8Decoder.zig`; the C++ path implements the same maximal-subpart rule per Unicode Table 3-7).
- `printRuns`: splits the decoded stream into printable runs and executes C0 controls between runs. A single AND-mask test classifies both C0 and C1 ranges for u32 codepoints (`(cp & ~0x9F) == 0`). C1 controls arriving via UTF-8 are ignored. The run-end search is hand-vectorised with `@Vector` because the compiler does not auto-vectorise early-exit loops.
- `Terminal.printSlice` and friends: specialised at comptime by width class (narrow, wide) and by whether grapheme clustering (mode 2027) and styling are active. Codepoints <= 0xFF are always width 1 and cannot interact with clustering, so no width-table lookup is needed. Runs are batch-filled into cells; a width transition ends a batch. Per-row flag updates are amortised per run.
- OSC and APC payload bulk-consume: vector compare for bytes that terminate or abort the string (BEL/CAN/SUB/ESC; for APC also >= 0x80), then hand the whole slice to the OSC/APC sub-parser.
- `stream.nextSliceUntil(input, boundaries)`: runs the same parse but stops after a requested boundary (e.g. ground, string start/end), so a proxy/multiplexer can find exact control-string edges with the real parser.
- Unicode property lookup: three-stage codepoint tables (`unicode/lut.zig`, 256-codepoint pages, generated at build from the `uucode` library, MIT AND Unicode-3.0). Properties include width (0/1/2), grapheme-break class and symbol/emoji flags. `unicode/grapheme.zig` provides break and width-effect logic.
- C++ Highway/simdutf are used for `index_of`, `utf8DecodeUntilControlSeq` and `codepointWidth` with runtime dispatch across targets. Newer code prefers portable vectors. The `memset` replacement (`quirks_memset.zig`) was written because Zig 0.16 dropped LLVM auto-vectorisation, which had made ASCII stream throughput 2.8x slower; lesson: do not rely on auto-vectorisation for hot loops.

### Rendering
- One generic renderer over GraphicsAPI (Metal, OpenGL), with the hierarchy API > Target > Frame > RenderPass > Step > Pipeline, so a second backend (e.g. wgpu) is a bounded task. Swap chain of multiple frames in flight.
- `renderer/cell.zig` `Contents`: flat `bg_cells` array indexed `row*cols+col` (the bg shader reads it as a storage buffer) plus per-row foreground lists (text, underline, strikethrough, overline), with list 0 reserved for the cursor so it draws first. Row-wise clear enables row-wise dirty rebuilds.
- Text vertex: glyph atlas position/size, bearings, grid position, color, atlas selector (grayscale or color), glyph flags (no-min-contrast, cursor glyph). The vertex shader draws instanced quads (triangle strip, four vertices from `gl_VertexID`) and the fragment shader reads background colors for contrast.
- Font: `SharedGrid` caches codepoint to font index and glyph renders; atlases are grow-only. Box drawing, powerline, braille, block elements, cursor shapes and underline styles are drawn procedurally into the atlas (`font/sprite`), sprites addressed by pseudo-codepoints above U+10FFFF. Nerd Font symbol glyphs are scaled/aligned to cell via a generated constraint table (`font/nerd_font_attributes.zig`).
- Colour: `alpha-blending` = native / linear / linear-corrected (corrected linear blending gives native-looking text without fringe darkening). `minimum-contrast` applies WCAG ratio to fg vs bg per cell, except emoji and images. Custom user shaders (shadertoy-compatible prefix) with ping-pong textures.
- Ligatures: shaping runs are broken under the cursor by default (`font-shaping-break`, documented as currently macOS-only).
- Images: kitty graphics with animation. Animation frames are composed eagerly to full-size RGBA (documented trade of memory for simplicity, bounded by the same byte limit). Default image storage limit 320,000,000 bytes (`image-storage-limit`).

### Search
`terminal/search/`: a dedicated thread with a 24 ms refresh timer. `sliding_window.zig` encodes pages' text into a circular byte buffer and slides across page boundaries, keeping just enough overlap to fit the needle; it supports forward and reverse order and returns selections as pin pairs. It copies data to minimise lock time, but still takes the global terminal lock. The source TODO list is candid: no n/total progress, no refresh pause when unfocused, notifications not coalesced.

### Snapshots and tmux
- `terminal/snapshot/`: a versioned binary record format with envelope, per-record types (terminal, screen, page, style, hyperlink, history, kitty, checkpoint, continuation), a Kaitai Struct description and a verifier. Principle in its AGENTS.md: validate on encode, degrade gracefully on decode. Intended for persistence and handoff.
- `terminal/tmux/`: tmux control-mode (DCS 1000p) parser, layout parser and a `Viewer` state machine that mirrors a tmux session's windows/panes locally. The source notes fragility against tmux internals as of Dec 2025.

### Shell integration injection
`termio/shell_integration.zig`: detect shell from the command basename (bash, elvish, fish, nushell, zsh) or forced by config, then inject without touching user dotfiles:
- bash: relaunch with `--posix` and point `ENV` at the integration script; the script restores normal startup file behaviour and turns POSIX mode off. Original `--rcfile`, `--norc`, `--noprofile`, `ENV` and `HISTFILE` handling is passed via `GHOSTTY_BASH_*` variables and restored.
- zsh: temporarily set `ZDOTDIR` to the bundled dir, remember the previous value, restore it after our `.zshenv` runs. Fails if a system-wide `/etc/zshenv` overrides `ZDOTDIR` (documented). Requires zsh 5.1+.
- fish, elvish: prepend the bundled dir to `XDG_DATA_DIRS` (vendor_conf.d / lib autoload), record it in `GHOSTTY_SHELL_INTEGRATION_XDG_DIR` and strip it afterwards.
- nushell: same XDG trick plus `-e "use ghostty *"`.
- Features are passed as a sorted comma list in `GHOSTTY_SHELL_FEATURES` (`cursor:blink|steady`, `path`, `ssh-env`, `ssh-terminfo`, `sudo`, `title`). The scripts emit OSC 133 A/B/C/D (with `cl=line`, `k=s|i` kinds, exit status on D, guard so an unclosed C is closed with D, and `P` instead of `A` to avoid fresh-line behaviour on PS1 redraw), OSC 7 for cwd, and title updates. A `sudo` shell function wrapper preserves terminfo; ssh wrappers set TERM fallback and install terminfo remotely with `tic` and a local cache (`+ssh-cache`).
- No integration is injected when running an explicit `-e` command (safety).

### Quick terminal
Config keys `quick-terminal-position|size|screen|animation-duration|autohide|space-behavior|keyboard-interactivity`, `gtk-quick-terminal-layer|namespace`. On Wayland it uses wlr-layer-shell; if the compositor lacks the protocol the feature is disabled with a warning. Toggled by the `toggle_quick_terminal` action (no default bind), by `ghostty +toggle-quick-terminal` over D-Bus, or by a `global:` keybind. Global shortcuts on Linux use the `vicinae-hotkey-v1` Wayland protocol where available, else the XDG GlobalShortcuts portal. macOS is where this feature originated; Linux depends on compositor support.

## Feature inventory (grouped)

### VT and protocol coverage
- xterm-class VT with DEC modes, alt screen, scroll regions (including left/right margins), tab stops, charsets, DECRQM, DA1/2/3, XTVERSION, XTGETTCAP, XTCHECKSUM (`vt-xt-checksum-report`), `enquiry-response`, `vt-kam-allowed`, `vt-window-resize-allowed`.
- Kitty keyboard protocol (five flags, push/pop stack of 8 in `terminal/kitty/key.zig`), xterm modifyOtherKeys 2, DECBKM, modes 1035/1036.
- Kitty graphics (with animation), kitty text sizing (OSC 66: scale 1 to 7, width 0 to 7, fractional numerator/denominator, valign, halign), kitty clipboard (OSC 5522, with a documented list of reference-implementation behaviours), kitty DnD (OSC 72; receive-only, local-only, explicit divergence list), kitty colour (OSC 21), kitty desktop notifications, kitty OSC 99 family (parsers dir lists `kitty_desktop_notification`), iTerm2 OSC 1337 subset, OSC 9 / 9;4 progress, OSC 7 pwd, OSC 8 hyperlinks (+ id), OSC 52 clipboard, OSC 133 semantic prompts, OSC 22 mouse shape, OSC 3008 hierarchical context (parser present; unverified how used).
- Glyph Protocol (APC, id `25a1`, verbs s/q/r/c): apps register custom glyphs as `glyf`, `colrv0` or `colrv1` payloads at runtime to avoid patched fonts. Experimental and Ghostty-specific, worth tracking.
- Mode 2027 (grapheme clustering), synchronized output (2026) with 1 s safety reset, bracketed paste, focus events, mouse modes (X10, 1000, 1002, 1003, UTF8, SGR, URxvt, SGR-pixels), `mouse-shift-capture`.
- tmux control mode viewer.
- libghostty-vt: the VT core exported as a C library, also targeting wasm32-freestanding.

### Unicode
- Per-codepoint width 0/1/2 table, grapheme break property, VS15/VS16 handling in `Terminal.print` (explicit 0xFE0E/0xFE0F branches), `grapheme-width-method = unicode|legacy`; a program that sets mode 2027 forces unicode width. Documented tension: unicode width can desync cursors with programs still using wcswidth.
- Fast path excludes anything that could be a grapheme continuation (zero-width, joiners, variation selectors) and, under 2027, anything that is not a grapheme break against the previous cell.

### Selection, links, clipboard
- Pin-based selection (rectangle and line modes), word boundaries configurable by `selection-word-chars`, `selection-clear-on-typing|copy`, `copy-on-select`, trailing space trim, `right-click-action`, `middle-click-action`.
- `adjust_selection`, `select_all`, `scroll_to_selection`, `copy_url_to_clipboard`, `write_*_file` (scrollback/screen/selection to file or open in editor).
- Links: default URL matcher, OSC 8 (`link-osc8`), `link` regex entries mapped to open or any binding action (the doc says setting it is not yet possible, so it is effectively a built-in only), `link-previews`.
- Clipboard safety: `clipboard-read|write` allow/deny/ask, `clipboard-write-limit-bytes` (64 MiB default), `clipboard-paste-protection`, `clipboard-paste-bracketed-safe`, `clipboard-codepoint-map` (substitute codepoints on copy).
- Prompt navigation: `jump_to_prompt:N` via OSC 133 row flags; click-to-move cursor (`cursor-click-to-move`) using prompt/input marks.

### Fonts
- `font-family` with per-style families, `font-style*`, `font-synthetic-style`, `font-feature`, `font-variation*`, `font-codepoint-map` (per-range font override), `font-thicken`, `adjust-*` (cell size, baseline, underline, strikethrough, overline, cursor, box thickness, icon height), `freetype-load-flags`, `font-shaping-break`, procedural sprites and Nerd Font constraints, colour emoji atlas.

### Windowing and UI (GTK/macOS)
- Windows, tabs, splits (`new_split`, `goto_split`, `resize_split`, `equalize_splits`, `toggle_split_zoom`), `unfocused-split-opacity|fill`, tab overview, tab/window/surface title prompts, command palette (built-in plus `command-palette-entry`), `background-opacity`, `background-blur`, `background-image*`, window save state, `window-padding-*` with balance, step-resize, resize overlay, secure keyboard entry, `notify-on-command-finish` (never/unfocused/always, needs OSC 133), bell features (audio, attention, title, border), desktop notifications, progress style (OSC 9;4), undo/redo of close operations with `undo-timeout` (macOS only), inspector (terminal state inspector built on Dear ImGui via dcimgui).
- Themes (`theme = light:X,dark:Y`), `palette-generate` and `palette-harmonious` (derive the 256 colour cube from base 16, optional inversion), `cursor-*`, `selection-*`, `search-*` colours, `bold-color`, `faint-opacity`.
- Linux: per-surface transient systemd scope (`linux-cgroup`, with memory and process limits) so one runaway shell cannot take down the app.

### Process and environment
- `command`, `initial-command`, `env`, `working-directory`, `window-inherit-working-directory` / tab / split variants, `wait-after-command`, `abnormal-command-exit-runtime`, `input` (preload bytes), `term` (default `xterm-ghostty`, ships its own terminfo), `shell-integration`, `shell-integration-features`.

## Control surface (config, actions, IPC)

### Config
- Plain `key = value` file with repeatable keys, `config-file` includes (optional `?` prefix, per upstream docs; unverified here), `config-default-files`, CLI overrides as `--key=value`, `+validate-config`, `+show-config`, `+explain-config` (runtime help generated from doc comments via `helpgen`), `+list-fonts|themes|keybinds|actions|colors`. Config reload via the `reload_config` action. Deprecated/renamed keys are handled by a compatibility table (e.g. `scrollback-limit` renamed to `scrollback-limit-bytes` in 1.4).
- Doc comments in the Zig struct are the single source of truth for docs, `--help`, and `+explain-config`. Each key states "Available since: X".
- Gaps relevant to us: no GUI settings editor on Linux beyond opening the file; no schema validation beyond per-key parse errors; no per-profile or per-event configuration.

### Keybinds (syntax worth copying)
- Form: `keybind = [prefix:]*trigger=action[:param]`. Triggers join modifiers and one key with `+` in any order (`ctrl`, `alt`/`opt`/`option`, `shift`, `super`/`cmd`/`command`; duplicates invalid; sided names only for `key-remap`).
- Keys: unicode character (matches the produced character, layout-aware), `physical:KeyA` style names (W3C code names, layout-independent), or `catch_all`. Physical beats unicode if both are set. `catch_all` lookup tries with modifiers first, then without.
- Sequences: `ctrl+a>n=new_window`. No hard length limit. No timeout; the pending sequence waits indefinitely. Unbound key mid-sequence: the whole sequence is replayed to the program unless a matching `catch_all=ignore` exists. Binding a prefix as a leaf removes the sequences under it, and vice versa.
- Prefixes: `all:` (every surface), `global:` (OS-wide; implies all; no sequences), `unconsumed:` (still send the key to the program), `performable:` (consume only if the action can run, e.g. copy with no selection; also resets a sequence if not performable).
- Special actions: `ignore`, `unbind`, `csi:`, `esc:`, `text:` (Zig string-literal escapes), `end_key_sequence`.
- Chains: `keybind = chain=<action>` appends to the most recent binding, works with sequences and tables and shares the original flags.
- Key tables: `name/trigger=action`; actions `activate_key_table:<name>`, `activate_key_table_once:<name>`, `deactivate_key_table`, `deactivate_all_key_tables`. Lookup runs innermost to outermost; `name/` alone clears a table; a table cannot be activated if it is already innermost; one-shot tables deactivate on the first non-catch-all binding; sequences inside tables leave the table active on a bad key. This enables vim-like copy mode purely declaratively.
- `key-remap = from=to` for modifiers (sided or generic, non-transitive, affects both bindings and encoding).
- Commands in the palette are just titled actions (`input/command.zig`: action + title + description); any command is bindable. Custom entries via `command-palette-entry`.

### Action vocabulary (`input/Binding.zig` Action union; counted by reading the enum)
About 100 actions, grouped: input (`text`, `csi`, `esc`, `cursor_key`, `reset`), clipboard (copy/paste/selection/url/title), font size, search (`start_search`, `search:<text>`, `navigate_search`, `end_search`, `search_selection`), scroll (`scroll_to_top|bottom|row|selection`, page and fractional page and line), `jump_to_prompt`, `adjust_selection`, file export (`write_scrollback_file`, `write_screen_file`, `write_selection_file`), tab/window/split management, title setting and prompts, `toggle_*` (fullscreen, maximize, decorations, float on top, secure input, mouse reporting, command palette, quick terminal, visibility, background opacity, readonly, split zoom), config (`open_config`, `reload_config`), undo/redo, key table control, close variants, quit, `crash` (for testing). Parameters are typed per action (enums, ints, floats, strings).

### IPC
- Minimal. Linux: D-Bus via GApplication actions (`new-window`, `new-window-command`, `new-tab`, `toggle-quick-terminal`), invoked as `ghostty +new-window [-e ...]`, targeting an instance by class or auto-detect. macOS: AppleScript (`macos-applescript`). There is no JSON control socket, no `send-text`, no `get-text`, no layout query. This is a clear gap that Apotheosis's socket+CLI should fill.
- `libghostty` C API exposes embedding actions/commands but is not an IPC.

### Security-relevant knobs
`clipboard-*`, `vt-xt-checksum-report`, `title-report` (default off upstream per docs; unverified here), `vt-window-resize-allowed`, `vt-kam-allowed`, paste protection, `image-storage-limit`, `app-notifications`, cgroup isolation, secure keyboard entry on macOS.

## Performance (numbers and method)

Published numbers: none that are verifiable.
- README (`README.md` "Competitive Performance"): "Ghostty and Alacritty are usually within a few percentage points of each other on various benchmarks, but are both something like 100x faster than Terminal.app and iTerm." No hardware, workload, or tool is stated. Treat as marketing, unverified.
- ghostty.org/docs/about (fetched): states the aim is the same class as the fastest terminals, says it is faster in some benchmarks and slower in others, and says detailed benchmarks are planned. No numbers.

In-repo measured facts (from code comments, so author-measured, not independently reproduced):
- Row `styled` hint flag: unstyled-row erase/insert/delete fast path about 4x faster (`terminal/page.zig`, `Row.styled`).
- `screen-clone` plain-text fast path (raw cell copy when no managed memory): about 300 percent faster (`terminal/render.zig`, ~line 2725).
- Compressed scrollback: text-heavy pages compress to about 10 to 30 percent (config doc); LZ4 page data decodes as millions of tiny ops (mostly zero literals plus a 4 to 18 byte match), so per-item overhead dominates.
- memset replacement: 1.5x to 2x faster for 192 B to 32 KiB fills and about 4x for zero fills of 1 MiB or more (aarch64, versus Zig compiler_rt at that time); the loss of auto-vectorisation had made `+terminal-stream` 2.8x slower on ASCII.
- Page standard size: about 400 KiB on ReleaseFast (from `compress/AGENTS.md`).
- Heap versus pooled pages: using heap pages for a tall-terminal case instead of staying one pool item over limit was about 17 percent slower.
- Code alignment sensitivity: CSI-heavy input runs 3 to 5 percent faster at some function start addresses on Apple Silicon; the benchmark pins `align(64)` so branch comparisons measure the code, not the linker.

Method (`src/benchmark/AGENTS.md`, `src/benchmark/*.zig`, `src/synthetic/`):
- Two tools: `ghostty-gen` (synthetic data: ASCII, UTF-8, OSC, kitty, random bytes, with fixed seeds) and `ghostty-bench` (consumes a pre-generated file). Never pipe generator into bench; generate once, reuse identical files across revisions.
- Build with ReleaseFast. Compare binaries with `hyperfine` with warmups and repeated runs, use medians, never run benchmarks in parallel, keep terminal dimensions and flags identical.
- Bench set: terminal parser, OSC parser, APC parser, full terminal stream (readonly handler, closest to the real IO thread; options for rows, cols, grapheme-width, scrollback max bytes since defaults differ 10 KB library vs 50 MB app and can swing results 2x), terminal resize/reflow, screen clone, scrollback compression, page compression (compress/decompress/store/report modes, real page dumps as corpus), codepoint width, grapheme break, hyperlink map, shaper rows, snapshot encode/decode, formatter.
- Codec method: differential property suite (round trip, independent format walker, corruption/truncation), min-of-N in a standalone harness, one change at a time, re-measure final state, run-to-run noise a few percent.
- What is missing: no end-to-end latency (keypress to photon) numbers, no frame-time or input-lag methodology, no cross-terminal comparison methodology (e.g. vtebench settings).

## What Apotheosis should take

1. Self-contained, offset-addressed pages as the scrollback unit, with a 64-bit cell, interned styles and side tables. In Rust this maps to a `Box<[u8]>`/mmap block with `u32` offsets and safe accessors, or a struct-of-arrays page with index handles that is `Clone`/`Send`. This is the foundation that makes snapshots, compression, background search and render snapshots cheap.
2. Pins/tracked-pin set with page serials and a `garbage` state, as the only way to reference positions that survive reflow and pruning. Fits our handle-based, no `Rc<RefCell>` house style.
3. Per-page ref-counted style table (Robin Hood, load 0.8125, id as index, id 0 default, lazy reclamation). Keep the "bg-only cell" tag so blank coloured cells never touch the style table.
4. One-sided hint flags on rows (styled, grapheme, hyperlink, prompt) to skip work on common rows.
5. Bulk text fast path: scan to next ESC, decode UTF-8 in bulk, ASCII bypass, printable-run splitting, width-class specialised row fill, grapheme clustering gate by "cannot be a continuation". In Rust, use `simdutf8`-style validation crates or portable code that autovectorises, plus `memchr`; verify with benchmarks since Ghostty's own lesson is not to trust auto-vectorisation.
6. Bulk-consume OSC/APC/DCS payloads separately from the state machine. Note this matters most for kitty graphics and our native media protocol payloads.
7. A `nextSliceUntil`-style API (parse until a boundary) so the multiplexer and the sandboxed decoder helper can see exact string edges with the same parser.
8. Render-state snapshot with two-phase update (short lock to copy dirty rows, finish outside the lock), row-level dirty flags, and row-wise GPU buffer clearing; cursor list drawn first. Our renderer is wgpu, so the "bg cells as a storage buffer plus instanced text quads" design ports directly.
9. Scrollback compression with retained virtual mapping (cold pages LZ4-compressed, physical pages released with `MADV_DONTNEED`). LZ4 needs a pure-Rust crate (e.g. `lz4_flex`, MIT; verify licence/version before adoption). Idle-driven, activity-serial scheduling and a verification pass are worth copying. Use real page dumps as the benchmark corpus.
10. Scrollback limits in both bytes (uncompressed logical) and lines, enforced by whole-page pruning.
11. The keybind grammar: modifiers + key, physical versus unicode keys, `catch_all`, sequences, key tables with one-shot and stack semantics, chained actions, prefixes (`all`, `global`, `unconsumed`, `performable`). Map these into KDL nodes rather than a flat string, but keep the same semantics and the same compact string form for the palette and CLI.
12. Commands are just titled actions; the palette is generated from the action registry plus user entries.
13. Shell integration by injection without editing dotfiles: ZDOTDIR for zsh, `--posix` plus ENV for bash, XDG_DATA_DIRS for fish/elvish/nushell, feature list env var, OSC 133 with `cl=`, `k=`, exit status on D, guard for unclosed C, OSC 7, `sudo`/ssh helpers. Ship the scripts as resource files.
14. Semantic prompt row flags for fast jump-to-prompt, click-to-move cursor, notify-on-command-finish, and a select-last-output action (not in Ghostty's list, natural extension).
15. Pin-pair search results via a sliding window over page text with overlap sized to the needle, on its own thread, with a copy-then-search structure to limit lock time. Improve on it: progress (n/total), coalesced notifications, pause when hidden.
16. Procedural box-drawing/powerline/braille/cursor/underline sprites and a Nerd Font constraint table (generate our own table from public data), so no patched fonts are required. Consider the Glyph Protocol as an open item.
17. Alpha blending modes (linear-corrected text), WCAG `minimum-contrast`, and palette generation from base colours. Cheap, visible quality wins.
18. Procedural kitty features worth parity: graphics with animation (eager compose to RGBA, byte-limited storage), text sizing OSC 66, clipboard OSC 5522, DnD OSC 72, colour OSC 21.
19. Layer-shell quick terminal with animation, autohide, screen choice, keyboard interactivity; global bind via XDG GlobalShortcuts or the compositor protocol. On Abyss we can do better with a first-party protocol.
20. Benchmark method: pre-generated deterministic corpora, ReleaseFast, hyperfine, medians, serial runs, fixed alignment of the measured function if needed. Add what Ghostty lacks: input-to-photon latency and frame pacing.
21. Per-surface cgroup/systemd scope for resource isolation on Linux (we can use it directly; it fits the sandbox story).
22. Snapshot format as a versioned, record-based binary with validate-on-encode and degrade-on-decode, for session restore and hand-off between processes.

## What it should avoid

- C/C++ in the VT path: Ghostty's SIMD and unicode width live in Highway/simdutf C++; FreeType, HarfBuzz, oniguruma, zlib, glslang and spirv-cross are C/C++. None are permitted in our build; use pure-Rust equivalents (for example `rustybuzz`/`harfrust`, `swash` or `skrifa`, `fontdb`, `regex`) after checking licences.
- GTK4/libadwaita for UI: our interface is wgpu-drawn glass. The GTK runtime is useful only as a feature checklist.
- A flat `key = value` config with free-form string values for structured things (keybind strings with embedded prefixes, `font-feature`, `theme = light:x,dark:y`, `quick-terminal-size`). Use typed KDL nodes with the same grammar available as a compact string.
- Indefinite key-sequence waits with no timeout or visual hint. Add an optional timeout, a visible pending-sequence indicator and cancel (escape).
- Single global terminal mutex shared by IO, render, search. Our core is single-threaded per surface with channels, so prefer message passing plus immutable snapshots.
- Feature gating by OS: undo and many window features are macOS-only. We should offer parity on Wayland.
- No JSON control socket: a design hole to fill.
- `link` regex config that cannot be set, and `font-shaping-break` that is macOS-only: avoid shipping partially wired options.
- Silent per-page capacity growth by clone on style-table overflow without back-pressure telemetry; at least expose counters.
- Reliance on `MADV_DONTNEED` as "compression" without a fallback: Ghostty disables compression where retained-mapping decommit is not available. Acceptable for Linux, but be explicit.
- Unicode width default tension: `unicode` width can desync with shells using wcswidth. Keep a switch, and let shell integration tell the shell what width mode is active.
- Marketing-grade perf claims. Publish numbers with method.

## Sources

Primary (source, upstream paths in repo `ghostty-org/ghostty`, commit cf3a8a6):
- `src/terminal/page.zig` (Page, Row, Cell, std_capacity, grapheme/string allocators, hyperlink limits)
- `src/terminal/PageList.zig` (Node, Pin, tracked_pins, growCapacity, grow_hints, IncrementalCompressionState, CompressionIterator, recycle_node, limits)
- `src/terminal/compress.zig`, `compress/Page.zig`, `compress/lz4.zig`, `compress/AGENTS.md`
- `src/terminal/style.zig`, `ref_counted_set.zig`, `bitmap_allocator.zig`
- `src/terminal/Parser.zig`, `parse_table.zig`, `stream.zig` (nextSlice, nextSliceCapped, printRuns, consumeApcString, consumeOscString, nextSliceUntil), `UTF8Decoder.zig`
- `src/terminal/Terminal.zig` (print, printSlice, grapheme gating, VS15/VS16)
- `src/simd/vt.zig`, `vt.cpp`, `index_of.cpp`, `codepoint_width.zig`; `src/quirks_memset.zig`
- `src/unicode/main.zig`, `lut.zig`, `grapheme.zig`
- `src/terminal/render.zig` (RenderState two-phase update, dirty tracking)
- `src/renderer/generic.zig`, `cell.zig`, `shaders/glsl/cell_text.v.glsl`
- `src/terminal/search/Thread.zig`, `search/sliding_window.zig`
- `src/terminal/kitty/{key,graphics_animation,graphics_storage,clipboard,color,dnd}.zig`, `terminal/osc/parsers/kitty_text_sizing.zig`, `terminal/apc/glyph.zig`
- `src/terminal/tmux/viewer.zig`, `src/terminal/snapshot/{snapshot.zig,AGENTS.md}`
- `src/input/Binding.zig` (Action union, trigger parsing, Flags), `input/command.zig`, `input/key_encode.zig`
- `src/config/Config.zig` (keybind docs ~lines 1640 to 1940, scrollback, quick-terminal, shell-integration, alpha-blending, minimum-contrast, link, clipboard, linux-cgroup)
- `src/termio/shell_integration.zig`, `termio/Thread.zig`, `termio/Termio.zig`, `src/shell-integration/README.md`, `shell-integration/zsh/ghostty-integration`
- `src/apprt/ipc.zig`, `apprt/gtk/ipc/new_window.zig`, `apprt/gtk/winproto/wayland.zig`
- `src/benchmark/AGENTS.md`, `src/benchmark/TerminalStream.zig`, `src/synthetic/`
- `build.zig.zon` (dependencies, licences), `LICENSE` (MIT), `README.md` (performance claim)

Docs (fetched):
- https://ghostty.org/docs/about (no benchmark numbers published; architecture overview)
- https://ghostty.org/docs/vt/concepts/sequences (control-sequence documentation; reference page linked from it)

External specs referenced by the source (not re-read here): https://vt100.net/emu/dec_ansi_parser, https://sw.kovidgoyal.net/kitty/ (graphics, text sizing, clipboard, DnD), http://bjoern.hoehrmann.de/utf-8/decoder/dfa, https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html.

Unverified items in this dossier: the `config-file` optional `?` prefix and `title-report` default (from memory of upstream docs, not checked in this checkout); OSC 3008 usage; anything marketed as a percentage or multiple in the README.
