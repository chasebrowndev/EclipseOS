# Topic: scrollback storage, search, copy mode, selection, semantic zones

Researched 2026-11-12 against the shallow clones under `/home/risk/.cache/apotheosis-prior-art/` (ghostty, kitty, foot, wezterm, alacritty, tmux, vte, konsole, contour, windows-terminal, zellij). Nothing was built or run. Techniques are described in prose; escape sequences, protocol fields and config key names are quoted as interface facts. Source locations are `path#function`. Anything not read in source or docs is marked "unverified".

## Summary

Scrollback in a terminal is three problems that look like one:

1. **Storage.** How rows are held, how much memory they cost, and how the oldest are dropped. Designs range from a ring of row pointers (foot, alacritty) through segmented mmap arrays (kitty), to linked lists of page-sized slabs with interned styles (ghostty), to compressed on-disk streams (VTE, konsole file history).
2. **Re-layout.** Reflow on column change must keep the cursor, selection, search results and semantic marks pointing at the right text. Everybody that reflows keeps a per-row "this line continues on the next row" flag and rebuilds logical lines.
3. **Access.** Search, selection and semantic navigation all want the same primitive: iterate logical lines (wrapped rows joined) with a map from text offset back to (row, column).

The three most transferable findings:

- **Ghostty's page model plus idle LZ4 compression of cold pages** (`terminal/PageList.zig`, `terminal/compress/`) is the most advanced storage design found. Its config documents 70-90% physical memory saving on text-heavy history. The scrollback limit stays a logical, uncompressed limit. Pages are decommitted with `MADV_DONTNEED` while the virtual range is retained, so restore never allocates.
- **Search as a background pass over immutable history plus a re-searched active area** (ghostty `terminal/search/screen.zig`), with a sliding window that keeps needle-length overlap between pages. Wezterm and alacritty bound the cost differently: wezterm debounces 350 ms and searches 1000-row chunks; alacritty caps type-ahead search at 1000 lines.
- **Block records keyed by a per-row block id** (contour `docs/drafts/semantic-blocks.md` section 4) is the right shape for OSC 133 zones. It survives reflow and eviction, and gives O(1) row-to-block lookup. Wezterm instead recomputes zones by scanning cells, which is simpler but O(history) per query.

Apotheosis is Rust with a single-threaded core owning state, a pooled blocking-work thread, and a "no allocation in hot paths" rule on input delivery. That favours: page slabs allocated from a pool, search running on a snapshot or under a short-lived state borrow in bounded slices on the calloop thread (or on a worker over copied page text), and compression done on idle ticks with a serial/cursor state machine.

## Architecture (how it is built)

### Ghostty: PageList of slab pages

