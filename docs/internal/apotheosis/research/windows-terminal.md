# Windows Terminal (microsoft/terminal) — prior-art dossier for Apotheosis

Subject commit: shallow clone of `main` at `f6de9e0` (read 2026-11-12). Licence: MIT (`LICENSE`).
Paths below are relative to the repo root unless stated. Per the prior-art rule this document
describes techniques only; config key names, escape sequences and JSON action names are quoted as
interface facts.

## Summary

Windows Terminal (WT) is the strongest reference for four things Apotheosis cares about:

1. **One action vocabulary** reused by keybindings, command palette, new-tab menu and CLI. Actions
   have stable string IDs, are layered across defaults/fragments/user files, and a keybinding is
   just `keys -> action id`.
2. **Cascading settings** (defaults -> fragments -> user, plus `profiles.defaults` -> profile)
   with a settings GUI whose every control knows whether its value is inherited, overridden or
   locally set.
3. **Atlas renderer**: a small D3D11 glyph-atlas renderer with a CPU-side glyph cache, rect-packed
   atlas, instanced quads, dirty-rect presentation and a flip-model swap chain.
4. **Shell integration marks** as per-row metadata (OSC 133 / 633 / 1337 SetMark / ConEmu 9;12) feeding
   scrollbar pips, prompt navigation and "select command output".

Its weaknesses are equally instructive: the GUI rewrites the whole `settings.json` from the model
(user comments and layout are not preserved by that path, see Control surface), there is no
IPC/control socket beyond the `wt` command line, windows are coordinated by a COM
monarch/peasant scheme that is Windows-specific, and several features rely on the shell or a Windows
service. Kitty keyboard protocol and sixel are implemented; kitty graphics protocol and
multiplexing-style detach/attach are not.

## Architecture (how it is built)

Layering (top to bottom), from `src/cascadia`:

- `WindowsTerminal` / `TerminalApp`: window host, `TerminalPage` (tabs, panes, palette, dispatch).
  `Pane` is a binary tree (`_firstChild`/`_secondChild`, `_splitState`, `_desiredSplitPosition`,
  `_lastActive`, `_zoomed` in `TerminalApp/Pane.h`); leaf panes own a content control. The tree
  supports `NavigateDirection`, `SwapPanes`, `AttachPane`/`DetachPane` (used for moving a pane to
  another tab/window) and `Maximize` (zoom).
- `TerminalControl`: `ControlCore` (renderer + terminal glue, no UI), `ControlInteractivity` (mouse,
  selection, scroll logic, UI-independent), `TermControl` (XAML host), `SearchBoxControl`.
- `TerminalCore` (`Terminal` class) + `src/buffer/out` (`TextBuffer`, `Row`, `Marks.hpp`) +
  `src/terminal/parser` (state machine, `OutputStateMachineEngine`) + `src/terminal/adapter`
  (`AdaptDispatch`, VT semantics) + `src/terminal/input` (key encoding, kitty keyboard).
- `TerminalConnection`: ConPTY, Azure Cloud Shell, SSH/WSL connectors. ConPTY is a Windows
  pseudoconsole that translates console API calls to VT; not relevant to a Unix pty.
- `TerminalSettingsModel`: the settings object graph (profiles, actions, colour schemes, themes,
  fragments, application state) with JSON (de)serialization, inheritance, and `defaults.json`.
- `TerminalSettingsEditor`: settings UI (XAML pages bound to view-models over the model).
- `src/renderer/base` produces GDI-like primitives; `src/renderer/atlas` (`AtlasEngine`) consumes them.
  Its README admits that breaking the buffer into GDI-style primitives and re-assembling them for
  DirectWrite is wasteful and bug-prone and that the engine should be given the buffer directly.
- Settings model uses X-macro tables (`MTSMSettings.h`: `(type, Name, "jsonKey", default)`) so that a
  single line adds a setting to the model, JSON read/write, and the UI plumbing; action list is
  likewise an X-macro (`AllShortcutActions.h`, `ALL_SHORTCUT_ACTIONS` and `..._WITH_ARGS`).

Atlas renderer (`src/renderer/atlas/`, ~8.8k lines):

