# Prior art: foot (https://codeberg.org/dnkl/foot)

Researched 2026-11-12 against a shallow clone of master (1.28.0 plus unreleased fixes, commit 8768826, 2026-10-27), at `/home/risk/.cache/apotheosis-prior-art/foot`. Licence MIT. Techniques are described in prose; escape sequences, protocol names and config keys are quoted as interface facts. Source locations are given as `file#function`.

## Summary

foot is the incumbent Wayland terminal for people who want speed and small size. It is about 49k lines of C. It renders with pixman on the CPU into wl_shm buffers, with no GPU. It wins on four things:

1. A byte-at-a-time VT state machine with an ASCII fast path.
2. Cell-level and row-level dirty tracking that reaches the compositor as per-row `wl_surface.damage_buffer` rectangles.
3. A memfd "sliding window" trick that makes small scrolls nearly free.
4. A tiny dependency set: pixman, fcft, wayland, xkbcommon, fontconfig, tllist and, optionally, utf8proc.

Its weakness is the design ceiling of a CPU renderer. It has no image protocol beyond sixel, no ligatures in the grid, no tabs or splits, no scripting or IPC beyond "spawn a window", and no live config reload. It has a deliberately small UI surface: keyboard-driven URL mode, scrollback search, and an optional client-side-decoration title bar.

foot users will notice the loss of these behaviours (full list under "What Apotheosis should take"):
- startup in single-digit milliseconds, through server mode and socket activation;
- near-zero idle cost;
- tiny RSS;
- correctness of reflow and wrap;
- synchronized updates and the delayed-render anti-flicker timer;
- OSC 133 prompt jumping and `pipe-command-output`;
- URL jump labels;
- box-drawing drawn by the terminal itself;
- the whole `foot.ini` surface;
- `footclient` semantics, where the exit code is the shell's and `-N` detaches.

## Architecture (how it is built)

### Process and threading model

- One main thread runs an epoll loop, the "fdm", in `fdm.c`. It multiplexes the Wayland fd, every pty master, timerfds, signalfds and the server socket.
- fdm has three priority hook lists (low, normal, high) that run after each epoll batch.
- The renderer registers a hook, `render.c#fdm_hook_refresh_pending_terminals`, that renders every terminal marked "refresh pending". Rendering therefore happens once per loop iteration, after all pty reads in that batch. It is also gated by the Wayland frame callback.
- Each terminal owns N render worker threads. N defaults to the number of logical CPUs. The `workers` option sets it, and 0 disables threading.
- The VT parser and the grid are only ever touched by the main thread.
- Workers only rasterize rows. The main thread pushes dirty row numbers onto a queue, posts a start semaphore, and waits on a done semaphore. Each worker has its own pixman image and its own damage region for the same shm buffer (`render.c#render_worker_thread`). Workers are given sentinel row numbers for "frame done", "quit" and "pre-apply damage".
- Slow work is isolated. Fonts load in parallel threads at startup (`terminal.c`, `thrd_create` near font loading). The notification, URL launch and utmp helpers are spawned as child processes.
- The pty read loop reads up to 24 KiB per `read()` and at most 10 iterations per epoll wake (`terminal.c#fdm_ptmx`). This keeps the loop fair across windows. When the pty is hung up it drains without a limit.

### Server/client mode

- `foot --server` hosts many windows in one process. They share fonts, the glyph cache and the parsed config, but each window has its own render threads.
- The socket path is `$XDG_RUNTIME_DIR/foot-$WAYLAND_DISPLAY.sock`. A numeric argument means "this fd is a socket from the supervisor", which gives systemd or s6 socket activation. Units `foot-server.service` and `foot-server.socket` are shipped.
- `footclient` connects to the socket and sends one packed binary message (`client-protocol.h`): flags (hold, no_wait, xdg_activation_token), cwd, token, `-o` overrides, argv and optionally its environment. The server replies with the child's exit code when the window closes.
- footclient forwards SIGUSR1 and SIGUSR2 to the server as an IPC frame (`FOOT_IPC_SIGUSR`).
- `-N` makes footclient detach immediately. `-E` runs the shell in footclient's environment, not the server's.
- Costs: one crash kills every window, and heavy output in one window starves the others because I/O is single-threaded.
- This is the only IPC. There is no query, no layout control and no config push.

### Grid and scrollback

- A `struct cell` is 12 bytes: a 32-bit `wc` plus an 8-byte attribute word. The attribute word is bold, dim, italic, underline, strike, blink, conceal, reverse, 24-bit fg and bg, a 2-bit colour source (default, palette or RGB), and the `clean`, `confined`, `selected` and `url` bits (`terminal.h`). Cells are kept small on purpose so that more scrollback fits and fewer cache lines are touched.
- Combining characters and grapheme clusters are stored out of line. The cell's `wc` is an index into a "composed" table (`CELL_COMB_CHARS_LO..HI`), keyed by a hash chain of codepoints. Wide-glyph tail cells hold a `CELL_SPACER` value.
- The grid is a ring of row pointers whose count is a power of two. A scrollback of `lines` becomes `lines + rows` rounded up to the next power of two. Index arithmetic is `& (num_rows - 1)`, and `grid->offset` is the ring head. Rows are allocated lazily and may be NULL until first used. The scroll viewport is `grid->view`.
- Scrolling the screen is O(1): `offset` advances and the recycled row is erased. Scrollback coordinates are "sb-relative", with 0 being the oldest row.
- Each row has `dirty` and `linebreak` flags and an optional `extra` block. The `extra` block is allocated on demand and holds OSC-8 URI ranges and styled-underline ranges. A row has `shell_integration` data, with a prompt marker and command start/end columns for OSC 133 (`terminal.h#struct row`).
- The `linebreak` flag marks hard line ends. This lets reflow, URL detection and selection treat wrapped rows as one logical line.
- Reflow on resize (`grid.c#grid_resize_and_reflow`) rewraps the logical lines and tracks selection endpoints as "tracking points". During an interactive drag-resize foot does a cheap truncating resize and holds the original grid. It reflows the original to the final size only after `resize-delay-ms` of idle time (`render.c#delayed_reflow_of_normal_grid`). The pty is paused with `term_ptmx_pause` while this is pending, and the winsize is sent to the child only at the end.
- The alternate screen is a separate grid with no scrollback. `grid_snapshot` clones a grid for URL mode.