- A screen is a doubly linked list of page nodes (`PageList.zig#List`, `Node`). Top node is oldest scrollback, last node holds the active area. Each page is one contiguous allocation with a fixed layout (`page.zig#Page.layout`, `Capacity`): rows, cells, a style table, a grapheme map, a hyperlink table, and string storage. All internal references are offsets, so a page can be moved or copied bytewise.
- A `Cell` is a packed 64-bit struct (`page.zig#Cell`): content tag, 21-bit codepoint or palette/RGB colour for empty cells, a style id (0 is the default style, no lookup), wide flag, and a 2-bit `semantic_content` (output/input/prompt). A `Row` is a packed integer carrying wrap flags, a `blank` flag, and a 2-bit `semantic_prompt` (none, prompt, prompt_continuation).
- Styles are reference-counted and interned per page (`ref_counted_set.zig`), so styled cells cost 64 bits regardless of attribute count.
- `std_capacity` pages come from a memory pool (`PagePool`) that is demand-paged by the OS, so idle preheated pages cost address space only. Pages that need more styles or graphemes than std capacity are grown (`growCapacity`, `growHint`) and then do not use the pool.
- Pruning: `PageList.grow` adds a row. If the last page has spare capacity it just bumps the row count; otherwise it allocates a page. If the total would exceed `max_size` it pops the first page and reuses its pool item. Limits can be bytes (`scrollback-limit-bytes`, default 50,000,000) or lines (`scrollback-limit-lines`, default unlimited); the first reached wins. Granularity is one page, so the real limit slightly exceeds the configured one. `Limits.enforce` (`.lines`, `.retain`) keeps memory in the pool at steady state.
- **Tracked pins** (`PageList.Pin`, `trackPin`): (node, row, col) handles registered with the list. Resize, reflow and prune fix them up. The viewport, the cursor, selections and search highlights are pins, which is how they survive reflow and eviction.
- **Cold-page compression** (`PageList.compress`, `compress/lz4.zig`, `compress/Page.zig`): pages wholly before the active boundary and not intersecting the viewport are eligible. A page is LZ4-block compressed (an allocation-free in-tree implementation, hash table fixed at 16 KiB), then the physical memory is decommitted while the virtual range stays allocated; restore recommits and decodes in place. Compression runs in three modes: `incremental` (one bounded step, result says whether to continue), `drain`, and `full`. An `IncrementalCompressionState` holds a page-serial cursor, a `did_compress` flag and a verification pass, so it converges after pages get decompressed by search or scrolling. An `activity_serial` lets the caller know when to schedule another pass. Config key `scrollback-compression` (default true). Unsupported on targets without strict decommit.
- **Reflow** (`PageList.resize`, `resizeCols`, `ReflowCursor`): copies rows from old pages into new pages cell by cell, joining soft-wrapped rows into logical lines and re-splitting at the new width. Options: `reflow` on/off (off truncates), a `cursor` to keep stable, `blank` (rows the caller will clear, such as a shell prompt the shell redraws, which become one blank row each so the shell's cursor-up count stays right), and a flag controlling whether growing rows may pull history into the active area (false for ConPTY-style children that keep their own screen).
- **Search** (`terminal/search/`): `ScreenSearch` is a state machine combining `ActiveSearch` (the active area, re-searched when dirty), `PageListSearch` (history, assumed immutable, searched in reverse from the active boundary via a `SlidingWindow`), and `ViewportSearch` (re-search only the viewport pages for highlight rendering). `SlidingWindow` holds a circular buffer of page text encoded to bytes plus a meta ring mapping byte offsets to page chunks; it prunes only when data has been searched and cross-page overlap for the needle length is retained. The search runs on its own thread (`search/Thread.zig`) with a 24 ms refresh timer, taking the terminal mutex only to copy page text. Results are `FlattenedHighlight` (chunk lists per page) and the selected match is a tracked highlight so it follows scrolling and eviction. Results survive switching to alt screen and back. Matching is a case-insensitive ASCII substring (`std.ascii.findIgnoreCase`); no regex, and `case` handling is ASCII-only in this snapshot.
- **Selection** (`Selection.zig`, `SelectionGesture.zig`, `Screen.zig#selectWord/selectLine/selectOutput/selectAll`): tracked-pin endpoints, rectangular flag, word boundaries from `selection_codepoints.zig` (default boundary set: NUL, space, tab, `'`, `"`, U+2502, backtick, `|`, `:`, `;`, `,`, `(`, `)`, `[`, `]`, `{`, `}`, `<`, `>`, `$`). `SelectionGesture` is a pure state machine: click count up to 3 within `repeat_interval` and `max_distance`, drag behaviours per click count, autoscroll ticks, pressure/deep-press. `selectOutput` uses the cell `semantic_content` to select a command's output, returning nothing if the point is on a prompt or input cell. Config: `selection-word-chars`, `selection-clear-on-typing`, `selection-clear-on-copy`, `clipboard-trim-trailing-spaces`, `copy-on-select`, `click-repeat-interval`.

### Kitty: segmented history plus pager text ring

- `HistoryBuf` (`kitty/history.c`) stores lines in mmap-allocated segments of 2048 lines (`SEGMENT_SIZE`), each segment holding a CPU-cell array, a GPU-cell array (the renderer uploads these directly) and per-line attrs. Segments are added on demand, so memory tracks use. Logical index is a ring over the whole buffer; `segment_for` maps a row to segment and offset.
- Rewrap on resize (`screen.c#rewrap`, `line-buf.c#rewrap`, `history.c#rewrap`) streams old rows into a new history buffer of the new width using a `continued` line attribute. The current prompt can be excluded from rewrap (`prevent_current_prompt_from_rewrapping`) when the shell will redraw it (`redraw=0` on OSC 133;A means the shell will not redraw, so kitty must not erase).
- **Pager history** is a second, separate ring buffer of UTF-8 plus ANSI text (`pagerhist_*`, option `scrollback_pager_history_size` in MB, default 0, max 4 GB, roughly 10,000 lines per MB of 100-char ASCII). It is not interactively scrollable; it is piped to the `scrollback_pager` program. It is rewrapped lazily (`pagerhist_rewrap_to`) by re-flowing the text stream. This is the cheap way to offer "huge" history without paying for cells.
- `scrollback_lines` default 2000; negative means effectively unlimited.
- Search is not in the core: `search_scrollback` opens the pager with the selection pre-searched (changelog), and markers (`docs/marks.rst`) highlight regex/text matches per line only, recomputed when a line changes, up to 3 groups (`mark1_foreground` ...). `toggle_marker`, `create_marker`, `remove_marker` actions.
- Selection: `EXTEND_CELL/WORD/LINE/LINE_FROM_BEGIN/LINE_FROM_POINT/WORD_AND_LINE_FROM_POINT` extend modes, `rectangle_select`; `select_by_word_characters` default `@-./_~?&=%+#` plus any Unicode alphanumeric, with a separate `select_by_word_characters_forward`; `strip_trailing_spaces`; `copy_on_select`.
- Shell integration (`docs/shell-integration.rst`): `scroll_to_prompt` (negative/positive/0), `show_last_command_output`, `mouse_select_command_output`, `mouse_show_command_output`, and OSC 133 extensions: A with `redraw=0`, `special_key=1`, `k=s`, `click_events=1|2`; C with `cmdline=` (printf %q encoded) or `cmdline_url=`; D with exit status.

### Foot: ring of row pointers

- Grid is an array of row pointers whose length is `lines + visible rows` rounded up to a power of two (`doc/foot.ini.5.scd`, `[scrollback] lines`, default 1000), so ring indexing is a mask (`grid.c`, `& (num_rows - 1)`). Rows are allocated lazily; null rows at the start mean "not yet scrolled into". `ensure_view_is_allocated` clamps the viewport so it never shows unallocated rows.
- Reflow (`grid.c#grid_resize_and_reflow`) walks old rows into a new ring, using a per-row linebreak flag, translating "tracking points" (cursor, selection ends, search position, sixel anchors) via a sorted comparison context; sixel images that cannot be mapped are destroyed. A `TIME_REFLOW` build flag logs the elapsed time.
- Search (`search.c`): incremental per keystroke over cell rows, smart case (`hasc32upper(buf)` picks case-sensitive when the needle has an uppercase char), matching compares cell by cell including composed (combining) sequences; empty cells match a space. Direction forward/backward and "backward same position". The match becomes a selection so the normal copy path works. No regex in scrollback search; regex exists for URL and `regex-copy` modes. Search has extend-left/right and scrollback page keys inside search mode.
- Actions to external tools: `pipe-visible`, `pipe-scrollback`, `pipe-selected`, `pipe-command-output`; `prompt-prev`/`prompt-next` (needs OSC 133;A which sets `shell_integration.prompt_marker` on the row); `scrollback-indicator` position/format; word delimiters default `,│`|:"'()[]{}<>`, whitespace always a delimiter; `selection-target` none/primary/clipboard/both.

### Wezterm: stable row indices, logical-line search, semantic zones

- Each line has a stable row index that survives scrolling (`StableRowIndex`); zones, search results and copy-mode cursor are expressed in it.
- `LocalPane::search` (`mux/src/localpane.rs`) iterates logical lines in a stable range (`for_each_logical_line_in_stable_range`), concatenates the wrapped physical lines into one haystack, and runs either a string match (case-sensitive, case-insensitive via lowercasing both sides, smart case) or a compiled regex, with an optional result limit and capture-group handling. A map from haystack offset to (physical row, column) turns matches into coordinates. Patterns: `CaseSensitiveString`, `CaseInSensitiveString`, `CaseSmartString`, `Regex` (copy-mode also has `CurrentSelectionOrEmptyString`).
- The copy-mode overlay (`wezterm-gui/src/overlay/copy.rs`) debounces typing 350 ms with a cookie (`schedule_update_search`), searches backward from the end in 1000-row chunks (`SEARCH_CHUNK_SIZE`) so first results appear fast, and keeps a per-line result index. Saved patterns persist per tab.
- `CopyModeAssignment` is the vi-ish vocabulary: viewport top/middle/bottom, scrollback top/bottom, selection modes (`Cell`, `Word`, `Line`, `SemanticZone`, `Block`), start/end of line content, word motions, page motions, `MoveToSelectionOtherEnd`, match navigation (`PriorMatch`, `NextMatch`, `PriorMatchPage`, `NextMatchPage`, `CycleMatchType`, `EditPattern`, `AcceptPattern`), semantic-zone motions (`MoveBackwardSemanticZone`, `MoveForwardZoneOfType(SemanticType)`), and f/t jumps (`JumpForward { prev_char }`, `JumpAgain`, `JumpReverse`). All of it is a key table users can override.
- Semantic zones (`term/src/terminalstate/mod.rs#get_semantic_zones`): each line exposes `semantic_zone_ranges()`; the function walks every physical line, opening a new zone when the semantic type changes (`SemanticType` Output=0, Input=1, Prompt=2). O(lines) per call; the GUI caches by sequence number (`SemanticZoneCache`).
- Defaults: `scrollback_lines` 3500, max 999,999,999; `selection_word_boundary` default ` \t\n{[}]()"'` + backtick.

### Alacritty: ring storage and DFA regex search

- `grid/storage.rs`: a Vec of rows with a `zero` offset; rotate is modular addition on `zero` rather than moving data. Shrinking only truncates the allocation when it exceeds the live length by `MAX_CACHE_SIZE` (1000) rows. History `scrolling.history` default 10000, limit 100000.
- Reflow lives in `grid/resize.rs`, keyed on the `WRAPLINE` cell flag at the last column of a row.
- Search (`term/search.rs`): `RegexSearch` builds four lazy DFAs from `regex-automata` (forward and reverse for both directions) so it can find the match end and then the match start without backtracking. Case-insensitive unless the pattern has an uppercase letter. Cache and NFA size limits follow the regex crate's meta engine bounds. Matches cross wrapped rows because the iterator treats `WRAPLINE` as no break. Type-ahead search is capped at 1000 lines (`event.rs#MAX_SEARCH_WHILE_TYPING`), the full search runs on confirm; search history holds 255 entries.
- Vi mode (`vi_mode.rs#ViMotion`): cell/word/line/paragraph/bracket motions, first/last, high/middle/low, semantic word left/right; `semantic_escape_chars` default `,│`|:"' ()[]{}<>\t`. Hints (`display/hint.rs`) label regex matches in view with a key sequence.

### tmux

- Grid (`grid.c`) holds history in the same array as the screen. When history reaches `history-limit` (default 2000 lines), `grid_collect_history` frees 10% of the limit in one batch (`hlimit / 10`) and moves the rest down, amortizing the cost. Rows carry `GRID_LINE_WRAPPED`. Reflow is `grid_reflow` with `grid_reflow_move` copying row segments.
- Copy mode (`window-copy.c`) is a table of named commands: `search-forward`, `search-backward`, `search-forward-incremental`, `search-backward-incremental-regex`, `search-again`, `search-reverse`, `copy-pipe`, `copy-pipe-and-cancel`, `copy-pipe-output`, `append-selection`, `other-end`, `set-mark`, `jump-to-mark`, `next-prompt`, `previous-prompt`, `jump-forward/backward/to-forward/to-backward/again/reverse`, `goto-line`, `top-line`, `scroll-middle`, `toggle-position`, `select-line`. Every command is bindable in `copy-mode` (emacs) and `copy-mode-vi` tables. `word-separators` option; `history-file` for command prompt history only.

### VTE and Konsole: compressed disk-backed history

- VTE `ring.hh` stores scrollback in three append-only streams: row records, UTF-8 text, and attribute-change records with hyperlink data. The row stream is regenerated on rewrap. Streams live in an unlinked temp file via three layers (`vtestream-file.h`): `VteSnake` maps logical offsets to at most three contiguous file regions in 64 kB blocks with sparse short blocks (so the filesystem keeps the compression gain), `VteBoa` compresses each block with LZ4 and encrypts it with AES-256-GCM with an overwrite counter in the IV, and `VteFileStream` batches writes until a block is full and caches reads. A visible-rows floor keeps recent lines in memory. Its code is GPL; this dossier takes the design only.
- Konsole `history/HistoryFile.cpp`, `HistoryScrollFile.cpp`: unlimited history is three temp files (index of line offsets, cells, line flags) memory-mapped; the temp location is configurable (system temp, cache dir, or a chosen directory). `SaveHistoryTask`, `SaveHistoryAutoTask`, `SearchHistoryTask` give save-to-file and a background search task.

### Contour: struct-of-arrays lines and block store

- `grid/LineSoA.hpp`: lines are struct-of-arrays, blank lines O(1) to construct, 64-byte aligned vectors for SIMD scans. `Grid` takes `reflowOnResize` and `maxHistoryLineCount` (finite or infinite).
- `docs/drafts/semantic-blocks.md` (design draft; features marked shipped there: output folding, block-atomic eviction, prompt navigation, copy last command, OSC 3008 context model): a `CommandBlockId` (uint32) on every row (zero extra memory), stamped fill-once as the cursor arrives, carried across reflow per logical line; a `CommandBlockStore` with up to 1000 records (`CommandBlockRecord`: state Prompting/Running/Finished, command line source Reported/Recovered, exit code, outcome, prompt/command timestamps, duration C-to-D, working directory snapshotted at C with OSC 3008 context first, then OSC 7). Records outlive their rows, enabling an "output evicted" placeholder and a recent-commands picker. `A` discards a Prompting record that never ran; `D` with no Running record is ignored (tcsh's unconditional D); ids never reused except on exhaustion, which bumps an epoch. Gutter with exit-status colouring, line numbers (absolute/relative/hybrid), per-line timestamps, scrollbar marks, sticky command header, finish notifications for long commands, vi text objects `im`/`am` for blocks. Persisting evicted output to disk was explicitly rejected there as "a second store of scrollback to secure and bound, for little gain".
- Vi mode (`input/vi/ViInputHandler.hpp`): text objects `i`/`a` + `w W p ( [ { < " ' \`` and `m` (line mark); search with `ScrollMarkUp/Down`, `CopyPreviousMarkRange`, Find at `Ctrl+Shift+F`, previous/next prompt `Ctrl+Alt+K/J`.

### Windows Terminal

- Search (`buffer/out/search.h`): flags `CaseInsensitive`, `RegularExpression`, results are `til::point_span` lists, `IsStale` compares a buffer mutation id so results are recomputed only when the buffer changed.
- Marks (`buffer/out/Marks.hpp`): per-row `ScrollbarData` with category (Default, Error, Warning, Success, Prompt), optional colour and exit code; FTCS command end maps exit code to Error/Success. Actions: `ScrollToMark`, `ClearMark`, `SelectCommand`, `SelectOutput`, `ExportBuffer`; profile setting `autoMarkPrompts`, `historySize`.

### Zellij

- Scrollback in `zellij-server`; search options (case, whole word, wrap) as a plugin-visible search; `EditScrollback` dumps to an editor. Details not verified beyond file presence.

## Feature inventory (grouped by category)

Storage
- Ring of row pointers, power-of-two sized (foot); modular-offset Vec ring (alacritty); 2048-line mmap segments (kitty); slab pages with interned styles (ghostty).
- Limits in lines (all), bytes (ghostty), or both (ghostty: first reached).
- Batch eviction in 10% chunks (tmux), page-granular eviction (ghostty), per-row ring overwrite (foot, alacritty, kitty).
- Cold-page LZ4 + decommit (ghostty); LZ4 + AES-GCM disk file (VTE); mmap temp files (konsole).
- Separate text-only pager history (kitty).
- Block-atomic eviction and evicted-output placeholder (contour).

Re-layout
- Wrapped-flag-based reflow (all). Cursor/selection/search/sixel translation (foot tracking points, ghostty tracked pins).
- Shell prompt protection from reflow (ghostty `blank` option, kitty `redraw=0` and prompt exclusion).
- Reflow disable option (ghostty `reflow` false truncates); "don't pull rows from history" flag for ConPTY (ghostty).

Search
- Substring, smart case (foot, wezterm, alacritty), explicit case flags (WT, wezterm), regex (wezterm, alacritty, WT, tmux regex incremental).
- Wrapped-line-aware matching (wezterm logical lines, alacritty WRAPLINE).
- Incremental: debounce 350 ms + 1000-row chunks (wezterm); type-ahead capped to 1000 lines (alacritty); 24 ms refresh timer and background thread (ghostty).
- Results cached with stable positions; selected match tracked across scrolling and eviction (ghostty).
- Search persists across alt screen (ghostty), search history (alacritty 255, wezterm saved pattern).
- Match navigation incl. page-wise (wezterm), n/N, "search selection" (ghostty `search_selection`, kitty).

Copy / vi mode
- Dedicated keyboard selection mode: wezterm key table, alacritty vi mode, tmux copy-mode(-vi), contour vi mode with text objects, foot has keyboard search and URL/regex modes but no full vi mode (unverified: no free-cursor mode found in the code read).
- Selection kinds: cell, word, line, semantic zone, block/rectangular.
- Hint/quick-select: label visible regex matches (alacritty hints, wezterm quickselect, foot `show-urls-*`/`regex-copy`, kitty `kitten hints`).

Selection semantics
- Word chars: boundary-set model (ghostty, foot, alacritty, wezterm, tmux separators) versus word-set model (kitty: alphanumeric plus a listed set, separate forward set).
- Click counts 1-3 within interval and distance (ghostty); drag behaviours per click count; autoscroll; extend modes.
- Copy hygiene: trim trailing spaces (ghostty, kitty), clear on typing/copy (ghostty), copy-on-select targets (foot none/primary/clipboard/both).
- Output-only selection by semantic content (ghostty `selectOutput`, kitty `mouse_select_command_output`, WT `SelectOutput`).

Semantic zones and shell integration
- OSC 133 A/B/C/D plus extensions: kitty `redraw`, `special_key`, `k=s`, `click_events`, `cmdline`, `cmdline_url`; contour N, I, P (`k=r` right prompt), L; WT also OSC 633 (VS Code).
- Per-cell semantic content (ghostty) versus per-row marks (foot, kitty, WT) versus per-row block id (contour) versus per-cell type with zone scan (wezterm).
- Navigation: prompt up/down (foot `prompt-prev/next`, kitty `scroll_to_prompt`, tmux `next-prompt/previous-prompt`, ghostty `jump_to_prompt`, WT `ScrollToMark`, contour).
- Actions on blocks: copy output, pipe command output (foot `pipe-command-output`, tmux `copy-pipe-output`), show last output in pager (kitty), fold, sticky header, exit-status gutter and scrollbar marks, long-command notifications (contour).

External tools
- Pipe scrollback/visible/selection to commands (foot), open scrollback in a pager/editor (kitty `scrollback_pager`, zellij `EditScrollback`), write scrollback/screen to file with copy/html variants (ghostty `write_scrollback_file`, `write_screen_file:copy,html`), export buffer (WT).

## Control surface (config, actions, IPC)

Config keys seen (interface facts):
- ghostty: `scrollback-limit-bytes`, `scrollback-limit-lines`, `scrollback-compression`, `selection-word-chars`, `selection-clear-on-typing`, `selection-clear-on-copy`, `clipboard-trim-trailing-spaces`, `copy-on-select`, `click-repeat-interval`, `scroll-to-bottom`, `mouse-scroll-multiplier`, `shell-integration`, `shell-integration-features`.
- kitty: `scrollback_lines`, `scrollback_pager`, `scrollback_pager_history_size`, `scrollback_fill_enlarged_window`, `wheel_scroll_multiplier`, `wheel_scroll_min_lines`, `select_by_word_characters`, `select_by_word_characters_forward`, `strip_trailing_spaces`, `copy_on_select`, `mark1_foreground`..`mark3_background`.
- foot: `[scrollback] lines`, `multiplier`, `indicator-position` (none/fixed/relative), `indicator-format` (percentage/line/custom); `word-delimiters`, `selection-target`.
- alacritty: `scrolling.history`, `selection.semantic_escape_chars`.
- wezterm: `scrollback_lines`, `selection_word_boundary`, copy-mode key tables.
- tmux: `history-limit`, `word-separators`.

Actions (names as interface facts): ghostty `search:<text>`, `start_search`, `end_search`, `navigate_search`, `search_selection`, `select_all`, `scroll_to_top/bottom/row`, `scroll_to_selection`, `jump_to_prompt:<n>`, `write_scrollback_file:<copy|paste|open>[,html|vt]`, `write_screen_file`; kitty `toggle_marker`, `create_marker`, `remove_marker`, `scroll_to_prompt`, `show_last_command_output`, `search_scrollback`; foot `prompt-prev`, `prompt-next`, `pipe-*`, `select-begin`, `select-begin-block`, `select-word`, `select-word-whitespace`, `select-row`, `scrollback-up-half-page`; wezterm `CopyMode`, `Search(Pattern)`, `ScrollToPrompt`, `SelectTextAtMouseCursor("SemanticZone")`.

IPC: wezterm exposes `get_semantic_zones`, `get_lines_as_text`, `get_text_from_semantic_zone` to Lua and `wezterm cli get-text`; kitty `kitten @ get-text --extent screen|all|selection|last_cmd_output|first_cmd_output_on_screen|last_non_empty_output` (kitty remote control; extent names from memory, unverified in this pass); tmux `capture-pane`, `copy-mode -e`, `send-keys -X <command>`. None expose search results over IPC as a first-class stream except tmux `#{search_match}` formats (unverified).

## Performance (numbers + method)

All figures are documented defaults or constants read from source; none are benchmarks I ran.

| Item | Value | Method / source |
|---|---|---|
| Ghostty compressed page size | 10-30% of uncompressed for text-heavy history | Config doc comment on `scrollback-compression` in `src/config/Config.zig` (author estimate, content dependent) |
| Ghostty default scrollback | 50,000,000 bytes per surface | `scrollback-limit-bytes` |
| Ghostty search refresh | 24 ms timer (about 40 FPS) | `search/Thread.zig#REFRESH_INTERVAL` |
| Ghostty LZ4 workspace | 16 KiB fixed hash table, zero allocation | `compress/lz4.zig` header comment |
| Kitty history segment | 2048 lines per mmap segment | `history.c#SEGMENT_SIZE` |
| Kitty pager history density | about 10,000 lines per MB (100-col ASCII) | option doc |
| Kitty default lines | 2000 | `scrollback_lines` |
| Foot default lines | 1000, allocation rounded up to power of two | `foot.ini` man |
| Wezterm search debounce / chunk | 350 ms / 1000 rows | `overlay/copy.rs#schedule_update_search`, `SEARCH_CHUNK_SIZE` |
| Wezterm default lines | 3500, max 999,999,999 | `config.rs` |
| Alacritty type-ahead cap | 1000 lines | `event.rs#MAX_SEARCH_WHILE_TYPING` |
| Alacritty history | default 10000, max 100000 | man page |
| Alacritty storage slack | 1000 rows before truncating | `grid/storage.rs#MAX_CACHE_SIZE` |
| tmux default | 2000 lines; evicts 10% of limit per collection | `options-table.c`, `grid.c#grid_collect_history` |
| VTE block size | 64 kB blocks, LZ4 then AES-256-GCM | `vtestream-file.h` header comment |
| Contour block records | 1000 max | `semantic-blocks.md` section 4.3 |

No published reflow or search throughput numbers were found in source; foot has a `TIME_REFLOW` compile-time log only. Apotheosis should set its own budgets (see below).

## What Apotheosis should take

1. **Page slabs with interned styles, 64-bit cells, row flags** (ghostty). Fits "handle-based state, no Rc graph": pages owned by one struct, referenced by index and offset. Allocation-free steady state because pages are recycled from a pool.
2. **Idle, incremental LZ4 compression of cold pages with logical (uncompressed) limits** (ghostty). Pure Rust LZ4 (`lz4_flex`, MIT; unverified current licence/version) or a small in-tree block codec. Run one bounded step per idle tick; track by page serial, not pointer. Never compress viewport-intersecting or boundary pages.
3. **Tracked positions** (ghostty pins, wezterm stable row indices, foot tracking points). Use a stable monotonic row serial plus per-page offsets. Everything user-visible (selection, search match, viewport, vi cursor, zone marks) holds a tracked pin that reflow and eviction fix up.
4. **Both byte and line limits, first-reached wins** (ghostty), exposed as config and settings GUI, per-profile. Optional "unlimited" is bounded by disk spill, not RAM.
5. **Disk spill as opt-in tier**: oldest pages compressed (LZ4) into an unlinked temp file under `$XDG_RUNTIME_DIR` or `$XDG_CACHE_HOME`, fixed-size blocks (VTE 64 kB model), sparse short blocks, optional AEAD (RustCrypto `aes-gcm`, Apache-2.0/MIT, unverified crate version) as VTE does. Contour rejected persistence as a second store to secure; we should keep it opt-in, memory-only by default, and zero secrets: honour the root invariant that password-role content is never stored (no `no-echo`/password rows persist) and never log content.
6. **Kitty-style text-only pager history** as a cheap deep tier option: UTF-8 plus SGR ring, lazily reflowed, used for pipe-to-pager/export/search of very old output only.
7. **Search architecture**: logical lines (wrapped rows joined) with an offset-to-(row,col) map (wezterm), searched newest-first in time-budgeted slices (wezterm 1000-row chunks, ghostty sliding window with needle-length overlap), cached results split into immutable history results and re-searched active results (ghostty), selected match tracked (ghostty). Provide literal, smart-case, and regex modes (see take 8), whole-word, wrap-around, search across alt/primary screens with persisted results.
8. **Regex engine**: Rust `regex`/`regex-automata` (MIT/Apache-2.0) with lazy DFA forward and reverse like alacritty to get leftmost-first starts and ends without backtracking, cache and NFA size limits, case-insensitive unless the pattern has an uppercase letter. Because the engine is linear-time, a search over a pathological pattern cannot hang; still apply the time budget.
9. **Time budget rule**: each search slice yields after N microseconds or M rows; type-ahead search bounded to a window around the viewport until the user confirms (alacritty), full background pass after a debounce (wezterm 350 ms is long; 100-150 ms is a proposed starting point, unverified). Expose budgets in config.
10. **Copy mode / vi mode as a key table of Actions** (wezterm `CopyModeAssignment`, tmux copy-mode commands), not a hard-coded handler. This fits the shared Action vocabulary: every motion (word, WORD, paragraph, bracket, jump f/t/;/, , semantic-zone, mark set/jump, page, viewport H/M/L) is an action users can rebind in KDL and the settings GUI. Add contour's text objects (`iw aw iW aW ip ap i( i[ i{ i< i" i' i\``, `im/am` for command blocks).
11. **Selection kinds**: cell, word, line, rectangular, semantic zone/output (wezterm `SelectionMode`). Selection endpoints are tracked pins, rectangular flag is part of the selection, gesture state machine is pure and testable (ghostty `SelectionGesture`).
12. **Word boundary config**: support both models. Default to a boundary set (ghostty list) with Unicode word segmentation (UAX 29) as an alternative, plus a kitty-style separate forward set; also "URL/path-aware smart selection" (regex rules for URLs, paths with `:line:col`, hashes, IPs, emails) tried on double-click before the boundary model, all editable rules (`event -> actions` for matches, opening via configured handler).
13. **OSC 133 as first-class, with block records keyed by a per-row block id** (contour). Support A, B, C, D, N, I, P (including `k=s/c/r`), L; kitty extensions `redraw`, `click_events`, `cmdline`, `cmdline_url`, `aid`; and tolerate sloppy shells (tcsh unconditional D, empty-Enter prompts, nested shells) per contour's transition table. Also keep per-cell semantic content (ghostty) so input vs output cell selection works within a row.
14. **Block features**: select/copy/pipe/open block output, rerun, fold, sticky command header, exit-status gutter/scrollbar marks, long-command finish notifications, prompt navigation actions, "last command output" to pager. Records outlive evicted rows with an "output evicted" placeholder.
15. **Reflow correctness features**: protect the live prompt rows during reflow when the shell will redraw (ghostty `blank` and kitty `redraw`), option to truncate instead of reflow, option not to pull rows from history (for apps that keep own screen), reflow progress bound for huge histories (do reflow lazily by page, visible area first; this is a recommendation, not found in a prior-art project).
16. **Export surface**: `write_scrollback_file` style actions with plain/ANSI/HTML variants and copy/paste/open targets (ghostty), `pipe-*` (foot), plus JSON control socket methods `scrollback.get_text`, `scrollback.search`, `zones.list`, `block.get` returning bounded slices.
17. **Scrollbar/indicator**: scrollbar marks for blocks, matches, errors (WT), scroll position indicator formats (foot).

## What it should avoid

1. **Unbounded in-memory scrollback by default** (kitty warns it slows the terminal). Default to a byte limit.
2. **Recomputing zones by scanning all cells per query** (wezterm). Use block records plus per-row ids.
3. **Global-lock search that holds the terminal mutex** (ghostty's own comment admits contention). Our core is single-threaded: copy bounded page text to a worker, or slice on the loop thread.
4. **Search restart on resize** (ghostty restarts the whole search when dimensions change). Use tracked pins and re-run only affected pages.
5. **Per-line-only matching** (kitty markers cannot cross lines). Search must cross wrap boundaries; markers/highlights should too.
6. **ASCII-only case folding** (ghostty snapshot). Use Unicode simple case folding and normalisation-aware matching (also for combining sequences, as foot's composed-cell matching does).
7. **Persisting evicted output silently** (security). Contour rejected it; VTE encrypts. If we spill, make it opt-in, AEAD-encrypted with an in-memory key, unlinked file, mode 0600, never include password-role content, and clear on exit.
8. **Fixed batch eviction that causes visible stutter** (tmux frees 10% at once: fine in C for tens of thousands of rows, but with compression and tracked pins a single large prune must be incremental).
9. **Hard-coded copy mode key handling** (foot lacks a free-cursor mode; alacritty's vi mode is not remappable beyond bindings). Everything goes through the Action table.
10. **Click-to-move-cursor by synthetic arrow keys** (kitty's own docs call it "not fully robust"); prefer the `click_events` protocol where the shell opts in, fall back only when configured.
11. **Pointer-based tracking structures** across page moves: use serial ids (ghostty compression cursor moved to serials after pointer bugs: stated in its comment).
12. **Logging selection, clipboard or search needle text** (root invariant: human input never logged by content).

## Sources

Local source (shallow clones, 2026-11-12):
- ghostty: `src/terminal/PageList.zig` (`grow`, `resize`, `resizeCols`, `ReflowCursor`, `compress`, `IncrementalCompressionState`, `Pin`, `trackPin`, `highlightSemanticContent`), `src/terminal/page.zig` (`Cell`, `Row`, `SemanticPrompt`, `SemanticContent`), `src/terminal/compress/lz4.zig`, `compress/Page.zig`, `src/terminal/search/{screen,pagelist,active,viewport,sliding_window,Thread}.zig`, `src/terminal/Selection.zig`, `SelectionGesture.zig`, `selection_codepoints.zig`, `Screen.zig` (`selectWord`, `selectLine`, `selectOutput`), `src/config/Config.zig`, `src/input/Binding.zig`.
- kitty: `kitty/history.c` (`add_segment`, `segment_for`, `pagerhist_rewrap_to`), `kitty/screen.c#rewrap`, `kitty/line-buf.c#rewrap`, `kitty/screen.h` (extend modes), `kitty/options/definition.py`, `docs/marks.rst`, `docs/shell-integration.rst`, `docs/changelog.rst`.
- foot: `grid.c` (`grid_resize_and_reflow`), `search.c` (`matches_cell`, `find_next`), `doc/foot.ini.5.scd`, `osc.c` (case 133).
- wezterm: `mux/src/localpane.rs#search`, `wezterm-gui/src/overlay/copy.rs`, `config/src/keyassignment.rs` (`CopyModeAssignment`, `Pattern`, `SelectionMode`), `term/src/terminalstate/mod.rs#get_semantic_zones`, `wezterm-cell/src/lib.rs#SemanticType`, `config/src/config.rs`.
- alacritty: `alacritty_terminal/src/grid/storage.rs`, `grid/resize.rs`, `term/search.rs` (`RegexSearch`), `vi_mode.rs`, `alacritty/src/event.rs`, `alacritty/src/display/hint.rs`, `extra/man/alacritty.5.scd`.
- tmux: `grid.c#grid_collect_history`, `window-copy.c` command table, `options-table.c`, `input.c#input_osc_133`.
- vte: `src/ring.hh`, `src/vtestream-file.h`, `src/vtestream.h`.
- konsole: `src/history/HistoryFile.cpp`, `HistoryScrollFile.h`.
- contour: `docs/drafts/semantic-blocks.md`, `src/vtbackend/grid/{Grid,LineSoA}.hpp`, `src/vtbackend/input/vi/ViInputHandler.hpp`, `docs/configuration/key-mapping.md`.
- windows-terminal: `src/buffer/out/search.h`, `src/buffer/out/Marks.hpp`, `src/cascadia/TerminalSettingsModel/AllShortcutActions.h`, `Profile.idl`.
- zellij: file presence only (`zellij-server/src/panes/search.rs`).

External docs cited by name only, not fetched in this pass (unverified): iTerm2 shell integration / FinalTerm OSC 133 documentation (https://iterm2.com/documentation-escape-codes.html, referenced by kitty docs); freedesktop semantic prompts proposal (referenced by contour draft); LZ4 block format (https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md, quoted in ghostty `lz4.zig`); `regex-automata` crate (https://docs.rs/regex-automata, referenced by alacritty search.rs).
