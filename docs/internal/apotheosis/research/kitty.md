# Prior art: kitty

Licence note: kitty is GPL-3.0. This dossier draws on its published docs (interface facts) and on a shallow read of the tree for technique. No source is reproduced. Escape syntax, key names and defaults are interface facts. Items marked "unverified" were not confirmed against a primary source in this pass.

## Summary

kitty is a GPU-accelerated terminal (C core, Python for config, kittens and remote-control glue) and the origin of most modern terminal protocol extensions: graphics (APC G), keyboard (CSI u progressive enhancement), text sizing (OSC 66), notifications (OSC 99), pointer shapes (OSC 22), plus an extended clipboard protocol. It also ships a built-in tab/window/layout multiplexer, a JSON remote-control API, and "kittens" (helper programs that use the protocols). Its strength is breadth of well-specified, capability-queryable protocols. Its weaknesses for us are the embedded Python (config, watchers, custom hint processing, auth scripts), a bespoke config syntax, and a GPL licence that forbids any code reuse.

For Apotheosis the high-value items are: the keyboard protocol (must implement fully), the graphics protocol including unicode placeholders and animation, OSC 66 text sizing, OSC 99, OSC 22, the remote-control match language and capability/password model, the layout and session vocabularies, the hints and choose-files ideas, and the input_delay/repaint_delay/sync model for latency. All scripting hooks must be replaced by the shared Action vocabulary.

## Architecture (how it is built)

- Process model: one process owns all OS windows. A dedicated child-monitor thread does PTY I/O and VT parsing; the main thread does rendering and window-system events. They hand off through shared screen buffers guarded by a lock. Source: kitty/child-monitor.c (run loops), kitty/screen.c (parser dispatch).
- Parser: UTF-8 decode plus escape state machine, with SIMD (128/256/512-bit) accelerated scanning for the "plain text run" fast path. Source: kitty/simd-string-*.c, docs performance page.
- Cell storage: line buffers of fixed-size cell structs with separate per-line attributes, a history buffer (ring) with optional pagerhist for the pager, and a per-cell char-properties lookup (kitty/char-props.c) used for width and grapheme decisions. Source: kitty/line-buf.c, kitty/history.c.
- Render: instanced draw of cell quads from a cell-data buffer updated only for changed lines; glyphs are shaped per run (HarfBuzz), rasterised by FreeType/CoreText, and stored in a sprite-sheet texture array with a shaped-run cache; box-drawing and powerline symbols are drawn procedurally rather than from fonts. Source: kitty/fonts.c, kitty/shaders.c, kitty/glyph-cache.c, kitty/shaped-run-cache.c, kitty/decorations.c.
- Graphics storage: images held in RAM with a quota (320 MB per buffer), animation frames spilled to a disk cache; placements reference images by id and scroll with text. Source: kitty/graphics.c, kitty/disk-cache.c; quota from graphics protocol doc.
- Windowing: a patched GLFW fork (C) for Wayland/X11/macOS. Not applicable; we own the Wayland client.
- Kittens: Python programs run in a kitty window or as `kitten` Go binaries; they talk to the terminal over the same escape protocols, so every kitten is replaceable by any client.
- Config: `kitty.conf` line syntax (`option value`, `map shortcut action args`, `include`, `envinclude`), Python-defined option parsers generated from one definition table (kitty/options/definition.py), which also generates docs. The single-definition-table idea is directly useful for the Apotheosis settings GUI plus KDL plus docs.

## Feature inventory (grouped by category)

