# Apotheosis research: keyboard input

Method: primary docs fetched 2026-11-12 (kitty keyboard protocol spec, crates.io API, docs.rs for kbvm). Items from memory of xterm/foot/ghostty/wezterm behaviour are marked "unverified". No upstream source was cloned for this dossier; source pointers are paths to confirm in implementation.

## Summary

- Terminal keyboard input is three layers: (1) Wayland delivery (wl_keyboard keymap fd + evdev scancodes, xkb state, repeat info), (2) text composition (xkb dead keys, Compose, text-input-v3 IME), (3) VT encoding (legacy, xterm modifyOtherKeys, fixterms CSI u, kitty protocol).
- The kitty keyboard protocol (5 flags, per-screen stacks, CSI u) is the only encoding that is complete and unambiguous; adopt it in full, plus modifyOtherKeys 1/2 and legacy for compatibility.
- Encoding must be a pure function: (key event, xkb-derived data, mode flags, DECCKM/DECKPAM/etc.) -> bytes. Keep it testable with golden tables, allocation-free (fixed stack buffer).
- kbvm (MIT OR Apache-2.0, v0.2.0, 9 releases since Jan 2025, 24k downloads) is a licence-clean pure-Rust XKB implementation including Compose and a lookup-table client; young, single maintainer. Recommend it behind a trait with a differential test against libxkbcommon dev-only (not in the build).
- IME via text-input-v3 needs preedit drawn inline with cursor-rect feedback; this is where most terminals are weak.

## Architecture (how it is built)

Reference pipeline (foot/wezterm/ghostty pattern):
1. wl_keyboard.keymap gives an XKB keymap fd; compile once per keymap event; build xkb state. wl_keyboard.key gives evdev code (+8 for xkb keycode); modifiers event updates state; repeat_info gives rate/delay and the client must run key repeat itself (timerfd/calloop timer), cancelling on release or focus loss.
2. Per key event derive: consumed-modifier-aware keysym, UTF-8 text, the unshifted codepoint (layout level 0 for the key), the shifted codepoint, and the base-layout (US/group 0) codepoint. These feed kitty's `unicode-key-code`, shifted-key and base-layout-key fields. Kitty requires key code to be the unshifted lowercase form, so the terminal must query level-0 symbol of the key, not the current-level symbol.
3. Modifier mask: shift, alt, ctrl, super, hyper, meta, caps_lock, num_lock. Resolve via xkb virtual-modifier names (Mod1 = Alt, Mod4 = Super, Hyper/Meta from keymap) rather than hard-coded indices. Compute "consumed" mods (xkb_state_mod_index_is_consumed equivalent) so Shift+; on a layout that yields ':' is not reported as shift+;.
4. Encoder selected by per-screen mode state: kitty flag stack (main and alt screen separate), modifyOtherKeys level, DECCKM, DECKPAM, DECBKM, DECNKM, alt-sends-escape (metaSendsEscape), backarrow key mode. Kitty flags take precedence when non-zero.
5. Text path: if no IME active and encoding yields text, send UTF-8. Compose and dead keys are resolved by xkb state (keysym stream) before the encoder, so the encoder sees the composed codepoint. Under kitty flag 8 the encoder sends escape codes with associated text (flag 16) instead.
6. IME path (zwp_text_input_v3): enable/disable on focus and when the app is at a prompt-ish state; send set_cursor_rectangle (cell rect of cursor in surface coords) and set_surrounding_text/content_type if desired; commit; receive preedit_string/commit_string/delete_surrounding_text/done. Apply state atomically on `done` using the serial rule. Keys are consumed by the IME when the compositor routes them via input-method; with text-input-v3 plus input-method-v2 the client does not see those keys, so the terminal need not filter. Render preedit as an overlay at the cursor (underline, cursor span from preedit cursor_begin/end), never written to the grid or PTY until commit.
7. Compose: xkb Compose tables (XCompose, locale-driven) either via the xkb layer (dead keys through keymap) or a client-side Compose state machine, which kbvm provides.

## Feature inventory