### VT parser

- `vt.c` is a table-free Paul Williams state machine with explicit UTF-8 continuation states (`STATE_UTF8_21` and so on). It dispatches one byte at a time through `state_*_switch` functions. There is no SIMD.
- The speed comes from `terminal.c#ascii_printer_fast`, a function pointer swapped in when no charset, insert mode or OSC-8 is active. It writes the cell, advances the cursor, and only dirties the row. Any state that would change printing, such as a G0 charset, OSC-8 or insert mode, swaps in `ascii_printer_generic` (`term_update_ascii_printer`).
- Non-ASCII goes to `term_process_and_print_non_ascii`, which does the combining and grapheme work (see Unicode below).
- 8-bit C1 controls are not supported, which is a deliberate simplification.

### Rendering (pixman + fcft)

- Buffers are wl_shm. Foot keeps a small chain of buffers and tracks each buffer's `age`. It assumes the compositor may release a buffer immediately on commit. If it does not, after 10 frames it warns, or turns on "pre-apply damage".
- Frame flow (`render.c#grid_render`):
  1. Get a buffer from the chain.
  2. Dirty the old and new cursor cells.
  3. If the buffer age is greater than 0, "re-apply old damage" by copying the previous frame's damaged pixels in, so the new buffer is correct before drawing.
  4. Replay queued scroll damage.
  5. Sync the selection `selected` bits (done lazily here so that printing does not have to maintain them).
  6. Run the overflowing-glyph pre-pass, which re-dirties neighbours of glyphs that bleed into adjacent cells.
  7. Render sixels.
  8. Render dirty rows, in parallel when there are workers.
  9. Union per-worker damage regions.
  10. Emit one `wl_surface_damage_buffer` per rectangle.
  11. Draw overlays (flash, IME preedit, scrollback position).
  12. Request a frame callback, attach and commit.
- Renders are throttled by the frame callback. If a callback is outstanding, the refresh is recorded as `pending` and replayed from `frame_callback`. This is the vsync equivalent.
- `render_cell` takes about 500 lines. It resolves fg and bg (palette, RGB, reverse, dim, bold-as-bright, selection, URL, alpha-mode), draws background rectangles, blits the glyph, and then draws underlines, strikes and the cursor.
- Glyphs come from fcft, a separate library by the same author. fcft wraps FreeType, fontconfig and optionally HarfBuzz, and returns pre-rasterized pixman images from a per-font glyph cache.
- Box and block drawing (U+2500 to U+259F plus the legacy computing ranges) is drawn procedurally in `box-drawing.c`, a 3450-line file that fills pixman images at the cell size. It is on by default (`box-drawings-uses-font-glyphs=no`). The reasons given are gap-free lines, no AA artefacts and no reliance on fallback fonts.
- Colour: `gamma-correct-blending` blends in linear space. It selects a 10-bit surface format, or 16-bit if the compositor supports it, to limit banding. `surface-bit-depth` overrides this.
- Colour management: foot creates a parametric sRGB image description when the compositor offers `wp_color_manager_v1`.
- Transparency: `[colors] alpha` and `alpha-mode`. The `blur` key requests `ext_background_effect_manager_v1`.
- Client-side decorations are drawn by foot with pixman (`render_csd_*`) when the compositor has no server-side decoration. This accounts for much of `render.c`.

### Damage and scrolling (the latency design)

- Per-cell `clean` bits and a per-row `dirty` bit decide what is re-rasterized. Only dirty rows are touched. Typing one character re-rasterizes one row (plus the old and new cursor cells). The damage sent to the compositor is the union of the rectangles actually drawn.
- Scroll damage is recorded separately as a list of `{type, region, lines}` entries (`terminal.c#term_damage_scroll`). Consecutive scrolls of the same region are merged by adding their line counts. Replaying a scroll moves the old pixels instead of re-rendering text.
- Two ways to replay a scroll (`render.c#grid_render_scroll`):
  - `memmove` of the pixel rows.
  - "SHM scrolling" (`shm.c#shm_scroll`). The wl_shm pool is a memfd that is mapped much larger than the buffer, up to `max-shm-pool-size-mb`, default 512 MB, maximum 2 GB. The buffer sits at an offset inside it. To scroll by N rows, foot advances the buffer's start offset by `N * stride`. It creates a new `wl_buffer` and pixman image at the new offset, and releases the abandoned head with `fallocate(FALLOC_FL_PUNCH_HOLE)` so physical memory does not grow. Only the fixed regions above and below the scroll region (margins, scroll-region remainders) are `memmove`d. When the offset reaches the end of the pool the buffer wraps back to the start with a one-off copy.
  - The heuristic: use SHM scrolling only when `lines + region.start + (rows - region.end) < rows / 2`, which means few bytes to touch. Otherwise `memmove`. The comment in the code says the two cost about the same per byte moved.
  - SHM scrolling is 64-bit and Linux only, and it needs hole punching.
