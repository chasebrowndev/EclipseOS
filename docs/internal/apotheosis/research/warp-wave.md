# Dossier: Warp and Wave Terminal (UX ideas only)

Method: official docs fetched 2026-11-12 (docs.warp.dev, docs.waveterm.dev), Warp engineering blog, GitHub READMEs. No source cloned; no source read. Several doc pages were navigation-only in the fetch; those facts are marked unverified.

## Summary

Warp and Wave are the two "modern terminal" products that reshape the terminal around structured units instead of a raw cell grid. Warp's central idea is the **block**: one command plus its output as a selectable, copyable, linkable, filterable unit, built from shell prompt hooks. Its second idea is an **editor-grade input line** separate from the program grid. Wave's central idea is the **workspace of blocks**: terminal, file preview, web, editor and sysinfo widgets tiled together, all driven from the shell by a CLI (`wsh`). Both bolt on cloud accounts and AI; for Apotheosis only the structural ideas matter (blocks, input editor, parameterised workflows, in-shell control CLI, remote helper injection), plus anything an agent can perceive (block boundaries, exit status, structured output).

Biggest takeaways: (1) block boundaries come from shell integration (OSC 133-style marks) and Apotheosis can have them without a per-command grid; (2) the control CLI inside the terminal (wsh) validates our "one Action vocabulary + JSON socket" design; (3) Warp's account/cloud coupling and Wave's Electron are the things to avoid.

## Architecture (how it is built)