### Graphics
- Graphics protocol: APC `ESC _ G <k=v,...> ; <base64> ESC \`. Formats `f=24` RGB, `f=32` RGBA, `f=100` PNG, `f=8`/`f=16` palette-indexed that follows theme (0.50). zlib `o=z`. Mediums `t=d|f|t|s` (direct, file, temp file, shared memory) with `S`/`O` size and offset. Temp-file medium only accepted from known temp dirs and path must contain `tty-graphics-protocol`; all read failures return a single generic error so clients cannot probe the filesystem. Chunks <= 4096 bytes with `m=1` continuation.
- Actions `a=T` transmit+display, `a=p` place, `a=q` query, `a=f` frame, `a=a` animation control, `a=c` compose, `a=d` delete. Detection: send query then DA1; DA1 answer without graphics answer means unsupported. Feature query `a=q,e=<action>` (0.50).
- Placement: ids `i`, `p`; source rect `x y w h`; cell size `c r`; pixel offsets `X Y`; z-index `z` (negative under text, very negative under non-default backgrounds); `C=1` no cursor move; relative placements via `P Q H V` with chain depth >= 8 and ECYCLE/ENOPARENT/ETOODEEP errors.
- Unicode placeholders: virtual placement `U=1`, then text U+10EEEE with row/column combining diacritics and image id in the foreground colour, so images survive reflow, scrolling and work inside tmux. Missing diacritics are inferred from the cell to the left.
- Animation: frames from `a=f` with background colour or base frame, gap `z` ms, `X=1` replace not blend, terminal-driven (`s=1` stop, `2` wait-at-end, `3` loop, `v` loop count) or client-driven (`c` frame). `a=c` composes rectangles between frames.
- Delete modes `a=d,d=<x>`; lowercase keeps data, uppercase frees. Delete by id, number, cursor, cell, column, row, z, range, animation frames.
- Interaction rules: reset, alternate-screen switch and clear-screen remove images; text erase does not.
- icat kitten (`kitten icat`) is the reference display client; `kitten @ set-background-image`, window logos for decoration.

### Input
- Keyboard protocol: `CSI unicode-key:shifted:base ; modifiers:event ; text u`. Flags 1 disambiguate, 2 event types (press/repeat/release), 4 alternate keys (shifted and base-layout codes), 8 report all keys as escapes, 16 associated text. Modifiers bitfield+1: shift 1, alt 2, ctrl 4, super 8, hyper 16, meta 32, caps 64, num 128. Set `CSI = flags ; mode u` (1 replace, 2 set bits, 3 clear bits), query `CSI ? u`, push `CSI > flags u`, pop `CSI < n u`; separate stacks for main and alternate screens; popping empty resets. Keypad, media and modifier keys in PUA from 57344. Doc explicitly disparages xterm modifyOtherKeys.
- Pointer shapes OSC 22: set by CSS-cursor names, `>` push, `<` pop, `?` query, `?__current__`, per-screen stacks (min 16), terminal may override during selection or hyperlink hover; independent of mouse reporting.
- Mouse: click-to-move-cursor at prompt (needs OSC 133), ctrl+shift+right-click output to pager, click on hyperlinks, `mouse_map` bindable actions with press/release/drag/multi-click modes; `mouse_hide_wait`; touch scroll multiplier.
- Unicode input kitten (`kitten unicode_input`): search by name, recent, emoji and nerd-font tabs.
- Key map syntax supports multi-key chords (`ctrl+a>b`), modal `--new-mode`/`--mode` keymaps (unverified exact flags), `combine` of multiple actions in one binding.

### Unicode and fonts
- OSC 66 text sizing: `ESC ] 66 ; k=v:k=v ; text ST`, keys `s` scale 1-7, `w` width 0-7, `n`/`d` fractional scale (n<d), `v`/`h` alignment 0-2; text <= 4096 bytes; client decides width, terminal renders in `s*w` by `s` cells; cursor advances `s*w`; erase/insert/delete remove any multicell block they touch; blocks bigger than screen discarded. Detection by three CPR probes. Width-only fallback allowed.
- Font features: `font_features` per face (OpenType tags), `disable_ligatures` (never/always/cursor), `symbol_map` (codepoint ranges to a font), `narrow_symbols`, `modify_font` (cell width/height, baseline, underline thickness), `text_composition_strategy` (gamma/contrast for blending), `undercurl_style`, `box_drawing_scale`. `kitten choose_fonts` interactive picker with live preview, variable-font axes and named instances.
- Grapheme clustering and width: terminal uses its own tables (char-props.c); the OSC 66 `w` field is the escape hatch for width disagreement between app and terminal.

### Rendering and performance
- `repaint_delay` default 10 ms (~100 FPS), `input_delay` default 3 ms, `sync_to_monitor` default yes. Semantics from definition.py: repaint_delay is ignored when input is pending; input_delay is ignored when the input buffer is nearly full and when under 1 KB is pending, but at most once per delay period so streams of tiny writes still coalesce.
- Synchronized updates (DECSET 2026) supported; docs say lack of it costs other terminals 20-50% on benchmark.
- `cursor_trail`, `background_opacity`, `background_blur`, `dynamic_background_opacity`, `window_logo`, `tab_bar_style` (fade/slant/separator/powerline/custom/hidden), `scrollback_indicator`, visual window select (`visual_window_select_characters`).
- Resize debounce `resize_debounce_time`; cell-grid resize reflows lines (kitty/resize.c).

### Notifications
- OSC 99: `ESC ] 99 ; k=v:... ; payload ST`. Keys: `i` id, `d` done (0 more chunks), `e` base64, `p` payload type (title, body, close, icon, buttons, `?` query, alive), `a` actions (focus, report, `-` prefix), `c` close report, `o` occasion (always, unfocused, invisible), `u` urgency 0-2, `w` expiry ms, `n` icon name, `g` icon cache id, `s` sound name, `t` type, `f` app name. Buttons separated by U+2028. Replace by reusing `i`. Report-on-activate returns `OSC 99 ; i=<id> ; ST` (button number as payload). Raw payload <= 2048 bytes. Plain text must contain no C0/C1; otherwise base64. Identifier charset restricted and sanitised on echo. Capability query returns supported key values.
- `notify_on_cmd_finish` (never/unfocused/invisible/always with duration threshold and optional action) built on OSC 133.
- Bell: audio, visual (`visual_bell_duration`), tab bell indicator, `command_on_bell`, window alert/urgency.

### Shell integration and navigation
- OSC 133 marks A (prompt start), C (command executed, with command line), D (end with exit status); B parsed but unused. Features: jump to previous/next prompt, open last command output in pager, select command output, click-to-move-cursor, notify on finish.
- OSC 7 cwd reporting in kitty-specific `kitty-shell-cwd://HOST/PWD` form; used for new-window-with-cwd.
- Injection per shell without editing rc files: zsh via ZDOTDIR shim, fish via XDG_DATA_DIRS, bash via POSIX-mode ENV; `KITTY_SHELL_INTEGRATION` env carries options (`no-rc`, `no-cursor`, `no-title`, `no-cwd`, `no-prompt-mark`, `no-complete`, `no-sudo`, `disabled`).
- clone-in-kitty (new window with env/cwd/venv), edit-in-kitty (edit remote files locally), sudo/run0 wrappers passing TERMINFO, ssh kitten (copies terminfo and shell integration to remote, `kitten ssh`, per-host config in ssh.conf).
- Hints kitten: types url, path, line, hash, word, ip, hyperlink, linenum, regex (named groups become args); actions `-` paste, `@` clipboard, `*` primary, `default` open, `launch ...`; `--multiple`, `--alphabet`, `--hints-offset`, `--prefix-free`, `--ascending`; label colours. `--customize-processing` is Python (reject).
- Scrollback: `scrollback_lines` (default 2000, unverified), pager-driven search (`scrollback_pager`, default less), built-in incremental search overlay kitten in recent versions (unverified), `show_scrollback`, `show_last_command_output`, `scrollback_fill_enlarged_window`. Markers (`create_marker`, regex/text/iregex highlights) for in-grid highlight.
- Hyperlinks OSC 8 with `allow_hyperlinks`, `open_url_with`, `url_prefixes`, `detect_urls`, `url_style`.
- choose-files kitten (0.45): fuzzy file picker with tree, preview (syntax highlighting, video via ffmpeg, ebook), modes file/dir/save, multi-select, .gitignore-aware, MIME and glob filters, preview disk cache 0.5 GB, `choose-files.conf`; desktop-ui kitten lets GUI apps use it as open/save dialog (mechanism unverified, presumably xdg-desktop-portal).