- When the compositor does not release buffers immediately, foot must double-buffer (copy the last frame's damaged area into the new buffer first). `pre-apply-damage` hands that copy to a worker thread right after commit so it overlaps with idle time.
- `min-stride-alignment=256` is set so that AMD GPUs can import the shm buffer directly.
- `damage-whole-window` is an escape hatch for a Gnome fractional-scaling bug.

### Latency and flicker control

- Delayed render (`terminal.c#fdm_ptmx`, `fdm_delayed_render`; tweak `delayed-render-lower` and `delayed-render-upper`):
  - Every pty read re-arms a "lower" timerfd, default 0.5 ms.
  - The first read of a burst also arms an "upper" timerfd, default 8.33 ms (half a 60 Hz frame).
  - The frame renders when lower fires with no more data, or when upper fires, whichever comes first.
  - The point is that a client's multi-`write()` screen update (erase, then fill) is almost never rendered half-done, and the delay stays inside the same vsync interval so it should not cost a frame.
  - Setting either value to 0 disables it.
- Application Synchronized Updates (private mode 2026, and the older DCS `= 1 s` and `= 2 s`) suppress rendering entirely until the end marker, except for overlays. While enabled the refresh hook skips the grid (`fdm_hook_refresh_pending_terminals`).
- Key presses go straight to the pty with no render dependency. The render path only starts when output comes back.
- Presentation feedback: with `wp_presentation` and `presentation-timings` enabled, foot records input time and commit time and logs the feedback. This is a built-in latency probe.
- `render-timer=osd|log|both` shows per-frame time on screen or in the log. The log splits total time into wait (pre-apply), rendering and double buffering. The OSD shows only the total.
- No external typing-latency numbers (typometer or similar) were found in primary sources, so none are reported.

### Unicode

- Width: `c32width` is custom, based on the system `wcwidth` plus tables.
- Grapheme clustering is optional, behind the build flag `grapheme-clustering` using utf8proc. It is on by default at runtime and can be toggled with DECSET 2027 (the contour spec) or `tweak.grapheme-shaping`.
- The stateful break detector `utf8proc_grapheme_break_stateful` is fed `(last, wc)`. Foot refuses to merge a codepoint into a cluster directly after a cursor move (changed in 1.28).
- Without clustering, only zero-width combining marks (width 0 and `wc >= 0x300`) are merged. A pre-composed glyph is substituted when the font has one and the width matches (`fcft_precompose`).
- The cluster's cell width is configurable via `grapheme-width-method`: `wcswidth` (sum), `double-width` (the default, capped at 2) or `max`. The documented reason is cursor desync with applications that use `wcswidth` themselves.
- Shaping is per grapheme (`fcft_rasterize_grapheme_utf32`), so flags, ZWJ emoji and keycaps render. Text-run shaping (HarfBuzz) is used only for the OSD and search box, not the grid. **There are no ligatures in the grid.**
- OSC 66 (kitty text sizing) is supported only for `w` (width).
- There is no bidi or RTL support.

### Wayland protocol coverage (registry binds in `wayland.c#handle_global`)

- Core: `wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output`, `wl_data_device_manager`.
- Shell and window: `xdg_wm_base`, `zxdg_decoration_manager_v1`, `zxdg_output_manager_v1` (needed for output geometry and names), `xdg_toplevel_icon_manager_v1`, `xdg_toplevel_tag_manager_v1` (new in 1.28, for compositor window rules), `xdg_activation_v1`, `xdg_system_bell_v1`.
- Input: `zwp_text_input_manager_v3` (IME), `zwp_primary_selection_device_manager_v1`, `wp_cursor_shape_manager_v1`.
- Presentation and scale: `wp_presentation`, `wp_viewporter`, `wp_fractional_scale_manager_v1`, `wp_single_pixel_buffer_manager_v1`.
- Effects and colour: `ext_background_effect_manager_v1` (blur), `wp_color_manager_v1` (sRGB image description).
- Misc: `wl_fixes` (`ack_global_remove`).
- Not used: `zwp_keyboard_shortcuts_inhibit`, `ext-session-lock`, `wp_content_type`, `zwp_pointer_constraints`, `wp-tearing-control`, `wp_security_context`, `linux-dmabuf`, `wp_alpha_modifier`.
- `xdg_toplevel` state: tiled edges are handled, and the "suspended" state drives terminal visibility reports (private mode 2033).
- Multi-seat is supported, with a separate keyboard, pointer and IME state per seat.

### Input

- Key handling uses xkbcommon with compose. Keys are matched in layers: foot key-bindings first (modifier and keysym, with a raw-keycode fallback), then the kitty keyboard protocol if enabled, then legacy xterm encodings (`modifyOtherKeys` through `CSI > 4 ; v m`).
- The kitty keyboard protocol is implemented with flags 1, 2, 4, 8 and 16 (disambiguate, report events, alternates, all keys as escapes, associated text) and a push/pop stack of 8 entries per screen (`input.c#kitty_kbd_protocol`, `kitty-keymap.h`, a sorted table searched by `bsearch`).
- Key repeat is generated client-side from `wl_keyboard.repeat_info`.
- Mouse reporting supports X10, 1000, 1002, 1003, SGR (1006), urxvt (1015) and SGR-Pixels (1016). It also has alternate scroll mode (1007).
- Selection modes (`selection.c`): char, word (with `word-delimiters`), whitespace-word, enclosing-quote, row, block (Ctrl), and extend. The selection is copied to PRIMARY on release. `selection-target` selects primary, clipboard, both or none.
- Paste is sanitized unless bracketed: `\r\n` and `\n` become `\r`, ESC and other C0 controls are stripped. Bracketed paste (2004) passes through unchanged.
- Text-input-v3 provides IME with preedit rendering and a cursor rectangle report. DECSET 737769 is a foot-specific IME mode.
- Touch input: tap is click, drag scrolls, and long-press then drag is a mouse drag (`long-press-delay`).
- Unicode input (`unicode-input`) lets the user type hex digits and have them inserted as a codepoint.

### Search

- Search (`search.c`) is an incremental, modal overlay. It matches directly against grid cells with no text extraction (`matches_cell`), going forward or backward from the previous match.
- It uses smart case: case-insensitive unless the query has an uppercase character.
- It matches composed (combining) cells.
- There is no regex and no whole-word option.
- Known bug noted in source: matches that start outside the view but end inside are not found.
- The selection grows to cover the match. The user can extend it by char, word or line to refine the query, which is a distinctive foot feature (`search_extend_*`).
- Return copies the match to PRIMARY and exits with the selection kept.

### URL mode

- Activated with Ctrl+Shift+O by default. It takes a snapshot of the visible grid (`url.grid_snapshot`) so the screen does not change under it, and renders from the snapshot.
- Detection (`url-mode.c#regex_detected`, `urls_collect`):
  1. For each logical line in the view (rows joined across `linebreak == false`), build a UTF-8 string.
  2. Keep a parallel array mapping each byte offset to a grid coordinate.
  3. Run the POSIX ERE (`regcomp` and `regexec`) over it. The first marked subexpression is the match.
  4. Map the match back to cells.
  5. Merge OSC-8 hyperlinks, which come from row ranges.
  6. Remove overlapping matches.
- Labels are generated in `generate_key_combos` by a breadth-first expansion of the `label-letters` alphabet (default `sadfjklewcmpgh`). The shortest label set that covers N URLs is used, which gives a prefix-free set. Labels are reversed so the last typed letter is the most local. Duplicate URLs share a label.
- Typing a label launches via `url-launch` (default `xdg-open ${url}`) or copies (`show-urls-copy`). Pressing `t` toggles whether the URL text is shown next to the label. `show-urls-persistent` keeps the mode open.
- Custom `[regex:name]` sections with `regex-launch` or `regex-copy` bindings reuse the same engine. They launch commands with `${match}`.
- An XDG activation token is requested before launching so the opened app can take focus.

### Sixel

- `sixel.c` is 2.2k lines. It decodes DCS `q` in a streaming state machine, with raster attributes, repeat, colour introducers (RGB and HLS) and graphics newline and carriage return.
- Pan and pad are derived from P1 (aspect ratio); both are clamped to 5 as of 1.28. P2=1 gives a transparent background.
- The image buffer is a growing ARGB (or 10-bit) array, resized horizontally and vertically on demand up to 10000x10000 and 1024 colours. Those limits are reported and settable through XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`).
- Special cases have specialised inner loops for aspect ratio 1:1 (`sixel_add_ar_11`) and a run-length fill (`sixel_add_many_*`).
- Colour registers can be private per image (mode 1070) or shared. Palette and geometry reports are answered.
- Images are stored as `struct sixel` entries in a per-grid list sorted by position, with a scaled-pixman cache per cell size. They are overwritten and erased cell-by-cell when text is printed over them (`sixel_overwrite_*`), scrolled with the grid, and reflowed with it (`sixel_reflow_grid`).
- Rendering clips the image to the visible rows and composites it onto the buffer with `pixman_image_composite32`. It uses OP_SRC for opaque images and OP_OVER for transparent ones. The image damage is added to the frame damage.
- DECSET 80 (sixel display mode; implemented in `csi.c` though absent from the ctlseqs table) and 8452 (cursor to the right of the image) are supported.
- Trailing fully-transparent rows are trimmed at DCS end, but only for transparent images.
- There is no kitty graphics protocol and no iTerm2 inline images.

### Notifications, bell and clipboard

- Notifications: OSC 9, OSC 777 `notify` and OSC 99 (kitty). They are routed to a configurable command with templated arguments (`${title}`, `${body}`, `${app-id}`, `${icon}`, `${urgency}`, `${replace-id}`, `${action-argument}` and more) (`notify.c`). Foot reads the notification daemon's stdout to learn the assigned ID, which supports replace and activation callbacks. Notification activation focuses the window and reports back to the app.
- Bell is configurable on four channels: system bell, urgency (xdg-activation, with red margins as fallback), notification, and visual flash. A command can also be run. Mode 1042 lets the app toggle these.
- OSC 52 is gated by `osc52 = disabled | copy-enabled | paste-enabled | enabled`. The default is `enabled` (verified in the man page and `config.c`).

### Shell integration

- OSC 7 sets the cwd, which `spawn-terminal` and `-D file://...` use. OSC 133 A, C and D are supported (not B). `prompt-prev`, `prompt-next` and the `pipe-command-output` action use the row `shell_integration` data.
- `pipe-visible`, `pipe-scrollback`, `pipe-selected` and `pipe-command-output` feed text to an arbitrary command's stdin. This is foot's main "scripting" mechanism.
- `OSC 176` sets the app-id at runtime. `OSC 555` flashes the window.
- XTGETTCAP is answered from the built-in terminfo. The terminfo entries `foot` and `foot-direct` are shipped (no ncurses dependency needed at runtime).
- `XTVERSION` is answered.

## Feature inventory (grouped by category)

### VT and control sequences
- VT220-class core plus xterm extensions: DECSET and DECRST with XTSAVE and XTRESTORE, DECRQM, DECSTBM, DECSCUSR, rectangle operations (DECCRA, DECFRA, DECERA, DECSERA, DECCARA and DECRARA), REP, tab stops, origin mode, reverse wraparound (45), `CSI 14/16/18 t` reports, window title push and pop.
- SGR: bold, dim, italic, underline (styled: single, double, curly, dotted, dashed), blink, reverse, conceal, strike, underline colour (58 and 59), 256 and 24-bit colour, colon sub-parameters.
- Palette and colour: OSC 4, 10, 11, 12, 17, 19 and their resets; OSC 11 accepts an alpha value; XTPUSHCOLORS, XTPOPCOLORS and XTREPORTCOLORS; colour-theme reports (CSI ? 996 n and DECSET 2031) with SIGUSR1/SIGUSR2 switching a light and dark pair.
- Other modes: focus events (1004), bracketed paste (2004), synchronized updates (2026 and DCS), grapheme clustering (2027), in-band resize (2048, listed as TODO in the ctlseqs table, so treat as not implemented), visibility reports (2033), kitty keyboard protocol, `modifyOtherKeys`.
- Window: OSC 0/1/2 title, `locked-title`, OSC 22 pointer shape, OSC 8 hyperlinks, OSC 52 clipboard, XTGETTCAP, DECRQSS (DECSTBM, SGR, DECSCUSR), XTVERSION.
- Missing: kitty graphics, kitty file transfer, OSC 1337 (iTerm2), OSC 133 B and the `k` and `aid` options, tmux control mode, DEC locator, ReGIS, Tektronix, C1 8-bit controls, bidi (see "What a foot user would miss" for what to keep).

### Unicode and fonts
- fontconfig syntax for `font`, `font-bold`, `font-italic` and `font-bold-italic`. A comma-separated list means primary plus user-ordered fallbacks, then fontconfig's dynamic fallback. Each fallback can have its own size and style.
- Bitmap fonts, colour emoji, subpixel rendering (follows the compositor or fontconfig), fractional scaling and DPI-aware sizing, `fontfeatures` OpenType features (per font, e.g. `ss01` or `cv01=1`), on-the-fly font resize with `font-size-adjustment` in points, pixels or percent, line height and letter spacing, per-axis letter offset, underline and strike position and thickness, `overflowing-glyphs`, `font-monospace-warn`.
- Procedural box drawing, with `box-drawing-base-thickness` and `box-drawing-solid-shades`.
- `scaling-filter` for downscaling bitmap emoji (default lanczos3).

### Input
- Remappable key bindings, mouse bindings, search and url-mode bindings, `[text-bindings]` for arbitrary byte sequences, `selection-override-modifiers`, `hide-when-typing`, touch gestures.
- Compose and dead keys via xkbcommon, multi-seat, IME.

### Rendering and performance
- Described above. Notable knobs: `workers`, `delayed-render-lower` and `delayed-render-upper`, `pre-apply-damage`, `max-shm-pool-size-mb`, `min-stride-alignment`, `surface-bit-depth`, `render-timer`, `damage-whole-window`, `gamma-correct-blending`.

### Window and UI
- No tabs, no splits, no palette, no scrollbar. UI is the CSD title bar and buttons, a scrollback position indicator (`indicator-position`, `indicator-format`), the search box, URL labels, a unicode-input overlay, a flash and a render-timer OSD.
- Padding (`pad`, with centring modes), `resize-by-cells`, `resize-keep-grid`, initial size in pixels or chars, initial mode (windowed, maximized, fullscreen), `title`, `app-id`, `toplevel-tag`, `dpi-aware`, alpha with `alpha-mode`, blur, optional CSD with configurable colours and buttons, `hide-when-maximized` and `double-click-to-maximize`.

### Scrollback, selection and search
- Scrollback with power-of-two ring and configurable `lines` (default 1000), `multiplier`, page, half-page and line scrolling actions, home and end.
- Selection by mouse, with word, row and block modes and `word-delimiters`; copy to PRIMARY, CLIPBOARD or both; extend with right click.
- Incremental search with selection-extending keys, smart case, and pasting into the search box.
- URL mode and `regex-*` modes, `pipe-*` actions.

### Shell integration and navigation
- OSC 7 and OSC 133 A/C/D, `prompt-prev` and `prompt-next`, `pipe-command-output`, `spawn-terminal` in the cwd, `-D` with `file://` URIs.

### Config, automation and integration
- INI file with sections and `include`, `-o` overrides, `-C` check (exit 230 on error), SIGUSR1 and SIGUSR2 colour theme switching, `color-theme-switch-*` and `color-theme-toggle` actions.
- No live reload. Config changes apply to new windows only (server mode loads once).
- Desktop notifications, bell channels, utmp via a helper binary (`utmp-helper`), `--pty` to display an existing pty (VM consoles), hold mode, socket-activated server, per-window `-o` overrides, `--login-shell`.
- `-e` is accepted and ignored for xterm compatibility.

### Security
- OSC 52 gating, paste sanitization, `locked-title`, a title sanity check (`is_valid_utf8_and_printable`), no OSC 52 read or paste unless enabled, no remote-control channel, no window-ops that move the window (position queries always return 0,0), `CSI 21 t` not supported.
- Sandboxing: none. Foot itself is an unsandboxed process. The only privilege separation is the external utmp helper.

## Control surface (config, actions, IPC)

### Config
- File: `$XDG_CONFIG_HOME/foot/foot.ini` (INI). Sections: main, environment, security, bell, desktop-notifications, scrollback, url, regex:NAME, cursor, mouse, touch, colors-dark, colors-light, csd, key-bindings, search-bindings, url-bindings, text-bindings, mouse-bindings, tweak.
- Empty values are not allowed. String values may be quoted.
- `include=/abs/path` with its own section scope. `-o [section.]key=value` and `footclient -o` override per window.
- `-C` validates and exits 230 on failure. Exit codes: 220 for footclient failure, 230 for server or config failure.
- Reload: none. Only the colour theme switches at runtime (SIGUSR1, SIGUSR2, actions). Setting `initial-color-theme` selects which pair is used first.

### Actions (the entire vocabulary)
- Scroll: `scrollback-up-page`, `-half-page`, `-line`, the matching `-down-*`, `scrollback-home`, `scrollback-end`. Mouse variants: `scrollback-up-mouse`, `scrollback-down-mouse`.
- Clipboard: `clipboard-copy`, `clipboard-paste`, `primary-paste`.
- Search: `search-start`.
- Font: `font-increase`, `font-decrease`, `font-reset`.
- Window: `spawn-terminal`, `minimize`, `maximize`, `fullscreen`, `quit`.
- Pipes: `pipe-visible`, `pipe-scrollback`, `pipe-selected`, `pipe-command-output`. Syntax is `pipe-visible=[cmd args] Combo`.
- URL and regex: `show-urls-launch`, `show-urls-copy`, `show-urls-persistent`, `regex-launch`, `regex-copy`.
- Prompts: `prompt-prev`, `prompt-next`.
- Other: `unicode-input`, `color-theme-switch-dark`, `color-theme-switch-light`, `color-theme-toggle`, `noop` (unbinds), `cancel` (in modal modes).
- Mouse-only: `select-begin`, `select-begin-block`, `select-word`, `select-word-whitespace`, `select-quote`, `select-row`, `select-extend`, `select-extend-character-wise`.
- A binding is `action=Mod+Mod+key [more combos]`. Keys are xkb keysym names. Buttons are libinput names (`BTN_LEFT`, `BTN_WHEEL_BACK` and so on) with an optional `+N` click count.
- Key bindings are scoped by mode (normal, search, url) and fire on press; the manual notes bindings do not get a second action on release.
- There is no way to bind a sequence of actions, to pass arguments to anything other than `pipe-*`, or to react to events (no rules).

### IPC
- The only control channel is the footclient spawn protocol (above). No JSON, no query, no event stream, no way to enumerate windows or send keys or text to a running window. Escape-sequence queries (XTGETTCAP, XTVERSION, DECRQSS, `CSI t`) are the only introspection.

## Performance (numbers + method)

All numbers are from foot's own `doc/benchmark.md` (the author's measurements; vtebench, `./target/release/vtebench -b ./benchmarks --dat /tmp/<terminal>`). Times are in milliseconds, lower is better. They date from 2022-05-12 and were measured on foot 1.12.1. They are old and the compared terminals are old versions. Treat them as indicative of foot's design, not as a current ranking.

