# Prior art: WezTerm

Subject: https://github.com/wezterm/wezterm (MIT, Wez Furlong). Source read from a shallow clone at
`/home/risk/.cache/apotheosis-prior-art/wezterm` (commit at HEAD on 2026-11-12). Techniques are
described in prose; interface facts (action names, config keys, PDU names) are quoted. Items I did not
verify directly in source or docs are marked "unverified".

## Summary

WezTerm is a GPU-accelerated Rust terminal with a built-in multiplexer, a Lua configuration/scripting
layer, and its own cross-platform window crate. Its three most transferable ideas for Apotheosis are:

1. **Domains.** A domain is a source of panes (local PTY, unix-socket mux server, ssh, tls, exec,
   WSL). The GUI is just a client attached to one or more domains; panes from any domain render
   identically. This is the right abstraction for local, remote and agent sessions.
2. **One typed action enum (`KeyAssignment`) used by keys, mouse, palette, launcher and `EmitEvent`**,
   plus key tables (modal binding stacks) and a leader key. This is exactly the Action vocabulary
   Apotheosis wants; WezTerm proves it works, but its escape hatch is Lua callbacks.
3. **Lua as the control plane.** Most real-world Lua configs do a small number of things (conditional
   config by host/OS, event hooks that restyle tabs/status, programmatic layouts, custom
   pickers/prompts, reacting to shell user-vars). Each maps to a declarative KDL feature if we
   provide: conditions, event->actions rules, format templates, layouts, and typed prompt actions.

Weaknesses worth avoiding: the Lua surface is huge (179 config options plus about 55 `wezterm.*`
functions, 84 key-assignment docs, 12 window events) and is the main source of config breakage and
startup latency; the mux protocol ships whole styled lines rather than a terminal state delta stream
and requires matching versions; multiplexing is bolted on rather than session-persistent by default;
the GUI is a custom toolkit (box model) that is limited versus a full glass UI; Wayland support has
been a long-running source of bugs (unverified claim from issue tracker; not sampled here).

## Architecture (how it is built)

Workspace crates (from the repo tree): `term` (VT state machine, `terminalstate/` with
`kitty.rs`, `iterm.rs`, `sixel.rs`, `image.rs`, `performer.rs`, `keyboard.rs`, `mouse.rs`),
`termwiz` (terminal toolkit: cells, surface with change log, escape parser, input decoder,
terminfo caps, line editor, widgets; MIT, published to crates.io), `mux` (Mux, Domain, Pane, Tab,
Window, tmux control-mode client), `codec` (PDU wire format), `wezterm-client`, `wezterm-mux-server-impl`,
`wezterm-ssh` (libssh2 or libssh backends), `wezterm-font` (locator + shaper + rasterizer),
`wezterm-gui` (renderer, overlays, tab bar, palette), `window` (own windowing: X11/Wayland/macOS/Windows),
`config` (Lua loader, serde types, `keyassignment.rs`), `lua-api-crates/*` (wezterm.* modules).

**Mux model.** Mux is a process-global singleton holding Windows -> Tabs -> split trees of Panes,
each Pane owned by a Domain. Domains implement a trait with spawn, split, attach, detach, and a state
(attached/detached). Pane trait abstracts "something that renders lines": `LocalPane` wraps a
`Terminal` plus a PTY; `ClientPane` (wezterm-client) is a remote proxy with its own renderable cache.
(mux/src/domain.rs, mux/src/pane.rs, mux/src/localpane.rs, wezterm-client/src/pane/renderable.rs)

**Domains.**
- Local: default; PTY via the `portable-pty` crate.
- Unix: a `wezterm-mux-server` listens on a unix socket (default under the runtime dir, ownership
  checked unless `skip_permissions_check`); GUI auto-starts the server unless `no_serve_automatically`;
  optional `proxy_command` replaces a direct connect (for WSL/netcat/socat); optional
  `local_echo_threshold_ms` enables predictive local echo when measured RTT exceeds the threshold.
