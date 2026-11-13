# Prior art: tmux and zellij (built-in multiplexer)

Research date 2026-11-12. Source trees inspected (shallow clones, never built):
zellij 0.46.0 (`~/.cache/apotheosis-prior-art/zellij`, MIT) and tmux post-3.7c "3.8" development tree
(`~/.cache/apotheosis-prior-art/tmux`, ISC). Both are newer than most documentation, so several
facts below (tmux floating panes, zellij native terminal window) come straight from source and
CHANGELOG/CHANGES and are not yet in the public docs. Prose only; no source is reproduced.

## Summary

- Both tools solve the same problem: a long-lived server process owns the ptys and the screen model;
  thin clients attach over a Unix socket. That gives detach/attach, multi-client, and survival of
  the UI. Apotheosis is a GUI terminal, so the server/client split is a decision, not a given. The
  recommendation is: keep panes, tabs and layouts in a session model that is separable from any one
  window, and make the persistence boundary explicit (see "What Apotheosis should take").
- tmux is a single-threaded libevent C server, ~99 commands, a format language (`#{...}`) used for
  status, titles, hooks and scripting, and a control mode (`-C`) that is effectively a machine API.
  Session persistence across reboot is NOT built in (tmux-resurrect/continuum are external).
- zellij is a multi-threaded Rust server (separate threads for screen, pty, wasm plugins,
  pty_writer, background jobs) whose clients receive pre-rendered ANSI (or a structured binary
  frame, wire v4). It has declarative KDL layouts, swap layouts that re-tile automatically by pane
  count, floating and stacked panes, built-in session serialization and "resurrection", a WASM plugin
  system with a permission model, a CLI (`zellij action ...`, `zellij pipe`, `zellij subscribe`)
  and, new in 0.46, a native terminal window (`zellij-terminal`, glow/glutin/swash/winit/wayland-client)
  and a web client. Zellij is therefore the closest existing analogue of "terminal with built-in mux".
- The key lesson from both: panes, floating panes, stacking, layouts, hooks/events, a scriptable
  control channel and resurrection are table stakes. Apotheosis's stated constraint (no embedded
  scripting language, one Action vocabulary) is closest to zellij's Action enum plus KDL, but
  deliberately drops WASM plugins; what plugins give zellij (status bars, pickers, notifications)
  must instead be first-party glass UI driven by the Action/event vocabulary.

## Architecture (how it is built)

### tmux
- One server per socket, socket at `$TMUX_TMPDIR` (default `/tmp`) under `tmux-<uid>/` with owner/
  mode check at startup (`tmux.c`). Clients speak imsg (OpenBSD message framing) over the socket;
  `tmux-protocol.h` defines MSG_VERSION, MSG_IDENTIFY_* (client sends flags, term name, features,
  cwd, environment, tty fd via fd passing), MSG_COMMAND, MSG_READ_OPEN etc. `PROTOCOL_VERSION` is 8
  and must be bumped on any change.
- Event loop is libevent, strictly single-threaded; each pane pty is a bufferevent
  (`window.c: window_pane_read_callback` disables reads when downstream is backed up).
- Command execution goes through a queue (`cmd-queue.c`): every command (from key binding, hook,
  config file, control client, `run-shell` callback) is parsed to a command list, expanded with
  formats, and queued per client; `if-shell`, `run-shell`, `wait-for` allow sequencing and async
  waits. There is no scripting language but there is a conditional/loop-free format language
  (`format.c`, 7.3k lines) with comparisons, `#{?cond,a,b}`, loops over windows/panes
  (`#{W:...}`), string ops and regex substitution.
- Screen model: `grid.c` stores cells compactly (a small fixed cell with an "extended" flag; cells
  needing more than the compact form spill to a side array), with history as lines scrolled off the
  top and `grid_collect_history` trimming to `history-limit` (default 2000 lines, per session).
  `input.c` is a table-driven VT parser. `screen-redraw.c` computes borders and draws only changed
  regions to each client's `tty.c`, which diffs against the client's terminal capabilities
  (terminfo + `terminal-features` overrides).