Machine A: i9-9900, 64 GB, Radeon RX 5500XT. Window 2040x1884. Fantasque Sans Mono 10pt/23px. 10000 lines of scrollback.

| Benchmark | foot 1.12.1 (no PGO) | foot GCC+PGO | Alacritty 0.10.1 | URxvt 9.26 | XTerm 372 |
|---|---:|---:|---:|---:|---:|
| cursor motion | 14.07 | 10.40 | 24.97 | 23.38 | 1622.86 |
| dense cells | 45.46 | 29.58 | 97.45 | 10828.00 | 2323.00 |
| light cells | 4.40 | 4.34 | 12.84 | 12.17 | 49.81 |
| scrolling | 116.35 | 135.31 | 121.69 | 108.30 | 4041.33 |
| scrolling fullscreen | 5.66 | 5.70 | 10.92 | 12.09 | 124.25 |
| unicode | 10.92 | 11.56 | 15.94 | 1012.27 | 4779.33 |

Machine B: i5-8250U, 8 GB, Intel UHD 620. Window 945x1020. Dina pixelsize 12. 10000 lines of scrollback. Non-PGO foot: cursor motion 16.74, dense cells 54.10, light cells 9.66, scrolling 150.47, scrolling fullscreen 12.02, unicode 21.78. Alacritty on the same machine: 23.22, 89.43, 20.19, 129.22, 19.69, 26.11.

