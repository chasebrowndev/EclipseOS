# iTerm2 research dossier (docs only)

Method: official iTerm2 documentation pages only (macOS app, closed to our build; no source read). Items not on the pages fetched are marked "unverified". Descriptions are of techniques, not code.

## Summary

iTerm2 is the feature-leadership reference for terminal-as-workspace. Its distinguishing ideas, all worth studying for Apotheosis:

1. Shell integration marks (OSC 133 plus proprietary OSC 1337) turn the scrollback into a list of commands with exit codes, cwd, host and user. Everything else (select output, navigate, filter, alerts, history, frecent dirs, captured output) is built on those marks.
2. Triggers: regex on screen lines, plus 16 event triggers, mapped to 30 actions. This is the closest thing to an event -> actions rules engine in any terminal.
3. Smart selection: scored, precision-class regex rules with attached context-menu actions.
4. tmux -CC control mode: tmux windows and panes become native tabs and splits.
5. Automatic profile switching by user, host, path, job with a numeric score and a profile stack.
6. Inline images and file transfer via OSC 1337, with a multipart variant that works through tmux.
7. Instant replay, broadcast input, coprocesses, annotations, badges, status bar, password manager, hotkey window, dynamic profiles, Python API.

Its weaknesses for us: macOS-only, config in plist and GUI with JSON dynamic profiles, scripting via an embedded Python runtime, a proprietary escape namespace, GPU renderer without ligatures.

## Architecture (how it is built)

Only what the docs state:
- A native macOS app. The GPU renderer is Metal-based. Docs state ligatures are unsupported by the GPU renderer and enabling them costs performance because they go through CoreText, which is slower than Core Graphics.
- The composer (Auto Composer) is a native text view replacing the shell prompt.
- Shell integration is a per-shell script (bash, zsh, fish, xonsh; tcsh partially) that emits escape codes at prompt start, prompt end, command start, output start and finish. A trigger-based fallback exists for hosts where no script can be installed.
- Scripting: a Python API (runtime installed by the app, scripts folder, REPL, Script Console). Wire transport is unverified here.
- tmux integration: iTerm2 speaks tmux control mode and mirrors tmux state as native objects; tmux stays the source of truth for sessions.
- Dynamic profiles: JSON/XML/plist files in a watched folder; each needs Name and Guid; one malformed file blocks all changes.

## Feature inventory

### Triggers (automation)
- Regex (ICU) matched per screen line when a newline or cursor-moving escape is processed. BEL is evaluated. Null bytes treated as spaces. Only the last 3 wrapped display lines of a long line are matched (configurable).
- Options: Instant (fire as soon as match, once per line; may make `.*` match less), Job filter (active only while a foreground job with that name runs; case-insensitive), parameter interpolation (`\0`, `\1`..`\9`, `\a \b \e \n \r \t \xNN`).
- 30 actions: Annotate, Bounce Dock Icon, Buffer Input, Capture Output, Change Style, Enter/Exit Workgroup, Fold to Named Mark, Highlight Line, Highlight Text, Inject Data, Invoke Script Function, Make Hyperlink, Open Password Manager, Post Notification, Prompt Detected, Report Directory, Report User & Host, Ring Bell, Run Command, Run Coprocess, Run Silent Coprocess, Send Text, Set Mark, Set Named Mark, Set Tab Status, Set Title, Set User Variable, Show Alert, Stop Processing Triggers. Parameter required: Run Command, Run Coprocess, Post Notification, Send Text, Show Alert.
- 16 event triggers, any action usable with any event: Prompt Detected, Command Finished (exit-code filter), Directory Changed, Host Changed, User Changed, Idle (default 30 s), Activity After Idle, Session Ended, Bell Received, Long-Running Command (default 60 s, optional command regex), Custom Escape Sequence, Notification Posted (OSC 9), Progress Bar Changed, Job Started, Job Ended, Variable Changed. Captures feed `\1`..
- URL scheme `iterm2:` can import triggers, behind a confirmation dialog.