- Split into `AtlasEngine.cpp` (turns buffer rows into per-row glyph runs, outside the console lock
  where possible, `.r.cpp`), `AtlasEngine.api.cpp` (setters run under the lock) and two backends:
  `BackendD3D` (the real renderer) and `BackendD2D` (pure Direct2D fallback for remote desktop and
  no/old GPUs). `GraphicsAPI` setting: Automatic / Direct2D / Direct3D11; `rendering.software` picks
  the WARP software adapter.
- Text path: per row, characters are split by font fallback (`MapCharacters`; the code notes it is
  slow and can return the same face repeatedly, so adjacent identical mappings are merged), then a
  cheap "is text simple" check (`GetTextComplexity`) lets runs with no shaping needs skip the shaper
  and map one glyph per cell using the cell advance. Complex runs (ligatures, combining marks, font
  features) go through the full DirectWrite shaper. With any font features set the fast path is
  skipped entirely.
- Glyph cache: a flat open-addressing hash set keyed by (font face, glyph index) giving an atlas
  rectangle, offset, size and a `ShadingType`. New glyphs are rasterized by Direct2D into the atlas
  texture and placed by `stb_rect_pack` (skyline packer). When the atlas is full the frame is
  flushed, the atlas is reset (and grown if still small) and the glyph retried; there is no
  eviction, only wholesale reset.
- GPU submission: each visible glyph/line/cursor/rect becomes one 20-byte-ish `QuadInstance`
  (type, rendition scale, i16x2 position, u16x2 size, u16x2 texcoord, u32 colour); the struct is
  deliberately uninitialised and i16-positioned because instance-buffer size was the biggest lever on
  performance and power. One instanced draw. Cell backgrounds are a separate small bitmap
  (one texel per cell) uploaded when its generation changes and sampled by the pixel shader.
- Shading types: grayscale text, ClearType (sub-pixel) text, built-in glyphs, pass-through (colour
  glyphs/emoji drawn by D2D), dotted/dashed/curly/solid lines, cursor, filled rect. A single
  pixel shader switches on shading type.
- Gamma/contrast: DirectWrite's alpha-correction is ported into HLSL (`dwrite_helpers.hlsl`):
  gamma ratios and "enhanced contrast" constants are passed in a constant buffer; separate
  contrast values for ClearType and grayscale; thin-font contrast boost.
- Built-in glyphs (`BuiltinGlyphs.cpp`): box drawing U+2500.. (0xA0 chars), Powerline U+E0B0.. (16),
  Symbols for Legacy Computing U+1FB00.. (0x95) are drawn procedurally from a compact table of
  4 packed 32-bit instructions per glyph (shapes: filled 25/50/75/100 %, light/heavy line, rect,
  round rect, ellipse, closed/open paths) so they tile seamlessly regardless of font. Soft font
  (DECDLD) glyphs live in a private-use range U+EF20..U+EF80.
- Ligature overhang: glyphs wider than a cell and extending past a half-cell trigger
  `_drawTextOverlapSplit`, which slices the glyph into per-cell chunks so each chunk takes its own
  cell's foreground colour (colour changes inside a ligature).
- Double-width/height lines (DECDHL/DECDWL): glyphs are scaled via `renditionScale`; double-height
  halves are stored as separate atlas entries to emulate clipping.
- Presentation (`AtlasEngine.r.cpp`): flip-model swap chain, `BufferCount = 3`, frame-latency
  waitable object with max frame latency 1, `DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL` with `Present1` dirty
  rects and scroll rect (rows scrolled are copied on the GPU, only invalidated rows redrawn),
  falling back to `FLIP_DISCARD` when Present1 is disabled (`rendering.disablePartialInvalidation`);
  `ALPHA_MODE_IGNORE` when the background is opaque so DWM can do independent flip, premultiplied
  alpha only when transparency is on. The code comment says DWM engineers asked for sequential flip
  to enable Panel Self Refresh, which needs dirty rects.
- Custom post-process pixel shader (`experimental.pixelShaderPath`, `pixelShaderImagePath`,
  `experimental.retroTerminalEffect`): terminal is first drawn to an offscreen texture, then a user
  HLSL shader runs with a constant buffer of `time`, `scale`, `resolution`, `background`. The
  built-in "retro" shader is scanlines + blur.
- Versioned invalidation: `_generation`, `_fontGeneration`, `_miscGeneration` counters decide which
  resources (const buffer, font-dependent objects, background bitmap) to rebuild.

Settings model and layering (`TerminalSettingsModel`):