Observations:
- foot beats Alacritty by about 2x on the dense, light, cursor-motion, fullscreen-scroll and unicode cases. These measure parser plus full-screen throughput.
- foot ties or is slightly behind on partial-region scrolling benchmarks (scrolling, top and bottom region, small regions): 116 to 150 ms against 105 to 145 ms. That is the memmove and SHM scroll path.
- PGO helps the parse-heavy benchmarks by about 25 to 35 percent. It makes the scrolling benchmarks slightly worse on machine A.
- The foot wiki (https://codeberg.org/dnkl/foot/wiki/Performance) states the design intent: "designed and optimized for interactive use". It says foot should lose to GPU terminals when the full grid is redrawn every frame, and that damage tracking is what makes typing fast. It also says Alacritty does not do damage tracking. Alacritty renders empty cells very quickly, so a mostly-empty full screen can be fast in both. It gives no latency, startup or memory numbers.
- The vtebench method measures time to consume a stream of output, not input-to-photon latency. Neither the repo nor the wiki publishes a latency figure.

Not found in primary sources (unverified, do not quote): input-to-photon latency in ms; cold-start time; resident memory per window. The wiki and README only say qualitatively that foot is "lightweight" in dependencies, disk and memory, and that server mode saves memory and startup time. Measure these ourselves.

Memory model facts that can be derived from the source:
- Cell cost is 12 bytes. A 10000-line scrollback at 200 columns is therefore about 10000 x 200 x 12 = 24 MB if every row is allocated. Rows are allocated lazily, and the ring is rounded up to a power of two (so 10000 lines allocate a 16384-row ring, with unused rows as NULL pointers).
- Virtual address space: up to 512 MB (default) of pool per window for SHM scrolling. This is not resident, as the manual says.

## What Apotheosis should take

Grouped by decision; the structured rows carry the detail.

1. **Damage discipline, even on a GPU.** Track dirty cells and rows. Skip frames when nothing is dirty. Send per-region damage and use frame callbacks and `wp_presentation` timing. A wgpu renderer should still not redraw an idle terminal, and it should upload only dirty rows to the glyph or cell buffer.
2. **Anti-flicker scheduling.** Port the lower and upper delayed-render timers, add 2026 synchronized updates, and render after all pty reads in the loop iteration. Expose both timers as settings.
3. **Fair pty reading.** Cap bytes per read and iterations per wake. Pause the pty during interactive resize and reflow after an idle delay (`resize-delay-ms`).
4. **Small cells and a power-of-two ring.** The 12-byte cell and the ring with lazy rows are good prior art for memory per scrollback line.
5. **ASCII fast path in the parser.** Keep a hot printer that is swapped when modes require the slow one. Do not copy the byte-at-a-time model without measuring. SIMD or `memchr`-style scanning for printable runs is available in Rust.
6. **Server and client semantics.** Keep `foot --server` behaviours: fast new windows, exit code passthrough, `-N` detach, `-E`, socket activation with a supplied fd, and per-window overrides. Apotheosis can do better with a daemon that owns all windows and panes, a JSON control socket and a CLI client (the fixed decision).
7. **Complete the user-visible features:** keyboard-driven URL mode with jump labels (and a persistent variant), `regex:` custom matchers, OSC 8, OSC 7, OSC 133 A, C and D with prompt jumping and "pipe last command output", the `pipe-*` family, search that extends its selection, smart-case search, and scrollback position indicator.
8. **Selection details:** primary selection on release, word, whitespace-word, quote and row clicks, block selection, right-click extend, and paste sanitization when not bracketed.
9. **Procedural box and block drawing** at cell size, gap-free, with solid shades. In a wgpu pipeline this is best done in a shader or a pre-rasterized atlas.
10. **Terminal protocol breadth that foot already has:** kitty keyboard protocol with its stack, SGR-Pixels, styled and coloured underlines, XTGETTCAP, XTVERSION, DECRQM, rectangle ops, colour-stack ops, OSC 4, 10, 11, 12, 17 and 19, OSC 52 policy, OSC 9, 99 and 777 notifications with templated actions, OSC 66 width, 2027, 2031 and 2033, in-band resize (2048), and sixel with XTSMGRAPHICS.
11. **Wayland integration list.** Bind everything foot binds, plus `ext-session-lock` awareness, `keyboard-shortcuts-inhibit`, `pointer-constraints` and security-context if useful. Keep `toplevel-tag`, `xdg-activation` for URL launches and notification activation, fractional scale with viewporter, cursor shape, system bell, toplevel icon, text-input-v3 IME, and `ext_background_effect` for glass.
12. **Output-aware DPI and per-seat state.** Multi-seat is something foot does correctly.
13. **Built-in probes.** A frame timer (OSD and log) and presentation feedback logging. Make them first-class and exposed to the settings GUI and CLI.
14. **Config hygiene:** `-C` style validation with a distinct exit code, per-invocation overrides (`-o`), `include`, and `locked-title`-style security toggles. Apotheosis' KDL and settings GUI should cover every foot.ini key (the full list is in the config section above).
15. **Light and dark pair with CSI ? 996 n and 2031**, plus SIGUSR switching, mapped to the Abyss theme.
16. **Behaviours with a documented reason that users rely on:** double-width cap for grapheme clusters, no merge after cursor move, trimmed transparent sixel rows, `pad` with centring, `resize-by-cells`, and the red-margin urgency fallback (replace with a better in-UI cue).
17. **Config presentation.** `foot.ini`'s man page is a model: each key has a default and a rationale. Apotheosis' settings GUI should show the same default and rationale text.
18. **`--pty` mode** (display an existing pty) for VM and serial consoles, and `utmp` via a separate helper process.

## What it should avoid

- **CPU rendering as the only path.** It caps what the glass UI can do (blur, per-pixel alpha, animated tabs). Keep the grid on the GPU, but keep foot's damage thinking.
- **memfd-sliding-window scrolling.** It is an clever answer to a pixman-and-shm problem that does not exist in a wgpu renderer. Port the idea (do not redraw scrolled content) with a ring-buffered row texture or instanced rows indexed by the ring offset.
- **One thread for all windows in server mode.** foot's multiplexed I/O lets one busy window starve others and one crash kills all. Give each pane its own parser thread, or isolate panes per process.
- **No per-window crash isolation.** Prefer a supervisor that survives a renderer crash.
- **No live reload.** Apotheosis should hot-reload KDL and apply settings from the GUI instantly.
- **Per-grapheme-only shaping.** Users with ligature fonts will otherwise leave. Decide on text-run shaping for the grid.
- **Ignoring bidi, and the old `wcwidth` mismatch problem.** Specify the width policy explicitly, with a toggle.
- **Lazy `selected` bit sync in the render path.** It works but is a source of bugs. Prefer selection as a separate range structure intersected at draw time.
- **POSIX `regcomp` for URL matching.** Use a Rust regex crate (allowed licences: MIT or Apache-2.0) with the same logical-line mapping. The foot URL regex is long and hard to maintain. Keep it as an overridable default.
- **A modal URL mode that freezes the grid by cloning it.** It works, but a snapshot of the cells in view only is cheaper.
- **Silent unbounded window growth in sixel.** Cap decoded image bytes, not just dimensions (10000x10000 x 4 bytes is 400 MB).
- **Shell-out-only automation.** foot's `pipe-*` and `notify` shell out to external commands, so every automation is a process spawn. Apotheosis' Action vocabulary and rules should cover these natively and keep shell-out as one action.

### Behaviours a foot user would miss (checklist)

1. Window opens in a few milliseconds, with no visible font loading (server mode, socket activation).
2. No flicker on programs that erase then redraw (delayed render timers, 2026).
3. Typing latency under one frame on an idle system, and no CPU use when idle.
4. Exit code of `footclient` is the shell's, and `-N` returns immediately.
5. Ctrl+Shift+N opens a window in the current directory (OSC 7).
6. Ctrl+Shift+Z and X jump between prompts (OSC 133 A). Pipe the last command's output (OSC 133 C and D).
7. Ctrl+Shift+O labels URLs on screen and opens by typing letters.
8. Incremental search that can extend its match, with smart case.
9. Primary selection on select, middle-click paste, shift-click override of mouse reporting, ctrl for block selection.
10. Box-drawing characters that join perfectly at any font size.
11. Per-fallback-font size and style. Font fallback order is user-controlled.
12. Sixel images that scroll, reflow and clip correctly.
13. Fast resize without reflow flicker (`resize-delay-ms`).
14. OSC 52 per-direction enable, notification command templates, bell routing (system, urgent, notify, visual, command).
15. `foot.ini` keys: Apotheosis should provide an importer and a mapping table. Config key names are interface facts and can be quoted in such a table.
16. `-e` accepted, `-o key=value` on the command line, `--app-id` and `--class`, `-W` cols x rows, `-D` cwd, `-L`, `-H`, `--pty`.
17. Fractional scaling that is pixel exact with viewporter, and DPI-aware font sizing.
18. IME preedit rendering through text-input-v3, and multi-seat.
19. Light and dark colour pair switch by signal.
20. `text-bindings` to map a key to raw bytes.
21. Touch: tap, drag scroll, long-press drag.
22. `terminfo` entries `foot` and `foot-direct` shipped, so applications can detect it. Apotheosis needs its own terminfo (name decision) plus a compatibility story for software that checks `TERM=foot*`.

## Sources

Primary, read locally (clone at `/home/risk/.cache/apotheosis-prior-art/foot`, commit 8768826):
- `README.md`, `CHANGELOG.md`, `meson.build`, `LICENSE`.
- `doc/foot.1.scd`, `doc/footclient.1.scd`, `doc/foot.ini.5.scd`, `doc/foot-ctlseqs.7.scd`, `doc/benchmark.md`.
- `render.c`: `grid_render`, `grid_render_scroll`, `render_worker_thread`, `frame_callback`, `fdm_hook_refresh_pending_terminals`, `delayed_reflow_of_normal_grid`, `render_sixel_images`, `render_sixel_chunk`.
- `shm.c`: `shm_scroll`, `shm_scroll_forward`, `shm_scroll_reverse`, `wrap_buffer`, `shm_chain_new`.
- `terminal.c`: `fdm_ptmx`, `fdm_delayed_render`, `ascii_printer_fast`, `term_update_ascii_printer`, `term_process_and_print_non_ascii`, `term_damage_scroll`.
- `vt.c`: `vt_from_slave`, `state_ground_switch`. `grid.c`: `grid_resize_and_reflow`, `grid_snapshot`. `terminal.h`: `struct cell`, `struct attributes`, `struct row`, `struct grid`.
- `search.c`: `matches_cell`, `find_next`, `search_extend_*`. `url-mode.c`: `regex_detected`, `generate_key_combos`, `urls_assign_key_combos`.
- `sixel.c`: `sixel_init`, `sixel_unhook`, `sixel_add_*`, `sixel_reflow_grid`, XTSMGRAPHICS handlers.
- `server.c`, `client.c`, `client-protocol.h`, `fdm.c`, `wayland.c#handle_global`, `input.c#kitty_kbd_protocol`, `selection.c` (paste sanitizer), `notify.c`.

Primary, web:
- https://codeberg.org/dnkl/foot (project)
- https://codeberg.org/dnkl/foot/wiki/Performance (qualitative performance notes; fetched via summarizer)
- https://github.com/alacritty/vtebench (benchmark tool used by foot's doc)
- https://gitlab.freedesktop.org/terminal-wg/specifications/-/merge_requests/2 (synchronized updates)
- https://rockorager.dev/misc/visibility-reports/ (private mode 2033)
- https://sw.kovidgoyal.net/kitty/underlines/ (styled underlines)

Not verified: external input-latency measurements, startup time, and resident memory per window.
