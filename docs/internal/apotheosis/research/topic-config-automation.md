# Topic dossier: configuration, automation, accessibility

Prior-art rule applies: techniques and interface facts only; no source is reproduced. Items marked "unverified" were not confirmed against a primary source. Research date 2026-11-12.

## Summary

Every mature terminal ends up with the same four layers: (1) a declarative option file, (2) a binding table mapping keys/mouse to named actions, (3) an out-of-band control channel (socket plus CLI), and (4) some way to react to events (Lua callbacks in wezterm, triggers in iTerm2, watchers and kittens in kitty, nothing in Alacritty and Ghostty). Users who outgrow layers 1-2 are pushed into scripting, which is the thing Apotheosis forbids. The design opportunity is to make layer 4 declarative: rules of the form `event + match -> actions`, using the same Action vocabulary as keybinds. What users actually automate (from dotfiles and issue trackers) falls into a small set: per-OS/per-host config, theme switching on dark/light, modal key tables (resize, copy), project layouts and session restore, "send text to pane" macros, quake/dropdown windows, status bar content, tab titles, prompt-aware navigation, opening files/URLs with a chosen program, notifications on long commands, and external control (editor integration, tmux-like scripting). Each maps to an Action, a key table, a rule, a profile, a template, or a control-socket call (see the mapping table below).