- Layer order for globals: `defaults.json` (embedded) -> user `settings.json`; for profiles:
  inbox defaults -> fragments (app-provided JSON dropped in `%LOCALAPPDATA%\Microsoft\Windows
  Terminal\Fragments\<app>\*.json` and a machine-wide path) -> `profiles.defaults` -> the profile.
  Each object holds an ordered list of parents (`IInheritable`: `AddLeastImportantParent`,
  `AddMostImportantParent`, `_FinalizeInheritance`); every setting is `std::optional<T>` per layer, and a
  getter walks up the parents until it finds a value, so "unset" is a first-class state and the
  UI can show "inherited from X".
- `ToJson()` emits only locally-set keys, so the file stays sparse; `Profile::FromJson` / JSON helpers
  in `JsonUtils.h` (typed `GetValue`/`SetValueForKey` with converter traits per enum) do the
  mapping.
- Fixups/migrations (`FixupUserSettings`) rewrite old keys, set a flag, and if anything changed
  the whole file is rewritten. Generated ("dynamic") profiles (WSL, PowerShell, Azure, SSH hosts)
  come from `IDynamicProfileGenerator`; disabled via `disabledProfileSources`.
- Reload: file watcher -> `ReloadSettingsThrottled` -> `ReloadSettings` -> `SettingsChanged`
  event. A hash of (contents, last-write-time) detects self-writes so the app does not reload its own save.
- Write: `CascadiaSettings::WriteSettingsToDisk` serialises the model with jsoncpp
  (4-space indent, precision 6) and writes atomically (`write_utf8_string_to_file_atomic`). The
  file is regenerated from the model, so comments and key order in the user's file are not preserved
  by this path (inferred from reading `ToJson()`; not tested). A backup of the first-ever settings
  file is described in the method comment.

## Feature inventory (grouped by category)

Command palette / navigation
- Modes: action mode (default), commandline mode (type `>` or backspace on empty box; runs `wt`
  command lines in the current window), tab switcher (MRU or in-order, anchor key release commits),
  tab search. Nested commands (parent -> children, back navigation) and `iterateOn` expansion
  (`profiles`, `schemes`, with `${profile.name}`/`${scheme.name}` templates).
- Fuzzy matching: vendored fzf V2 algorithm (`src/cascadia/fzf`): multi-term patterns, case folding
  via ICU, character classes (lower/upper/digit/non-word) with boundary/camelCase bonuses; match
  ranges returned for highlight runs. Items sorted by score then by name; subtitle also matched
  (max of name and subtitle score).
- Action names are synthesised from action+args when no `name` is set (`GenerateName`).
- Suggestions UI (recent commands, snippets/tasks via `sendInput` actions in a `snippets` folder,
  shell-driven completions via OSC 633 `Completions`), Quick Fix (CmdNotFound via OSC 9001).

Actions / keybindings
- ~100 shortcut actions (`AllShortcutActions.h`): copy/paste, tabs, panes (`splitPane`, `movePane`,
  `swapPane`, `resizePane`, `moveFocus`, `togglePaneZoom`, `focusPane`, `closeOtherPanes`),
  scroll, marks (`addMark`, `scrollToMark`, `clearMark`, `clearAllMarks`), selection (`selectCommand`,
  `selectOutput`, `markMode`, `toggleBlockSelection`, `switchSelectionEndpoint`, `expandSelectionToWord`),
  `find`/`findMatch`, `sendInput`, `multipleActions`, `executeCommandline`, `exportBuffer`, `clearBuffer`,
  `adjustFontSize`, `adjustOpacity`, `setColorScheme`, `setTabColor`, `renameTab/Window`, `globalSummon`,
  `quakeMode`, `toggleBroadcastInput`, `togglePaneReadOnly`, `openWorkspace`/`workspaces` (newer).
- Binding model: `actions` array (each `{command:{action,...args}, id, name?, icon?}`) and
  `keybindings` array (`{keys, id}`); legacy inline `{command, keys}` still parsed. IDs of built-ins use
  a `Terminal.` prefix; later layers override by id; `{ "id": X, "command": null }` removes an action;
  an unbound key entry disables a default chord. Key chords use names like `ctrl+shift+t`,
  with scancode and virtual-key forms.

