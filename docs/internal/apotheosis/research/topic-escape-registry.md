# Escape sequence registry for a modern terminal (Apotheosis research)

Method note: this dossier was written from the author's working knowledge of the primary sources listed under Sources. The sources were not re-fetched in this run, and no upstream source was cloned. Version numbers, exact key letters and CVE ids that were not re-checked are marked "unverified". Syntax quoted below is interface fact. Technique descriptions are prose.

## Summary

A modern terminal is a state machine (the DEC/ECMA-48 parser from Paul Williams' vt100.net diagram) plus a growing set of de-facto extensions. They fall into four tiers.

1. Core ECMA-48/DEC/xterm. C0/C1 controls, ESC, CSI cursor and erase, SGR, DEC private modes, DCS, OSC titles and colours.
2. Query and report channels. DA1/DA2/DA3, XTVERSION, DECRQM, DECRQSS, XTGETTCAP, DSR, XTWINOPS. Apps use these to discover features instead of trusting $TERM.
3. Ecosystem extensions that are now effectively mandatory. SGR colon sub-parameters and styled underlines, OSC 8 hyperlinks, mode 2004 bracketed paste, mode 2026 synchronized output, mode 1006 SGR mouse, mode 1004 focus events, OSC 52 clipboard, OSC 133 semantic prompts, the kitty keyboard protocol and the kitty graphics protocol.
4. Newer proposals with real adoption. Mode 2027 grapheme clusters, 2031 colour-scheme notification, 2048 in-band resize, 1016 pixel mouse, OSC 66 text sizing, OSC 99 notifications, OSC 5522 rich clipboard, OSC 3008 context signalling, OSC 9;4 progress, and the OSC 1337 iTerm2 family.

Apotheosis should implement tiers 1 to 3 completely, adopt the useful parts of tier 4, and put every sequence that reads state or touches the clipboard, files or the desktop behind a per-sequence policy with a consent prompt. That fits the existing EclipseOS model, where everyday features go through consent prompts and policy editing is superuser-only. The main design point is a sequence-capability table. Each sequence maps to {parser handler, feature-detection answer (DECRQM/XTGETTCAP), policy class, Action hook}. The same table drives settings GUI toggles, KDL keys and the JSON control socket.

## Architecture (how it is built)

### Parser

- Use the Williams state machine: ground, escape, escape-intermediate, CSI entry/param/intermediate/ignore, DCS entry/param/intermediate/passthrough/ignore, OSC string, SOS/PM/APC string. Transitions "anywhere" cover CAN, SUB and ESC. Alacritty's `vte` crate, Ghostty's table-driven parser and Contour's `vtparser` all follow this.
- Parameters. Up to roughly 16 to 32 numeric params. Each param may carry colon-separated sub-params. A bitmask records which separators were colons. Overflow clamps; it never allocates. Ghostty and `vte` both keep sub-params as a flag on the param list.
- Keep the hot path allocation-free. The parser emits borrowed slices or a fixed-size struct. OSC and APC payloads accumulate in a bounded buffer with a per-sequence cap (OSC 52 and kitty graphics need large caps; titles do not). Over the cap, the parser drops the sequence and returns to ground. It must never truncate silently into a valid-looking command.
- UTF-8 decoding sits in front of or inside the ground state. A SIMD fast path for plain ASCII/UTF-8 runs (Ghostty, Alacritty `vte` with `simdutf`-style batching) is a major throughput win, because most bytes are printable text.
- C1 controls (0x80..0x9F) are only recognised as controls when the terminal is in 8-bit mode. In UTF-8 mode they are never valid single bytes. Treat the 7-bit ESC forms as canonical and never parse a raw 0x9B as CSI under UTF-8. xterm, kitty and Ghostty all take this approach. Doing otherwise is a known injection vector.
- DCS and APC are separate sub-parsers that stream. Sixel and kitty graphics payloads can be megabytes, so the parser hands bytes to a streaming decoder in chunks instead of buffering a whole sequence.
- String terminators. Accept ST (ESC \) everywhere. Accept BEL only for OSC (xterm legacy, near-universal). Reply with the same terminator the request used. This matters for OSC 52 and colour queries, because some apps parse the reply strictly.

### Dispatch and state

- Handlers are a flat enum, `Action`-style, produced by the parser and consumed by the screen model on the core thread. This matches EclipseOS's single-threaded, handle-based style.
- Mode state is a bitset keyed by DEC private mode number, with a saved-mode stack for XTSAVE/XTRESTORE (`CSI ? Pm s` / `CSI ? Pm r`, xterm). DECRQM (`CSI ? Ps $ p`) answers from that bitset. The reply codes are 0 unknown, 1 set, 2 reset, 3 permanently set, 4 permanently reset. DECRQM is the discovery mechanism for 2026, 2027, 2031 and 2048.
- Replies go to the PTY master through a write queue. Replies are byte-exact, never logged by content, and rate-limited. The write queue is bounded, so a hostile program that floods queries cannot make the terminal allocate without limit.

### Capability and policy layer

Each sequence gets a policy class. This is the Apotheosis addition.

- allow: pure rendering effects with no side channel.
- ask: needs a consent prompt (clipboard read, OSC 52 write from a non-focused pane, file download, notification, open-URL).
- deny-by-default: reads local state back into the PTY stream (title report, window-state report, font queries).

The class is stored per profile, overridable per rule (event to actions), and visible in the settings GUI.

## Feature inventory (grouped by category)

Legend. Impl = who implements it, from memory and not re-surveyed this run: X xterm, K kitty, G Ghostty, W WezTerm, C Contour, I iTerm2, F foot, A Alacritty, T Windows Terminal, V VTE, tmux. "wg" means a terminal-wg spec exists.

### 1. C0 and C1 controls and bare ESC

| Item | Purpose | Notes |
|---|---|---|
| BEL 0x07 | bell | Rate-limit. Route to the notification action. Never play audio unconditionally. |
| BS 08, HT 09, LF 0A, VT 0B, FF 0C, CR 0D | cursor motion | LF/VT/FF are the same, subject to LNM (mode 20). |
| SO/SI 0E/0F | charset shift | Needed for DEC special graphics line drawing. |
| ENQ 05 | answerback | Default empty. Never send non-empty text (injection into the PTY). |
| CAN 18, SUB 1A | abort sequence | The parser handles them from any state. |
| ESC 7 / ESC 8 | DECSC/DECRC | Save/restore cursor, attributes, charset. |
| ESC D, E, M | IND, NEL, RI | Index and reverse index. |
| ESC H | HTS | Set tab stop. |
| ESC = / ESC > | DECKPAM/DECKPNM | Keypad mode. |
| ESC c | RIS | Full reset. Must also reset the policy state that is per-session (kitty keyboard stacks, graphics, hyperlinks). |
| ESC ( 0 / ESC ( B etc. | designate charset | G0..G3. |
| ESC # 8 | DECALN | Fill with E (test pattern). |
| ESC SP F/G | S7C1T/S8C1T | Ignore S8C1T under UTF-8. |
| C1 via ESC Fe | NEL, HTS, RI, SS2/3, DCS, CSI, ST, OSC, SOS, PM, APC | 7-bit forms only. |

### 2. CSI

Cursor and edit.
- CUU/CUD/CUF/CUB (A B C D), CUP/HVP (H f), CNL/CPL (E F), CHA (G), VPA (d), HPA/VPR/HPR.
- ED (J, with 3 clearing scrollback), EL (K), ECH (X), ICH (@), DCH (P), IL (L), DL (M), REP (b), SU/SD (S T).
- DECSTBM (r), DECSLRM (s, only when mode 69 DECLRMM is set), DECSCUSR (`CSI Ps SP q`, cursor shape and blink), DECSTR (`CSI ! p`).
- Tab control: CHT (I), CBT (Z), TBC (g).
- DECCRA/DECFRA/DECERA/DECSERA rectangular ops. xterm, Contour, foot and WezTerm have some of them. They are low priority.

SGR (`CSI Pm m`).
- Attributes: 0 reset, 1 bold, 2 faint, 3 italic, 4 underline, 5 and 6 blink, 7 reverse, 8 conceal, 9 strike, 21 double underline (ECMA-48 meaning; some terminals treat it as bold-off), 22 to 29 resets, 53/55 overline.
- Colours: 30 to 37, 40 to 47, 90 to 97, 100 to 107. 38 and 48 take both `38;5;n` / `38;2;r;g;b` (semicolon, legacy) and `38:5:n` / `38:2::r:g:b` (colon, ITU T.416) forms. The colon form has an optional colour-space id slot, which is why the empty field matters. Parse both; always emit colon form only to apps that asked via DECRQM/terminfo `Smulx`/`setrgbf`.
- Underline styles: `4:0` none, `4:1` single, `4:2` double, `4:3` curly, `4:4` dotted, `4:5` dashed. Underline colour: `58:2::r:g:b` / `58:5:n`, reset `59`. Originated in kitty and VTE, widely copied. Fallback when unsupported: render a plain underline.
- Unknown sub-parameter forms must be consumed and ignored, never reinterpreted. A classic bug is `38:2:...` being treated as `38;2` followed by a stray colour.

Reports and queries.
- DA1 (`CSI c`, reply `CSI ? 62 ; ... c`), DA2 (`CSI > c`), DA3 (`CSI = c`), XTVERSION (`CSI > 0 q`, reply `DCS > | name version ST`).
- DSR 5/6 (status and cursor position), DECXCPR (`CSI ? 6 n`). Mode 996-style colour-scheme query: `CSI ? 996 n`, reply `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light).
- XTWINOPS (`CSI Ps t`). Safe: 14/16/18/19 report pixel and cell sizes. Restricted: 21 (title report, injection vector), 22/23 (title stack push/pop, which is fine), 1 to 10 and 3/4/5/8 (move, resize, raise windows). On Wayland, window moves are impossible, and resize or maximise should be ask-or-deny.
- DECRQM (`CSI ? Ps $ p` and ANSI `CSI Ps $ p`).
- XTSAVE/XTRESTORE of private modes, XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`, sixel geometry and colour registers).

Keyboard.
- Kitty keyboard protocol. `CSI > flags u` push, `CSI < n u` pop, `CSI = flags ; mode u` set, `CSI ? u` query. Flags: 1 disambiguate, 2 report event types, 4 report alternate keys, 8 report all keys as escape codes, 16 report associated text. Main and alternate screens have separate stacks. Key events use `CSI unicode-key-code ; modifiers : event-type ; text u`, with legacy letter forms for some keys.
- xterm modifyOtherKeys: `CSI > 4 ; 2 m`. Support it as a fallback for apps that do not speak kitty's protocol (legacy vim and tmux).
- Mode 1 (DECCKM), 66 (DECNKM), DECBKM (67).

### 3. DEC private modes (`CSI ? Pm h` / `l`)

| Mode | Name | Purpose | Notes |
|---|---|---|---|
| 1 | DECCKM | application cursor keys | |
| 3 | DECCOLM | 80/132 columns | Usually ignored or gated by mode 40. Clears the screen. |
| 5 | DECSCNM | reverse video | |
| 6, 7 | DECOM, DECAWM | origin mode, autowrap | |
| 12 | att610 | cursor blink | |
| 25 | DECTCEM | cursor visibility | |
| 45 | | reverse wraparound | |
| 47/1047/1049 | | alternate screen | 1049 saves the cursor, switches and clears. Make the scrollback behaviour a setting. |
| 69 | DECLRMM | left/right margins | |
| 80 | DECSDM | sixel scrolling | Inverse semantics. Get it right or sixels scroll wrongly. |
| 1000/1002/1003 | | mouse tracking modes | Normal, button-motion, any-motion. |
| 1004 | | focus in/out | `CSI I` / `CSI O`. Cheap. Required by vim, tmux and neovim. |
| 1005, 1015 | | UTF-8, urxvt mouse encodings | Legacy. 1005 is ambiguous. Keep 1015 low priority. |
| 1006 | | SGR mouse encoding | `CSI < b ; x ; y M/m`. Mandatory. |
| 1007 | | alternate scroll | Wheel becomes cursor keys on the alternate screen. |
| 1016 | | SGR-Pixels mouse | Same format as 1006 with pixel coordinates. Needs the cell size in pixels. |
| 1010/1011 | | scroll-on-output / key | |
| 1042 | | urgency on BEL | |
| 1048 | | save/restore cursor | |
| 2004 | | bracketed paste | Wraps pasted text in `CSI 200 ~` ... `CSI 201 ~`. |
| 2026 | | synchronized output | `CSI ? 2026 h` begin, `l` end. The terminal buffers screen updates and presents atomically, with a timeout (suggested about 1 s or a bounded size) so a crashed app cannot freeze the screen. wg spec. |
| 2027 | | grapheme clustering | Terminal treats a cluster (UAX #29) as one cell sequence, with width by the cluster rules. DECRQM decides. wg spec (unverified). |
| 2031 | | colour scheme change notification | Unsolicited `CSI ? 997 ; 1|2 n` when the OS theme flips. Paired with the `CSI ? 996 n` query. Originated in Contour. |
| 2048 | | in-band size reports | When set, the terminal sends `CSI 48 ; rows ; cols ; height_px ; width_px t` on every resize. It replaces SIGWINCH plus ioctl for remote and containerised use. Originated in the Contour/neovim community. |
| 9001 | | win32-input-mode | Windows Terminal only. Skip. |

The mode 2026 and 2048 sequences are the kind a Wayland-native terminal can do better than most, because the compositor frame callback gives a natural present point.

### 4. DCS (`ESC P ... ST`)

- DECRQSS: `DCS $ q Pt ST`, with Pt among `m` (SGR), `r` (DECSTBM), ` q` (DECSCUSR), `"q` (DECSCA) and `s`. Reply `DCS 1 $ r Pt ST`, or `DCS 0 $ r ST` if invalid. Security: the reply echoes terminal-held state into the PTY. The classic injection (an iTerm2 bug via DECRQSS combined with tmux integration mode) came from echoing attacker-controlled bytes. Echo only strictly formatted values you generate, never the request body.
- XTGETTCAP: `DCS + q hex(name) [; hex(name)...] ST`. Reply `DCS 1 + r hex(name)=hex(value) ST` or `DCS 0 + r ST`. It lets apps query terminfo-style capabilities without a terminfo db. Ship an in-binary capability table (`TN`, `Co`, `RGB`, `Smulx`, `Setulc`, `Su`, `Ms`, `fullkbd`, etc.) so SSH to hosts without Apotheosis terminfo still works.
- Sixel: `DCS Pa ; Pb ; Ph q <data> ST`. Raster attributes (`" Pan ; Pad ; Ph ; Pv`), colour definitions (`# Pc ; Pu ; ...`), repeat (`! n c`), carriage return `$` and newline `-`. Implementers: xterm, foot, WezTerm, Contour, mlterm, iTerm2, Ghostty (unverified in 2026), VTE (recent). Not Alacritty, and kitty explicitly declines. Security and perf: cap the pixel area and colour registers (the default is 256, with XTSMGRAPHICS to set). Decode to RGBA in a worker.
- tmux passthrough: `DCS tmux ; <payload with every ESC doubled> ST`. tmux unwraps it for the outer terminal only if `allow-passthrough` is on. The inner terminal sees only the unwrapped bytes. A terminal needs no special support. It does need to decode graphics and OSC replies without relying on the TTY being direct (no cell-size ioctl, so use 2048 or XTWINOPS).
- DECUDK, DECDLD (soft fonts), DECRSPS, DECRQTSR: legacy; ignore without error. Always consume to ST.
- DCS `= 1 s` / `= 2 s`: synchronized update (the older BSU/ESU form from iTerm2/kitty). Support it as an alias of mode 2026.
- Ignore-and-swallow rule. An unrecognised DCS must be consumed to ST silently. It must never leak payload as printed text.

### 5. OSC (`ESC ] Ps ; Pt ST`)

| OSC | Purpose | Implementers | Security notes |
|---|---|---|---|
| 0 / 1 / 2 | set icon+title / icon / window title | all | Strip C0/C1 from titles. Cap length. Never provide the title report (`CSI 21 t`) by default. |
| 4 | set/query indexed colour (`OSC 4 ; n ; spec`, query `?`) | X, K, G, W, F, V, C, A | Query replies leak only the theme. Fine. |
| 5 / 6 | special colours | X | Low priority. |
| 7 | report cwd as `file://host/path` | K, G, W, F, V, I, C, T | Percent-decode. Verify host. Used for "new tab here". Do not trust it for anything security-relevant. |
| 8 | hyperlink `OSC 8 ; params ; URI ST`, param `id=` | nearly all | Scheme allow-list (https, http, mailto, file only for local host, ssh). Show the target on hover. Block `javascript:`-style schemes. Never open on a bare click without a modifier if the URI differs from the text. |
| 9 | iTerm2 notification `OSC 9 ; msg` | I, G, W, K, F, C | Notification spam and spoofing. Rate-limit and show origin pane. Conflicts with ConEmu sub-commands (see 9;4). |
| 9;4 | ConEmu progress `OSC 9 ; 4 ; st ; pr`, st 0 remove, 1 normal, 2 error, 3 indeterminate, 4 paused/warning | T, G, K (recent), C, W (unverified) | Safe. Maps naturally to tab progress and a taskbar/compositor hint. |
| 10 to 19 | dynamic colours: 10 fg, 11 bg, 12 cursor, 13 pointer fg, 14 pointer bg, 15 to 19 Tek/highlight | X, most | Query is safe. Setting from apps is a feature (vim background detection). Wire to the theme layer. |
| 21 | kitty colour-control (set/query/reset many colours in one call) | K | Unverified details. |
| 22 | pointer shape (`OSC 22 ; name`) | X, K, G, F, C | Map the CSS/X cursor names to compositor cursor shapes via cursor-shape-v1. Kitty also has push/pop forms in its extension. |
| 50 | font query/set | X | Avoid. Font set was a CVE class (xterm OSC 50, CVE-2022-45063, unverified). |
| 52 | clipboard `OSC 52 ; Pc ; base64 ST` (`?` to read) | X, K, G, W, F, T, C, A, tmux | Highest-risk sequence. Write: prompt or allow only if the pane is focused. Read: deny by default, ask each time. Cap the size. Never log content. Select/primary mapping to Wayland selections: `c` is clipboard, `p` and `s` are primary. |
| 66 | kitty text sizing `OSC 66 ; key=val:... ; text ST` (keys s scale 1..7, w width in cells, n/d fractional scale, v/h alignment) | K, G (unverified in 2026) | Requires the renderer to support multi-cell scaled glyph blocks, so it affects the grid and font-fallback design. Detect by a CPR before and after. |
| 99 | kitty desktop notifications (chunked, id, buttons, urgency, icon, close and activation reports, query) | K, G (partial, unverified), F (unverified) | Richer than OSC 9. Activation reports go back to the app, so consent is needed. Map to the freedesktop notifications service (`ec-services`). |
| 104 / 105 | reset indexed / special colours | X, most | |
| 110 to 119 | reset dynamic colours 10 to 19 | X, most | |
| 133 | semantic prompts: `A` prompt start, `B` input start, `C` output start, `D;exit` command end. Kitty and Ghostty add options (`k=`, `redraw=`, `aid=`, `cl=`, `click_events`). | K, G, W, I, V, F, C, T | Shell cooperation. Lets the terminal jump between prompts, select command output, show exit-status marks, and implement scrollback-aware resize reflow at the prompt. wg spec (unverified status). Treat as untrusted text input. A malicious `cat` can forge marks, so never make security decisions from them. |
| 777 | rxvt-style extension; `OSC 777 ; notify ; title ; body` and (VTE) `precmd`/`preexec` | V, F, Ghostty, W, C | Notification path. Rate-limit. |
| 1337 | iTerm2 family: `File=` (inline images and downloads), `SetUserVar=name=base64`, `CurrentDir=`, `SetMark`, `RemoteHost`, `ShellIntegrationVersion`, `Copy=:base64`, `StealFocus`, `RequestAttention`, `ClearScrollback`, `SetBadgeFormat`, `ReportCellSize`, `Capabilities` | I, W, K (partial), Contour (partial) | Inline images are a decoder attack surface. Downloads need a save prompt, never auto-write. SetUserVar is an automation event source. Implement a minimal safe subset (File inline, SetUserVar, ReportCellSize). |
| 5522 | kitty extended clipboard: MIME-typed read/write, with explicit read permission flows and `type=` | K (unverified version) | Better than 52 for images/HTML. Permission is per MIME type. Needs a Wayland data-offer bridge. |
| 3008 | context signalling (systemd): `start=<id>;type=...;user=...;hostname=...;cwd=...`, `end=<id>;exit=...` | systemd 257+ shell hooks, VTE and Ghostty (unverified) | Tells the terminal that a shell, container or ssh session started or ended. Basis for per-context tab titles and badges. Treat as untrusted metadata. |
| 1 ; sub-forms of 9 (`OSC 9;9;cwd`) | ConEmu/Windows Terminal cwd | T | Cheap to accept as an OSC 7 alias. |

### 6. APC

- APC string: `ESC _ ... ST`. Terminals that do not understand APC must swallow it to ST and print nothing.
- Kitty graphics protocol: `ESC _ G <key=val,...> ; <base64 payload> ESC \`. Keys: `a` action (T transmit+display, t transmit, p put, d delete, q query), `f` format (24, 32, 100 PNG), `t` transmission medium (d direct, f file, t temp file, s shared memory), `i`/`I` image id / number, `p` placement id, `m` more-chunks flag (chunks up to 4096 bytes), `o=z` zlib, `q` quiet level, `c`/`r` cell box, `x y w h` crop, `X Y` pixel offset, `z` z-index, `C` cursor movement, `U=1` unicode-placeholder mode (placeholders are U+10EEEE with combining diacritics encoding row and column, colour encodes the image id). Animation uses `a=f` (frame), `a=a` (control), `a=c` (compose). Implementers: kitty, Ghostty, WezTerm (partial), Konsole (partial), foot (no, unverified), iTerm2 (partial).
  - Security notes. File and temp-file and shared-memory transmission read local paths. Deny by default for non-local sessions (an ssh session cannot legitimately name a local path). For `t=t`, only delete a file whose name contains `tty-graphics-protocol` and which sits under a temp dir. Reject `/proc`, `/sys`, `/dev` paths. Apply per-terminal GPU/memory quotas with LRU eviction (kitty uses a 320 MB default quota, unverified). Always reply with `OK` or an error code string on query (`q=0`) so apps can detect support. Replies are APC too and must not be injected into the user's input stream when quiet.
- tmux does not forward APC; use passthrough or unicode placeholders.

## Control surface (config, actions, IPC)

Proposed KDL surface, aligned with the "one Action vocabulary" rule. Key names below are proposals, not existing config.

```
escapes {
    clipboard-write "ask"        // allow | ask | deny
    clipboard-read "deny"
    title-report "deny"          // CSI 21 t
    window-ops "safe"            // none | safe | all   (XTWINOPS)
    hyperlinks { schemes "https" "http" "mailto" "ssh" }
    notifications "ask"          // OSC 9 / 99 / 777
    graphics { kitty "allow"; sixel "allow"; iterm-inline "allow"; file-transmission "local-only"; quota-mb 320 }
    sync-output-timeout-ms 150
    answerback ""
    max-osc-bytes 4096
}
```

- Actions. A parsed sequence can raise an event the rule engine consumes (`osc-notify`, `osc-progress`, `osc-user-var name=...`, `osc-prompt-start`, `command-finished exit=N`, `osc-cwd-changed`, `focus-in`, `colour-scheme-changed`, `bell`). Rules map events to actions such as `notify`, `set-tab-badge`, `copy-last-output`, `jump-prompt`, `open-url`.
- Capability introspection. A CLI command (`apotheosis ctl caps`) and a JSON-socket method list the full sequence table with its current policy and per-sequence hit counters. This makes bugs reproducible without reading logs.
- Debug action. A "sequence inspector" pane shows the parsed event stream (not input keystrokes, in line with the invariant that human input is never logged by content). It shows PTY output only.
- Settings GUI. One page of toggles for the policy classes, with a "what does this allow" tooltip for each.

## Performance (numbers + method)

No numbers were measured in this run. Published or well-known figures, all unverified unless sourced:

- Parsing throughput is dominated by the ground-state UTF-8 path. Ghostty and Alacritty-class parsers reach several GB/s on ASCII-heavy input with SIMD scanning. Contour's `contour-benchmark` / `vtbench` and the community `vtebench` (Alacritty) are the standard harnesses, run as `vtebench` workloads (`dense_cells`, `scrolling`, `scrolling_in_region`, `unicode`, `light_cells`, `medium_cells`).
- Mode 2026 cost: one extra buffer per synchronised frame. Benefit: removes tearing in tmux, vim and neovim redraws.
- Graphics: decode kitty PNG and sixel off-thread. Upload to the GPU as a texture atlas or standalone textures, keyed by image id, with quota-based eviction.
- Method for Apotheosis: run `vtebench` and a replayed `esctest`-style corpus in CI. Add a fuzz target on the parser (`cargo-fuzz`, allowed since it is a dev tool, though it is a C++ libFuzzer wrapper at run time; if that is a problem, use `proptest`). Assert no panic, no unbounded allocation, and bounded output.

## What Apotheosis should take

1. A table-driven Williams parser with bounded buffers and colon sub-params.
2. Complete SGR including `4:x`, `58`/`59` and both `38/48` colour forms.
3. Modes 1004, 1006, 1016, 2004, 2026, 2027, 2031 and 2048, with DECRQM answers for every one.
4. The kitty keyboard protocol, with modifyOtherKeys as a fallback.
5. XTGETTCAP with an embedded capability table and XTVERSION.
6. OSC 4, 7, 8, 10 to 19, 22, 104 to 119, 133 and 9;4, plus OSC 52 under a policy.
7. The kitty graphics protocol including unicode placeholders and animation, and sixel as a compatibility feature.
8. OSC 99, 9 and 777 notifications mapped to the freedesktop notification service, with rate limits.
9. OSC 66, OSC 5522 and OSC 3008, as second-wave items once the grid supports multi-cell glyph blocks.
10. A sequence-capability table with policy classes, an inspector, and a ctl command that dumps it.
11. Mode 2026 presented against the Wayland frame callback.

## What it should avoid

- Echoing request bodies in replies (DECRQSS, XTGETTCAP). Reply only with values you generate.
- Title reporting and window-op reports by default.
- Auto-reading the clipboard (OSC 52 `?`) or auto-writing downloaded files (OSC 1337 `File=`).
- C1 8-bit control parsing under UTF-8.
- Unbounded OSC, DCS or APC buffers. Also unbounded graphics memory.
- Executing anything from a hyperlink with an unknown scheme, or from OSC 7 / 133 / 3008 metadata.
- Win32-input-mode (9001), DECUDK and font-setting OSC 50.
- Treating tmux passthrough as trusted. Anything inside it is as untrusted as the PTY.
- Silently diverging from a spec. Where the terminal-wg text and kitty or xterm disagree, record it as a decision in the ADR trail.

## Sources

Primary documentation. Fetched content was not re-verified in this run.
- xterm control sequences: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html (CSI, DCS, OSC, DECRQSS, XTGETTCAP, XTWINOPS, XTSAVE).
- VT100.net parser state diagram: https://vt100.net/emu/dec_ansi_parser
- kitty protocols: https://sw.kovidgoyal.net/kitty/protocol-extensions/ , /graphics-protocol/ , /keyboard-protocol/ , /desktop-notifications/ , /text-sizing-protocol/ , /clipboard/ , /color-stack/ (path of the last one unverified).
- terminal-wg specifications: https://gitlab.freedesktop.org/terminal-wg/specifications (synchronized output, grapheme clusters, colour-scheme notification, in-band resize, semantic prompts; the exact set of merged specs is unverified).
- Hyperlinks (OSC 8): https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda
- Contour VT extensions: https://contour-terminal.org/vt-extensions/ (2026, 2027, 2031, 2048 origins).
- Ghostty docs: https://ghostty.org/docs/vt (escape support tables).
- ConEmu progress OSC 9;4: https://conemu.github.io/en/AnsiEscapeCodes.html
- iTerm2 proprietary sequences: https://iterm2.com/documentation-escape-codes.html
- systemd context signalling (OSC 3008): https://systemd.io/OSC_CONTEXT/ (unverified path).
- tmux passthrough: tmux man page, `allow-passthrough`; tmux wiki FAQ on `DCS tmux;`.
- Sixel: https://vt100.net/docs/vt3xx-gp/chapter14.html
- Source findings, by technique only and not re-read this run: Alacritty `vte` crate (parser, params with subparams), Ghostty `src/terminal/Parser.zig` and `stream.zig` (parser and dispatch), kitty `kitty/vt-parser.c` (batched UTF-8 decode and dispatch) and `kitty/graphics.c` (quota and file transmission checks), foot `vt.c` and `csi.c`. All unverified against current heads.