- Layout is a tree of cells (`layout.c`): split cells (horizontal/vertical) with leaf panes.
  Preset layouts live in `layout-set.c` (even-horizontal, even-vertical, main-horizontal(-mirrored),
  main-vertical(-mirrored), tiled; names accept unambiguous prefixes). Layouts serialize to a string
  with a 16-bit rotate-and-add checksum prefix (`layout-custom.c: layout_checksum`) so `select-layout`
  can restore geometry. In the current tree the string is a JSON subset (`V` version, `L` layout
  object) that also carries floating panes; control-mode clients receive the old format unless they
  opt into a new-layouts flag.
- Modes (`window-copy.c`, `window-tree.c`, `window-buffer.c`, `window-client.c`, `window-customize.c`,
  `window-switch.c`, `window-panes.c`, `window-clock.c`, `mode-tree.c`) are pane-resident UIs
  built on one shared tree/list widget (`mode-tree.c`). 3.8 makes display-panes a mode too.

### zellij
- Process model: a server per session (`zellij-server`), clients (`zellij-client`), and since 0.46
  a native GUI client (`zellij-terminal`). Session sockets live under a socket dir; session listing
  is a directory scan plus a session index; `--` after `attach -a` can start an initial command.
- Server threads (named in `zellij-server/src/lib.rs`): `server_listener` and per-client
  `server_router`, `screen` (the layout and pane state owner, 16.6k lines in `screen.rs`), `pty`
  (async readers per pty feeding `terminal_bytes.rs`), `pty_writer`, `wasm` (plugin host), and
  `background_jobs`. Threads communicate via typed instruction enums over channels
  (`thread_bus.rs`). So state has a single owner (Screen) and others message it.
- IPC: protobuf contract (`client_server_contract/client_to_server.proto`, `server_to_client.proto`),
  roughly 37 client->server and 30+ server->client message kinds: attach/detach, resize, key, action,
  host color/size replies, kitty-graphics/sixel/zlib capability announcements, session list/preview/
  attention, pane mirror subscription, render frame ack. Plugin API is also protobuf
  (`plugin_api/*.proto`).
- Rendering to clients: the server holds a character grid per terminal pane (`panes/grid.rs`,
  `terminal_character.rs` where `TerminalCharacter` is a Copy struct with a shared-pointer style),
  composes frames in `output/mod.rs` (`CharacterChunk`s with per-client serialization, cursor
  motion, kitty-graphics and sixel placement), and sends an ANSI string to a terminal client. For
  smarter clients there is a "structured render" wire format (`zellij-utils/src/structured_render.rs`:
  version 4, 48-byte header, row records, 20-byte cells, link table, cluster table for grapheme
  clusters, scroll and shift records, graphics records inline or via shared file) with frame acks
  (`RenderFrameAck`) for back-pressure. The native window and web client consume this.
- Native terminal (`zellij-terminal`): own emulation module (damage tracking as `Everything` or
  `Rows`), glyph atlas (4096-wide, 16 MiB mask and 16 MiB colour budgets, 32k entries), sprite-drawn
  box characters, a `Pacer` that derives the frame interval from the monitor refresh rate
  (clamped to 20 Hz-500 Hz, fallback 60 Hz), scroll animation, retained scene graph. OpenGL (glow),
  not wgpu.
- Plugins: WASM via wasmi 1.1.0 (an interpreter; zellij moved wasmer -> wasmtime -> wasmi, see
  CHANGELOG), WASI-based, `ZellijPlugin` trait (`load`, `update(Event)->bool`, `pipe`, `render`).
  Plugins subscribe to event types, issue commands from a ~177-variant command protocol, can have
  background "workers", and are gated by 17 permission types (ReadApplicationState,
  ChangeApplicationState, OpenFiles, RunCommands, OpenTerminalsOrPlugins, WriteToStdin, WebAccess,
  ReadCliPipes, MessageAndLaunchOtherPlugins, Reconfigure, FullHdAccess, StartWebServer,
  InterceptInput, ReadPaneContents, RunActionsAsUser, WriteToClipboard,
  ReadSessionEnvironmentVariables). Permission grants are cached to disk. Default plugins:
  tab-bar, status-bar, compact-bar, strider (file tree), session-manager, plugin-manager,
  configuration, context-menu, prompt, layout-manager, multiple-select, about, share.