Panes / tabs / windows
- Binary-tree panes, split size fraction, split direction auto (longer axis), zoom, read-only pane,
  broadcast input, drag tabs between windows, tab tear-off, pane move to new tab/window, restore last
  closed, duplicate tab, per-tab colour/title, tab width modes, tab row in title bar.
- Process Model 2.0: a "monarch" process owns window naming and global hotkeys; "peasants" are
  windows; `wt -w <name|id|0|last>` targets an existing window; `windowingBehavior`
  (`useNew`, `useExisting`, `useAnyExisting`). Persisted window layouts restore tabs/panes on start.
- `wt` command line: `new-tab`, `split-pane`, `focus-tab`, `move-focus`, `swap-pane`, `focus-pane`,
  `;` separated sub-commands; same arg names as actions.

Quake mode / summon
- `globalSummon` registered with the OS hotkey API by the monarch only; args `desktop`
  (`any|toCurrent|onCurrent`), `monitor` (`any|toCurrent|toMouse|onCurrent|int`), `name`,
  `toggleVisibility`, `dropdownDuration`, `dropdownDuration` ignored if animations are disabled.
  `quakeMode` = `globalSummon` on a window named `_quake` (docked to top, full width, only bottom edge
  resizable, focus mode, not a target for "most recent window"). Minimize-to-tray / tray icon.

Profiles
- Profile = commandline, starting directory, icon, tab title, colour scheme, font, appearance
  (`appearance` + `unfocusedAppearance` objects), padding, background image, acrylic/opacity, bell
  style, history size, snap on input, altGr aliasing, cursor, scrollbar state, `source` for generated
  profiles, GUID identity, `hidden`, `elevate`, `closeOnExit`, `tabColor`, `bellStyle`, environment reload.
- Colour schemes (named, 16 colours + fg/bg/cursor/selection), themes (window chrome) with
  light/dark pair and `colorScheme` pair that follows system theme.

Terminal core / input / unicode / graphics
- VT: DEC private modes incl. synchronized output 2026, grapheme clustering 2027, win32-input-mode
  9001; kitty keyboard protocol progressive-enhancement flags 1/2/4/8/16 (`terminalInput.hpp`);
  sixel (`SixelParser`), DECDLD soft fonts, DECDHL/DECDWL, hyperlinks OSC 8, OSC 52 clipboard, OSC 4/10/11/12/17
  colours, OSC 9;4 taskbar progress, OSC 9;9 cwd, OSC 7 cwd (OSC 7 is in the table), OSC 133,
  OSC 633 (VS Code), OSC 1337 SetMark, OSC 9001 (WT private). No kitty graphics protocol found in source.
- Text measurement: `compatibility.textMeasurement` (`graphemes|wcswidth|console`) and
  `compatibility.ambiguousWidth`; grapheme-cluster-aware `Row`.
- Colour correction: `adjustIndistinguishableColors` (`never|indexed|always` plus automatic),
  `intenseTextStyle`, 
  `bellStyle` (audible/window/taskbar flags).
- URL detection (`experimental.detectURLs`), copy formatting (html/rtf), `trimBlockSelection`,
  `copyOnSelect`, `focusFollowMouse`, `wordDelimiters`, scroll-to-zoom / scroll-to-opacity,
  `repositionCursorWithMouse`.

Search / scrollback
- `SearchBoxControl`: overlay with case sensitivity, regex, wrap-around; match highlight; scrollbar
  shows search matches as marks (user story H in marks spec); history size configurable (`historySize`);
  `exportBuffer` to file; `clearBuffer` (screen/scrollback/all).

Shell integration
- Marks live on `Row`s as `ScrollbarData {category, color, exitCode}`; text attributes mark regions as
  prompt / command / output; `MarkExtents` is computed on demand (start, end, commandEnd, outputEnd).
  Categories: Default, Error, Warning, Success, Prompt. `autoMarkPrompts`, `showMarksOnScrollbar`.
  Actions scroll between marks, select previous command output, add manual mark. Roadmap items in the
  spec: gutter icons with quick actions, separators, collapse, sticky header, bookmark with comment;
  marks surviving reflow is marked "Crawl" (not solved at spec time).

Settings UI
- Navigation: General (Startup, Interaction, Rendering), Appearance (Global, Colour schemes, Themes),
  Profiles (Defaults, each profile, Add), Actions (keyboard), plus "Open JSON file" button. Each
  setting wrapped with an inheritance indicator. Edits are held in a cloned settings object with an
  explicit Save/Discard bar (rejected alternative: auto-save). A live `TermControl` preview shows font
  and appearance changes. Actions page allows add/edit/remove key chords and edit action args in a
  modal; settings that only make sense in the palette are not forced to have keys.

