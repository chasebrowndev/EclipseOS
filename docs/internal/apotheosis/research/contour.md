# Prior art dossier: contour

Subject: contour-terminal/contour (https://github.com/contour-terminal/contour), Apache-2.0, C++23 with Qt/QML.
Snapshot studied: shallow clone of default branch on 2026-11-12 (`/home/risk/.cache/apotheosis-prior-art/contour`). The tree is far ahead of the 0.4-era contour most people know: it now has a daemon, workspaces, command blocks, a control API, MCP server, WASM extensions and a native chrome. Docs are in `docs/`, published at https://contour-terminal.org/.

Prior-art rule: this file describes techniques and interface facts only. No source is reproduced.

## Summary

Contour is the terminal with the broadest and most disciplined VT conformance story among the GPU terminals, and the originator of several de facto standards: DEC mode 2027 (grapheme clustering / "Terminal Unicode Core"), mode 2026 synchronized output (adopted from iTerm2's DCS form, re-spelled as a DEC private mode), mode 2028 (reflow control), 2029 (passive mouse tracking), 2030 (grid cell selection reports), 2031 (palette-change notifications), buffer capture (`CSI > Pl ; Pr , t` answered by `PM 314`), and the Good Image Protocol (GIP, DCS `! g`). Its other notable assets are a complete vi-like input mode family with hint mode, a VT320-style indicator status line with a placeholder template language, an input-binding model keyed on (mode flags x modifiers x input x click count), and, in the newest tree, a typed control API with CLI, JSON, MCP and a WIT description.

What it is worth to Apotheosis: (1) the conformance harness design (vttest and esctest driven headlessly, golden dumps, ratcheted known-gaps), (2) the mode set to implement, (3) the vi/hint/command-block interaction model, (4) the binding match model with tri-state mode filters, (5) the unified-API-with-reflection idea behind its control surface. What to avoid: Qt/C++ dependency stack, YAML with "replace not merge" binding semantics, HarfBuzz/FreeType/fontconfig (C libraries, disallowed), and a few over-broad escape-sequence extensions.

## Architecture (how it is built)

Crate/library layering (CMake targets under `src/`):

- `vtparser`: DEC-ANSI state machine (Paul Williams style) with a "bulk scan" fast path. Runs of printable bytes, CSI parameter bytes and DCS/APC payload bypass the per-byte state machine when it would only forward them; a `BulkScan::Disabled` mode routes every byte through the machine and is the reference the shortcuts are tested against, so shortcuts cannot change events. SIMD intrinsics wrapper in `crispy/Intrinsics.hpp`. (`src/vtparser/BulkScan.hpp`, `Parser.cpp`)
- `vtbackend`: the terminal model, no GUI. Sub-areas: `core` (Color, Image, Search, WorkingDirectory, UntrustedText), `grid` (Grid, Line, LineSoA, GraphemeClusters), `screen` (Screen = sequence handlers, Terminal = session object, Settings), `vt` (function table, SequenceBuilder, SGR writer, soft fonts, text sizing, hierarchical context, semantic prompt, program status, progress), `graphics` (Sixel, ReGIS, kitty graphics + animation, GIP message parser), `input` (InputGenerator for key encoding, InputBinding, MatchModes, ClickCounter, `vi/`), `links` (link detection, path probing), `shell` (command blocks, folding, sticky header, scrollbar marks, prompt regions), `state` (TerminalState codec for handover/persistence).
- `vtpty`: PTY abstraction (UnixPty, ConPty, ChannelPty for daemon, SSH session, MockPty), process spawn ladder, optional systemd scope placement and cgroup per session (`SystemdScopePlacement`, `session-oom-isolation.md`), sandbox info.
- `vtrasterizer`: render-target-agnostic rasteriser. Text renderer with a texture atlas (`TextureAtlas.hpp`, `AtlasBudget.hpp`), box-drawing generated procedurally (`BoxDrawingRenderer`), decoration renderer for underline styles, image renderer, cursor/background renderers, soft-font and ReGIS font rasterisers, a `TextClusterGrouper` and `RunPen` for cluster runs, `GlyphSlicing`/`GlyphScaling` for OSC 66 sized text.
- `text_shaper`: abstraction over shaping backends (HarfBuzz + FreeType + fontconfig/DirectWrite/CoreText). Text is shaped per "word" (split at spaces, attribute changes), then per script/presentation run; the cache key is the codepoint sequence plus shared SGR attributes, the cached value is glyph ids and positions (`docs/internals/text-stack.md`).
- `vtworkspace`: tab/pane/layout tree model (LayoutTree, Pane, Tab, SessionModel, Arrangement, model events), GUI-free.
- `vthost`: session host and daemon: native wire protocol (PDUs, deltas, snapshots), flush policy, tmux control-mode interop, latency benchmark.
- `vtcontrol`: control API: Catalog of methods and events, Dispatch, JSON codec, Describe/Reflect, Target resolution (id, label, 1-based position, id suffix), WIT export.
- `vtextension`: WebAssembly (component model) extension host: Engine, Manifest, ExtensionConsent and GrantStore, DeadlineGuard, worker pool, StatusLineAdapter.
- `vtconformance`: vttest and esctest drivers against a headless terminal.
- `contour`: Qt/QML GUI: config (YAML), actions, bindings, display (QRhi renderer glue), window chrome, settings window, command palette, quick terminal, remote.

Core model facts:

- Cell storage moved to a struct-of-arrays line (`LineSoA`) with 64-byte aligned vectors for SIMD. Each line carries a "shape" (`Uniform`, `Mixed`, and a wider class for cells spanning rows/columns or carrying image fragments). A line only degrades, never re-promotes without reset. Uniform lines (one codepoint per cell, one column per cell, one rendition) render as one batched run and accept bulk ASCII writes. (`grid/LineSoA.hpp`)
- Grapheme clusters: a cell holds up to `MaxGraphemeClusterSize = 16` codepoints, sized from the longest RGI emoji sequence (10 codepoints) plus headroom; the earlier cap of 7 split the 10-codepoint kiss emoji (found via jquast's ucs-detect). Cluster storage is a side pool. (`grid/LineSoA.hpp`, `grid/GraphemeClusters.cpp`)
- Cluster width under mode 2027 uses a `ClusterWidthPolicy`: by default width follows the first codepoint, under cluster-aware policy the cluster's whole width wins, and "a cluster widens and never narrows" (`clusterColumns`, `appendCodepointToCluster`, `Screen::applyClusterWidthChange` in `grid/GraphemeClusters.hpp`, `screen/Screen.cpp`). The same walk is shared by text measurement for the grid and text truncation for UI so cuts never split a cluster.
- Images are cell-fragment based: an `Image` is placed over a rectangle of cells, each cell stores a fragment reference; `FragmentPlacement` computes per-cell source rect and target rect given alignment and resize policy (letterboxing allowed). Image pool is an LRU of 100 named images for GIP.
- Rendering: `RenderBuffer` is a snapshot produced under the terminal lock and consumed by the renderer thread; mode 2026 simply freezes snapshot refresh.
- Daemon/client: a screen increment ("Delta") protocol with snapshots; output that answers a recent keystroke flushes immediately (echo window 50 ms, one immediate flush credit per input), other output coalesces for 20 ms (`vthost/FlushPolicy.hpp`).

## Feature inventory (grouped by category)

### VT core and conformance
- Emulation levels up to VT525 (function table tags each sequence with a minimum `VTType`), including VT52 mode, DECCOLM 80/132, DECSTBM, DECSLRM left/right margins (mode 69), DECCRA/DECFRA/DECERA/DECSERA rectangular ops, DECIC/DECDC, DECBI/DECFI, DECRQCRA rectangular checksums, DECRQSS, DECRQM/DECRPM for every private mode, XTGETTCAP, DECSCUSR, protected areas (DECSCA/DECSED/DECSEL), double-width/double-height lines, soft fonts (DECDLD), macros (DECDMAC), character sets including NRCS (mode 42), VT320 status lines (DECSASD/DECSSDT), Sixel with private colour registers (mode 1070) and sixel scrolling (mode 80), ReGIS (vector graphics, with its own font rasteriser).
- DEC private mode table (from `core/Primitives.hpp`, `DECModeNumbers`): 1-8, 9, 10, 12, 19, 25, 30, 40, 41, 42, 45, 46, 47, 64, 66, 67, 69, 80, 1000-1007, 1015, 1016, 1036, 1039, 1045, 1047-1049, 1070, 2004, 2026, 2027, 2028, 2029, 2030, 2031, 2034, 2048, 5522, 8452, 9001 (Win32 input), plus a block of accepted-but-inert VT520 modes (18, 34-36, 57, 60, 61, 68, 73, 81, 96-106). Accepting and answering DECRQM for inert modes rather than replying "unknown" is deliberate conformance.
- Reverse wraparound 45 and extended 1045; MetaSendsEscape 1036; AltSendsEscape 1039.
- Protocol extensions: kitty keyboard protocol (CSI u with 32-deep flag stack, documented as "CSIu extended keyboard protocol"), modifyOtherKeys, kitty text sizing OSC 66 (scale 1..7, width 0..7, fractional n/d, v/h alignment), kitty clipboard and kitty graphics with animation, OSC 8 hyperlinks, OSC 52 advertised via DA1 parameter 52, OSC 9;4 progress, OSC 7501 program status, OSC 3008 hierarchical context (UAPI.15), OSC 133 semantic prompts, OSC 1337 SetUserVar, OSC 99/desktop notifications, mode 2031 palette notifications, mode 2048 in-band resize, mode 5522 paste MIME notifications, save/restore SGR (`CSI # {` / `CSI # }`), XTWINOPS window size stack.
- Conformance harness: `src/vtconformance` drives vttest (golden dumps, 143 reviewed goldens over 18 scenarios) and esctest (unattended, gates the build) against a headless terminal. Details in Performance/What to take.

### Unicode
- Mode 2027 grapheme clustering, VS15/VS16 handling and cursor placement per "Terminal Unicode Core" spec (https://github.com/contour-terminal/terminal-unicode-core); RTL explicitly out of scope.
- Emoji incl. ZWJ sequences and skin tones, tag-sequence flags; wide-char continuation cells; width tables via Unicode data.
- Text shaping with ligatures, bold/italic/bold-italic fallback chains, separate emoji font fallback list; bundled Nerd Fonts symbols font; private-use glyph handling (`vt-extensions/private-use-glyphs.md`).
- Mode 2028 reflow control: on resize, lines reflow unless the app disabled reflow for the current and following lines (for prompts).

### Input
- Mode-aware binding model (below). Kitty keyboard flags, modifyOtherKeys, application cursor/keypad, backarrow mode 67.
- Mouse: 9, 1000-1003, SGR 1006, URXVT 1015, SGR-pixels 1016, alternate scroll 1007, passive tracking 2029 (app wants to observe, terminal keeps selection), grid-cell selection reports 2030.
- Multi-click counting (up to a max count) with click-count-keyed bindings; Ctrl+triple-click selects a command's output block.
- Read-only mode ("ToggleInputProtection") blocks keystrokes such as Ctrl+C from reaching the app. Secure input: lock indicator for panes whose tty is echo-off and canonical-on; Secure Keyboard Entry on macOS.
- IME toggle action; quick terminal via GlobalShortcuts portal on Linux.
- Vi modes (see Navigation).

### Fonts and rendering
- GPU renderer (Qt RHI; OpenGL/Vulkan/Metal/D3D through Qt). Texture atlas with explicit budget. Procedural box drawing. Underline styles and decoration geometry as unit-tested pure functions (`UnderlineGeometry.hpp`). Blur behind transparent background on KDE/Windows, blurrable background image, opacity actions.
- OSC 66 text sizing rendered by slicing scaled glyphs across cells (`GlyphSlicing`, `GlyphScaling`).
- Size indicator on resize, Fira-Code-inspired progress bar rendering.

### Graphics
- Sixel, ReGIS, kitty graphics (with zlib inflate) and kitty animation, and GIP (below). XtSmGraphics queries for limits. Image layers (below/replace/above text).
- Image cursor policy: after placement the cursor goes below the last image row; sixel keeps its own placement rules (mode 8452 for cursor next to graphic).

### Good Image Protocol (GIP)
DCS `! g` carrying a comma-separated `key=value` header, `;`, then a body; values or body prefixed with `!` are base64 (header values and body both handled by `MessageParser`, `src/vtbackend/graphics/MessageParser.cpp`). The operation is header key `o`:
- `u` upload: `n` name (ASCII alnum + underscore, 1..512), `f` format (RGB, RGBA, PNG, auto), `w`/`h` pixel size (optional for PNG), `s` request status. Body size must equal w*h*bytes-per-pixel for raw. Status replies are numeric (0 ok, 2 invalid image data).
- `r` render by name: `r`/`c` rows and cols to cover, `x`/`y` pixel source origin, `w`/`h` source size, `a` alignment (9 positions), `z` resize (no-resize, fit, fill, stretch), `L` layer (0 below, 1 replace, 2 above), `l` auto-scroll, `u` cursor goes below image, `s` request status.
- `s` oneshot: upload and render in one message, no name retained.
- `d` release by name.
- `q` query: reply `DCS ! g s=8, m=<max images>, b=<max bytes>, w=<max w>, h=<max h> ST`; advertised in DA1 via a `GoodImageProtocol` attribute.
Design points: named upload then cheap re-render avoids retransmitting pixels; layer parameter gives watermark and overlay; explicit cell-rect target with alignment/resize policy is more deterministic than kitty's pixel-offset placement. Gaps: no animation, no transport-encoding header yet (a TODO in the parser), no shared-memory or file transport. Spec lives outside this repo (unverified location); the above was read from the implementation.

### Media
- No native video. Animation only via kitty animation and sixel streams (the code explicitly rate-limits a "sixel video" drop log). Nothing relevant for the media-protocol decision beyond kitty animation.

### Multiplexing
- Tabs, splits, zoom, swap, move, resize, split orientation toggle; coloured tabs and tab badges; drag and drop of tabs/panes onto pane edges; named Layouts (open together) with SaveLayout and LaunchLayout actions.
- Daemon mode (v0.7+, experimental): sessions survive the window; `contour client` auto-starts a daemon with lifecycle `exit-when-idle` (10 s grace); `persistent` and `exit-with-last-session` policies; attach locally or remotely over `ssh://` (no network listener besides sshd) or `tls://` (token plus pinned CA, fingerprint trust-on-first-use dialog); workspaces hold tabs; `contour switch` re-points a window with a user prompt (30 s wait, Deny/Switch, remembered per daemon pair); tmux control-mode interop in both directions (`docs/persistent-sessions.md`).
- Restored sessions: history reappears above a "Restored" divider with one toast.

### Scrollback and search
- Reflowing scrollback; find bar with live match count floating over the searched pane; next/previous match (F3), search selection, clear highlight; regex engine abstraction (`RegexEngine.hpp`).
- Screenshot to file/clipboard (`ScreenshotVT`, `SaveScreenshot`, `CopyScreenshot`), VT buffer capture sequence (permissioned), `contour pane capture`.

### Shell integration
- OSC 133 (A/B/C/D) and OSC 3008. Command blocks: per-block id, exit status, duration, exit-status gutter ("rail"), line numbers, per-line timestamps, folding (`za`, `zo`, `zc`, `zM`, `zR` in vi mode), scrollbar marks, sticky command header, block pills/chips, copy last command prompt/output/block, rerun command, open output in pager (`command_blocks.pager`), select command output by Ctrl+triple-click.
- Finish notifications when a long command ends in an unfocused tab. Offer to install shell integration for bash, zsh, fish (shows the one line added). Integrations for bash, zsh, fish, tcsh, PowerShell.
- Mode 2034 semantic block query returns structured JSON blocks for agents/accessibility tools (needs OSC 133).
- OSC 7501 program-status protocol (idle/working/done/blocked/error, ids with hierarchy, base64 title/msg, strict byte limits, DoS-bounded).
- Working directory tracking (OSC 7 and context) feeds new tab/split cwd, `{Cwd}` and `{Git}` placeholders (Git branch read from `.git/HEAD` in the background, no subprocess).

### Navigation
- Vi-like input modes: insert (default, passthrough), normal, visual, visual-line, visual-block, plus hint mode. Entered with Ctrl+Shift+Space.
- Operators/motions in normal mode: counts, `h j k l w b e | 0 $ gg G { }`, `[m ]m` (prev/next mark: prompt or user mark), `]x [x` (next/prev link), text objects `iw aw iW aW`, quote/bracket pairs `i" a" i' i( i[ i{ i<` and backtick, `ip ap`, and `im am` (lines between two marks). Operators `y` (yank), `o` (open), `p` paste, `yy`, `gx` open link under cursor, `gh`/`gH` hint copy/open, `mm` user mark toggle, `/`-style search integration, fold keys. The motion that leaves the viewport scrolls it. A jump history (`JumpHistory`) and a `Selector` (selection state machine) are separate units. Key-sequence dispatch uses a trie map keyed on sequences so multi-key commands resolve incrementally.
- Hint mode: overlay alphabetic labels on detected patterns (URLs, paths, git hashes, custom regex); action per hint: copy, open, paste, copy-and-paste, select; scope visible or scrollback up to a limit (`HintTypes.hpp`).
- Vertical line markers: jump between marks in history; bookmarks via `mm`.
- Clickable links: URL, file path and user patterns; path existence probed before it counts as a link (`PathProbe`); URL templates per pattern (`LinkUrlTemplate`), `file://` handling.

### Config
- YAML `contour.yml` with `profiles`, color schemes, `hosts:` (plus `hosts/<name>.yml` side files written by the Settings UI), `input_mapping`, `status_line`, `extensions`, `quick_terminal`, `secure_input`, `permissions.*` (`allow|deny|ask`), `defaults_edition: classic|native2026` (new defaults gated by an edition marker so old configs keep old behaviour). `contour generate config` writes every default out. Live reload; atomic file writes (`AtomicFileWrite`). A Settings window exists and field metadata (`ConfigDocumentation`, `SettingsFields`) is shared with the docs.
- Binding model details in Control surface.

### Automation
- Control API (`vtcontrol`): methods and events catalog; every verb has a CLI form and a JSON form. See Control surface.
- WebAssembly status-line extensions, sandboxed, with declared commands gated by a consent store.

### UI
- Native chrome (Windows 11, macOS, Linux), light/dark, command palette (`OpenCommandPalette`), context menu, toasts, tab bar position/visibility options, "Terminal" character-cell UI style, indicator status line, size indicator, keyboard focus cycling through chrome (Ctrl+F6 with focus ring).

### Integration
- MCP server (`contour mcp`) so coding agents split panes, run commands and read output in their own tab; env vars `CONTOUR_PANE`, `CONTOUR_TAB`, `CONTOUR_WORKSPACE`, `CONTOUR_DAEMON` let a shell find itself.
- Desktop notifications, file manager open, text-to-speech actions (`SpeakSelection`, `StopSpeaking`), drag-and-drop of files types escaped paths, KDE blur.

### Security
- Per-profile permissions for risky sequences (`capture_buffer`, etc.), `ask` prompts; a refused buffer capture is answered with the empty terminator so clients never hang. `UntrustedText` type in core marks text that came from the program stream. Control requests over network refuse unverified TLS. Base64/size limits on OSC 7501 and GIP parsers (`MaxKeyLength`, `MaxParamCount`, `MaxBodyLength` in `MessageParser`).

### Accessibility
- Reports caret position and rect, visible text, and live prompt region (from OSC 133 marks) to OS accessibility; announcements have a separate setting; tab list semantics; focus ring; reduce-motion respected by the quick terminal (`docs/accessibility.md`).

### Packaging
- Linux (distro packages, Flatpak/Snap per project docs, unverified), macOS (signed app), Windows, FreeBSD. Release signing docs in `docs/internals`.

## Control surface (config, actions, IPC)

Configuration: single YAML file with profiles; per-profile `status_line`, `permissions`, `extensions`; global `input_mapping`, `hosts`, `quick_terminal`. KDL equivalent would map profiles to nodes. Contour's `defaults_edition` is a notable idea: explicit edition token that gates new defaults.

Actions: ~147 action structs in `src/contour/config/Actions.hpp` (cataloged by `ActionCatalogEntry`), covering clipboard, scroll, search, font size, opacity, tabs, panes, workspaces, daemons, blocks, layouts, screenshots, vi mode, trace-mode debugging (`TraceEnter`, `TraceStep`, ...), `SendChars`, `WriteScreen`, `ChangeProfile`, `ReloadConfig`, `Quit`. Actions carry typed arguments (direction, tab index, ratio, etc.).

Binding model (`vtbackend/input/InputBinding.hpp`, `MatchModes.hpp`, `contour/config/BindingCollisions.hpp`):
- A binding is (MatchModes, Modifiers, Input, ActionList, optional clickCount). `MatchModes` holds two bitmasks, `enabled` and `disabled`, over flags AlternateScreen, AppCursor, AppKeypad, Insert (vi insert), Select (visual), Search, Trace, BlockFocus. So each flag is tri-state: any, must be on, must be off. A binding written `mode: ~Alt` style means disabled.
- Lookup is exact match on modes+modifiers+input+clickCount (a click-count binding never answers an ordinary lookup, and vice versa). Mode filters of two bindings that can both hold are "overlapping"; `BindingCollisions` detects pairs that can fire together and differ only by modifiers, used for config validation.
- One key may fire an `ActionList` (several actions in sequence).
- Built-in bindings are consulted after the user's table, so new default chords reach old configs; but defining `input_mapping` replaces the generated defaults wholesale (documented as a footgun, mitigated by writing all defaults on first run). Platform-aware defaults: `Primary` modifier maps to Ctrl+Shift on Linux/Windows and Cmd on macOS; every command has a chord on both.
- Mouse: button + modifiers + click count; wheel and horizontal swipe switch tabs.

IPC (`docs/control-api.md`, `src/vtcontrol`):
- `contour` CLI verbs: `ls`, `new`, `split`, `send` (text, `--enter`, `--wait` exits with the command's status), `pane capture`, `close`, `label`, `block list|read`, `switch`, `wait`, `events [--follow] [--kind]`, `api list|describe|call|wit`.
- Target resolution: full id (`pane:<uuid>`), label (case-insensitive), 1-based position, id suffix of at least 4 hex digits; ambiguity is an error listing candidates; no silent fallback when `$CONTOUR_PANE` is closed (exit status 3). Defined exit codes (2 usage, 3 not found, 4 unknown host, 5 refused).
- Transports: Unix socket, `HOST:PORT` over TLS with token file and CA, saved host names, `ssh://` URLs. Unverified TLS refused before anything is sent.
- Reflection: the API is described in a WIT package `terminal-extensions:control@0.1.0`, so the same catalog drives CLI, JSON, MCP tools and WASM imports. Permission levels (`read`, `write`) gate methods; some methods (workspace.switch) are socket-only and explicitly excluded from MCP and extensions so an agent cannot re-point its own window.
- Events stream with kind filters.
- No scripting language; the extension mechanism is sandboxed WASM, restricted to status-line drawing and declared commands.

## Performance (numbers + method)

All numbers are contour's own claims from its docs; not reproduced by us.

Throughput (`docs/extensions.md`, "Throughput"): `bench-headless throughput`, 32-thread Linux machine, release build, 16 MiB per workload fed from memory (no PTY, no GUI), indicator status line shown and terminal focused; each cell faster of two interleaved rounds, each round fastest of 5 runs. Results with no extensions: ascii 238 MiB/s, colored 161 MiB/s, sgr 95 MiB/s, utf8 36 MiB/s, tui 261 MiB/s. Misbehaving WASM guests spinning to their time limit changed these by under 1% (alt-screen sgr dipped about 4% with four busy guests). Observation: the utf8 workload is 6 to 7 times slower than ascii, which shows grapheme/width handling is the slow path.

Profiling (`docs/internals/profiling.md`): example utf8 primary-screen run 47.3 MiB/s at 3.13 Ginstr, 536 Mcyc (perf counter based, instruction counts used because they are steadier than MiB/s on shared machines). Tracy zones inside the renderer (`TracyCellZone.hpp`).

Memory (`docs/extensions.md`): `bench-headless memory`, each pane 120x40 with 1000 lines scrollback, 60 Hz refresh for 5 s, RSS before/after. One pane with no extensions 4.5 MiB; ten panes 21.0 MiB; so about 1.8 MiB per additional pane at that scrollback. One WASM extension adds about 1.6 MiB per pane; empty module 1.4 MiB.

Latency (`docs/internals/latency.md`): budgets relative to in-process for daemon mode: launch to first prompt pixel at most +10 ms median; keystroke to pixel over local socket at most +1 ms median. Method: an "echo child" prints `> ` and echoes bytes; probes watch the cursor column (not glyph) so detection is rendering-independent; headless mode times from the `writeInput` call, e2e times from key event delivery on the GUI thread to the `frameSwapped` of the first frame drawing the cursor at the expected column, with swap interval 0 (unpaced) because a 60 Hz vsync quantises to 16.7 ms and hides a 1 ms difference; gate compares medians and reports INCONCLUSIVE with fewer than 10 startup or 200 keystroke samples. Sample table: keystroke in-process p50 85.7 us, daemon p50 134.1 us (headless, informational); startup in-process p50 2.6 ms, daemon 3.1 ms. CI records numbers as artifacts and never fails on them (shared runners too noisy); the gate is a reproducible local run on three OSes. Bulk output coalesces at 20 ms; echo flush window 50 ms.

Takeaway: measure the pipeline in a headless harness with fixed workloads (ascii, colored, sgr, utf8, tui), report MiB/s plus instruction counts, and gate latency on medians with minimum sample counts.

## What Apotheosis should take

1. DECRQM-answering for the whole mode table, including inert VT520 modes; query support for every private mode.
2. Mode 2027 semantics, with their cluster width policy rule (cluster widens, never narrows), a cluster cap of at least 16 codepoints, and sharing one cluster-walk between grid layout and UI truncation. Also publish our own conformance results against Terminal Unicode Core and ucs-detect.
3. Headless conformance harness driving vttest and esctest, golden dumps, causal barrier (answer on stream markers, not clocks), a separate wedge timer, ratcheted known-gap lists, and a documented "recorded vs reviewed" golden status.
4. The tri-state mode-flag binding match model plus click-count in the key, collision detection at config-validate time, and platform-neutral "Primary" modifier naming. Fix its flaw: bindings should merge over defaults by default, with an explicit "unbind" entry, not replace the table.
5. A typed Action vocabulary with a catalog (name, args, docs, category) from which palette, keybinds, CLI and JSON control are generated. Contour's API reflection into WIT is the closest analogue to our requirement.
6. Control CLI conventions: target resolution order (id, label, position, suffix), error-on-ambiguity, no silent fallback, stable exit codes, `--wait` returning the command's status, `events --follow`, env vars giving a shell its own pane/tab/workspace.
7. Vi mode family with text objects including mark-delimited objects, `]x` link motion, fold keys, hint mode with scrollback scope and per-hint action.
8. Command blocks as first-class (OSC 133 plus OSC 3008): gutter rail, sticky header, folding, scrollbar marks, block copy/rerun, finish notifications, block read API.
9. Indicator status line placeholder language (`{Name:Flag,Key=Value}` with Left/Right/Hide/Color roles) as one source for built-in segments; implement in KDL with role-based colours. Skip the WASM extension host (we have no embedded scripting).
10. Grid design ideas: line shapes with degrade-only transitions for fast paths, SoA cells, bulk-ASCII path in the parser guarded by a reference disabled mode.
11. Echo-aware flush policy for any daemon/mux split: immediate flush for echo within a 50 ms window with a one-immediate-flush-per-input credit, 20 ms coalescing otherwise.
12. Latency measurement method: cursor-column probe, unpaced swaps, median budget gate, minimum sample counts.
13. Secure-input detection via tty echo-off plus canonical-on, with documented blind spots; show a lock indicator.
14. GIP's named-upload plus render-by-name model, explicit cell rect with alignment/resize policy and layer parameter, as the model for our native image path (not necessarily its wire syntax). Add an animation story contour lacks.
15. Permissions per profile (`allow|deny|ask`) for risky sequences, and answer refusals in a way that never hangs the client (empty terminator).
16. `defaults_edition` style gating of changed defaults so config evolution does not break users.
17. Accessibility: caret/prompt-region reporting driven by OSC 133 marks, reduce-motion honoured. Map to AT-SPI.
18. Program-status (OSC 7501), progress (OSC 9;4), semantic block query (mode 2034) for agent/tool friendliness; adopt the limits discipline (byte caps, depth caps, discard-whole on violation).

## What it should avoid

- Qt/QML and C++ stack; HarfBuzz, FreeType, fontconfig, CoreText: all outside our allowed library set. Need Rust-native shaping and rasterisation (cosmic-text/swash/rustybuzz style crates; unverified licence status, check before adoption).
- YAML and "defining input_mapping drops built-in bindings" semantics.
- Two parallel config homes (main file, `hosts/*.yml` side files) unless the settings GUI needs them; one KDL tree with includes is simpler.
- Escape sequences with collision history: its buffer-capture sequence originally collided with xterm XTSMTITLE and had to gain a `,` intermediate; avoid inventing new CSI finals without checking xterm's table, and prefer OSC/DCS/APC namespaces.
- Mode numbers added by one terminal and left unspecified (2029, 2030) before a spec exists.
- Experimental daemon with its own wire protocol plus tmux interop plus TLS plus SSH plus MCP all at once; scope risk. Pick one remote transport first.
- GIP's weaknesses: no transport-encoding header, no animation, no shm/file transport, spec outside the repo. Do not copy the base64 `!` prefix convention without a length cap and streaming decode.
- Per-session cgroup/systemd-scope placement may be useful but is distro-coupled; make it optional.
- Rely on mode 2026 with no timeout: contour's doc admits no consensus; we should define a hard timeout (suggest about 150 ms to 1 s, to be decided).

## Sources

Docs (primary, read from clone and published at https://contour-terminal.org/):
- `docs/features.md`, `docs/input-modes.md`, `docs/control-api.md`, `docs/persistent-sessions.md`, `docs/remote-hosts.md`, `docs/extensions.md`, `docs/secure-input.md`, `docs/accessibility.md`, `docs/quick-terminal.md`
- `docs/configuration/key-mapping.md`, `docs/configuration/indicator-statusline.md`
- `docs/internals/vt-conformance.md`, `docs/internals/latency.md`, `docs/internals/text-stack.md`, `docs/internals/profiling.md`
- `docs/vt-extensions/unicode-core.md`, `synchronized-output.md`, `line-reflow-mode.md`, `buffer-capture.md`, `semantic-block-query.md`, `osc-3008-context.md`, `osc-9-4-progress.md`, `program-status.md`, `csi-u-extended-keyboard-protocol.md`, `clickable-links.md`
- `docs/drafts/grid-redesign.md`

Source (upstream paths and functions; read for technique only):
- `src/vtbackend/core/Primitives.hpp` (DECMode enum, `DECModeNumbers`, `toDECModeNum`, `fromDECModeNum`)
- `src/vtbackend/grid/GraphemeClusters.hpp`/`.cpp` (`clusterColumns`, `graphemeClustersOf`, `columnsOnTheGrid`), `grid/LineSoA.hpp` (`LineShape`, `MaxGraphemeClusterSize`)
- `src/vtbackend/graphics/MessageParser.cpp` (`MessageParser::pass`, `flushHeader`, `finalize`)
- `src/vtbackend/screen/Screen.cpp` (`hookGoodImageProtocol`, `handleGipUpload`, `handleGipRender`, `handleGipRelease`, `handleGipQuery`, `parseGipRenderParams`)
- `src/vtbackend/core/Image.hpp` (`ImageLayer`, `ImageResize`, `ImageAlignment`, `FragmentPlacement`)
- `src/vtbackend/input/InputBinding.hpp` (`match`), `input/MatchModes.hpp`, `input/vi/ViInputHandler.hpp`, `input/vi/HintTypes.hpp`
- `src/contour/config/Actions.hpp`, `config/BindingCollisions.hpp` (`modesOverlap`, `canFireTogether`)
- `src/vtbackend/vt/TextSizing.hpp` (OSC 66 request decoding)
- `src/vthost/FlushPolicy.hpp`
- `src/vtparser/BulkScan.hpp`
- `src/vtconformance/` (harness; documented in vt-conformance.md)

External specs referenced:
- Terminal Unicode Core: https://github.com/contour-terminal/terminal-unicode-core
- UAPI.15 OSC context: https://uapi-group.org/specifications/specs/osc_context/
- OSC 8 hyperlinks: https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
- DECRQM/DECRPM: https://vt100.net/docs/vt510-rm/DECRQM.html
- Kitty protocols (keyboard, graphics, text sizing): https://sw.kovidgoyal.net/kitty/ (referenced by contour; not re-read here)
- vttest, esctest (referenced by contour docs; not re-read here)

Unverified: that mode 2027 originated with contour (contour's own docs say it authored the spec; third-party adoption claims such as ghostty/foot/wezterm support were not checked); GIP spec location and any wire detail beyond the implementation read; Linux packaging formats; licence status of Rust shaping crates.