### Shell integration and marks
- Mark per prompt (blue triangle in margin, red on failure, exit code in context menu). Next/previous mark navigation. "Alert on next mark" for long commands.
- Select Output of Last Command; command selection scopes Find, Filter and Select All to one command's output.
- Command history per user@host (popup, autocomplete, toolbelt; up to 200 persisted per user/host).
- Recent directories by frecency, starrable.
- Download with scp (right-click filename) and upload (Option-drag). it2dl, it2ul, imgcat, it2 CLI install under `$HOME/.iterm2/`.
- Auto Composer: native prompt editor.
- Multi-line prompts: place mark explicitly via a prompt-mark helper in PS1.
- Auto-load integration over SSH or login shell (3.5+).

### Captured output
- Trigger action Capture Output adds matching lines to a toolbelt panel linked to command history. Click scrolls to line and flashes; double-click runs the trigger's coprocess parameter with captures (example types an editor open command); right-click toggles a checkmark for handled entries. `OSC 1337 ; ClearCapturedOutput` clears before a build. Needs scrollback big enough.

### Smart selection
- Quad-click selects; right-click selects and adds actions to context menu; Cmd-click runs first matching rule action.
- Algorithm: each rule regex tried on screen text; only matches containing the cursor character count; longest match per rule scored by length; candidate from highest precision class with any match wins; highest score within class. Precision classes: Very Low .. Very High.
- Actions: Open File, Open URL, Run Command, Run Coprocess, Send Text, Run Command in Window, Copy. Substitutions `\0..\9 \d \u \h` or interpolated `matches[]`.
- Default rules: words, namespace::ident, paths, quoted strings, dotted names, URI schemes (mailto http https ssh telnet), @selector, emails.
- Semantic history: Cmd-click file opens in default app, URL, editor or custom command with placeholders (filename, line, text before/after, cwd). OSC 8 `file:` links with `#line:col` fragment honoured (3.4+).

### Inline images and files (OSC 1337)
- `ESC ] 1337 ; File = args : base64 BEL` with args name (base64), size, width, height, preserveAspectRatio (default 1), inline (default 0 = download). Units: cells, `px`, `%`, `auto`. Any format macOS decodes, animated GIF, PDF. Multipart: MultipartFile, FilePart chunks, FileEnd (3.5+, works through tmux; chunk limit 1 MiB, 256 B on old tmux). Retina-correct since 3.2. Kitty graphics protocol also supported. Feature Reporting protocol for detection.

### tmux -CC
- tmux windows/panes -> native windows/tabs/splits; close kills tmux window; split -> `split-window`; resize -> `resize-pane`; client size = smallest attached client. Detach/reattach restores layout; 3.7 restores per-pane modifyOtherKeys and bracketed paste (tmux 3.5+). OSC 52 query replies need tmux 3.6+. Dashboard for many windows. Collaboration by multiple attachers. Limits: tmux tab cannot also hold non-tmux splits; empty gray areas when window larger than tmux max; recovery keys (Esc detach, X force exit, L log protocol, C command).

### Profiles and switching
- Automatic Profile Switching rules: `user@`, hostname (with `*`), `:path` (with `*`), `&job` (full command line since 3.6), `!` sticky prefix. Score: exact host 16, wildcard host 8, job 4, user 2, exact path 1, wildcard path 0. Highest wins. Per-session profile stack starting at original profile; matching a profile already on stack pops above it; else push; no match empties stack (sticky excepted). Requires shell integration or trigger workaround.
- Dynamic profiles (watched folder). Badges (interpolated text watermark), status bar (per pane or per window, can show tmux status), toolbelt.

### Session control
- Broadcast input: current session only, all panes in all tabs, all panes in tab, toggle individual sessions; optional background pattern indicator on receivers; docs warn to be careful.
- Instant replay: Cmd-Opt-B, left/right arrows through time, Esc exits; 4 MB history per session default; frame timestamps to the second.
- Coprocess: process bound to one session, receives all session output; started by key action, trigger (visible or silent), smart selection, capture output.
- Session restoration policies, window arrangements, Undo Close, buried sessions, archives, Restart Session. Hotkey window (needs accessibility permission on macOS).
- Annotations: attach notes to cursor word, selection, or via trigger/escape (`AddAnnotation`, `AddHiddenAnnotation`).
- Copy mode with key-action scripting. Filter on Find. Paste safety (multi-line warning, tab conversion prompt, Paste Slowly: 16-byte batches, 125 ms delay). Secure Keyboard Entry.
- Password manager: Keychain-backed list; can open on session start; trigger action Open Password Manager. Details of trigger matching and storage beyond that: unverified (page 404).