Integration / packaging / security
- Default-terminal handoff (registry), Windows shell extension, UAC elevation helper (`ElevateShim`),
  Azure Cloud Shell connector, SSH host profile generator, store/MSIX/portable packaging.
- Fragments are a third-party extensibility mechanism with ownership tags (`OriginTag`) and a
  disable list; fragment actions must carry IDs and cannot override built-in IDs.

## Control surface (config, actions, IPC)

- File: single JSON-with-comments `settings.json` (+ embedded `defaults.json`, `$schema` in
  `doc/cascadia/profiles.schema.json` for editor completion). Top-level: globals, `profiles`
  (`defaults` + `list`), `schemes`, `themes`, `actions`, `keybindings`, `newTabMenu`.
- Action shape: `{"command": {"action": "splitPane", "split": "right", "profile": "..."}, "id": "User.X"}`;
  bare string commands for arg-less actions; `multipleActions` takes an array of actions;
  `iterateOn` + `${...}` templates generate menu items; nested `commands` build submenus.
- Actions take typed args defined in IDL (`ActionArgs.idl`), validated at parse; argument-bearing
  actions listed once in `ALL_SHORTCUT_ACTIONS_WITH_ARGS`. Dispatch is one `ShortcutActionDispatch::DoAction`
  that keybindings, palette, menus and command line all call.
- IPC: no control socket. Remote control is by the `wt` CLI (`-w` window targeting through the
  monarch's COM interface), `sendInput`/`executeCommandline` actions, and OSC sequences from inside the terminal.
  Settings change detection is by file watcher and hash.
- GUI/text sync: GUI edits a cloned model and, on Save, serialises the whole model to the file;
  file edits trigger a throttled reload that replaces the model. The UI keeps a "Open JSON file" escape hatch.
  There is no per-key surgical edit.

## Performance (numbers + method)

I found no authoritative, reproducible latency/throughput numbers in the cloned repository (no
benchmark docs beyond unit tests). What can be said from primary sources:

- Design-level budgets visible in code: 3 swap-chain buffers; max frame latency 1 (waitable object);
  dirty-rect presentation; instance buffer sized to cell count with 16-bit positions; atlas reset
  rather than eviction.
- PR #11623 (AtlasEngine introduction, merged 2021-11-13, as summarised by a search result and
  unverified against the primary page): author described it as unoptimised, with glyph hashing up to a
  third of CPU time and VT parsing plus buffer management dominating otherwise. Not independently verified.
- The Atlas engine became the default renderer around Preview 1.21 and the old DxEngine was removed
  (secondary report; unverified).
- Method for Apotheosis: do not quote WT numbers; measure ourselves with `vtebench`-style throughput
  and a key-to-photon camera/`present` timestamps.

## What Apotheosis should take

1. Stable string action IDs, `actions` separate from `keybindings`, layered override by ID, removal by null.
   (Maps cleanly to KDL nodes: `action "term.split-right" { split "right" }` and `bind "ctrl+shift+d" "term.split-right"`.)
2. Typed action args with one dispatcher shared by keys, palette, rules, menus, CLI and socket.
3. Template expansion (`iterateOn`) for generating palette entries per profile/scheme, but done with a
   closed vocabulary rather than string substitution if possible.
4. Palette with modes (actions, commandline, tab switch), nested commands, fzf-style scoring with match ranges
   for highlighting; MRU tab switcher with anchor-key commit.
5. Cascading settings with optional-per-layer values and an "inherited from" indicator in the GUI;
   `profiles.defaults` layer; sparse serialization.
6. Per-row mark metadata, `MarkExtents` derived on demand, OSC 133/633/1337/ConEmu 9;12 all folded into one model,
   scrollbar pips, scroll-to-mark and select-output actions.
7. Atlas-style renderer techniques: shelf/skyline-packed glyph atlas with hash cache, instanced quads, per-cell
   background texture, single shader switching on shading type, procedural box-drawing/Powerline/legacy-computing
   glyphs, per-cell colour slicing for ligatures, cheap "simple text" fast path, generation counters,
   damage rects + scroll copy, waitable/low-latency present, opaque-background fast path.