Accessibility: no GPU terminal other than Windows Terminal and Contour exposes the grid meaningfully. AccessKit's unix adapter (accesskit_unix 0.24.1, MIT OR Apache-2.0) is the viable Rust route over AT-SPI/D-Bus, and a design based on a text-snapshot plus incremental diff events (VTE, Windows Terminal) is the proven shape. Ghostty's GTK apprt has an in-source comment saying there is no accessibility consumer yet, and wezterm has an open request (wezterm #913, 9 reactions). This is an area where Apotheosis can lead.

## Architecture (how it is built)

### kitty (GPL-3; docs read, tree skimmed)
- Config is a line-based `option value` file; `map` lines bind shortcuts; `include`, `globinclude`, `envinclude`, `geninclude` (runs a program and includes its output). `KITTY_OS` lets a single file branch by OS. Reload on file change (`auto_reload_config`), on a key, or on SIGUSR1. Option definitions live in one Python table that generates the parser, docs and defaults (kitty/options/definition.py). Source: https://sw.kovidgoyal.net/kitty/conf/
- Mappings: sequences `a>b>c` with per-keypress timeout; `combine` joins several actions in one binding; modal maps via `map --new-mode` / `--mode`, with `--on-unknown` (beep, ignore, end, passthrough, passthrough_and_end), `--on-action end`, `--timeout`; conditional maps with `--when-focus-on <match>`. Source: https://sw.kovidgoyal.net/kitty/mapping/
- Remote control: `allow_remote_control` (no/yes/password/socket variants), `listen_on` unix or tcp socket, `kitten @ <cmd>`. Commands (about 44 modules under kitty/rc/): ls, launch, send-text, send-key, get-text, set-colors, get-colors, set-font-size, set-tab-title, goto-layout, load-config, create-marker, set-user-vars, signal-child, action (run any map action), and more. Targeting via `--match field:regex` with and/or/not, fields id, title, pid, cwd, cmdline, env, var, state, neighbor, session, recent. Password mode: named passwords each granting a glob list of command names; interactive allow/deny prompt when no password configured; password requests are encrypted with a time-based nonce. Source: https://sw.kovidgoyal.net/kitty/remote-control/
- User variables (`set-user-vars`, OSC 1337 SetUserVar compatible) act as the bridge between the shell and match expressions (`var:in_editor`).

### wezterm (MIT; tree read)
- Config is a Lua program returning a table; typed structs on the Rust side (config/src/config.rs, keyassignment.rs with roughly 175 KeyAssignment enum variants, about 86 documented assignment pages). A filesystem watcher (notify crate) triggers reload and notifies subscribers; a reload re-evaluates all Lua. Source: wezterm/config/src/lib.rs (reload, subscribe_to_config_reload, watch_path).
- Events: window-config-reloaded, window-focus-changed, window-resized, format-tab-title, format-window-title, update-status, update-right-status, bell, open-uri, user-var-changed, new-tab-button-click, augment-command-palette; mux events too. Handlers are Lua functions registered with `wezterm.on`. Source: https://wezterm.org/config/lua/window-events/
- Key tables: `ActivateKeyTable` with name, timeout_milliseconds (resets per match), one_shot (default true), replace_current, until_unknown, prevent_fallback; `PopKeyTable`, `ClearKeyTableStack`. Stack-of-tables model. Source: https://wezterm.org/config/lua/keyassignment/ActivateKeyTable.html ; wezterm/config/src/keyassignment.rs (ActivateKeyTable, KeyTables).
- Per-window config overrides (`window:set_config_overrides`) are the idiom for dynamic theming; calling it inside window-config-reloaded can loop (documented caveat).
- Search shows 5056 GitHub code hits for "wezterm.on" in wezterm.lua files, i.e. event handlers are the dominant advanced use (GitHub code search count, indicative only).

### Ghostty (MIT; tree read)
- Flat `key = value` file, repeatable keys, `config-file` includes (optional with `?` prefix; unverified for exact syntax), themes are just config files that cannot set `theme`/`config-file`; `theme = light:X,dark:Y` follows desktop colour scheme. A static typed "conditional" state (theme light/dark) exists for conditional config. Source: ghostty/src/config/Config.zig (theme), conditional.zig.
- Keybinds: `trigger=action[:param]`, prefix flags all:, global: (macOS only), unconsumed:, performable: stored as a packed bit set per binding; sequences with `>` (leader/final handled by a non-allocating iterator); `chain=<action>` appends actions to the previous binding; key tables (`activate_key_table`, `activate_key_table_once`, `deactivate_key_table`) with `catch_all`; `end_key_sequence`. Later binding wins for equal triggers. Source: ghostty/src/input/Binding.zig (Flags, Parser, SequenceIterator, Action); https://ghostty.org/docs/config/keybind
- Config diagnostics are collected into an ErrorList rather than aborting (ghostty/src/config/ErrorList.zig); CLI actions `+list-themes`, `+validate-config` (unverified), `+show-config` provide introspection. No control socket at time of reading (unverified for latest main); automation goes through platform IPC (D-Bus activation, AppleScript on macOS).
- Accessibility: Binding/Config mention none; apprt/gtk/class/application.zig has a comment that GTK has no accessibility consumer for this yet. No dedicated ghostty accessibility issue was found in a reaction-sorted search (unverified). Source: ghostty tree.

### Windows Terminal (MIT; tree and docs read)
- settings.json with JSON schema (editor completion), layered: built-in defaults.json, user file, fragment extensions, dynamic profile generators (WSL, Azure, PowerShell, SSH). Source: src/cascadia/TerminalSettingsModel/defaults.json, ActionMap.cpp, https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions
- Actions are separated from keys: an action has a stable `id` (e.g. `Terminal.FindText`, user ids `User.X`) and a `command` (string, or object with `action` + arguments); `keys` entries bind to an id. Same entries feed the command palette; `name`, `icon`, nested `commands` and `iterateOn` (profiles/schemes/…) generate palette menus. `multipleActions` runs several; `sendInput`, `splitPane`, `newTab`, `findMatch`, `globalSummon`, `quakeMode`, `openSettings` etc. Source: docs above (action formats section).
- Command line `wt` subcommands (`new-tab`, `split-pane`, `focus-tab`, `;` chaining) are the automation CLI and the same format feeds `startupActions`. `windowingBehavior` (useNew/useExisting/useAnyExisting) with `--window name`. `firstWindowPreference: persistedWindowLayout` restores windows, tabs, panes, cwd (not contents). Source: https://learn.microsoft.com/en-us/windows/terminal/customize-settings/startup
- Settings UI is a separate XAML page set that edits the same model and writes JSON; the file stays authoritative and a "Open JSON file" action exists. Known pain: profile reordering (#8914, 183 reactions), roaming settings (#2933, 167), profile-specific keybindings (#5790, 134).
- Accessibility: UIA provider with text ranges over the buffer (src/types/UiaTextRangeBase.cpp, ScreenInfoUiaProviderBase) plus an automation peer that raises text-changed, selection-changed, cursor-changed and notification events; new output is announced via RaiseNotificationEvent with processing mode All so keyboard events can interrupt; a code comment records that blocking on the async call stopped NVDA from receiving text-changed events. Source: src/cascadia/TerminalControl/TermControlAutomationPeer.cpp (NotifyNewOutput, SignalTextChanged, SignalCursorChanged).

### iTerm2 (GPL-2; docs read)
- Triggers: per-profile list of regex -> action, 30 action types (Annotate, Bounce Dock Icon, Buffer Input, Capture Output, Change Style, Fold to Named Mark, Highlight Line/Text, Inject Data, Invoke Script Function, Make Hyperlink, Open Password Manager, Post Notification, Prompt Detected, Report Directory, Report User & Host, Ring Bell, Run Command, Run Coprocess, Run Silent Coprocess, Send Text, Set Mark, Set Named Mark, Set Tab Status, Set Title, Set User Variable, Show Alert, Stop Processing Triggers, Enter/Exit Workgroup). ICU regex, matched one line at a time at newline or cursor-movement, "Instant" flag fires mid-line (needed for password prompts without newline), last three wrapped lines considered, capture groups \0-\9 substituted into parameters. Source: https://iterm2.com/documentation-triggers.html
- Profiles carry nearly everything (colours, keys, triggers, badges, automatic profile switching by host/user/path rules); dynamic profiles are JSON files in a watched directory.
- Security caveat: Run Command/Send Text/Inject Data from regex on untrusted output is a command-injection surface; the docs have no dedicated security section.

### Alacritty (Apache-2.0/MIT; tree read)
- TOML, `[general] import = [...]` with a recursion limit and merge of imported tables beneath the main file (alacritty/src/config/mod.rs load_imports); invalid keybindings are logged and dropped without failing the whole file; live reload watches file and imports. `alacritty migrate` rewrites old YAML to TOML. Source: alacritty/src/config/mod.rs, ui_config.rs (deserialize_bindings).
- IPC: `alacritty msg` over a unix socket with exactly three messages: create-window (with its own options), config (apply overrides at runtime, per window or global), get-config. Source: alacritty/src/cli.rs (SocketMessage). Deliberately minimal.
- Hints: regex + post-processing + action (copy, paste, select, or external program) + mouse/binding trigger; vi mode and search are modal subsystems with their own binding sets. Source: alacritty/docs/features.md.

## Feature inventory (grouped by category)

### Config files
- Layering: defaults < system < user < includes < CLI `-o` overrides < runtime overrides (kitty, alacritty, ghostty).
- Includes: relative, glob, optional include, include-from-env, generated include (kitty geninclude; reject for Apotheosis because it executes code).
- Conditional blocks: kitty `KITTY_OS`, ghostty typed light/dark state; wezterm uses Lua `if`. Users need: per-OS, per-host, per-compositor, per-output scale, light/dark, on-battery.
- Themes as plain config fragments with restrictions on which keys they may set (ghostty).
- Hot reload with diagnostics reported rather than fatal (ghostty ErrorList, alacritty logged drop).
- Schema for editor completion (Windows Terminal JSON schema).
- Introspection: `show-config`, `list-themes`, `list-keybinds`, `validate-config`.
- Migration command between config versions (alacritty migrate).
- Single definition table feeding parser, docs, defaults and GUI (kitty definition.py).

### Bindings
- Single chords, multi-key sequences with timeout (`a>b`), key tables as a stack (wezterm) or named modes (kitty, ghostty), catch-all, one-shot, until-unknown, prevent-fallback.
- Per-binding flags: unconsumed (pass through), performable (only if action applicable), all-surfaces, global (system-wide).
- Action chaining/combine in one binding (kitty combine, ghostty chain, WT multipleActions).
- Conditional bindings by focused-program state (kitty --when-focus-on var:in_editor).
- Mouse bindings by button/count/modifier/mode.
- Stable action ids separate from key assignment (Windows Terminal) so palette, menus, and keys share one object.
- Key names by layout-independent keycode vs produced character (kitty native_code; ghostty unicode fallback).

### Events and rules
- wezterm event hooks (about 12 window events plus mux events), iTerm2 triggers (regex on output), kitty watchers (Python callbacks on window events) and marker/`--when-focus-on`, ghostty none, Windows Terminal none.
- User-variable change events (OSC 1337 SetUserVar) as the shell-to-terminal signal.
- Output triggers: highlight, notify, set mark, capture, send text, run command, silent coprocess.
- Status-line formatting hooks (wezterm update-status, format-tab-title): the most common reason users write Lua.

### Control channel
- kitty: ~44 remote commands, match language, per-command password ACL, socket or in-window escape transport.
- alacritty: 3 messages. wezterm: `wezterm cli` (list, spawn, split-pane, send-text, get-text, activate-pane, set-tab-title, zoom, rename-workspace; verify list at https://wezterm.org/cli/cli/index.html, unverified here). Windows Terminal: `wt` command line with `;` chaining.
- Ghostty: no socket (unverified for latest).

### Sessions and layouts
- kitty session files (`startup_session`, `launch`, `new_tab`, `layout`, `cd`), wezterm workspaces and mux domains, WT startupActions and persistedWindowLayout, iTerm2 arrangements.

### Settings UI
- Windows Terminal: full GUI plus JSON; iTerm2: Preferences window is the primary store; kitty/wezterm/alacritty/ghostty: text only (long-standing complaint).

### Accessibility
- VTE (GTK): AT-SPI text interface backed by a snapshot of visible text with per-byte attributes, line-break offsets, caret offset, and stale flags; emits text-changed insert/delete and text-caret-moved lazily after output settles. Source: vte/src/vteaccess.cc (snapshot_* fields, emit_text_changed_insert/delete, emit_text_caret_moved).
- Windows Terminal: UIA text ranges + peer events + notification events for new output (above).
- Contour: reports caret rectangle and visible text; with OSC 133 exposes the live prompt as its own focusable editable-text region; tab bar as tab list; every settings field labelled; Ctrl+F6 moves keyboard focus through window chrome; focus ring only for keyboard focus; `accessibility_caret_reporting` switch. Source: contour/docs/accessibility.md, display/TerminalAccessible.hpp.
- Ghostty: none on GTK (source comment). wezterm: open issue wezterm #913 "Accessibility for the visually impaired". Alacritty, kitty, foot, wezterm: none found in tree (rg for accesskit/atspi returned only contour; wezterm result unverified for docs).

## Control surface (config, actions, IPC)

What users automate, mapped to an Apotheosis KDL surface (proposed):

| User need (evidence) | Apotheosis mechanism |
|---|---|
| Per-OS/host/compositor config (kitty KITTY_OS, dotfile `if`) | `when { host "x"; compositor "abyss"; scheme "dark"; battery #true }` blocks wrapping any node; typed state, no expressions |
| Theme follows dark/light (ghostty light:/dark:) | `theme light="a" dark="b"`, plus rule `on scheme-changed -> set-theme` |
| Modal keys: resize/copy/leader (wezterm key tables, kitty modes, ghostty tables) | `keytable "resize" { bind "h" action="resize-pane left 3"; on-unknown "end"; timeout 1500; catch-all ... }` with enter/exit-table actions, stack semantics |
| Multi-action macros (kitty combine, WT multipleActions) | `action "macro-name" { run "split right" ; run "send-text \"cargo watch\\n\"" }` named action sequences, referenceable anywhere |
| Send text/command to pane (huge in dotfiles) | `send-text` and `send-key` actions with template variables; `run-in-pane` |
| Project layouts/sessions (kitty sessions, WT startupActions, tmuxinator) | `layout "dev" { tab { split dir=h { pane cwd="~/p" run="nvim"; pane } } }` plus actions `apply-layout`, `save-layout`, CLI `apo layout apply dev` |
| Profiles (WT, iTerm2) | `profile "ssh-prod" { inherits "default"; shell ...; theme ...; bind-overrides ...; tab-colour }` plus rule-based automatic selection (host, cwd, process) |
| Event-driven reactions (wezterm events, iTerm2 triggers) | `rule { on command-finished; where exit-code!=0 duration>10s; do notify "..." }`; events: bell, title-changed, cwd-changed, command-started/finished (OSC 133), user-var-changed, focus-changed, output-match, window-resized, config-reloaded, scheme-changed, pane-spawned/exited |
| Output triggers (iTerm2) | `rule { on output-match regex="..." instant=#false; do highlight-line ; do notify ...}` with capture variables `${1}`; dangerous actions (run-command, send-text) gated, see Security |
| Status bar/tab title content (wezterm format-tab-title) | `format "tab" "${index}: ${title|truncate 20} ${if zoomed '[Z]'}"`: a small template language with variables, filters and conditionals, no loops or functions; variables include cwd, process, user-vars, git branch (via shell-integration user var) |
| Per-app bindings (kitty when-focus-on) | `bind when { var "in_editor" } ...` using match language shared with the socket |
| Dropdown/quake terminal (wezterm #1751, 265 reactions; WT #9996) | `window "quake" { edge top; size 40%; hotkey via compositor protocol or Abyss binding }` and action `toggle-window quake`; needs the background process to survive last window |
| Cursor trail/visual effects (wezterm #7387) | plain config options under `effects`, not scripting |
| Open file/URL with chosen program (hints, clickable paths ghostty #1972, 231) | `hint "path" regex="..." do open-with "xdg-open"`; `opener` rules by mime/regex |
| External control (editor plugins, scripts) | JSON control socket, same Action vocabulary: `{"action":"split","args":{...},"match":"var:x and cwd:~/p"}`; subscribe to the same event stream rules use |
| Settings GUI parity | one option table (key, type, default, doc, constraints, ui widget) generating KDL parser, GUI, docs, `apo config show` |

### Action vocabulary rules (proposal)
1. Every action has a stable id (`pane.split`, `tab.close`, `search.next`), a typed argument struct, and is invocable from keybind, palette, rule, profile start-up, layout, menu, CLI and socket identically.
2. Actions are data (name + args), never code. Composition is only: sequence (`macro`), conditional (`when` matching the shared match language), and event-bound (`rule`).
3. The match language is one grammar (kitty-like: `field:regex`, and/or/not) used by socket targeting, rules' `where`, binding `when`, profile auto-select and layouts.
4. Capability tier per action (read, write-text, spawn, config, system). Rules and socket clients get a granted tier; interactive keybinds are unrestricted. Mirrors kitty's per-command password ACL.

## Performance (numbers + method)

No authoritative published numbers for config/automation paths were found; this area is latency-insensitive except as follows.
- Config reload: wezterm re-evaluates the entire Lua program on every reload and again per window for overrides; reported pitfalls are loops and slow handlers (wezterm docs caveat). No timing published (unverified). Target for Apotheosis: parse+validate typical config under 5 ms, apply without dropping a frame (own measurement needed).
- iTerm2 trigger matching is bounded by design: one line at a time, last 3 wrapped lines, and an explicit "Instant" mode that trades correctness of `.*` for earlier firing; the configurable "lines to match" setting is the throttle. Source: iTerm2 trigger docs.
- Accessibility: Windows Terminal comment records async RaiseNotificationEvent must not be awaited on the render thread; Contour states caret reporting costs effectively nothing while no AT client is attached and exposes a kill switch for latency diagnosis. Both imply: build the AT tree lazily, only after an AT client connects.
- Output trigger regex should run on completed logical lines off the render thread, compiled once (Rust `regex`, linear time, no backtracking), with a per-rule rate limit.

## What Apotheosis should take

1. Single option table that drives KDL parsing, defaults, docs and the settings GUI (kitty definition.py idea).
2. Action ids separate from bindings, shared by palette/menu/keys (Windows Terminal).
3. Key tables as a stack with timeout, one-shot, until-unknown, prevent-fallback, catch-all (wezterm + ghostty + kitty modes).
4. Per-binding flags: unconsumed, performable, all-surfaces; global via Abyss/portal only.
5. Sequences with timeout and chain/combine.
6. Declarative rules replacing Lua events and iTerm2 triggers; same actions.
7. User variables (OSC 1337 SetUserVar) as shell-to-terminal state and match field.
8. Match language shared across socket, rules, bindings, profiles.
9. Control socket with capability tiers, and a CLI that is a thin client; `get-text`, `send-text`, `ls` JSON, `launch`, `set-colors`, event subscribe.
10. Includes (relative, glob, optional), themes as fragments with a forbidden-key list, typed `when` conditions; no generated includes.
11. Diagnostics list instead of abort; keep last good config; show errors in a glass toast and in settings GUI; `validate` CLI; schema/KDL docs for editor completion.
12. Config migration command and versioned `version` node.
13. Persisted window layout and named layouts (WT, kitty sessions).
14. Quake/dropdown window and background-resident process (top-reacted request in wezterm and WT).
15. Accessibility from day one: AccessKit unix adapter, text snapshot plus diff events, prompt region via OSC 133, lazy activation, keyboard focus traversal through chrome, labels on all glass UI.

## What it should avoid

- Embedded scripting or `geninclude`/executable hooks; solve with rules and socket.
- Config options whose meaning depends on evaluation order of imperative code (wezterm overrides loop).
- Triggers that execute commands by default on arbitrary output; gate `run-command`/`send-text` from output rules behind explicit per-rule opt-in and a confirm-once prompt (iTerm2 lesson).
- Remote control open by default; kitty and alacritty default off. Socket must be a unix socket with 0600, per-user, off for child processes unless granted (env var passing).
- GUI that writes lossy JSON/KDL: GUI edits must be minimal-diff, preserve comments and ordering (KDL document model that round-trips).
- A settings GUI that is a second source of truth (WT problems with roaming, ordering).
- Profile-only keybindings missing (WT #5790): allow per-profile and per-`when` overrides from the start.
- Global hotkeys via X11 grabs; use compositor protocol on Abyss and portal elsewhere.
- Announcing every output line to screen readers (flood); coalesce and rate-limit.

## Accessibility technical notes

- accesskit_unix 0.24.1 (published 2026-10-25), licence MIT OR Apache-2.0, rust 1.87. Direct dependencies: accesskit ^0.25.2, accesskit_atspi_common ^0.21.1, atspi ^0.29 (proxies feature), zbus ^5.19 (async-io), futures-lite ^2.3, serde; optional async-channel, async-executor, async-task, futures-util (default feature async-io) or tokio + tokio-stream (feature tokio). accesskit, accesskit_consumer: MIT OR Apache-2.0. atspi 0.30.0: Apache-2.0 OR MIT. zbus latest 6.2.1: MIT. All within the licence allow-list. Source: crates.io API 2026-11-12.
- Caveat: pulls zbus (pure Rust D-Bus), consistent with "no C". Runtime needs the session bus and `org.a11y.Bus`; the adapter is activated on demand when an AT client registers (documented AccessKit behaviour; unverified here for exact timing).
- Tree design: root window node, tab list with tab nodes, per-pane Terminal-role node with text-run children per visible row (AccessKit has a TextRun role and text selection support), live-region node for announcements, prompt region from OSC 133. AccessKit models text by character offsets per run; per-row runs match VTE's line-break table. Mapping AccessKit's text-selection to terminal selection is needed for read-selection.
- Events: caret moved, text inserted/removed per row diff (cheap because the renderer already tracks dirty rows), selection changed, focus changed, announcements via live region with polite priority and coalescing window.

## Sources

- kitty remote control: https://sw.kovidgoyal.net/kitty/remote-control/
- kitty mapping: https://sw.kovidgoyal.net/kitty/mapping/
- kitty conf: https://sw.kovidgoyal.net/kitty/conf/
- wezterm ActivateKeyTable: https://wezterm.org/config/lua/keyassignment/ActivateKeyTable.html
- wezterm window-config-reloaded and event list: https://wezterm.org/config/lua/window-events/window-config-reloaded.html
- wezterm source: config/src/lib.rs (reload, watch_path), config/src/keyassignment.rs (ActivateKeyTable, KeyTables)
- ghostty keybind docs: https://ghostty.org/docs/config/keybind
- ghostty source: src/input/Binding.zig (Flags, Parser, SequenceIterator, Action), src/config/Config.zig (theme), src/config/conditional.zig, src/config/ErrorList.zig, src/apprt/gtk/class/application.zig (accessibility comment)
- Windows Terminal actions: https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions
- Windows Terminal startup: https://learn.microsoft.com/en-us/windows/terminal/customize-settings/startup
- Windows Terminal source: src/cascadia/TerminalSettingsModel/{defaults.json,ActionMap.cpp}, src/cascadia/TerminalControl/TermControlAutomationPeer.cpp (NotifyNewOutput, SignalTextChanged), src/types/UiaTextRangeBase.cpp
- iTerm2 triggers: https://iterm2.com/documentation-triggers.html
- Alacritty source: alacritty/src/config/mod.rs (load_imports), config/ui_config.rs (deserialize_bindings), cli.rs (SocketMessage), docs/features.md
- VTE: src/vteaccess.cc (snapshot, emit_text_changed_insert/delete, emit_text_caret_moved)
- Contour: docs/accessibility.md, src/contour/display/TerminalAccessible.hpp
- crates.io API: accesskit_unix, accesskit, accesskit_consumer, atspi, zbus (queried 2026-11-12)
- GitHub issue search by reactions (2026-11-12): wezterm #1751, #336, #7387, #549, #3516; alacritty #50, #910, #1615, #2410; microsoft/terminal #3656, #1860, #9996, #8914, #2933, #5790; wezterm #913 (accessibility); ghostty #1935 (tmux control mode, 766), #189 (GTK search scrollback, 1221), #1972 (clickable paths, 231), #5047 (toggle_background_opacity), #12852 (OSC 72 drag-and-drop)
- GitHub code search counts: "wezterm.on" in wezterm.lua = 5056; "map" in kitty.conf = 25344 (indicative; not a usage census)