- SSH: two flavours. Plain ssh domains run a PTY over libssh2/libssh (`ssh_backend`); `multiplexing =
  "WezTerm"` runs a remote `wezterm` server and tunnels the unix-domain protocol over ssh. Since
  20230408 ssh_domains auto-populate from `~/.ssh/config` with `SSH:` and `SSHMUX:` prefixes.
- TLS: `tls_servers` / `tls_clients`; the server creates its own CA and server cert in a pki dir
  (wezterm-mux-server-impl/src/pki.rs); `GetTlsCreds` PDU lets an ssh-bootstrapped client obtain client
  creds and then switch to TLS.
- Exec domains (Lua-defined: a function rewrites the spawn command, e.g. into docker/podman/kubectl),
  WSL domains (auto-discovered distros).
- tmux control mode domain (mux/src/tmux.rs, tmux_pty.rs): maps tmux windows/panes to native panes.
  Marked experimental.

**Mux protocol (codec/src/lib.rs).** Frames are: LEB128 length (top bit marks zstd-compressed
payload), LEB128 serial (request/response correlation), LEB128 PDU ident, then a varbincode
(serde) body. `pdu!` macro assigns explicit numeric identifiers so obsolete PDUs can be removed and
new ones added. `CODEC_VERSION = 46` at this commit; client and server must agree
(`GetCodecVersion`), which is why remote wezterm must match the local version. PDU set (about 60):
Spawn/SpawnV2, SplitPane, KillPane, ListPanes, WriteToPane, SendKeyDown, SendMouseEvent, SendPaste,
Resize, SetClipboard, GetLines, GetPaneRenderChanges (+ Response), SearchScrollbackRequest,
SetPaneZoomed, SetPalette, NotifyAlert, SetClientId/GetClientList, SetWindowWorkspace,
RenameWorkspace, MovePaneToNewTab, ActivatePaneDirection, AdjustPaneSize, GetImageCell, plus server
pushes (PaneRemoved, PaneFocused, TabResized, TabAddedToWindow, TabTitleChanged, WindowTitleChanged,
WindowWorkspaceChanged). Clients poll with GetPaneRenderChanges for dirty line ranges; the server
sends changed logical lines (cells, attributes, image refs) plus cursor and dimensions; client
caches lines keyed by StableRowIndex and fetches missing scrollback with GetLines on demand.
Images are fetched lazily by cell via GetImageCell. Trade-off: simple and robust to reconnect, but
carries styled cells rather than raw bytes so every client re-renders from model, and protocol
changes are frequent.

**termwiz.** Standalone crate: `Surface` (grid of cells with a `Change` log and delta computation
between surfaces), `render` backends (terminfo, Windows console), `terminal` abstraction (raw mode,
resize, input), `input` (decodes keyboard/mouse/paste incl. kitty and win32-input), `caps` (terminfo
and env probing), `lineedit` (readline-like editor with history/completion), `widgets` (layout
toolkit). It is the escape parser reused by the `term` crate, so parse and emit share a model.