8. Quake semantics as arguments (`monitor`, `desktop`-equivalent, toggle, animation duration honouring reduced
   motion), implemented through compositor protocol on Abyss rather than a global-hotkey API.
9. Kitty keyboard flags 1-31 and synchronized output / grapheme clustering modes (WT proves the feature set is wanted).
10. Atomic write + self-write hash to avoid reload loops; fixup/migration pass with version flag.
11. Fragments with ownership tags and ID-required actions as the extension surface (fits "manifest, off-by-default hooks").

## What it should avoid

- Regenerating the whole config file from the model on every GUI save (loses comments/ordering). Apotheosis
  must do surgical, format-preserving KDL edits so GUI and hand edits co-exist.
- Save-button-only model for settings that users expect to preview live; prefer live apply with revert, since
  the state is a pure function of the KDL document.
- Breaking the buffer into GDI-like primitives before rendering (their own README calls this wasteful); give the
  renderer the grid and attributes directly.
- Atlas full -> flush frame -> wipe everything. Prefer multi-page atlas with LRU or generation-based eviction.
- Two renderer backends (D2D and D3D) for fallbacks; Apotheosis has one wgpu path (plus software adapter via Vulkan
  lavapipe if available).
- Process-wide monarch coordinated over COM; use the compositor + JSON socket instead.
- String-substituted JSON templates (`${profile.name}`) as the only way to generate commands.
- Dependence on a shell-side cooperation for correctness of marks without a fallback (WT marks need
  OSC 133 or `autoMarkPrompts` heuristics).
- Marks that do not follow reflow; plan buffer-anchored marks from day one.

## Sources

Primary source (repo `microsoft/terminal` @ `f6de9e0`):
- `src/renderer/atlas/README.md`; `AtlasEngine.cpp` (`_mapRegularText`, `_mapCharacters`); `AtlasEngine.r.cpp`
  (swap chain creation, frame latency); `BackendD3D.h/.cpp` (`QuadInstance`, `AtlasGlyphEntry`, `ShadingType`,
  `_resetGlyphAtlas`, `_drawGlyphAtlasAllocate`, `_drawTextOverlapSplit`); `BuiltinGlyphs.h/.cpp`;
  `dwrite_helpers.hlsl`; `custom_shader_ps.hlsl`; `common.h` (`GraphicsAPI`, `AntialiasingMode`).
- `src/cascadia/TerminalSettingsModel/`: `AllShortcutActions.h`, `MTSMSettings.h`, `IInheritable.h`,
  `CascadiaSettingsSerialization.cpp` (`FinalizeLayering`, `FixupUserSettings`, `WriteSettingsToDisk`,
  `_writeSettingsToDisk`, `FindFragmentsAndMergeIntoUserSettings`), `ActionMap.h`, `Command.cpp` (`ExpandCommands`).
- `src/cascadia/TerminalApp/`: `CommandPalette.cpp`, `FilteredCommand.cpp`, `Pane.h`, `AppLogic.cpp` (`ReloadSettingsThrottled`).
- `src/cascadia/fzf/fzf.cpp` (`Match`, constants).
- `src/buffer/out/Marks.hpp`; `src/terminal/adapter/adaptDispatch.cpp` (`DoConEmuAction`, `DoITerm2Action`,
  `DoFinalTermAction`, `DoVsCodeAction`, WT action); `src/terminal/parser/OutputStateMachineEngine.hpp` (`OscActionCodes`);
  `src/terminal/adapter/DispatchTypes.hpp` (modes 2026/2027/9001); `src/terminal/input/terminalInput.hpp` (kitty flags).
- Specs in `doc/specs/`: `#6899 - Action IDs`, `#2046 - Command Palette`, `#653 - Quake Mode`, `#11000 - Marks/Shell-Integration-Marks.md`,
  `#1564 - Settings UI/{spec,cascading-settings}.md`, `#6900 - Actions Page/spec.md`, `#1595 - Suggestions UI`, `#5000 - Process Model 2.0`.

Web:
- https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions (action formats; fetched, partially read)
- https://github.com/microsoft/terminal/pull/11623 (AtlasEngine introduction; only seen through search summary, unverified)
- https://sw.kovidgoyal.net/kitty/keyboard-protocol/ (flags referenced by WT source)