### Multiplexing
- OS windows contain tabs contain windows (panes). Layouts per tab: stack, tall, fat, grid, splits, horizontal, vertical. Options `bias` 10-90, `full_size`, `mirrored`; splits has `equalize_on_window_close`, `proportional`, `split_axis`. `enabled_layouts` with inline options `tall:bias=70;full_size=2`. `layout_action` sends layout-specific verbs (increase/decrease full size, mirror, bias cycling, rotate, move_to_screen_edge, maximize, equalize). Border drag resize and keyboard resize mode.
- Sessions: text file, one keyword per line: `new_os_window`, `new_tab`, `layout`, `enabled_layouts`, `cd`, `launch` (with title/env/var/location/bias/hold), `focus`, `focus_tab`, `focus_matching_window`, `resize_window`, `os_window_size|position|state|title|class|name`, `focus_os_window`. `$VAR` expansion. `startup_session`, `goto_session`, `close_session`, `save_as_session` (options `--use-foreground-process`, `--relocatable`, `--base-dir`, `--match`, `--save-only`, `--os-window-geometry`). Position ignored on Wayland.
- Detach window/tab to another OS window; broadcast kitten (type into many windows); `launch --type=window|tab|os-window|overlay|overlay-main|background|clipboard|primary`; `--cwd=current|last_reported|oldest|root`, `--copy-env`, `--stdin-source=@screen|@screen_scrollback|@selection|@last_cmd_output`, `--hold`, `--allow-remote-control`, `--watcher` (Python, reject).
- Dockable panel kitten (`kitten panel`, layer-shell wallpaper/panel) and `quick_access_terminal` (dropdown); unverified details.