**Rendering.** `front_end` = OpenGL (default), WebGpu (wgpu: Vulkan/Metal/DX12) or Software.
Glyphs are shaped per line with HarfBuzz via a `ShapeCache` (keyed by line text, style runs,
direction; has a borrowed-key variant to avoid allocating on lookup), rasterised via FreeType (or
CoreText/DirectWrite where available) into a texture `Atlas` with sprites; when the atlas is full the
whole `GlyphCache` is recreated (simple eviction). Custom-drawn block, box, powerline and braille
glyphs avoid font gaps (`custom_block_glyphs`, `anti_alias_custom_block_glyphs`). Quads are batched
into vertex buffers per frame (`quad.rs`, `shader.wgsl`). Per-line render caches key on line
sequence number so unchanged lines are not re-shaped. Tab bar, palette, launcher, char-select and
pane-select are drawn by an in-process box-model UI (`termwindow/box_model.rs`), not native widgets.
(wezterm-gui/src/glyphcache.rs, shapecache.rs, termwindow/render/*)

**Overlays as panes.** Copy mode, quick select, launcher, debug overlay, confirm, prompt and
selector are implemented as overlay "panes" (`wezterm-gui/src/overlay/*.rs`) that temporarily replace
a pane's renderable and consume input through a key table (copy mode and search have their own key
tables, `copy_mode` / `search_mode`, user-overridable via `wezterm.gui.default_key_tables()`).

## Feature inventory (grouped by category)

**VT core.** Full xterm-class emulation, 256/truecolor, bracketed paste, focus events, mouse
(SGR, pixel?: unverified), alt screen, OSC 8 hyperlinks, OSC 52 clipboard, OSC 7 cwd, OSC 133
semantic prompts (zones: Prompt/Input/Output; `ScrollToPrompt`, `get_semantic_zones`,
copy-mode zone movement), OSC 1337 SetUserVar (`user-var-changed` event), OSC 9;4 progress
(`pane:get_progress`), synchronized output (unverified: grep not performed).

**Input.** kitty keyboard protocol (opt-in `enable_kitty_keyboard`, default false), CSI-u mode
(`enable_csi_u_key_encoding`), win32-input mode on Windows; key tables with `one_shot`,
`replace_current`, `timeout_milliseconds`; `leader` key; physical (`phys:`) vs mapped (`mapped:`) key
specs; `send_composed_key_when_left_alt_is_pressed` and friends; dead-key handling; IME (`use_ime`,
`xim_im_name`); `debug_key_events`.

**Fonts.** `wezterm.font_with_fallback`, per-attribute `font_rules` (bold/italic/intensity), HarfBuzz
OpenType features (`harfbuzz_features`, per-font `harfbuzz_features` too), `freetype_load_target`,
`freetype_load_flags`, `freetype_render_target`, `line_height`, `cell_width`, `cell_widths`
(per-codepoint-range width overrides), `use_cap_height_to_scale_fallback_fonts`,
`allow_square_glyphs_to_overflow_width`, bitmap emoji and COLR v0/v1 (`rasterizer/colr.rs`),
font locator backends (fontconfig, CoreText, GDI) and a built-in embedded fallback set (JetBrains
Mono, Noto Color Emoji, Symbols Nerd Font Mono, Roboto), `font_dirs`, per-window `font_size`
change actions (Increase/Decrease/Reset), `dpi`.

**Unicode.** `unicode_version` (default 9, selects width table for compat with programs; docs
explain emoji width differences), `treat_east_asian_ambiguous_width_as_wide`, grapheme clusters via
unicode-segmentation, emoji presentation handling in the shaper, `bidi_enabled`/`bidi_direction`
(experimental).

**Graphics.** iTerm2 inline images (OSC 1337 File), Sixel, kitty graphics protocol (enabled by
default; placements, z-index, animation frames support partial: see `kitty.rs`; I did not verify
animation completeness). Images stored in a shared `ImageData` with cell references and a TTL-based
cache (`image_cache`-like behaviour: unverified key name).

**Scrollback and search.** `scrollback_lines` (default 3500; lines are stored in memory with
stable row indices), Search overlay (case-insensitive default, case-sensitive, regex modes via
`CycleMatchType`, `SearchScrollbackRequest` PDU works against remote panes), `ClearScrollback`
(scrollback only / scrollback and viewport), `ScrollToPrompt`, `ScrollByLine/Page`, scrollbar.

**Copy mode (`ActivateCopyMode`).** Vim-like keyboard cursor with `CopyMode` assignments:
MoveToViewportTop/Middle/Bottom, MoveToScrollbackTop/Bottom, SetSelectionMode (Cell/Word/Line/
Block/SemanticZone), MoveBackwardWord/ForwardWord/ForwardWordEnd, MoveToStartOfLine(Content),
MoveByPage, JumpForward/Backward/Again/Reverse (f/t/F/T style), MoveBackwardSemanticZone etc,
MoveToSelectionOtherEnd, PriorMatch/NextMatch/CycleMatchType/EditPattern (search integrated into
copy mode), Close. Copy mode and search share a cursor model (`overlay/copy.rs`).

**Quick select (`QuickSelect`, `QuickSelectArgs`).** Scans the visible viewport with a regex list
(built-in: URLs, paths, git hashes, IPs, UUIDs, numbers, quoted strings, markdown links; extend with
`quick_select_patterns`; remove defaults with `disable_default_quick_select_patterns`), overlays 1-2
char labels from `quick_select_alphabet`, a typed label copies the match (Shift-label pastes via
`action`). QuickSelectArgs lets a binding set `patterns`, `alphabet`, `action` (an arbitrary
KeyAssignment run with the selected text, e.g. open or paste), `label`, `scope_lines`. Label
assignment is in `compute_labels_for_alphabet` (generates minimum-length prefix-free labels,
longer composed labels only when matches exceed alphabet size; filtering by typed prefix).
(wezterm-gui/src/overlay/quickselect.rs)

**Launcher / palette / selectors.** `ShowLauncher` and `ShowLauncherArgs{flags='FUZZY|LAUNCH_MENU_ITEMS|
DOMAINS|WORKSPACES|TABS|KEY_ASSIGNMENTS|COMMANDS', title}`; `launch_menu` entries are SpawnCommands;
`ActivateCommandPalette` lists every command with its bound key (`commands.rs` is the registry of
(name, brief, doc, action, menubar location)) and the `augment-command-palette` event lets Lua add
entries; `InputSelector` (fuzzy list, callback with id/label), `PromptInputLine`, `Confirmation`,
`PaneSelect` (letter labels over panes: modes Activate, SwapWithActive, MoveToNewTab...),
`CharSelect` (emoji/unicode picker with unicode_names), `ShowTabNavigator`.

**Workspaces.** A workspace is a string tag on each mux window; one workspace is active per GUI;
`SwitchToWorkspace{name, spawn}`, `SwitchWorkspaceRelative`, `RenameWorkspace`, `default_workspace`,
`mux.get_active_workspace`, `mux.all_windows`; workspaces persist across detach when in a mux server
domain (`SetWindowWorkspace`/`WindowWorkspaceChanged` PDUs). No built-in saved layouts; community
plugins (resurrect.wezterm, smart_workspace_switcher) serialise tab/pane trees themselves.

**Tabs and panes.** `SplitPane{direction, size={Percent|Cells}, top_level, command, domain}`,
`AdjustPaneSize`, `ActivatePaneDirection`, `RotatePanes`, `TogglePaneZoomState`, `MoveTab`,
`SpawnCommandInNewTab`, tab bar styles (fancy vs retro, `format-tab-title` event with TabInformation:
title, is_active, has_unseen_output, active_pane, tab_index), `tab_max_width`, `show_tab_index_in_tab_bar`,
`window_decorations`, `window_padding`, `inactive_pane_hsb`.

**Hyperlinks / mouse.** `hyperlink_rules` (regex -> format string with `$1`), `OpenLinkAtMouseCursor`,
`mouse_bindings` (Down/Up/Drag/Move/Scroll x Single/Double/Triple click x mods, `mouse_reporting`
context), `SelectTextAtMouseCursor` modes (Cell, Word, Line, Block, SemanticZone),
`swallow_mouse_click_on_*`, `bypass_mouse_reporting_modifiers`.

**Shell integration.** Ships `wezterm.sh` (OSC 7, OSC 133, user vars `WEZTERM_PROG`/
`WEZTERM_USER`/`WEZTERM_HOST`). Detection of ssh/foreground process via `pane:get_foreground_process_info`
(procinfo: name, argv, cwd, children), used by scripts to adapt keys/titles.

**Background / glass.** `window_background_image` (+ `_hsb`), `window_background_gradient`,
`window_background_opacity`, `text_background_opacity`, `kde_window_background_blur`,
`macos_window_background_blur`, `wayland_window_background_blur` (the Wayland one uses a
compositor protocol; unverified which), `win32_system_backdrop`. No native per-pane blur/rounded glass UI.

**CLI.** `wezterm cli` (talks to the running mux over a unix socket discovered from `WEZTERM_UNIX_SOCKET`
env): `list`, `list-clients`, `spawn`, `split-pane`, `send-text`, `get-text`, `activate-pane`,
`activate-pane-direction`, `activate-tab`, `adjust-pane-size`, `kill-pane`, `move-pane-to-new-tab`,
`rename-workspace`, `set-tab-title`, `set-window-title`, `zoom-pane`, `get-pane-direction`;
`wezterm connect <domain>`, `wezterm ssh`, `wezterm serial`, `wezterm imgcat`, `wezterm show-keys`,
`wezterm ls-fonts` (shows resolved fallback and glyph coverage per text), `wezterm replay`.

**Update / packaging.** Self-notification of new releases (`check_for_updates`), AppImage, deb, rpm,
Flatpak; nightly channel.

## Control surface (config, actions, IPC)

**Config.** `wezterm.lua` returns a table; `wezterm.config_builder()` validates keys with typos
reported; ~179 options at `docs/config/lua/config/*.md`. Reload on file change
(`automatically_reload_config`) with `window-config-reloaded` event; per-window overrides via
`window:set_config_overrides` (used for dark/light switching, opacity toggles, per-workspace fonts).
`wezterm.target_triple`, `wezterm.hostname`, `wezterm.glob`, `wezterm.run_child_process`,
`wezterm.read_dir`, `wezterm.json_parse`, `wezterm.time`, `wezterm.color` (parse, blend, contrast;
`wezterm.color.get_builtin_schemes`), `wezterm.gui.get_appearance` (light/dark),
`wezterm.plugin.require` (git-cloned Lua plugins), `wezterm.action_callback`, `wezterm.format`
(styled string builder: foreground/background/attributes/reset), `wezterm.nerdfonts`,
`wezterm.strftime`, `wezterm.battery_info`.

**Why users script (and the declarative equivalent Apotheosis needs).**

| Lua usage | Why | KDL/Action/rule equivalent |
|---|---|---|
| `if wezterm.hostname()==... / target_triple` | per-machine config | `when` conditions on profiles: host, os, env, appearance |
| `wezterm.gui.get_appearance()` + color scheme | auto dark/light | `theme { light "x"; dark "y"; follow-system }` |
| `format-tab-title` event building text, icons, colors from pane title/cwd/process | custom tab labels | tab title template with variables (`{cwd_basename}`, `{process}`, `{index}`, `{unseen}`, `{workspace}`) and per-condition styles |
| `update-right-status` (clock, battery, workspace, key-table name, leader indicator) | status bar | status-bar segments as typed widgets with templates; timer rule |
| `wezterm.on('gui-startup', ...)` spawn layout with `mux.spawn_window`, `pane:split` | restore project layout | `layout` documents (tree of splits with commands, cwd, domains, sizes) + action `ApplyLayout` + startup rule |
| `user-var-changed` | shell tells terminal things (e.g. nvim zen-mode, theme) | rule `on user-var <name> [=value] -> actions` |
| `bell` event / `window-focus-changed` | notifications, opacity on blur | rules `on bell`, `on focus-lost` -> `SetOpacity`, `Notify` |
| `open-uri` | route links to different apps | link rule with ordered handlers, `OpenWith`, per-scheme |
| `augment-command-palette` | custom palette entries | palette entries in config: `command "name" { action ... }` |
| `wezterm.action_callback` w/ `InputSelector` for project pickers, `PromptInputLine` for rename | interactive flows | actions `Pick{source=glob/command/list, then=actions}`, `Prompt{... then=...}` with `$input` variable |
| `SpawnCommand` generated from fs scan (ssh hosts, projects) | dynamic menus | dynamic palette sources: `sources { ssh-config; dir "~/src"; command "..." }` |
| `mux-is-process-stateful` | skip close prompt for some processes | `confirm-close` rules by process name/glob |
| Conditional keybinding if pane is vim (`pane:get_foreground_process_name`) with pass-through | smart-splits.nvim-style navigation | binding condition `when process="nvim"` / user-var `IS_NVIM` -> `SendKey` else focus move |
| `window:set_config_overrides` | runtime tweaks | `Set` action with typed setting path, scoped to window/tab/pane |
| `wezterm.plugin.require` | share config | profile/theme/layout packs as KDL files (no code) |

**Actions.** `KeyAssignment` enum (config/src/keyassignment.rs; docs under `keyassignment/`; 84
files) incl. `Multiple` (sequence), `EmitEvent`, `SendString`, `SendKey`, `SwitchToWorkspace`,
`ActivateKeyTable{name, one_shot, replace_current_table, timeout_milliseconds, until_unknown}`,
`PopKeyTable`, `DisableDefaultAssignment`, `Nop`, `CopyTo`/`PasteFrom` with ClipboardCopyDestination
(Clipboard, PrimarySelection, both), `CompleteSelectionOrOpenLinkAtMouseCursor`, `ScrollToPrompt`,
`SetPaneZoomState`, `SetWindowLevel`, `Hide/Show`, `QuitApplication`, `ReloadConfiguration`. The
same enum is used by keys, mouse bindings, launcher/palette, tab-bar button clicks and
`window:perform_action`. Actions are serde-(de)serialisable (internal use), which supports palette
and CLI display. Command registry in `wezterm-gui/src/commands.rs` gives each action a name, short
description, documentation string, and optional menu-bar placement; the palette derives from it.

**IPC.** No JSON; the `wezterm cli` uses the same binary PDU protocol as remote mux clients over the
local unix socket. There is no generic event subscription for scripts outside Lua (Lua events are
in-process only), which is a gap: external tools cannot react to focus/title/bell. Apotheosis's
JSON control socket with subscriptions closes this.

## Performance (numbers + method)

The project publishes few numbers. Facts available:
- `max_fps` default 60 (X11/Windows/mac), ignored on Wayland where frame callbacks pace painting.
  (docs/config/lua/config/max_fps.md)
- `animation_fps` default 10: cap for easing effects (cursor blink ease, text blink, visual bell) to
  save GPU. (docs/config/lua/config/animation_fps.md)
- `scrollback_lines` default 3500. (docs/config/lua/config/scrollback_lines.md)
- Compression of mux PDUs is opt-in by size (zstd; metrics histograms `pdu.encode.compressed.size`).
- No authoritative latency or throughput benchmarks in the repo; third-party typometer or `vtebench`
  comparisons exist but I did not retrieve them (unverified). The author's own docs describe
  rendering as "GPU accelerated" without published frame times.
- Method hints: internal `metrics` crate histograms; `ShowDebugOverlay` Lua REPL + log; `wezterm
  --config 'front_end="Software"'` for fallback comparisons.

## What Apotheosis should take

1. **Domain trait** (local, unix, ssh, tls, exec) with attach/detach, and the GUI as a thin client
   that renders panes from any domain identically. Do it session-persistent from day one.
2. **Single action enum** shared by every trigger, with a named, documented command registry from
   which palette, keybind UI and CLI help are generated. Include `Multiple`, `Nop`,
   `DisableDefaultAssignment`.
3. **Key tables + leader**, with timeout and one-shot, shown in a status indicator.
4. **Semantic-zone aware navigation**: OSC 133 zones driving ScrollToPrompt, copy-mode zone moves,
   select-zone, `get_text_from_semantic_zone` as a CLI/IPC call.
5. **Quick select** with prefix-free label generation, regex list, and a user `then` action.
6. **Copy mode** with a full vim-style motion set sharing the search cursor; make every motion an Action.
7. **Launcher flags** that unify spawn menu, domains, workspaces, tabs, key assignments and commands in
   one fuzzy picker, plus dynamic sources.
8. **Font stack**: ordered fallback with per-rule overrides, OpenType features at global and per-font
   level, per-range cell width overrides, cap-height scaling of fallback fonts, custom-drawn
   box/powerline/braille glyphs, a `ls-fonts`-style explain command, COLR v1 + bitmap emoji.
9. **Line shape cache with allocation-free borrowed key lookup** and per-line sequence numbers for
   dirty tracking; atlas recreation on overflow is acceptable but prefer LRU eviction.
10. **`unicode_version` selector** and ambiguous-width switch, but default to current Unicode with
    grapheme clustering (WezTerm's default of 9 is a legacy compromise).
11. **Config overrides scoped to window/tab/pane at runtime**, replacing `set_config_overrides`.
12. **Tab-title/status templates** as typed expressions, covering the `format-tab-title` use cases.
13. **Mux wire design lessons**: explicit numeric message ids, serial for correlation, length
    prefix with a compression flag, version handshake, separate server pushes. Improve with a
    forward-compatible schema and capability negotiation rather than strict equal version.
14. **Predictive local echo** for high-latency remote domains (`local_echo_threshold_ms` approach).
15. **Image protocol trio** (kitty, iTerm2, Sixel) with images referenced from cells and fetched lazily
    by remote clients.
16. **`proxy_command` for mux transport** and ssh_config auto-import to reduce setup.
17. **PKI for tls domains** generated by the server and a bootstrap-over-ssh credential exchange.
18. **Process introspection** (foreground process name/argv/cwd) as an Action condition and as IPC data.
19. **Backgrounds**: image, gradient, opacity with HSB tweak and inactive-pane dimming.

## What it should avoid

- **Embedded Lua** for logic; every callback site must become a declarative rule or a typed prompt action.
- **Opt-out of modern defaults**: kitty keyboard off by default, Unicode 9 width default, 3500-line
  scrollback. Choose modern defaults.
- **Version-locked mux protocol** that demands identical client/server builds, and shipping fully
  styled lines for every update (high bandwidth on bursts); prefer a negotiated schema and a
  damage-driven delta with scrollback paging.
- **Overlay-as-pane implementation** that swaps the renderable (hard to compose, hard to animate);
  Apotheosis has a real UI layer.
- **Whole-atlas flush on overflow** (visible hitch); use paged atlas with LRU.
- **Custom immediate toolkit limits**: fancy tab bar is a fixed shape, not themable beyond colors.
- **Config bloat**: 179 flat options with historic aliases; use grouped KDL blocks with schema + GUI.
- **Plugin installs by git clone at startup** (supply-chain and latency risk); no remote code.
- **Auto-starting remote binaries** without consent; require explicit trust for exec/ssh domains.
- **No per-pane/pane-group persistence of layout**; do not leave layout save/restore to plugins.
- Tmux control-mode as a headline feature: marked experimental, high complexity; skip or defer.

## Sources

Docs (primary):
- https://wezterm.org/ (config reference: https://wezterm.org/config/lua/config/index.html)
- https://wezterm.org/multiplexing.html
- https://wezterm.org/config/lua/keyassignment/index.html
- https://wezterm.org/copymode.html, https://wezterm.org/quickselect.html, https://wezterm.org/scrollback.html
- https://wezterm.org/config/lua/window-events/index.html
- https://wezterm.org/cli/cli/index.html
(local copies read from clone: docs/multiplexing.md, docs/config/lua/**)

Source (repo https://github.com/wezterm/wezterm, read in clone):
- codec/src/lib.rs `pdu!` macro, `encode_raw_as_vec`, `stream_decode`, `CODEC_VERSION`
- mux/src/domain.rs, mux/src/pane.rs, mux/src/localpane.rs, mux/src/tmux.rs
- wezterm-client/src/pane/renderable.rs `poll`, `get_lines`
- wezterm-mux-server-impl/src/pki.rs
- wezterm-gui/src/overlay/quickselect.rs `compute_labels_for_alphabet`
- wezterm-gui/src/overlay/copy.rs; config/src/keyassignment.rs (`CopyModeAssignment`, `KeyAssignment`)
- wezterm-gui/src/glyphcache.rs (`GlyphCache`, Atlas recreation), shapecache.rs (`BorrowedShapeCacheKey`)
- wezterm-gui/src/commands.rs (command registry)
- term/src/terminalstate/{kitty,iterm,sixel}.rs
- wezterm-font/src/{shaper/harfbuzz.rs, rasterizer/colr.rs, locator/*}
- termwiz/src/{render,input,caps,lineedit,widgets}