- Web client: built-in web server (feature `web_server_capability`), TLS required off-localhost,
  login tokens, websocket heartbeat, PWA manifest, mobile UI.
- Pane tree: tiled panes carry `PaneGeom` with `Dimension`s that are percent or fixed
  (`pane_size.rs`); resizing is a constraint solver (`tiled_panes/pane_resizer.rs`); stacks are a
  container of panes where all but one collapse to a title row (`tiled_panes/stacked_panes.rs`);
  floating panes (`floating_panes/`) have x/y/width/height as percent-or-fixed, a z-order, `pinned`
  (stays visible when layer is toggled), and a "find room" search that tries half-size centered,
  top-left and top-right candidates with a stepping offset until a non-overlapping position fits.

## Feature inventory (grouped by category)

### Sessions, detach/attach
- tmux: `new-session`, `attach-session`, `detach-client`, `switch-client`, session groups
  (shared window set), `has-session`, `list-clients`, `server-access` (multi-user ACL), `lock-*`,
  `detach-on-destroy`, `clear-on-attach` (3.8: keep previous screen contents in host scrollback).
- zellij: session names, aliases, `attach`, `list-sessions`, `kill-session`, `delete-session`,
  session-manager plugin with live previews, `mirror_session`, `on_force_close`, per-client focus
  and (0.45) per-client tab sizes when clients are not on the same tab, remote attach, `watch`
  (read-only follower), cross-session activity notifications and an inbox (0.46).

### Persistence and resurrection
- tmux: none built in. State is lost on server exit. Community plugins snapshot windows/panes/
  cwds/commands to disk and replay on restore (tmux-resurrect; unverified here, not inspected).