### Remote and automation
- Remote control: JSON requests carried in a DCS escape over the tty (from inside a window) or over a unix/tcp socket (`--listen-on`, `listen_on`, `KITTY_LISTEN_ON`). Gate: `allow_remote_control` yes/no/socket/socket-only/password. `remote_control_password` entries can be repeated, each limited to a list of command names with globs (e.g. `*-colors`). Unknown password triggers an interactive prompt whose decision lasts for the instance. Optional encryption with public key for SSH-forwarded use. Python `is_cmd_allowed` hook for custom auth (reject).
- Commands: launch, send-text, send-key, get-text, ls (JSON tree), focus-window/tab, close-window/tab, detach, set-tab-title, set-window-title, set-colors/get-colors, set-font-size, set-background-image/opacity, screenshot, create-marker, goto-layout, resize-window, run, kitten, action, load-config, signal-child, scroll-window, select-window, env, set-user-vars.
- `--match` language: `field:regex` combined with `and`, `or`, `not`, parentheses. Window fields id, title, pid, cwd, cmdline, num, env, var, state, neighbor, session, recent; tab fields id, index, title, window_id, window_title, pid, cwd, cmdline, env, var, state, session, recent. Negative ids count back from newest. `state:` focused/active/self/needs_attention/parent_active/parent_focused.
- User variables: `OSC 1337 SetUserVar` (iTerm2 compatible) surfaces as `var:` match field and in title templates.
- Actions: `kitten @ action <name args>` runs any mappable action remotely, which is the closest kitty gets to a single vocabulary. `combine` and `kitten`/`remote_control`/`remote_control_script` map actions exist. Watchers (Python callbacks on events) are the event hook; Apotheosis replaces with event -> actions rules.
- Event hooks otherwise: `command_on_bell`, `notify_on_cmd_finish`, `watcher`, `--on-focus-change`.

