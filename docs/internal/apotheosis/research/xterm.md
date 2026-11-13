# xterm control-sequence reference, vttest and esctest2: research dossier for Apotheosis

Prior-art rule applies: this dossier describes techniques and interface facts in prose. Escape-sequence syntax, message formats and config key names are quoted as interface facts. No source is reproduced.

Material read: `ctlseqs.txt` at XTerm patch #411 (2026/08/23) (the text form of the ctlseqs.html page; the .html URL returns 403 to the fetch tool, curl with a browser UA works); `xterm.man` from xterm-snapshots master; shallow clone of ThomasDickey/xterm-snapshots (source patchlevel 412) for `charproc.c`, `screen.c`, `util.c`, `scrollback.c`, `graphics*.c`, `ptyx.h`; shallow clone of ThomasDickey/esctest2 (last commit 2026-09-13); vttest-snapshots `main.c`, `CHANGES` (latest entry 20251205), `COPYING`. The vttest manpage URL (invisible-island.net/vttest/vttest.html) returned 403 and the vttest-snapshots `vttest.man` path 404ed, so vttest behavioural claims come from `main.c` menu tables only.

## Summary

xterm's ctlseqs document is the de facto superset reference for what a VT-compatible terminal can be asked to do. It is a catalogue of about 95 ESC/C1 forms, roughly 100 CSI finals (many with intermediates, `?`, `>`, `=`, `$`, `"`, `'`, `*`, `SP`, `#`, `!` variants), a dozen DCS strings, roughly 40 OSC numbers, five mouse protocols plus three encodings, two obsolete sub-emulations (VT52 and Tektronix 4014) and the DEC graphics pair (sixel, ReGIS).

Conclusions for Apotheosis:

1. Treat ctlseqs as the baseline contract and esctest2 as the executable form of it. esctest2 has 80 test modules and 559 test methods. It is GPL-2.0, so run it as an external conformance tool and never link or vendor it. vttest (MIT-style licence, ncurses-licence text in COPYING) is the human-driven companion.
2. Implement the xterm core (cursor, erase, scroll regions, left/right margins, rectangular ops, DECRQM and DECRPM for every mode we support, DECRQSS, DA1/DA2/DA3, XTVERSION, XTGETTCAP, XTSAVE/XTRESTORE, title stack, palette stack, SGR stack, mouse 1000/1002/1003 with SGR 1006 and SGR-pixels 1016, bracketed paste, focus events, alt screen 1049) and be exhaustive about reporting: every mode must answer DECRQM truthfully.
3. xterm is not the modern frontier. The patch #411 document has no OSC 8 hyperlinks, OSC 7, OSC 133, OSC 9/777 notifications, SGR 58/59 underline colour, `4:x` underline styles, synchronized output (2026), grapheme-cluster mode (2027), colour-scheme reporting, or the kitty keyboard protocol. A grep of the document confirmed none of those strings. Apotheosis must take these from kitty, WezTerm, foot, Ghostty and iTerm2 research, not from xterm.
4. Skip as obsolete: VT52 mode, Tektronix 4014, ReGIS (decide explicitly; recommendation is no), DECUDK, printer controller and media copy, smooth scroll, DEC locator, LED control, X property setting (OSC 3), the Tek and pointer dynamic colours (OSC 13-19 except 17/19 highlight), aixterm-only oddities, urxvt mouse 1015 and UTF-8 mouse 1005.
5. The security lesson is the biggest take-away: xterm's `allow*Ops` / `disallowed*Ops` resource families are the model for a per-capability gate on "report back" and "act on the host" sequences. Apotheosis should generalise them into one KDL policy with per-feature allow, deny and ask, a runtime query (XTQALLOWED style), and strict defaults for title reporting, window ops, font ops, clipboard read, and paste control characters.

## Architecture (how it is built)

Process model: xterm is an Xt/Xaw toolkit program. One VT widget owns a pty master; a `VTparse`/`doparsing` loop in `charproc.c` pulls bytes from the pty buffer (`in_put`) and drives a table-based state machine. The tables (`ansi_table`, `csi_table`, `csi2_table`, `dec_table`, `sos_table` and friends, referenced from `charproc.c` and `parse.h`) map a byte in the current state to a `CASE_*` code and the next state, so dispatch is one table lookup per byte with no per-sequence string compare. Strings (OSC, DCS, APC, PM, SOS) switch to a string-accumulating state and dispatch on the terminator; xterm replies with the same terminator (BEL or ST) the query used.