Warp (verified from Warp blog "How Warp Works", 2021, may be dated):
- Rust, GPU rendering (Metal first; OpenGL/WebGL planned then; now cross-platform, wgpu use unverified).
- Custom UI framework (element tree, Flutter-inspired) with only three GPU primitives: rectangles, images, glyphs from an atlas. Shaders ~200 lines.
- Blocks: shell `precmd`/`preexec` hooks (zsh/fish native, bash via bash-preexec) emit a custom DCS carrying encoded JSON session metadata; the terminal creates a block in its data model.
- Grid: forked Alacritty grid, but one grid per command+output, so output cannot overwrite earlier blocks.
- Input editor: a real text editor view (not the shell's readline); rope-like SumTree buffer with display transforms (folding); designed as an operation-based CRDT.
- Client is now open source: warpui_core and warpui crates MIT, the rest AGPL-3.0 (github.com/warpdotdev/warp README). AGPL means we must not copy; the MIT UI crates are not needed (we own wgpu UI).

Wave (verified from GitHub README and docs):
- Electron shell (`emain/`), TypeScript frontend, Go backend (`cmd/`, `pkg/`), Apache-2.0, ~22.7k stars. React/xterm.js unverified.
- `wsh`: a helper binary, also installed on SSH/WSL remotes at `~/.waveterm/bin/wsh`, giving remote shells control of the GUI and remote file/sysinfo access. IPC mechanism not documented in fetched pages (unverified).
- Env injection into shells: `WAVETERM_CONN`, `WAVETERM_BLOCKID`, `TERM_PROGRAM`.

## Feature inventory

Blocks (Warp): copy command / output / both; jump to start of output; re-input; share permalink (cloud); bookmark (Ctrl+Shift+B Linux, Alt+Up/Down to jump between bookmarks, session-only); find within block (Ctrl+Shift+F); filter block output lines (Alt+Shift+F); sticky command header (page not described, unverified); background blocks (unverified).

Input editor (Warp): modern text editing in the prompt, syntax and error highlighting, completions, autosuggestions, alias expansion, command inspector, Vim keybindings, command corrections, command search (history + workflows + notebooks + AI in one panel), per-session history isolation, synchronized inputs across panes. Multi-cursor: the engineering post says supported; docs fetch said not mentioned.

Workflows (Warp): named parameterised commands with description (searchable), `{{arg}}` placeholders (names A-Za-z0-9 - _), text or enum args, static or dynamic enums (shell command output parsed), Shift+Tab cycles args, insertion into the editor (not auto-run) with same-named args linked by multi-cursor, scope personal / team, YAML files still supported.

Warpify (Warp): turns subshells/SSH sessions into block-aware sessions; docs page 404 on fetch, details unverified.

Widgets (Wave): term, preview (files, markdown, images, directories), codeedit, process viewer, sysinfo, web; add, close, magnify, reorder, resize, navigate. Tabs with backgrounds, layouts, presets.

wsh commands (Wave, verified): `edit`, `view [-m]`, `getmeta`, `setvar/getvar` (scopes block/tab/workspace/global), `file cat|append|cp` with `wsh://local/~/` for local files from remote, `web open`, `run [-x -m]` (command in new block, optionally closes on exit), `ai`, `ssh [-i]`, `notify`.

Connections (Wave): dropdown input `user@host:port` or `wsl://distro`; reads `~/.ssh/config` (no `Match`); `connections.json` keys conn:wshenabled, conn:askbeforewshinstall, conn:wshpath, conn:shellpath, cmd:env, cmd:initscript(.sh/.bash/.zsh/.pwsh/.fish), ssh:* overrides, per-connection term:fontsize/fontfamily/theme; debug-connection levels; wsh only in bash/zsh/pwsh/fish.

AI (record only): Warp agent mode, AI command search, block-as-context; Wave AI sidebar via `wsh ai` with limits 200KB text, 5MB PDF, 7MB image, 15 files.

## Control surface

- Warp: GUI settings, keybinding editor, Command Palette, Warp Drive objects (workflows, notebooks, env vars), YAML workflow files. No public scripting or control socket found in fetched pages (unverified; Warp has a URI scheme, not verified here).
- Wave: JSON config files (`settings.json`, `widgets.json`, `connections.json`, termthemes; key names namespaced like `term:fontsize`, `conn:wshenabled`, only partly verified), global hotkey config with allowed key names list, `wsh` CLI as the control channel, block metadata get/set, variable scopes.

## Performance

- Warp: target 60 fps, claims "well over 144 fps" on 4K; average screen redraw 1.9 ms over one week (Warp blog, 2021; method: in-app telemetry average, not an independent benchmark).
- Wave: no published numbers found; Electron/xterm.js class (unverified).

## What Apotheosis should take

1. Blocks as first-class model driven by shell integration marks, over one continuous scrollback (not per-command grids): cheaper for reflow and search, still gives per-block copy, filter, find, bookmark, fold, re-run, exit status, duration.
2. Block actions in the shared Action vocabulary (copy-output, filter, bookmark, jump-prev/next-block, rerun) so palette, keybinds, rules and socket all reach them.
3. Sticky command header: pin the running/selected block's command line at top while scrolling its output.
4. Parameterised workflows as KDL objects with typed args, static and dynamic enums, insertion-not-execution, linked cursors.
5. A command-search panel merging history, workflows, actions in the glass palette.
6. Input editor as optional overlay (Phase 2): only when shell integration says the prompt is at an edit point; must never break raw programs. Keep it opt-in per profile.
7. In-terminal control CLI (wsh-like) over the existing JSON socket: open view/edit/run-in-split, get/set variables with scopes (block, tab, workspace, global), notify. Identify the caller block with an env var like WAVETERM_BLOCKID.
8. Remote helper injection with consent: ask-before-install, per-connection env and init scripts, graceful fallback with a visible indicator.
9. `run -x` semantic: spawn command in a new pane, close on success.
10. Synchronized input across panes (broadcast) with clear visual indicator.
11. Inline previews (images, markdown, directories) as block-adjacent panes rather than escape hacks, reusing our graphics protocol.
12. Per-session history isolation.

## What it should avoid

- Account/login wall and cloud sync as a dependency; cloud-only sharing permalinks (offer local export instead).
- Per-command grids (breaks resize reflow, full-screen app interplay, selection across blocks).
- Hijacking the prompt for programs in alternate screen or raw mode; Warp-style editor confusion with custom prompts (p10k).
- Electron-class memory and startup; UI toolkits that cannot hit a frame budget.
- Telemetry by default; AI features in the TCB path; cloud-synced workflows containing secrets.
- Bookmarks that vanish with session only (consider persistence), and docs/key-name drift (Wave's conn:enablewsh vs conn:wshenabled) which argues for schema-generated docs.
- Copying anything from Warp's AGPL code.

## Sources

- https://docs.warp.dev/terminal/blocks
- https://docs.warp.dev/terminal/blocks/block-actions
- https://docs.warp.dev/terminal/entry
- https://docs.warp.dev/knowledge-and-collaboration/warp-drive/workflows
- https://www.warp.dev/blog/how-warp-works
- https://github.com/warpdotdev/warp (README: licences)
- https://docs.waveterm.dev/wsh
- https://docs.waveterm.dev/widgets (headings only)
- https://docs.waveterm.dev/connections
- https://docs.waveterm.dev/config (headings only; key list unverified)
- https://github.com/wavetermdev/waveterm