### Security
- `clipboard_control` (write-clipboard, read-clipboard, write-primary, read-primary, with `-ask` variants) gating OSC 52; extended clipboard protocol OSC 5522 supports MIME types and permission prompts (unverified detail).
- Graphics temp-file path restriction and uniform errors; image storage quota.
- `paste_actions` (quote-urls-at-prompt, confirm, confirm-if-large, filter, replace-dangerous-control-codes, replace-newline) and bracketed paste sanitising.
- OSC 99 id sanitising; `allow_remote_control` default off; `confirm_os_window_close` with foreground-process awareness.
- `kitten @` password model above.

## Control surface (config, actions, IPC)

- Config: `kitty.conf` plus included files; every option defined once in a Python table with type, default, docs, grouping; CLI override `-o key=value`; live reload (`load_config_file`, SIGUSR1) and `kitten @ load-config`; `--config NONE`. Colour themes via `kitten themes` (browse/apply, `current-theme.conf` include).
- Actions: names with args in `map <keys> <action> <args>`; `combine : act1 : act2`; `kitten`, `launch`, `remote_control`, `send_text`, `goto_layout`, `layout_action`, `goto_session`, `show_kitty_env_vars`, `debug_config`. Also `mouse_map`. Shortcuts use `kitty_mod`. A `kitty +kitten command_palette`-style list of bindings exists in recent versions (unverified).
- IPC: remote control (above) is the only first-class external API. Wire format is JSON with a version, command name, payload, optional `async_id`, `cancel_async`, `stream` flag for chunked payloads (details from remote-control protocol page not read here; unverified).
- Env contract exported to children: `KITTY_WINDOW_ID`, `KITTY_PID`, `KITTY_LISTEN_ON`, `KITTY_PUBLIC_KEY`, `KITTY_INSTALLATION_DIR`, `TERM=xterm-kitty`, `COLORTERM=truecolor`.

## Performance (numbers + method)

| Metric | Value | Method |
|---|---|---|
| Throughput, kitty 0.33 ASCII / Unicode / CSI / Images / avg (MB/s) | 121.8 / 105.0 / 59.8 / 251.6 / 134.55 | `kitten __benchmark__` writes bulk data to tty, rendering suppressed by default, Linux X11, Ryzen 7 PRO 5850U, same font/size/window across terminals, default settings |
| Next best average | gnome-terminal 3.50.1 61.83; alacritty 0.13.1 54.05; wezterm 48.5; xterm 30.72; konsole 27.48; alacritty+tmux 24.73 | same |
| Latency | "best in class", no absolute number published on the page | macOS hardware latency rig; Linux Typometer; kitty ran with input_delay 3 ms |
| Min-latency settings | `input_delay 0`, `repaint_delay 2`, `sync_to_monitor no`, `wayland_enable_ime no` | doc recommendation, costs CPU/energy |
| Energy proxy | scrolling a file in less: kitty 6-8% CPU (X+term), xterm 5-7 (janky), termite 10-13, urxvt 12-14, gnome-terminal 15-17, konsole 29-31 | htop on i7-4820K, older hardware |
| Defaults | repaint_delay 10 ms, input_delay 3 ms, sync_to_monitor yes | source options/definition.py |
| Graphics quota | 320 MB per buffer | graphics protocol doc |

Technique notes: decoupled parser thread; SIMD fast path for text runs; glyph sprite cache in VRAM; per-line dirty tracking so a typical frame uploads a few bytes; coalescing of tiny PTY reads (input_delay) trades 3 ms for fewer partial frames and less CPU, bypassed when large backlogs build; multiplexers halve throughput (alacritty+tmux row), an argument for a built-in mux.

## What Apotheosis should take