Screen model (`ptyx.h`, `screen.c`, `scrollback.c`): the screen is an array of line pointers. Each line (`LineData`) is a header plus parallel arrays: base character data, video attributes, foreground/background colour numbers, optional character-set tracking, and per-cell combining-character arrays that are allocated only when needed (the struct ends in a variable-length tail of combining pointers, so lines without combining marks pay nothing). Two buffers exist: the normal buffer with saved lines (`saveLines`, default 1024 lines) and a fixed-size alternate buffer with no scrollback. Scrollback is a FIFO of rows allocated on demand (`scrollback.c` allocates a row per scrolled-off line sized to preserve the source line's data) with an index (`saveBuf_index`) rather than copying lines.

Scrolling and refresh: output that scrolls is accumulated into counters (scroll amount and refresh amount) and applied in one `FlushScroll` (`util.c`): jump scroll batches many line feeds into a single bitmap copy plus a refresh of only the newly exposed rows. It distinguishes full-line scrolls (which push lines into saved lines when the top margin is row 0 and the normal buffer is active) from partial (margin-restricted or left/right-margin) scrolls. `fastScroll` additionally suppresses screen refreshes when output has completely shifted the visible contents off screen (the `cat bigfile` case). This is xterm's entire answer to throughput: defer and coalesce, draw once.

Graphics (`graphics.c`, `graphics_sixel.c`, `graphics_regis.c`): sixel is parsed incrementally by a per-character state function into a `Graphic` that holds a pixel plane of register indices (not RGB), a colour-register table (compile-time ceiling 1024 registers, `MAX_COLOR_REGISTERS`), up to 16 live graphics (`MAX_GRAPHICS`), declared and actual sizes, and a "hole" sentinel value for transparent pixels. The image is clipped to `maxGraphicSize` (default 1000x1000, or "auto" derived from the emulated terminal ID). Sixel scroll mode (DECSDM reset, the default) places the image at the cursor and scrolls; DECSDM set pins it at the origin. Private mode 1070 gives each graphic private colour registers; mode 8452 leaves the cursor to the right of the image (RLogin behaviour). The aspect-ratio parameter P1 indexes a ten-entry vertical-scale table in `parse_sixel_init` (0 and 1 both mean 2:1, 2 is 4.5:1, down to 9 which is 1:1); VT240/330/340 emulation defaults to 2:1 and VT382 to 1:1.

Terminal identity and capability model: DA1, DA2, DA3 and DECRQSS answers are driven by the `decTerminalID` resource (VT100 up to VT525). The terminal emulates a family of DEC models; conformance level can be changed at runtime by DECSCL. Features like sixel (DA1 param 4) and ReGIS (param 3) are advertised only when configured.

Security architecture: every sequence that reads state back or controls the host is classified into a feature group (Color, Font, Mouse, Tcap, Title, Window, PasteControls). Each group has a master boolean `allowXOps` and a `disallowedXOps` list that names the sub-features that are blocked when the master is off. A runtime query (OSC 60/61/62) reports the state. Menus can flip groups at runtime but sequences cannot.

## Feature inventory (grouped by category)

### Sequence families (C0, C1, ESC)
- C0: NUL, ENQ (answerback), BEL, BS, HT, LF/VT/FF, CR, SO/SI (G1/G0 shift), CAN/SUB abort a sequence, ESC.
- C1 8-bit controls (IND, NEL, HTS, RI, SS2, SS3, DCS, SOS, CSI, ST, OSC, PM, APC): S7C1T and S8C1T and DECSCL choose 7-bit or 8-bit controls in replies. UTF-8 mode treats C1 as Unicode code points; vttest has an `allowC2Controls` knob for this.
- ESC forms: charset designation SCS to G0-G3 (including NRCS and DEC special graphics), locking and single shifts, DECSC/DECRC, DECALN (screen alignment fill), DECDHL/DECDWL/DECSWL (double-height and double-width lines), IND/NEL/HTS/RI, DECKPAM/DECKPNM, RIS, S7C1T/S8C1T, DECID, enter VT52 or Tektronix mode.
- ECMA-35/ISO-2022 `ESC % @` / `ESC % G` select default or UTF-8 character set.

### CSI families
- Cursor: CUU CUD CUF CUB CNL CPL CHA CUP HVP HPA HPR VPA VPR, CHT CBT, tabs TBC HTS, DECST8C, SCOSC and SCORC (only when DECLRMM is off).
- Erase and edit: ED (0-3, with 3 = erase saved lines, an xterm extension), EL, DECSED and DECSEL (selective, honour DECSCA protection), ICH DCH ECH IL DL, DECIC and DECDC (insert and delete column), REP, SU SD (and the ECMA-48 `^` variant), SL SR.
- Margins: DECSTBM, DECSLRM (with DECLRMM mode 69).
- Rectangular ops (VT400): DECCRA, DECFRA, DECERA, DECSERA, DECCARA, DECRARA, DECSACE, DECRQCRA (checksum, with XTCHECKSUM variants). These are heavily exercised by esctest2 (modules `deccra`, `decera`, `decfra`, `decsera`, `decrectops`).
- SGR: 0-9, 21-29, 30-37, 39, 40-47, 49, 90-97, 100-107, and ISO-8613-6 colour 38/48 with `:` (preferred) or `;` (legacy) separators in indexed and direct forms. There is no 58/59 underline colour and no `4:x` underline styles documented in patch #411.
- Reports: DSR 5 and 6, DECXCPR, DECRQM/DECRPM for ANSI and DEC modes (0-4 values: unknown, set, reset, permanently set, permanently reset), DA1/DA2/DA3, DECRQDE, XTVERSION (`CSI > 0 q` answered `DCS > | text ST`, our source emits `XTerm(<patch>)`), XTQMODKEYS, XTQFMTKEYS, XTREPORTCOLORS, XTTITLEPOS, XTSMGRAPHICS, DECRQSS (via DCS).
- Mode setting: SM/RM ANSI modes 2, 4, 12, 20; DECSET/DECRST/DECRQM with about 90 private modes (see Input and Mouse below); XTSAVE and XTRESTORE (`CSI ? Pm s` and `CSI ? Pm r`) save and restore individual private modes in a one-level cache.
- Stacks: XTPUSHCOLORS, XTPOPCOLORS (palette stack, depth 10), XTPUSHSGR and XTPOPSGR (`CSI # {` and `CSI # }` and aliases), XTWINOPS 22 and 23 title push and pop (indexed, depth 10).
- Cursor shape: DECSCUSR 0-7 (0 and 1 blinking block; 5 and 6 bar are xterm additions; 7 returns to resources).
- Miscellaneous: DECSTR soft reset, DECSCL conformance level (61-65), DECLL LEDs, DECSWBV/DECSMBV bell volume, DECSASD/DECSSDT status line, DECSNLS/DECSCPP/DECSLPP page size, DECRQUPSS and DECAUPSS supplemental sets, MC media copy (HTML dump `CSI 10 i`, SVG dump `CSI 11 i`).

### DCS families
- DECUDK (user-defined keys), DECAUPSS, DECRQSS (`DCS $ q Pt ST`, reply `DCS 1 $ r ... ST` or `DCS 0 $ r ST`; queried strings: SGR `m`, DECSCL `" p`, DECSCUSR `SP q`, DECSCA `" q`, DECSTBM `r`, DECSLRM `s`, DECSLPP `t`, DECSCPP `$ |`, DECSACE `* x`, DECSNLS `* |`, plus xterm's XTQFMTKEYS, XTQMODKEYS, XTSMTITLE), DECRSPS (restore presentation state from DECCIR or DECTABSR reports).
- XTGETXRES (`DCS + Q`), XTSETTCAP (`DCS + p`), XTGETTCAP (`DCS + q Pt ST`, hex-encoded names separated by `;`, replies `DCS 1 + r name=value ST` or `DCS 0 + r ST`; names cover special keys plus `Co`/`colors`, `TN`/`name`, `RGB`; an unknown name ends processing of the list).
- Sixel `DCS Pa ; Pb ; Ph q ... ST`; ReGIS `DCS Pm p ... ST`.

### OSC families
Numbers in patch #411: 0, 1, 2 (titles), 3 (X property), 4 and 104 (palette set, query with `?`, reset), 5 and 105 (special colours: bold, underline, blink, reverse, italic), 6 and 106 (enable special colours), 10-19 and 110-119 (dynamic colours: foreground, background, cursor, pointer fg and bg, Tek fg and bg, highlight bg, Tek cursor, highlight fg), 21 (DECSWT), 22 (pointer shape), 46 (log file), 50 (font, with `#` index syntax), 51 (reserved for Emacs), 52 (selection: `Pc ; Pd` where Pc is from `c p q s 0-7`, Pd is base64, `?` queries, anything else clears), 60/61/62 (XTQALLOWED, XTQDISALLOWED, XTQALLOWABLE), plus Sun/CDE `I`, `l`, `L`. Queries answer with a sequence that would set the current value, using the same terminator as the query. PM is ignored; APC and SOS are swallowed (esctest2 has `apc`, `pm`, `sos` modules testing exactly that).

### Keyboard (input)
- Legacy PC-style function keys with modifier parameter (2 to 16 encoding shift=1, alt=2, ctrl=4, meta=8 added to 1), DECCKM cursor-key application mode, keypad modes, VT220 and Sun/HP/SCO function-key modes (DECSET 1050 to 1053, 1060, 1061), backarrow (DECBKM 67), delete-is-DEL (1037).
- Meta and Alt handling: eightBitInput (set the high bit), metaSendsEscape (1036), altSendsEscape (1039), altIsNotMeta; the doc includes a full truth table.
- modifyOtherKeys: set with `CSI > 4 ; Pv m`, query with `CSI ? 4 m`, disable with `CSI > 4 n`. Level 1 encodes keys as `CSI 27 ; mod ; code ~` only when a modifier other than shift or control applies (alt-Tab is `CSI 27 ; 3 ; 9 ~`). Level 2 encodes every modified key (shift-Tab becomes `CSI 27 ; 2 ; 9 ~` instead of `CSI Z`). Level 3 also sends unmodified keys (space becomes `CSI 27 ; 1 ; 32 ~`). `formatOtherKeys=1` changes the layout to `CSI code ; mod u`, which collides with SCORC. A subparameter mask (`CSI > 4 : 1 m`) factors modifier bits out. Companion controls exist for cursor, function, keypad, modifier and special keys (resources modifyCursorKeys and the others, plus their `format*` twins). Non-character keysyms are mapped into private-use ranges (U+E001, U+F0000, U+EEEE) so that they can be carried as code points.
- Mouse (see below), bracketed paste (2004), focus events (1004), readline mouse and quoting modes (2001-2006, rarely implemented elsewhere), pointer hiding (XTSMPOINTER `CSI > Ps p`), XTSHIFTESCAPE (`CSI > Ps s`) controlling whether Shift overrides mouse reporting.

### Mouse
Protocols: X10 (9), VT200 normal (1000), highlight (1001, requires a cooperating program or xterm hangs), button-event (1002), any-event (1003), DEC locator (DECELR). Encodings: default (value+32 bytes, limit 223), UTF-8 (1005, deprecated), SGR (1006, recommended: `CSI < b ; x ; y M` or `m`, release distinguished by final byte), urxvt (1015, not recommended), SGR-pixels (1016). Wheel is buttons 4 and 5 (+64), extra buttons add 64 or 128. Alternate Scroll (1007) turns the wheel into cursor-up and cursor-down on the alternate screen. Focus in and out are `CSI I` and `CSI O`. Motion is reported only on cell change.

### Window and title operations (XTWINOPS, `CSI Ps ; Ps ; Ps t`)
Actions: 1 and 2 (de-iconify, iconify), 3 (move), 4 (resize in pixels), 5 and 6 (raise, lower), 7 (refresh), 8 (resize in characters), 9 (maximize variants), 10 (fullscreen), `>=24` (DECSLPP resize). Reports: 11 (state), 13 and 13;2 (position), 14 and 14;2 (pixel size), 15 (screen pixel size), 16 (cell size in pixels), 18 (text area in characters), 19 (screen in characters), 20 (icon label, reply `OSC L label ST`), 21 (title, reply `OSC l label ST`). Stack: 22 and 23 with sub-selector 0, 1, 2 and optional index 1-10. Title modes (`CSI > Pm t` and `CSI > Pm T`): set or query titles in hexadecimal or UTF-8 so that arbitrary bytes cannot be injected through a title round trip.

### Graphics
- Sixel: DECSDM (mode 80), private colour registers (1070), scrolling placement (8452), XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`, items: 1 colour registers, 2 sixel geometry, 3 ReGIS geometry; actions read, reset, set, read-max; status 0 success, 1 bad item, 2 bad action, 3 failure), DA1 parameter 4.
- ReGIS: DECSET 1070 shares with sixel; DA1 parameter 3; a 7754-line implementation in `graphics_regis.c`. Practically no one else implements it.

### Colour
256-colour and direct-colour SGR; palette set, query and reset through OSC 4, 104; dynamic colours OSC 10-19 and 110-119; colour stack XTPUSHCOLORS and XTPOPCOLORS; XTREPORTCOLORS; `reverseVideo` DECSCNM; colour attribute mapping for bold, underline, blink, reverse, italic (OSC 5, 6). The `directColor` resource asks the X server for exact colours rather than the nearest of 256.

### Character sets and Unicode
DEC Special Graphics, DEC Supplemental, DEC Technical, NRCS national sets, ISO Latin-1/2/5/Greek/Hebrew/Cyrillic supplements, UTF-8. Read-only mode reports for `utf8` (1020), `cjkWidth` (1021), `emojiWidth` (1022), `privateWidth` (1023). Wide characters are handled through `OPT_WIDE_CHARS` with per-line combining arrays. No grapheme-cluster mode.

### Selection, clipboard, paste
OSC 52 as above; DECSET 1040 (keep selection), 1041 (CLIPBOARD), 1044 (keepClipboard); bracketed paste 2004 with `allowPasteControls` default off: C0 controls other than formatting characters (tab, newline) are turned into spaces; the default blocked list is BS, DEL, ENQ, EOT, ESC, NUL and the stty-special characters.

### Scrollback and screen
Alternate screen via 47, 1047, 1049 (cursor save plus clear), controlled by `titeInhibit`; scroll-to-bottom on output (1010) and on key (1011); `saveLines` default 1024; ED 3 erases scrollback; reverse-wraparound 45 and extended 1045; DECCOLM with DECNCSM 95; origin mode, autowrap, DECLRMM.

### Misc and obsolete
VT52 mode (`CSI ? 2 l`), Tektronix 4014 (DECSET 38; xterm has a separate Tek widget), printer controller, screen dumps, logging (compile-time off), ENQ answerback, margin bell, LED states, smooth scroll, DECUDK, locator.

## Control surface (config, actions, IPC)

xterm's own surface is X resources (`XTerm*name`), command-line switches, menus (Ctrl+mouse popups) and translations. Relevant items to learn from:

- Resource-per-feature gating (the best idea): `allowColorOps`, `allowFontOps`, `allowMouseOps`, `allowPasteControls`, `allowTcapOps`, `allowTitleOps`, `allowWindowOps`, each with a `disallowed*Ops` sub-list of named actions (examples: `SetColor, GetColor, GetAnsiColor`; `SetFont, GetFont`; the window op names `GetWinTitle (21)`, `GetIconTitle (20)`, `GetSelection`, `SetSelection`, `SetXprop`, `GetChecksum`, `SetWinSizePixels (4)`, etc., matched case-insensitively, with wildcards, and by number for window ops). Defaults per the man page: colour, font, mouse, tcap and title ops allowed, window ops off, paste controls off.
- Runtime state queries: OSC 60, 61, 62 (XTQALLOWED family), DECRQM for modes, XTQMODKEYS and XTQFMTKEYS for keyboard formatting, DECRQSS for terminal state, XTGETXRES for resource values. Principle: every piece of state a program may want to restore must be queryable in the same syntax that sets it.
- State save and restore primitives: XTSAVE/XTRESTORE (modes), XTPUSH/XTPOP (colours, SGR, titles). Cheap and useful for programs that need to leave the terminal as they found it.
- Actions in translations (`insert-seven-bit`, `scroll-back`, `string`, `spawn-new-terminal`, `set-allow-window-ops`, etc.) are xterm's action vocabulary; it is bound to keys and mouse through a `translations` resource. This is a precedent for one shared Action vocabulary, but xterm has no IPC socket; remote control is by escape sequence only.
- Conformance knobs: `decTerminalID`, `decGraphicsID`, `maxGraphicSize`, `numColorRegisters` family, `sixelScrolling`, `titeInhibit`, `utf8Title`, `jumpScroll`, `fastScroll`, `saveLines`, `altSendsEscape`, `metaSendsEscape`, `eightBitInput`, `modifyOtherKeys`, `formatOtherKeys`.

## Performance (numbers + method)

xterm publishes no benchmark numbers. What the documentation and source give are mechanisms and limits:

- Jump scroll: default on; `-j` text in xterm.man says that moving multiple lines at a time "makes xterm much faster when scanning through large amounts of text". Method: coalesce scroll counters, one copy plus one refresh (`FlushScroll`, `util.c`).
- Fast scroll: defers or skips refreshes when output has fully scrolled visible content away (man page describes `cat` of a large file as the case).
- 50 ms `awaitInput` timeout exists only to support the Xaw3d arrow scrollbar.
- Graphics limits: `maxGraphicSize` default 1000x1000, `MAX_COLOR_REGISTERS` 1024, `MAX_GRAPHICS` 16 (source constants).
- Scrollback default 1024 lines.
- No GPU path: xterm draws through core X, XRender or Xft; throughput numbers elsewhere (for example vtebench comparisons) are not from xterm's maintainers and are unverified here.

Take-away for Apotheosis: its performance budget must come from our own harness. The useful xterm idea is structural: parse into a state machine with table dispatch, apply scrolls lazily as a counter, and draw once per frame, not per sequence.

## What Apotheosis should take

1. The sequence catalogue as the spec skeleton; mark each sequence supported, stubbed (parsed, ignored, still swallowed correctly) or rejected.
2. Truthful DECRQM and DECRPM for every mode, including the permanently-set and permanently-reset values; programs rely on this to probe.
3. XTVERSION, DA1, DA2, DA3 with an identity string that names Apotheosis and its version.
4. XTGETTCAP with a built-in capability table (names, `TN`, `Co`, `RGB`) and the hex framing, plus a shipped terminfo entry.
5. modifyOtherKeys levels 1 and 2 and the `CSI > 4 ; Pv m` family, as a compatibility layer next to the kitty keyboard protocol; query with XTQMODKEYS.
6. The allow/disallow security gating, generalised and made available per profile in KDL, with a query escape for introspection.
7. Title handling: title stack (22 and 23), title-mode hex and UTF-8 query to prevent injection through title round trip, and sanitising of control characters.
8. Mouse: 1000, 1002, 1003 with 1006 and 1016; 1004 focus; 1007 alternate scroll; the shift-override rule (XTSHIFTESCAPE).
9. Sixel with XTSMGRAPHICS, DECSDM, private registers (1070), 8452, DA1 param 4; cap sizes and registers explicitly.
10. Rectangular ops (DECCRA and friends) and DECRQCRA checksums; cheap to implement on a cell grid and exercised by esctest2.
11. XTSAVE/XTRESTORE, palette and SGR stacks.
12. Left and right margins (DECLRMM) since modern TUIs use them.
13. Conformance testing as CI: run esctest2 against Apotheosis in a headless harness (it needs a pty, a window id for window-op tests, and a way to report screen contents; we can feed it a test-only introspection socket instead of xwininfo), and keep a recorded known-bugs list per release; vttest as a manual or scripted smoke test.
14. Bracketed paste with default control-character neutralisation (the allowPasteControls default list) and an optional extra "strip ESC and C1" rule.
15. Mode reporting for width-related read-only modes (1020-1023 equivalents) if we add emoji and CJK width options.

## What it should avoid

- VT52 mode, Tektronix 4014, ReGIS, DECUDK, printer and media copy, DEC locator, LED control, smooth scroll, OSC 3, pointer-colour OSCs, logging OSC 46. Each adds parser state and security surface for near-zero users. Decide ReGIS explicitly; recommendation is no.
- urxvt mouse 1015 and UTF-8 mouse 1005 (xterm's own doc discourages them; 1015 collides with DL and window ops).
- Highlight mouse tracking 1001: the spec warns that xterm hangs waiting for the program.
- Letting any sequence resize, move, raise, lower or fullscreen the window by default, or report title, icon label, selection or checksum back by default. These are the classic injection vectors: a reported title plus an attacker-controlled title becomes input to the shell. Apotheosis should default these to deny or ask, and route allowed window actions through the compositor integration with rate limits.
- `formatOtherKeys=1`'s `CSI code ; mod u` overlap with SCORC (documented as a drawback by xterm).
- Un-gated OSC 52 read (query returns the clipboard to the application).
- Compile-time feature switches as a configuration model (xterm requires rebuilds for sixel, 256-colour, wide chars); ours should be runtime KDL.
- Depending on esctest2's GPL code in our build or vendoring any of it.
- Copying xterm's non-standard overloads without checking others' behaviour (for example mode 2004 and 47 are fine, but DECSET 1010 and 1011 carry rxvt semantics).

## Sources

Documentation (primary):
- https://invisible-island.net/xterm/ctlseqs/ctlseqs.html (text form fetched as ctlseqs.txt; patch #411, 2026/08/23); sections: Definitions, C1 controls, Device-Control functions, CSI functions, OSC, Alt and Meta Keys (modifyOtherKeys), Mouse Tracking, Sixel Graphics, ReGIS Graphics, Title Modes, Alternate Screen Buffers, Bracketed Paste Mode, VT52 Mode, Tektronix 4014 Mode.
- https://raw.githubusercontent.com/ThomasDickey/xterm-snapshots/master/ctlseqs.txt (same text) and `xterm.man` (resources allow*Ops, disallowed*Ops, jumpScroll, fastScroll, saveLines, maxGraphicSize, sixelScrolling, allowPasteControls).
- https://github.com/ThomasDickey/esctest2 (README.txt; `esctest/escargs.py` flags `--include`, `--expected-terminal`, `--options`, `--max-vt-level`, `--xterm-checksum`, `--xterm-reverse-wrap`, `--window-id`, `--action list-known-bugs`, `--timeout`, `--force`, `--stop-on-failure`, `--test-case-dir`; `escutil.py` functions `knownBug` and `optionRequired`; 80 test modules, 559 test methods; LICENSE is GPL-2.0).
- https://github.com/ThomasDickey/vttest-snapshots (`main.c` menu tables: cursor movements, screen features, character sets, double-sized characters, keyboard, terminal reports, VT52 mode, VT102 insert and delete, known bugs, reset and self-test, non-VT100 xterm tests; `vttest.h` version string "1.7b 1985-04-19" is the original, `CHANGES` has entries to 20251205; COPYING is ncurses MIT-style).

Source findings (upstream path + function; read only, not copied):
- xterm-snapshots `charproc.c`: `doparsing`, `VTparse`, `in_put`, `VTRun`; state tables `ansi_table`, `csi_table`, `csi2_table`, `dec_table`, `sos_table`; `CASE_REPORT_VERSION` (XTVERSION).
- `util.c`: `FlushScroll`, `xtermScroll`, `xtermScrollLR` (jump-scroll coalescing).
- `screen.c`: `ScrnWriteText`, `ScrnRefresh`; `scrollback.c`: scrollback FIFO row allocation; `ptyx.h`: `LineData`, `ScrnBuf`.
- `graphics.h`: `MAX_COLOR_REGISTERS`, `MAX_GRAPHICS`, `COLOR_HOLE`; `graphics_sixel.c`: `parse_sixel_init`, `parse_sixel_char`, `parse_sixel_incremental_display`, `parse_sixel_finished`.
- `patchlevel.h`: XTERM_PATCH 412 (the shipped ctlseqs text says #411).

Unverified: the claim that "most modern terminals implement X" for each sequence is from general knowledge of kitty, WezTerm, foot, Ghostty, Alacritty, iTerm2 and VTE and was not re-checked against their source in this pass; use the sibling dossiers for those. vttest and esctest2 results against any particular terminal were not run (no builds or execution permitted).