### Legacy encoding
- C0 control mapping for Ctrl+letter, Ctrl+[ \ ] ^ _ ? @ and space (NUL). Alt as ESC prefix (metaSendsEscape) vs high-bit 8-bit meta (rarely wanted).
- Cursor keys: SS3 vs CSI forms under DECCKM; with modifiers `CSI 1 ; mod A`. Function keys F1-F4 SS3 P-S vs CSI 11~..14~; F5-F12 `CSI 15~..24~`; keypad application mode SS3 forms under DECKPAM; Home/End `CSI H/F` vs `CSI 1~/4~`. 
- Modifier parameter = 1 + (shift 1, alt 2, ctrl 4, meta 8) in xterm; kitty extends to super 8, hyper 16, meta 32, caps 64, num 128.
- Backspace 0x7f vs 0x08 (DECBKM); Tab and Shift+Tab `CSI Z`; Enter 0x0d (LNM affects newline).
- Ambiguities legacy cannot express: Ctrl+I vs Tab, Ctrl+M vs Enter, Ctrl+[ vs Esc, Ctrl+Shift+letter, Super, release, Ctrl+digit.

### xterm modifyOtherKeys / formatOtherKeys
- `CSI > 4 ; Pv m` set (0 off, 1 level 1, 2 level 2), `CSI > 4 m` reset, query `CSI ? 4 m` (xterm DECRQM-like; unverified exact reply form). 
- Level 1: only keys that would otherwise be unrepresentable are encoded; level 2: nearly all modified keys encoded. formatOtherKeys 0: `CSI 27 ; mod ; code ~`; formatOtherKeys 1: `CSI code ; mod u` (fixterms form). Reference: https://invisible-island.net/xterm/modified-keys.html (fetch returned 403 to automation; content from memory, unverified in detail).
- Neovim, vim, tmux, emacs probe or enable this; WezTerm precedence: win32-input-mode > csi-u > modifyOtherKeys (https://wezterm.org/config/key-encoding.html).

### fixterms (CSI u)
- Paul Evans' "fixterms" proposal: `CSI code ; mod u`, always-on, not negotiable. Backward-incompatible for apps unaware of it; WezTerm docs explicitly discourage enabling it globally. Subsumed by kitty protocol flag 1. Support only as a per-profile opt-in or skip.

### Kitty keyboard protocol (spec: https://sw.kovidgoyal.net/kitty/keyboard-protocol/, verified)
- Flags: 1 disambiguate, 2 report event types, 4 report alternate keys, 8 report all keys as escape codes, 16 report associated text.
- Control: set `CSI = flags ; mode u` (mode 1 set-and-reset-others default, 2 set-listed-only, 3 reset-listed); push `CSI > flags u`; pop `CSI < n u`; query `CSI ? u` -> `CSI ? flags u`. Separate stacks for main and alt screen; pop on empty resets all; push on full evicts oldest (spec says bounded stack; implement limit 16, unverified number). Detection: send query then DA1; DA1 without kitty reply means unsupported.
- Encoding: `CSI unicode-key-code[:shifted[:base-layout]] ; modifiers[:event-type] ; text-codepoints u`. Modifier value = 1 + bits. Event types 1 press, 2 repeat, 3 release. Key code is unshifted lowercase.
- Flag 1: Esc, alt+key, ctrl+key, ctrl+alt, shift+alt use CSI u; Enter/Tab/Backspace stay legacy; keypad keys get distinct codes; lock modifiers not reported for text keys.
- Flag 2: repeat/release events; Enter/Tab/Backspace have no release unless flag 8.
- Flag 4: shifted and base-layout alt keys (only for keys already encoded as escapes). Lets apps match shortcuts across layouts (Ctrl+C on Cyrillic).
- Flag 8: all keys including text, modifiers-only events, Enter/Tab/Backspace as CSI u; text-producing keys send no text.
- Flag 16: associated text as code points in third field; control codes forbidden; only meaningful with 8.
- Functional keys: legacy forms kept (`CSI 3 ~`, `CSI 1 ; m A`), plus private-use range 57344-63743 for CAPS_LOCK 57358, SCROLL_LOCK 57359, NUM_LOCK 57360, PRINT_SCREEN 57361, PAUSE 57362, MENU 57363, F13-F35 57376-57398, KP_0..KP_EQUAL 57399-57415, KP_BEGIN, MEDIA_* 57428-57440, LEFT_SHIFT..RIGHT_META 57441-57452, ISO_LEVEL3_SHIFT 57453, ISO_LEVEL5_SHIFT 57454.
- Legacy text-key rules when flag 1 only: alt -> ESC prefix; ctrl -> legacy table; shift -> shifted char; ctrl+space NUL; everything else CSI u.
- Adopters (unverified list): kitty, foot, WezTerm (enable_kitty_keyboard, default false), Ghostty, Alacritty, iTerm2, Rio; clients: Neovim, helix, crossterm, fish, Emacs (kkp package).

### Wayland / xkb specifics
- Keymap via wl_keyboard fd (mmap, compile from string). Modifiers event: depressed/latched/locked/group. Repeat is client-side. Handle focus-leave: release all pressed keys (send release events under kitty flag 2) and cancel repeat.
- Keysym-to-key-name table for bindings: use xkb keysym names (project convention: Shift+` is `asciitilde`). For bindings, match on both keysym and raw keycode (physical) with explicit choice, and consider layout-independent matching via level-0 / base-layout symbol.
- Layout groups: track effective group; base layout key for kitty flag 4 comes from group 0 level 0.
- Caps/Num lock reporting: kitty bits 64/128 only reported for non-text keys unless flag 8.
- Compositor shortcuts inhibitor (keyboard-shortcuts-inhibit-unstable-v1) for passing through system shortcuts when focus is a terminal running a full-screen app. Optional on Abyss.

### IME (text-input-v3)
- Events: enter/leave, preedit_string(text, cursor_begin, cursor_end), commit_string, delete_surrounding_text, done(serial). Requests: enable, disable, set_surrounding_text, set_text_change_cause, set_content_type, set_cursor_rectangle, commit. Reference: https://wayland.app/protocols/text-input-unstable-v3.
- Terminal-specific challenges: no real editable buffer, so surrounding text is usually empty; cursor rectangle must follow the terminal cursor (and prompt position under shell integration OSC 133 for better candidate-window placement); preedit must not be committed to the PTY until commit; clear preedit on focus loss or mode change.
- Preedit rendering: draw over cells at cursor with underline style, distinct highlight for the active segment (cursor_begin..cursor_end), wide-character aware (East Asian width), wrap handling at right margin, hide when scrolled into scrollback.
- Interaction with kitty flag 8/16: IME commits arrive as text events; with associated-text, send commit as `CSI 0 ; ; text u`-style pure text event (key code 0 per spec) — apps that opt in get proper text; unverified whether other terminals do this.

### Dead keys and Compose
- Dead keys handled by xkb state (keysym dead_acute etc., resolved by compose/xkb). The terminal must never emit a dead keysym; wait for the composed result; show no preedit (some terminals show the pending dead key as preedit; optional nicety).
- XCompose sequences from user file (~/.XCompose), locale tables from /usr/share/X11/locale. kbvm offers ComposeTables from XCompose files.
- Ctrl+Shift+U unicode input is an IME (ibus/fcitx) feature, not xkb; do not reimplement, but a built-in "unicode input" Action could be offered (optional).

### Binding layer
- Keybind matching must happen before encoding: bindings consume the event; the unconsumed events go to encoder. Release events of consumed presses must also be swallowed.
- Pass-through binding (`Action::SendKeys`/send text/escape), chords/key tables (modal key tables like WezTerm key_tables, kitty `map a>b`), leader-key.
- Per-app mode awareness: bindings conditional on alt screen, kitty flags active, shell-integration state.

## Control surface (config, actions, IPC) - proposed for Apotheosis

- KDL: `keyboard { kitty-protocol "on|off|query-only"; modify-other-keys "off|1|2|app"; alt-sends-escape true; backspace "del|bs"; compose "xkb|builtin|off"; ime true; repeat { rate; delay } override; layout-independent-binds true }`.
- Per-profile and per-rule overrides (rules: event -> actions) so e.g. a profile for legacy tools disables kitty protocol.
- Actions: `send-text`, `send-keys` (symbolic, goes through encoder), `send-bytes` (raw escape), `toggle-kitty-protocol`, `key-table push/pop`, `show-key-debug`.
- Keyboard debug overlay ("show keys"): displays keysym, keycode, modifiers, consumed mods, and exact bytes sent; like kitty `kitten show_key` and foot's `showkey`-style debugging (unverified).
- IPC/CLI: `apo keys query` (current flags stack, modifyOtherKeys level per pane), `apo send-keys --pane` (injected through the encoder or raw), `apo events keys` (subscribe; must never log content by default per the EclipseOS no-keystroke-logging invariant; opt-in, per-pane, and flagged as sensitive, and password-detection via no-echo state should suppress).
- Security: key injection via control socket requires capability; OSC/CSI queries are read-only; pasted text goes via bracketed paste (separate topic).

## Performance (numbers + method)

- No authoritative published latency numbers verified in this pass. Known third-party measurement sources (unverified here): Dan Luu "Terminal latency" (2017), typometer, Ghostty/foot/kitty author claims. Do not quote numbers until reproduced.
- Budget guidance for Apotheosis (our own targets, not published figures): key event -> bytes written to PTY < 100 microseconds CPU in the encoder, zero heap allocation, at most one write syscall per event (batch repeat); key press to pixel on screen is dominated by PTY echo + render frame (see rendering topic).
- Method to measure: record wl_keyboard timestamps vs write(2) timestamp using tracing spans; golden-table tests for encoder; fuzz with random modifier/flag/key combinations.
- kbvm performance: docs.rs page makes no claims (checked); benchmark keymap compile time and key lookup vs libxkbcommon before committing; keymap compile happens once per keymap/layout change, not per key.

## What Apotheosis should take

1. Full kitty keyboard protocol, all five flags, per-screen stacks, query, with correct unshifted/shifted/base-layout derivation.
2. modifyOtherKeys 1 and 2 plus formatOtherKeys, for vim/tmux/emacs compatibility.
3. Legacy encoding done exactly per xterm termcap tables with DECCKM/DECKPAM/DECBKM.
4. Key repeat implemented client-side with compositor repeat_info, plus focus-loss release synthesis.
5. text-input-v3 with real inline preedit rendering and accurate cursor-rect, wide-char aware.
6. kbvm as the xkb layer behind a trait, with libxkbcommon-differential tests offline.
7. Single Action vocabulary for bindings, with layout-independent matching option and key tables.
8. Key debug overlay and `query` surface, shared with the settings GUI (live key tester).
9. Consumed-modifier logic and caps/num lock handling per kitty.
10. Optional shortcuts-inhibit on Abyss for full-screen apps.

## What it should avoid

- Enabling fixterms CSI u unconditionally (breaks unaware apps).
- Win32 input mode (Windows-only).
- Hard-coded keycode tables bypassing xkb (breaks non-US layouts).
- Logging key content anywhere (EclipseOS invariant); debug overlay is display-only and not persisted.
- Writing preedit into the grid.
- Relying on libxkbcommon C library (our rule: only libwayland, libxkbcommon allowed as system lib, but a pure-Rust path avoids C bindings crates; note libxkbcommon IS in the allowed list, so it is a legitimate fallback if kbvm proves insufficient; the xkbcommon crate is MIT and links the C lib).
- Embedding a scripting layer for key remaps; use Actions and rules.
- Implementing own input method framework (ibus/fcitx are the compositor/system side).

## Sources

- Kitty keyboard protocol: https://sw.kovidgoyal.net/kitty/keyboard-protocol/ (fetched)
- xterm modified keys: https://invisible-island.net/xterm/modified-keys.html (403 on fetch, unverified details)
- WezTerm key encoding: https://wezterm.org/config/key-encoding.html (fetched via raw docs)
- kbvm: https://crates.io/crates/kbvm (API fetched: MIT OR Apache-2.0, 0.2.0, 9 versions, created 2025-01-21, updated 2026-09-24, 24,262 downloads), https://docs.rs/kbvm/latest/kbvm/ (modules: xkb, xkb::compose, lookup, state_machine, evdev, keysym), repo https://github.com/mahkoh/kbvm
- text-input-v3: https://wayland.app/protocols/text-input-unstable-v3 (from memory, unverified)
- fixterms: http://www.leonerd.org.uk/hacks/fixterms/ (unverified)
- Source paths to study when implementing (unverified, not read): foot `input.c` and `kitty-keymap.h`/`key-binding.c`; kitty `kitty/key_encoding.py`, `kitty/keys.c`; ghostty `src/input/key_encode.zig`, `src/input/KeymapDarwin`; wezterm `termwiz/src/input.rs`, `wezterm-input-types`; alacritty `alacritty/src/input/keyboard.rs`.