### Escape codes (interface facts)
- `OSC 133 ; A|B|C|D[;status]`; `OSC 1337 ; SetMark | CurrentDir= | RemoteHost=user@host | ShellIntegrationVersion=n;shell | SetUserVar=name=base64 | AddAnnotation | AddHiddenAnnotation | ReportCellSize | RequestAttention=yes|once|no|fireworks | CopyToClipboard=name ... EndCopy | StealFocus | ClearScrollback | SetColors=key=value | ClearCapturedOutput`; `OSC 7` synonym of CurrentDir.

## Control surface (config, actions, IPC)
- Config: GUI settings per profile (Profiles > General/Colors/Text/Window/Terminal/Session/Keys/Advanced); stored as plist; dynamic profiles JSON/plist folder; Advanced Settings list of tunables (e.g. trigger line count, Retina images, separators).
- Actions: key mappings ("key actions") including Run Coprocess, Open URL/Semantic History, copy-mode commands, Send Text; menu items. Triggers share many actions. Not one unified vocabulary (actions differ between trigger, smart selection and key mapping) -- Apotheosis should unify.
- IPC: Python API (scripts, Invoke Script Function trigger/action), AppleScript (deprecated), `it2` CLI, `iterm2:` URL scheme (reveal; `command` always reviewed; `triggers` confirmed), custom escape sequences and user variables as in-band control channel.

## Performance (numbers + method)
Docs give almost no benchmarks. Stated figures: instant replay 4 MB/session default; trigger match window 3 wrapped lines; paste slowly 16 B / 125 ms; multipart image chunk 1 MiB. "Copy Performance Stats" and "Capture GPU Frame" exist as diagnostics. No throughput or latency numbers published in pages read (unverified elsewhere).

## What Apotheosis should take
- Marks as a first-class structure: per-command record (prompt, input, output ranges, exit code, cwd, host, user, duration). Accept OSC 133 A/B/C/D (with kitty/ghostty extensions) and OSC 7; treat iTerm's OSC 1337 CurrentDir/RemoteHost/SetMark/SetUserVar as compat inputs.
- Events -> actions rules using the same vocabulary as keybinds. Adopt iTerm's 16 event kinds as the seed list, with captures, plus screen-regex triggers with Instant and job filters. Cap regex time and lines; avoid arbitrary shell execution by default (sandbox/consent via Abyss).
- Smart selection with precision classes and per-rule actions; expose rule set as KDL.
- Captured output panel with clickable entries tied to a command.
- Automatic profile switching with additive score and profile stack and sticky marker.
- tmux -CC support (mux category; at least a design decision).
- OSC 1337 File and multipart inline images as a compat input into our native graphics path; support `iTerm2` imgcat users.
- Instant replay as a scrubber over a bounded time-indexed ring (damage log), not by re-parsing.
- Broadcast input with visible indicator and group targets.
- Paste safety suite, secure input, password manager (as secret-service/portal backed, never logged).
- Dynamic profiles as a watched KDL directory.

## What it should avoid
- Embedded Python runtime; use Action vocabulary and JSON socket.
- Separate vocabularies for triggers/smart selection/key actions.
- A single malformed dynamic file blocking all others (isolate per file).
- Proprietary OSC 1337 as the primary interface where standards exist (OSC 133, OSC 7, OSC 8, OSC 52, kitty).
- Unconfirmed command-running triggers that fire from untrusted terminal output (security).
- tmux limitations (cannot mix tmux and non-tmux splits in one tab) if avoidable.
- Ligature-free GPU path.

## Sources
- https://iterm2.com/documentation.html
- https://iterm2.com/documentation-triggers.html
- https://iterm2.com/documentation-shell-integration.html
- https://iterm2.com/documentation-images.html
- https://iterm2.com/documentation-tmux-integration.html
- https://iterm2.com/documentation-smart-selection.html
- https://iterm2.com/documentation-escape-codes.html
- https://iterm2.com/documentation-automatic-profile-switching.html
- https://iterm2.com/documentation-captured-output.html
- https://iterm2.com/documentation-one-page.html (instant replay, broadcast input, coprocess, hotkey, paste safety, dynamic profiles, it2 utilities)
- Password manager page: https://iterm2.com/3.0/documentation-password-manager.html returned 404 (unverified)