1. Keyboard protocol in full (all five flags, stacks per screen, query, release events); expose as a setting `keyboard-protocol`. Must.
2. Graphics protocol in full including unicode placeholders, relative placements, animation, delete semantics, shm medium, quotas, uniform errors. Must for video-over-kitty path (mpv --vo=kitty).
3. OSC 66 text sizing, as base for glass UI headings in grid and as a differentiator.
4. OSC 99 notifications routed to the Abyss notification service via D-Bus, with occasion/urgency/buttons/report and capability query.
5. OSC 22 pointer shapes with push/pop stack, mapped to cursor-shape-v1 Wayland.
6. Remote control semantics, but over the Apotheosis JSON control socket with the shared Action vocabulary; keep the `--match` boolean language and per-password/per-action allow-lists, replacing passwords with Unix peer credentials and Abyss capability checks.
7. Layout model: named per-tab layouts with inline options (bias, mirrored, full_size) plus a free splits tree; express as KDL and as Actions.
8. Session files: declarative, relocatable, `save_as_session`; express as KDL layouts/profiles.
9. Shell integration via env-injected shims with fine-grained opt-outs; OSC 133 A/B/C/D plus OSC 7 (standard `file://` form, not kitty-specific); jump-to-prompt, select last output, notify on finish, click-to-move.
10. Hints as an Action with typed matchers (regex with named groups, linenum) and an overlay drawn by the renderer; no Python hook, matchers declared in KDL.
11. Latency model: parser thread decoupled from render, input coalescing (input_delay semantics: skip when small pending or backlog large), repaint cadence aligned to Wayland frame callbacks, DECSET 2026.
12. Single option-definition table generating KDL schema, settings GUI and docs.
13. Per-cell procedural box drawing and powerline glyphs; text composition strategy knob for glass-over-blur contrast.
14. Font picker with live preview, variable axes, per-face features.
15. choose-files as a native glass file picker (and an xdg-desktop-portal FileChooser backend on Abyss).
16. paste_actions and clipboard_control permission model, wired to consent prompts.

## What it should avoid

- Embedded Python for config, watchers, hint processing, auth (`is_cmd_allowed`), custom tab bar functions, launch watchers. Replace with declarative rules and Actions.
- Bespoke line config; we use KDL.
- Remote control passwords in files/env; prefer peer-credential gating. Remote control over the tty DCS path should be off by default.
- kitty-specific variants where a standard exists (OSC 7 `kitty-shell-cwd://`, `TERM=xterm-kitty` requiring terminfo copies). Ship terminfo, set a conventional TERM with fallback, and advertise via XTVERSION and DA.
- Wayland feature gaps: window position ignored; own GLFW fork. We own the client.
- Python kittens with heavy dependencies (ffmpeg, calibre for previews); use our sandboxed decoder helper.
- Pager-based search as the only scrollback search; build a native search UI.
- Storing animation frames on disk without a cleanup and permission story; use memfd/tmpfs with quota and per-client accounting.
- Copying code or Python option tables verbatim (GPL-3.0).

## Sources

- https://sw.kovidgoyal.net/kitty/graphics-protocol/
- https://sw.kovidgoyal.net/kitty/keyboard-protocol/
- https://sw.kovidgoyal.net/kitty/text-sizing-protocol/
- https://sw.kovidgoyal.net/kitty/desktop-notifications/
- https://sw.kovidgoyal.net/kitty/pointer-shapes/
- https://sw.kovidgoyal.net/kitty/performance/
- https://sw.kovidgoyal.net/kitty/remote-control/
- https://sw.kovidgoyal.net/kitty/shell-integration/
- https://sw.kovidgoyal.net/kitty/layouts/
- https://sw.kovidgoyal.net/kitty/sessions/
- https://sw.kovidgoyal.net/kitty/kittens/hints/
- https://sw.kovidgoyal.net/kitty/kittens/choose-files/
- Source read for defaults and structure (technique only): kitty/options/definition.py (repaint_delay, input_delay, sync_to_monitor option text), kitty/child-monitor.c, kitty/screen.c, kitty/graphics.c, kitty/disk-cache.c, kitty/fonts.c, kitty/shaders.c, kitty/simd-string-*.c, kitty/char-props.c, kitty/history.c, kitty/resize.c, kittens/ directory listing. Shallow clone at /home/risk/.cache/apotheosis-prior-art/kitty.
- Not read (unverified): remote-control wire protocol page, extended clipboard protocol (OSC 5522), kitten ssh details, desktop-ui kitten mechanism, built-in scrollback search, command palette kitten.