- zellij: `session_serialization` (default true) periodically writes the session as a KDL layout
  (`session_serialization.rs: serialize_session_layout`) to the cache dir, including tabs, tiled
  and floating panes, geometry (percent), cwd, title, focus, borderless, stacked orientation,
  colours, running command (discovered from the pane's child process) and optionally pane viewport
  and up to `scrollback_lines_to_serialize` lines as separate content files
  (`serialize_pane_viewport`). `serialization_interval` sets cadence. `post_command_discovery_hook`
  rewrites discovered commands. Exited sessions are listed as "EXITED - attach to resurrect".
  Resurrected command panes start suspended ("press Enter to run") so nothing executes unprompted.
  0.46 adds `zellij action set-resurrection-command` so a program can declare how to relaunch itself,
  and `dump-layout`, `save-session`. Changelog notes a past "resurrection layout serialization
  corruption" bug (5690): write atomically.

### Panes and layouts
- tmux: split-window, new-pane (3.8), break/join/move/swap/rotate-pane, zoom, `select-layout`
  presets and custom layout strings, `resize-pane`, synchronize-panes, pane titles, pane border
  status/format/lines/indicators, `pane-scrollbars` with auto-hide (3.8), `window-size`, 
  `aggressive-resize`, `remain-on-exit` (incl. `failed-key`).
- zellij: KDL layout files with `pane`, `tab`, `tab_template`, `pane_template`, `children`,
  `split_direction`, `size` (percent or fixed), `borderless`, `focus`, `name`, `cwd`, `command`,
  `args`, `start_suspended`, `close_on_exit`, `plugin location=...`, `floating_panes` with
  coordinates, stacked containers. Swap layouts (`swap_tiled_layout`, `swap_floating_layout`) are
  ordered lists of layouts each guarded by `min_panes`, `max_panes` or `exact_panes`
  (`LayoutConstraint`); the server picks the first matching layout when the pane count changes and
  the user can cycle (NextSwapLayout) or select by name (0.46). `auto_layout` toggles it.
  `zellij --layout`, `new-tab --layout`, `override-layout`, `dump-layout`.

### Floating panes
- zellij: toggle visibility of the whole floating layer, per-pane pin, drag and resize with mouse,
  ctrl-drag to create (0.46), staggered placement, embed/float toggle of a pane, fullscreen float,
  plugin popups.
- tmux 3.8: `new-pane` with title (-T), border lines (-B), wait for exit (-W), modal (-O) with
  click-outside-to-close (-C), forward-all-keys (-K), Esc/C-c close (-D), stay-above-zoom (-A);
  Ctrl-drag creates; `move-pane`/`resize-pane` for floats; splitting a float makes a non-overlapping
  float; break-pane floats, join-pane tiles; `pane-border-status top-floating/bottom-floating`;
  layout clamped to window on resize (`layout_clamp_floating_panes`); one modal pane per window.

### Stacked panes
- zellij: a stack is N panes in one cell with only one expanded; others show one-line titles.
  0.45 redid the stack UI (stack lists) and 0.46 added horizontal stacks and a configurable list
  orientation per stack. Operations: combine aligned panes into a stack, break pane out of stack,
  resize stack, `stacked_resize` option, swap layouts that build stacks when pane count exceeds a
  threshold (`default.swap.kdl` "stacked" and "half-stacked").
- tmux: no stacked panes; closest is zoom or `layout main-*`.

### Copy mode, scrollback, search
- tmux: vi/emacs copy mode (`window-copy.c`), search, marks, rectangle
  selection, 3.8 auto-refresh of copy mode while content changes, paste buffers (`buffer-limit`),
  `capture-pane`, `pipe-pane`, `clear-history`, `set-clipboard` (OSC 52), `copy-command`.
- zellij: scroll mode and search mode in `panes/search.rs`, selection in `panes/selection.rs`,
  `EditScrollback` opens the scrollback in `$EDITOR`/`scrollback_editor`, `DumpScreen`,
  `copy_command`, `copy_clipboard`, `copy_on_select`, `scroll_buffer_size` (default 10000),
  OSC 8 hyperlinks, horizontal scroll for programs that request it (0.46).

### Hooks, events, notifications
- tmux: hooks `after-<command>` for every command plus client-*, session-*, window-*, pane-*
  events. 3.8 reworks hooks onto an internal event bus with key/value payloads, adds format
  "monitors" (poll a format each second; fire on change or when true), `set-hook -B/-E/-T`,
  `wait-for -E` and `-v`, and OSC 133 events `pane-command-started`, `pane-command-finished`,
  `pane-shell-prompt`. Activity/bell/silence monitoring per window.
- zellij: plugin event subscriptions (mode, tab, pane, session, key, mouse, visible, file-system,
  command results, pane-closed, etc.), per-pane `WatchPane` and `MutePane`, attention/inbox with
  OSC 7501 (zellij-specific; unverified semantics), desktop notifications, `notify` flag on command
  panes.

### Input and keybindings
- tmux: prefix model with key tables (`bind-key -T`), repeatable binds, `user-keys`, `extended-keys`
  (CSI u / xterm modifyOtherKeys), `escape-time`, `send-keys`, mouse binds. Default `C-b`.
- zellij: modal (normal, locked, pane, tab, resize, move, scroll, search, session, tmux-compat) with
  presets (`default`, `unlock-first`) selectable by name and leader keys; `clear-defaults=true`;
  kitty keyboard protocol support option; configurable mouse bindings (0.46); `Run` binding with
  `ZELLIJ_ORIGIN_PANE_ID` and background mode; nested-session input mode and keybinding reporting.

### Menus and UI chrome
- tmux: status line as formats, `display-menu`, `display-popup` (pty overlay), `choose-tree`,
  `customize-mode` (browse and edit every option, hook, env), `switch-mode`, `command-prompt`
  with history, 3.8 `command-prompt -P` in-pane prompt, builtin light/dark themes with theme
  detection and "themeblack" style names.
- zellij: pane frames (titles-only mode in 0.45), tab bar, status bar, compact bar, right-click
  context menu defined in KDL (`context_menu { pane {...} tab {...} bar {...} }` with
  `item/separator/remove/after/before/clear-defaults`), interactive settings screen
  (`configuration` plugin), plugin-manager, themes (KDL), theme hue (`explicit_theme_hue`).

### Integration
- tmux: control mode (`-C`) with `%output`, `%extended-output`, `%begin/%end/%error`, notifications
  for layout/window/session change, `pause-after` flow control, per-pane `refresh-client -A`
  on/off/pause, `refresh-client -B` subscriptions; iTerm2 uses it as a native-GUI backend.
  `passthrough` of escape sequences (`allow-passthrough`), `terminal-features` per terminal.
- zellij: `zellij action <~190 verbs>`, `zellij run/edit/plugin/pipe/subscribe/prompt/web/setup`,
  `zellij pipe` for named pipes into plugins, `zellij subscribe` to pane render updates
  (`SubscribeToPaneRendersMsg`), `dump-screen`, `dump-layout`, `send-keys`, `write`, `paste`,
  pane id targeting, `ZELLIJ_SESSION_NAME` env, `$ZELLIJ_PANE_ID`, and "distributions": using
  zellij as a library.

### Security
- tmux: socket perms check, `server-access` ACL, `allow-passthrough` (default unverified), `set-clipboard`
  `-S` socket paths, lock commands.
- zellij: plugin permission prompts and per-plugin grant cache, https required for non-local web
  server unless config dangerously disables certificate enforcement (0.46), login tokens,
  `RunActionsAsUser` permission, plugin sandbox (WASI preopens), pipe permission tightening.

## Control surface (config, actions, IPC)

- tmux config: line-oriented `tmux.conf` of commands (`set-option -g`, `bind-key`, `source-file`),
  options table of ~100+ options in `options-table.c` with scope (server/session/window/pane),
  types (number/string/flag/choice/colour/style/command/array), minimum/maximum/default, and
  `set -u` unset to inherit from parent scope; `@user` options. `show-options -F` (3.8) uses formats.
  All actions are commands, usable from bindings, hooks, CLI and control mode: one vocabulary.
  `list-commands` is introspection. Target syntax `session:window.pane` with `{marks}`, `{last}`,
  `{next}`, `{up-of}` tokens. `command-alias` for user aliases.
- zellij config: KDL `config.kdl`, `layouts/*.kdl`, `themes/*.kdl`. Options mirror the CLI options
  struct (`options.rs`) and merge (CLI over config over default). `keybinds` are per-mode maps
  from key to a list of Action nodes (`bind "Alt y" { NewTab; }`); the same Action enum (193
  variants) backs keybinds, plugin `RunActionsAsUser`, context menu items, layouts' `Run` nodes,
  and the CLI (`zellij action`, with a parallel clap enum). Config hot reload (0.46 adds an
  interactive settings screen that edits the KDL preserving comments, `config_file_edit.rs`).
  `plugins {}` alias block maps names to wasm locations with config blocks.
- IPC: tmux imsg (binary, versioned, fd passing); zellij protobuf over Unix socket with distinct
  message families (client->server 37 kinds, server->client 30+); control mode in tmux is a text
  protocol designed for programs (line-framed, `%begin <time> <num> <flags>` blocks).

## Performance (numbers + method)

No first-party performance figures were found in either source tree or changelog. Items that are
verifiable from source:
- tmux control mode flow control: `CONTROL_BUFFER_LOW` 512 and `CONTROL_BUFFER_HIGH` 8192 bytes
  of write-event watermark (`control.c`); output older than `pause-after` seconds pauses the pane
  for that client.
- tmux `history-limit` default 2000 lines; zellij `scroll_buffer_size` default 10000.
- zellij native client frame pacing: refresh-rate-derived interval, clamped 20-500 Hz, fallback 60 Hz.
- zellij atlas budgets: 16 MiB mask + 16 MiB colour textures, 32,768 entries max, 4096 px wide.
- zellij changelog says "improve output/stdout/scrolling speed" (PR 5815) and "reduce server memory
  by not copying builtin plugins onto the heap" (5622), with no figures. Method unknown.
- Known weak spot (from changelog): fragmented stdin on slow SSH produced garbage input (fixed,
  0.45, PR 5320), meaning input parsing must tolerate arbitrary segmentation.
Treat all published latency/throughput claims for tmux/zellij as unverified; Apotheosis should
benchmark its own mux path against `vtebench`-style throughput and a keypress-to-photon test.

## What Apotheosis should take

1. A first-class session/tab/pane/stack/float model with handle-based IDs that every Action can
   target (zellij's pane ids, tmux's target syntax with tokens).
2. Declarative KDL layouts with percent/fixed sizing, templates (`tab_template`/`pane_template`/
   `children`), floating coordinates, stacks, cwd/command/title/focus/borderless, `start_suspended`,
   plus swap layouts guarded by `min/max/exact_panes` and named selection.
3. Resurrection that is KDL layout based: write the live session as a layout, atomically, on an
   interval and on change; include cwd, discovered command, geometry, titles, optional scrollback
   content files; restore command panes suspended. Let programs declare their own resurrection
   command (shell-integration channel).
4. Floating panes as a layer: visibility toggle, per-pane pin, modal flag (tmux 3.8 modal with
   click-outside and key-forwarding), stay-above-zoom, ctrl-drag to create, non-overlapping
   placement search, clamp on resize.
5. Stacked panes with vertical and horizontal title lists, plus auto-stack swap layouts.
6. Hooks/events as `event -> actions` rules, using tmux 3.8's design as the model: event payload
   key/values, format "monitors" that fire on change, OSC 133 command-started/finished/prompt events,
   activity/bell/silence events. This is how Apotheosis gets plugin-like behaviour without scripting.
7. One Action vocabulary exposed as keybinds, palette, rules, profiles, layouts, context menu items,
   and the control socket; the CLI is generated from the same enum (avoid zellij's parallel
   hand-maintained clap enum drifting from Action).
8. A mux protocol designed for native clients: versioned, framed, back-pressured with frame acks,
   structured cell frames rather than ANSI re-serialization (zellij's structured_render lesson),
   shared-memory side channel for large graphics.
9. Control mode equivalent: line- or JSON-framed event stream with per-pane output subscription,
   pause/flow control watermarks, subscriptions to formats (tmux `refresh-client -B`).
10. Settings introspection: every option has scope, type, range, default, and appears both in the GUI
    and KDL (tmux `customize-mode` and zellij `configuration` plugin both do this; the GUI edits the
    KDL in place preserving comments).
11. Multi-client semantics: per-client focus and per-client viewport sizes (zellij 0.45), tmux
    `window-size` smallest/largest/latest/manual and `aggressive-resize`, read-only `watch` clients.
12. Session picker with live previews, resurrectable sessions listed alongside live ones, rename,
    and cross-session attention (inbox) with desktop notifications.
13. Plugin-equivalent chrome built in: tab bar, status area, file-tree picker, session manager,
    command prompt, context menu with KDL-customizable items, pane-frame titles and styles.
14. Copy/search/scrollback: copy mode with vi/emacs tables, search across panes, auto-refresh copy
    mode, edit-scrollback-in-editor, dump-screen, pipe-pane, OSC 52 and `copy_command`.
15. Permission-gated capabilities for anything that crosses a trust boundary (pane content read,
    input injection, run commands, clipboard write), borrowed from zellij's 17 permission types,
    but mapped onto Abyss capabilities rather than a plugin sandbox.

## What it should avoid

- Embedding WASM (or any scripting) for plugins: contradicts the fixed decision, and zellij's
  plugin runtime is the largest source of complexity (177 plugin commands, protobuf API, workers,
  permission cache). The wasm runtime churn (wasmer, wasmtime, wasmi) shows the cost.
- Re-serializing the whole grid to ANSI for every client (tmux and zellij terminal mode both pay
  this; it is the reason zellij built structured_render).
- tmux's prefix-key-only default and cryptic format strings as the primary UX; keep format
  expressions for rules but give them a GUI builder.
- Two parallel enums (Action vs CLI) with hand mapping.
- Non-atomic session serialization (zellij 0.46 fixed a corruption bug) and auto-running restored
  commands.
- Silent loss of state: tmux has no persistence; a GUI terminal must not either.
- Treating the multiplexer as an ANSI-terminal-in-terminal when the host is Apotheosis itself:
  nested-session passthrough (zellij `nested_session`, tmux `allow-passthrough`) is only needed when
  Apotheosis runs inside another mux; support it but do not design around it.
- Copying zellij's licence-incompatible constructs: none (MIT) but note zellij ships Noto Color Emoji
  assets in `zellij-terminal` (excluded from crate; separate licence file). Check fonts separately.

## Sources

Docs and changelogs
- https://zellij.dev/documentation/ (layouts, swap layouts, plugins, permissions, CLI actions)
- https://github.com/zellij-org/zellij/blob/main/CHANGELOG.md (0.45.0, 0.45.1, 0.46.0 entries)
- https://github.com/zellij-org/zellij/blob/main/docs/ARCHITECTURE.md
- https://github.com/tmux/tmux/blob/master/CHANGES (3.8 section: floating panes, JSON layouts, hooks)
- https://github.com/tmux/tmux/wiki/Control-Mode
- https://man.openbsd.org/tmux (manual; formats, options, hooks)

Zellij source (upstream path and function)
- `zellij-server/src/lib.rs` (thread spawns: server_listener, server_router, pty, screen, wasm, pty_writer, background_jobs)
- `zellij-server/src/screen.rs`, `zellij-server/src/tab/mod.rs`, `tab/swap_layouts.rs`, `tab/layout_applier.rs`
- `zellij-server/src/panes/floating_panes/mod.rs` and `floating_pane_grid.rs: find_room_for_new_pane`
- `zellij-server/src/panes/tiled_panes/stacked_panes.rs` (`combine_vertically_aligned_panes_to_stack`, `combine_horizontally_aligned_panes_to_stack`, `break_pane_out_of_stack`), `pane_resizer.rs`
- `zellij-server/src/output/mod.rs` (`serialize`, `serialize_chunks`, `serialize_kitty_frame`)
- `zellij-server/src/panes/kitty_graphics/`, `panes/sixel.rs`, `panes/grid.rs`, `panes/search.rs`
- `zellij-utils/src/session_serialization.rs: serialize_session_layout`, `serialize_tiled_pane`
- `zellij-utils/src/sessions.rs: get_resurrectable_sessions`
- `zellij-utils/src/input/layout.rs` (`LayoutConstraint`, `Run`, `SwapTiledLayout`), `input/actions.rs` (`Action`), `input/options.rs`, `input/config_file_edit.rs`, `input/keybind_presets.rs`
- `zellij-utils/src/client_server_contract/client_to_server.proto`, `server_to_client.proto`
- `zellij-utils/src/structured_render.rs` (wire v4 constants)
- `zellij-utils/src/plugin_api/plugin_permission.proto`, `plugin_command.proto`
- `zellij-utils/src/cli.rs` (`Sessions`, `CliAction`)
- `zellij-utils/assets/config/default.kdl`, `assets/layouts/default.swap.kdl`
- `zellij-terminal/src/render/pacing.rs`, `render/atlas.rs`, `emulation/damage.rs`
- `zellij-tile/src/lib.rs` (`ZellijPlugin` trait)

tmux source (upstream path and function)
- `server.c`, `proc.c: proc_send`, `client.c`, `tmux-protocol.h` (PROTOCOL_VERSION 8)
- `layout.c` (`layout_clamp_floating_panes`, `layout_floating_pane`), `layout-custom.c` (`layout_checksum`, `layout_parse_json`), `layout-set.c` (`layout_sets`)
- `control.c` (`CONTROL_BUFFER_LOW/HIGH`, `control_check_age`, `control_write_output`)
- `cmd-queue.c`, `format.c`, `options-table.c`, `grid.c` (`grid_collect_history`, extended cells), `input.c`, `window.c: window_pane_read_callback`
- `window-copy.c`, `mode-tree.c`, `window-customize.c`, `window-tree.c`

Not verified / not inspected: tmux-resurrect and tmux-continuum internals; zellij OSC 7501
semantics; any benchmark of tmux or zellij throughput or latency.
