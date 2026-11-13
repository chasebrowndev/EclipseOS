# Gap check: kbvm 0.2 and accesskit_unix 0.24 parity

Researched 2026-11-12. Source read from shallow clones of kbvm (master c590a12, "0.2.0", 2026-09-24) and AccessKit (master dea4384, 2026-11-09) under `/home/risk/.cache/apotheosis-prior-art/`. Crate metadata is from the crates.io API. libxkbcommon 1.13.2 and xkeyboard-config 2.49 on this host were used to size the test corpus.

## Summary

Verdicts, shortest first:

- **kbvm is usable as the primary input backend. It is not yet proven at parity.** A client needs a keymap parser, a stateless lookup table, consumed modifiers, key-repeat flags and Compose. All five exist and are documented. The licence tree is clean (MIT OR Apache-2.0 throughout). It has no `unsafe` in library code outside one env-var shim, and no C. The risks are maturity and test depth, not missing API.
  - It is a one-author crate (mahkoh, who also writes the Jay compositor).
  - It was first published in January 2025. Version 0.2.0 shipped on 2026-09-24. There are nine releases, about 24k downloads in total and about 5.8k recently.
  - MSRV is 1.95, edition 2024. The repo toolchain pins 1.97, so this is fine.
- **kbvm already differential-tests itself against libxkbcommon, but only partly.**
  - Keysym tables and name/char conversion are tested against a static libxkbcommon built from a git submodule with meson. That build is C, so Apotheosis must not copy it.
  - The compositor-side state machine is tested against `xkb_state_update_key`.
  - The client-side `LookupTable` and Compose have only golden-file or unit tests, with no oracle. The Compose corpus is five cases. This is the real gap, and it matches INPUT-07.
- **One semantic difference needs a decision.** kbvm computes "consumed" modifiers from the active mask, as `active & type_mask & !preserve`. libxkbcommon's default XKB mode defines consumed as "may affect translation", whether or not the modifier is active. The two give the same answer for "remaining active modifiers" in the matched-entry case. They may diverge in the no-match and preserve cases. Only the differential harness can settle this. The kitty keyboard protocol does not need consumed modifiers directly, but our shortcut matching will.
- **The kitty keyboard protocol inputs are all derivable from kbvm without libxkbcommon.**
  - The unshifted codepoint comes from a lookup with an empty modifier mask.
  - The shifted key comes from a lookup with Shift only.
  - The base-layout key should come from a static evdev-to-US-QWERTY table. That is cheap and has no dependency on the active layout.
  - Modifier bits for Super, Hyper, Meta, Caps and Num must be resolved by modifier name through the keymap, because they are virtual modifiers.
- **accesskit_unix can express a terminal.**
  - It has `Role::Terminal`, documented as "a multi-line document ... VT-100-style terminal". The AT-SPI role mapping is `Terminal`.
  - It has `Role::TextRun` with per-character byte lengths, positions, widths and word starts.
  - The AT-SPI adapter implements character extents, offset at point, range extents, selections, text attributes and caret-moved, text-inserted and text-removed events.
- **Licence and runtime are fine.** About 60 crates were checked. Every one is MIT, Apache-2.0, dual MIT/Apache, BSD-2, or MIT OR Unlicense. No GPL.
- **An async runtime is not required.** The default feature `async-io` runs a private thread with `async-executor`. The `tokio` feature exists for apps that already use tokio. Apotheosis (calloop plus wgpu) should take the default.
- **Lazy activation is real and is a good default.** The adapter thread watches `org.a11y.Status.IsEnabled` on the AT-SPI bus. Until it is true, no tree is built.
  - Apotheosis should use the "return None, then `update_if_active`" pattern so no tree is built while inactive.
  - There is a known upstream wart. `IsEnabled` is sticky: once Orca has ever started, it stays true across reboots (AccessKit #713, at-spi2-core #224, per DataTriny's issue text). The cost after that is only the tree-build cost, so we must make that cost small.
- **Orca testing is thin and must be treated as "unproven".**
  - AccessKit documents no tested Orca or AT-SPI versions.
  - Public issue evidence shows Orca 48 (issue #854, accesskit_atspi_common 0.19.1) and Orca 49.5 and 51.alpha on GNOME 49 Wayland (issue #713).
  - There is no public report of a terminal built on AccessKit text nodes tested with Orca.
  - Apotheosis will be the first, so budget for Orca testing and upstream fixes.
- **Wayland limit.** AT-SPI "screen" coordinates need the window position, and a Wayland client does not have it. `Adapter::set_root_window_bounds` is documented as only meaningful on X11. Window-relative coordinates work. Magnifier and review-cursor accuracy therefore needs a compositor-supplied position. Abyss can provide this and other compositors cannot.
- **Newton is on the horizon.** GNOME's Wayland-native accessibility project (Newton) is a push-based replacement for AT-SPI. It is prototype-stage, and AccessKit is its reference client. Design the accessibility layer so the tree builder is separate from the AT-SPI transport.

## Architecture (how it is built)

### kbvm

- **Layout.** The workspace has the `kbvm` library, `kbvm-proc` (an internal proc-macro using `syn` 3), `kbvm-cli` (a `kbvm` binary with `compile-xkb` and a compose tester), `kbvm-benches` (criterion), and four integration-test crates: `compile-tests`, `compose-tests`, `type-tests`, `x11-tests`.
- **Dependencies.** The library has 13 required dependencies. All are small pure-Rust crates: `thiserror`, `bstr`, `arrayvec`, `linearize`, `hashbrown`, `isnt`, `debug-fn`, `smallvec`, `indexmap`, `cfg-if`, `secure-execution`, `unicode-width` and `kbvm-proc`. A build-dependency on `pkg-config` only reads the xkeyboard-config path.
- **Features.** The default feature is `log`. Optional features are `registry` (serde, quick-xml: RMLVO registry), `compose` and `x11` (x11rb).
- **Pipeline.** Text or RMLVO goes through the lexer, parser, kccgst resolver and includer (include search path built from `XKB_CONFIG_EXTRA_PATH`, `XKB_CONFIG_ROOT` and a compiled-in default) into a `Keymap`.
  - `Keymap` is introspectable: keys, groups, levels, symbols, key types with `mask`/`preserved`/`consumed`, virtual modifiers, indicators.
  - `Keymap::format` writes it back as XKB text. A compositor sends that text through `wl_keyboard.keymap`.
  - `Keymap::to_builder` then gives either `build_lookup_table` (client) or `build_state_machine` (compositor).
- **`LookupTable`.**
  - It is a stateless function from (group, effective modifier mask, keycode) to a `Lookup`.
  - The `Lookup` iterates keysyms. It exposes `repeats()` and `remaining_mods()`.
  - It also applies optional Caps transform, Ctrl transform and Ctrl fallback. The fallback is the one that makes non-Latin layouts usable for shortcuts.
  - Each `KeysymProps` records whether a transform changed the keysym or character.
  - Per key, group selection honours the key's redirect setting for out-of-range groups.
  - The type mapping step masks the active modifiers to the type's mask and looks for an exact case. On a hit, consumed is `mods & case.consumed`, where the case holds `!preserve`. On a miss, level 0 is used and consumed is the masked modifiers (`group_type.rs`, `GroupType::map`).
- **`Components`.** A client updates it from `wl_keyboard.modifiers` (effective mods and group are the OR and sum of depressed, latched and locked). A compositor applies `state_machine::Event`s to it.
- **`StateMachine`.** Compositor-side, event-producing (`handle_key`), with 32-bit modifier masks (not truncated to 8 bits) and up to 32 groups per key.
- **Compose** (`xkb/compose.rs`, `compose/{lexer,parser,table}.rs`).
  - **Table.** The table is a flat array of nodes sorted by keysym inside each child range, searched by binary search. A node is either intermediate (a child range) or a leaf (an index into a payload table). The payload is an optional string plus an optional keysym.
  - **Search.** The file search order is `$XCOMPOSEFILE`, then `$XDG_CONFIG_HOME/XCompose` (or `$HOME/.config/XCompose`), then `$HOME/.XCompose`, then the locale file. The locale file is found through `<xlocaledir>/locale.alias` and `compose.dir`. A "C" locale is mapped to `en_US.UTF-8`. This matches the libxkbcommon documentation, which calls the XDG path "a libxkbcommon extension".
  - **Includes.** `include` supports `%L`, `%H`, `%S` and `%%`, with limits (max includes 1024, depth 128, statements 500,000, rules 100,000), recursion detection, and a `secure_execution` note for set-uid use.
  - **Parity gap.** The parser accepts `!` and `~` modifier syntax but ignores it (documented under "XCompose: Modifier requirements are ignored").
  - **Feed semantics.** `feed(state, keysym)` returns `None` for modifier keysyms and for non-starting keys in the initial state, and otherwise `Pending`, `Aborted` (state reset, keysym not re-fed) or `Composed {string, keysym}`. This matches the four libxkbcommon statuses (nothing, composing, composed, cancelled) apart from naming.
  - **Iteration.** The table can be iterated and formatted back to XCompose text, which is what makes a set-equality differential check possible.
- **Tests that exist.** `compile-tests` has about 55 golden-file keymap cases. `compose-tests` has 5. Both use `expected.txt` written on first run (`WRITE_MISSING`), so they are regression tests, not oracle tests. `kbvm/src/state_machine/tests.rs` and `keysym/tests.rs` link libxkbcommon through `libxkbcommon-test-linker`. The linker crate has a hand-written `extern "C"` block for about 15 libxkbcommon functions.
- **Maintenance signals.**
  - Single author; the clone is shallow, so I could not count commits.
  - Release cadence is 0.1.5 (Oct 2025), 0.1.6 (Mar 2026), 0.1.7 (Jul 2026), 0.2.0 (Sep 2026).
  - The project has a `cooldown.toml`, an MSRV workflow and a features workflow.
  - Bus factor is one. Mitigation is the thin API surface we use and the XkbBackend trait with the libxkbcommon fallback already planned in INPUT-07.

### accesskit_unix

- **Crates.** `accesskit` (schema: `Node`, `Role`, `TreeUpdate`), `accesskit_consumer` (tree model, text ranges, text diffing), `accesskit_atspi_common` (translation to AT-SPI types and events, no transport) and `accesskit_unix` (transport via zbus). Versions as of 2026-10-25: accesskit 0.25.2, accesskit_unix 0.24.1, atspi_common 0.21.1, consumer 0.39.2. MSRV is 1.87. The crates release in lockstep, roughly monthly.
- **Threading.**
  - The first `Adapter::new` spawns one thread, which runs `async_executor` on `futures-lite::block_on`.
  - The thread opens a zbus session-bus connection with `internal_executor(false)` and ticks it itself, so zbus does not spawn its own thread.
  - With `tokio`, the same loop uses tokio tasks and needs a runtime.
  - Messages from `Adapter` to the thread go over an unbounded channel. A global `OnceLock` holds the app context and the sender.
- **Activation.**
  - The thread creates a `StatusProxy` and listens for `IsEnabled` changes. The zbus property stream yields the current value first (documented in zbus 5.19 `proxy/mod.rs`), so an AT that started earlier is seen immediately.
  - When it becomes true, the thread resolves the AT-SPI bus address (`AT_SPI_BUS_ADDRESS`, or `org.a11y.Bus.GetAddress`), opens a second connection, registers the root application object, and calls `Socket.Embed`.
  - Per adapter, the state is `Inactive`, then `Pending` (the activation handler returned `None` and a full tree is awaited), then `Active`.
  - `update_if_active` takes a closure and calls it only when active, so the app builds `TreeUpdate`s only when needed.
  - If `IsEnabled` goes false, adapters deactivate and the deactivation handler fires.
  - All three handlers are always called from the adapter thread, not the app thread.
- **Object model.** Each node registers D-Bus interfaces (Accessible, Action, Component, Text, EditableText, Selection, Value and others) on demand, driven by `register_interfaces`/`unregister_interfaces` messages. `supports_text_ranges` is true for text inputs and for `Label`, `Document` and `Terminal` nodes that have at least one `TextRun` descendant.
- **Text model.**
  - A `Terminal` node has `TextRun` children, normally one per visual row.
  - Each run carries `value` (UTF-8), `character_lengths` (a list of u8 byte lengths, one per "character", so a grapheme cluster of up to 255 bytes), `word_starts`, `character_positions` and `character_widths` (f32, one axis in text direction, in the node's coordinate space), and optional text direction.
  - Offsets exposed over AT-SPI are Unicode scalar value indices over the concatenated run text (`to_global_usv_index`). That matches the ACCESSIBILITY-07 intent of "character indices into extracted text, not cell indices". The caret and selection live on the container as an anchor and focus (node plus character index).
  - `character_extents` finds the text position for the offset, takes its bounding box, and converts to AT-SPI coordinates by window or screen. `offset_at_point` inverts the node transform and calls `text_position_at_point`.
- **Events.** On each tree update, for every parent with changed text runs, `emit_text_change_if_needed_parent` builds the old and new full document text, trims the common prefix and suffix, and emits at most one `TextRemoved` and one `TextInserted` for the middle. It also emits `CaretMoved` and `TextSelectionChanged` when the container's selection changes while focused. Announcements are supported (`ObjectEvent::Announcement(text, politeness)`), which is the API for bell and notification text.

## Feature inventory (grouped by category)

### kbvm: keymap

- Parses full XKB keymap text, including `xkb_keycodes`, `xkb_types`, `xkb_compat`, `xkb_symbols`, with `include`, merge modes and `xkb_geometry` ignored. Documented deviations: trailing commas, empty lists, concatenation of repeated sections, omitted sections.
- Builds from RMLVO names, reading rules from the XKB root, with `XKB_CONFIG_EXTRA_PATH` and `XKB_CONFIG_ROOT`.
- Virtual modifiers are exposed by name with their real-modifier mask. Masks are 32-bit.
- Up to 32 groups per key. Each level can hold any number of symbols and actions, which libxkbcommon also allows for multi-keysym levels.
- Behaviours: only Locking, Overlay1, Overlay2 and RadioGroup. Actions: `SetMods`, `LatchMods`, `LockMods`, `SetGroup`, `LatchGroup`, `LockGroup`, `RedirectKey`, `SetControls`, `LockControls` (only overlay controls act on the state machine). The missing ones (pointer, screen-switch, terminate, private) do not matter to a terminal client.
- Includes keysym name, character, case and `is_modifier`/`is_keypad` queries.

### kbvm: client lookup

- Stateless lookup; the application owns group and modifier state from `wl_keyboard.modifiers`.
- Caps transform, Ctrl transform and Ctrl fallback, each switchable. Ctrl transform covers the classic control-character mapping.
- `remaining_mods()` (consumed-modifier subtraction) and `repeats()`.
- Group redirect for keys with fewer groups than the map.

### kbvm: Compose

- Locale resolution through `locale.alias` and `compose.dir`, `$XCOMPOSEFILE`, XDG and `~/.XCompose`, `%L %H %S %%` in includes.
- Multi-key sequences of any length, dead keys (they are ordinary keysyms), output as string, keysym or both, and an `Aborted` result on a non-matching key.
- Modifier syntax is ignored.
- Iteration and re-serialisation to XCompose text.

### accesskit_unix

- Roles: `Terminal`, `TextRun`, `Document`, `Label`, text input roles. Word starts are supplied by the app. Sentence boundaries are not supported (Orca falls back to line mode in Say All, per GNOME a11y blog).
- AT-SPI: Text (offset, extents, range extents, string at offset, selections, attributes, caret), EditableText, Component extents, Action, Selection, Value, plus cache add and remove, window activated and deactivated, state-changed.
- Text attributes are not yet exposed for font, size, style or colour (per a GNOME a11y report; unverified for 0.24).
- Lazy activation as described above.
- No per-character colour or attribute runs from the terminal grid are mapped. For a terminal, colour is not essential, but bold or reverse can be exposed only if the app sets the corresponding node properties and the adapter maps them.

## Control surface (config, actions, IPC)

Neither crate has user-facing config. What Apotheosis should expose, derived from what these crates make controllable:

- **Keyboard layer.**
  - Compose on or off, and an explicit Compose file path override. The default is the kbvm search order, because it equals libxkbcommon's.
  - Ctrl fallback for non-Latin layouts, on or off.
  - A `repeat` override, with delay and rate taken from `wl_keyboard.repeat_info` by default.
  - An xkb backend switch (`kbvm` or `libxkbcommon`). It is useful for bug triage and must exist for the fallback.
  - An action such as `compose-reload` to rebuild the table after the user edits `~/.XCompose`.
  - A debug action that dumps the last key event: keycode, group, modifiers, consumed, keysyms, unshifted and shifted codepoints.
- **Accessibility layer.**
  - `accessibility = auto | on | off`. Auto follows `IsEnabled`.
  - Rows exposed (visible only, or visible plus N lines of scrollback).
  - Update coalescing interval.
  - An action to announce text (`announce` with politeness), for use by rules (event bell, command-finished).
  - An IPC query that dumps the current AT-SPI tree for debugging.
- Environment variables honoured by kbvm that we should document and not shadow: `XKB_CONFIG_ROOT`, `XKB_CONFIG_EXTRA_PATH`, `XCOMPOSEFILE`, `XLOCALEDIR`, `XDG_CONFIG_HOME`, `HOME`, `LC_ALL`, `LC_CTYPE`, `LANG`. On the AT-SPI side: `AT_SPI_BUS_ADDRESS`.

## Performance (numbers + method)

- No published latency or throughput numbers exist for either crate. kbvm's `kbvm-benches` has criterion benches for keysym conversion (compared against libxkbcommon), the state machine, and one Compose no-match feed over a 52-rule table. The repo does not record results. AccessKit has none for the unix adapter.
- Structural costs that matter, from code reading:
  - **Compose feed** is a binary search over a child range per keypress. That is O(log n) with no allocation on the hot path. A full en_US table is about 5,200 lines on this host (62 locale Compose files exist). Table build time is not published; measure it and keep it off the startup critical path (build lazily on first dead key or Compose key, or on a worker thread).
  - **Lookup** is a hash-free indexed key table plus a linear scan of a type's cases. It needs no allocation if the iterator is not collected. Verify this for the "no allocation in the hot path" rule.
  - **AccessKit tree update** with text changes rebuilds the full old and new text of the container and diffs it. Cost is linear in container size per update. With a visible grid of, say, 60 by 200 cells that is about 12k characters per update, which is acceptable. Exposing scrollback would multiply it. Update rate must be coalesced.
  - **Scrolling output** produces one large removed-and-inserted span, not a line-wise event. Orca could then speak a whole screen. Expect to need app-side control: send updates for only the changed region where possible, mark noisy output as not live, or suppress text events while output is streaming.
  - A thread plus two D-Bus connections exist only when an AT is detected. Idle cost with no AT is one parked thread and one subscription.
- Suggested budgets for Apotheosis, to be measured: a key press to `lookup` result in under 5 microseconds; Compose table build for the en_US file under 20 ms off the main thread; AT update with a 60 by 200 grid under 2 ms at p99 on the main thread. These are targets, not published figures.

## What Apotheosis should take

1. Use kbvm as the primary XKB client backend behind the `XkbBackend` trait, keep libxkbcommon as the selectable fallback, and make "kbvm parity with libxkbcommon" a CI gate with the harness below.
2. Parse the compositor's keymap with `keymap_from_bytes`, then build a `LookupTable`. Keep `Components` in the input state. Use `remaining_mods` only after the harness has proven the difference in consumed semantics is harmless, or after it has been adapted.
3. Derive kitty protocol fields this way: unshifted codepoint from an empty-modifier lookup in the current group (lower-cased), shifted key from a Shift-only lookup, base-layout key from a static evdev-to-US table, and modifier bits resolved by modifier name through the keymap (including virtual Super, Hyper, Meta, NumLock). Call lookups with the Caps and Ctrl transforms turned off when probing.
4. Use kbvm Compose for dead keys, Multi_key and user `~/.XCompose`, honouring libxkbcommon's search order. Build it lazily. Treat `Aborted` as "discard, do not re-feed", which matches libx11 and libxkbcommon behaviour.
5. Build the oracle as a test-only workspace member that links the system libxkbcommon with its own `extern "C"` declarations. See "Differential corpus" below.
6. For accessibility, use `Role::Terminal` with one `TextRun` per visible row, `character_lengths` per grapheme cluster, `character_widths` of 1 or 2 cells for wide characters, `character_positions` per cell, trimmed trailing blanks, and a hard newline only where the row ends a logical line.
7. Always use `update_if_active` and return `None` from the activation handler. Hand the main thread a wake-up so the full tree is built there, because the activation handler runs on the AT-SPI thread and cannot touch the grid.
8. Coalesce AT tree updates (suggested 50 to 100 ms), cap exposed rows, and never expose scrollback by default.
9. On Abyss, feed the window's screen position into `set_root_window_bounds` so screen-coordinate extents are right for magnifiers and review cursors. Other compositors get window-relative coordinates only.
10. Use the Announcement event for bell, command-finished and notification text, wired as actions.
11. Add an Orca test matrix as a first-class deliverable: Orca 48, 49 and the current release on GNOME Wayland, plus a run on Abyss, with a fixed script (type, scroll output, selection, review cursor, caret reading).

### Differential corpus (how to build it without compiling C)

Use the allowed system libxkbcommon only in a test-only binary. kbvm's linker crate shows the technique (declare `extern "C"` and link by name with `pkg-config`), and no C is compiled. The `xkbcommon` crate 0.8.0 (MIT, links the system library, no build script) covers keymap creation, `get_as_string`, `key_get_syms_by_level`, `key_repeats`, state update and key-get-utf32. It does not bind `xkb_state_key_get_consumed_mods2` or the compose table iterator, so declare those two by hand. The iterator is in the installed header since 1.6.0.

- **Keymap corpus.**
  - Enumerate `evdev.xml` from xkeyboard-config. On this host that is 99 layouts, 502 variants, 224 options in 23 groups and 185 models.
  - Compile each layout and variant with model `pc105` and a few option sets (`grp:alt_shift_toggle`, `ctrl:nocaps`, `compose:ralt`, `lv3:*`, `numpad:*`).
  - Also compile a sample of the raw `.xkb` fixtures shipped in libxkbcommon's test data.
  - For each keymap, serialise it with libxkbcommon and parse that text with kbvm, which also tests the real client path.
- **Comparisons per keymap.**
  - **Static:** key set, group count, key repeat flag, per-level symbols, the real and virtual modifier name to mask map, and indicator names. Mask comparison matters because compositors send `wl_keyboard.modifiers` using the libxkbcommon index order.
  - **Dynamic:** for every key, group and a modifier mask over Shift, Lock, Ctrl, Alt, Mod2, Mod3, Mod4, Mod5 (about 38 million lookups for 600 keymaps at 250 keys, 4 groups and 64 masks, which is minutes). Compare keysyms, UTF-32, consumed mask (XKB mode and GTK mode) and the effective ctrl-transformed character.
  - Expect and whitelist known documented divergences (geometry, unsupported actions). Anything else is a defect to report upstream or a reason to switch backend.
- **Compose corpus.**
  - Take all 62 locale Compose files, the user-style files shipped in kbvm's `compose-tests`, and synthetic files for includes, `%L %H %S`, modifier syntax, duplicates and overriding rules, and prefix conflicts.
  - **Table equality:** iterate both tables and compare sequence to (string, keysym) as sets.
  - **Walk equality:** feed random keysym walks and compare the status, string and keysym at each step. Weight the walks toward prefixes of real sequences, plus modifier keys, and keys that abort.
  - Log divergence classes. The first to check is conflicts between a short sequence and a longer one that shares its prefix, and duplicate-rule precedence.
- **Gate.** Run it in a separate CI job that needs the libxkbcommon and xkeyboard-config packages. Pin both versions in the job and record them in the log. Failures on a version bump are information, not breakage.

## What it should avoid

- Do not use kbvm's `libxkbcommon-test-linker` or its meson submodule. That compiles C.
- Do not enable kbvm's `x11` and `registry` features unless needed. `registry` pulls serde and quick-xml; the settings GUI can use it for a layout picker, but keep it out of the terminal binary.
- Do not rely on `remaining_mods` semantics before the harness has compared them with libxkbcommon.
- Do not assume Compose modifier syntax works. kbvm ignores it, and so (per the libxkbcommon docs) do others in this class of library; keep user files free of it or warn.
- Do not call AccessKit from the main thread expecting synchronous handlers. All handlers run on the adapter thread.
- Do not expose scrollback or the whole grid as text. The full-text diff is linear in container size and event payloads can be huge.
- Do not pad rows with blanks, and do not use cell indices as offsets. AT-SPI offsets are character indices into the extracted text.
- Do not rely on `IsEnabled` as proof a screen reader is running. It is sticky after the first Orca run and may be always true on some distributions (AccessKit #314 and #713). Treat activation as a cost control, not a detector.
- Do not enable the `tokio` feature just to match the project style. It would require a tokio runtime that Apotheosis does not otherwise have.
- Do not promise attribute (colour, bold) accessibility until the adapter maps those properties (unverified for 0.24).
- Do not tie the accessibility design to AT-SPI only. Newton is a likely successor on GNOME Wayland.

## Sources

kbvm source (prior art, read-only; function names given):
- `/home/risk/.cache/apotheosis-prior-art/kbvm`, upstream https://github.com/mahkoh/kbvm
  - `kbvm/Cargo.toml` (features, dependencies, MSRV 1.95, edition 2024)
  - `docs/compatibility.md` (documented deviations)
  - `kbvm/src/lookup.rs` (`LookupTable::lookup`, `effective_group`, `repeats`, `Lookup::remaining_mods`)
  - `kbvm/src/group_type.rs` (`GroupType::map`, `map_preserve`)
  - `kbvm/src/components.rs` (`Components::update_effective`, `apply_event`)
  - `kbvm/src/state_machine.rs` (`StateMachine::handle_key`)
  - `kbvm/src/xkb/compose.rs` (`ComposeTableBuilder::build_`, `get_compose_file`, `resolve_locale`)
  - `kbvm/src/xkb/compose/table.rs` (`ComposeTable::feed`)
  - `kbvm/src/xkb/compose/parser.rs` (`parse_include`, `parse_modifiers`)
  - `kbvm/src/xkb/context.rs` (limits, env vars)
  - `libxkbcommon-test-linker/{build.rs,src/lib.rs}`
  - `compile-tests/`, `compose-tests/src/main.rs` (golden-file runners)
  - `kbvm-benches/benches/*`
  - `.github/workflows/integration-tests.yml`
- Crate pages: https://crates.io/crates/kbvm and https://docs.rs/kbvm/0.2.0/kbvm/ (20 dependencies listed: 18 normal, 1 dev, 1 build; no no_std or unsafe statement).

AccessKit source:
- `/home/risk/.cache/apotheosis-prior-art/accesskit`, upstream https://github.com/AccessKit/accesskit
  - `adapters/unix/Cargo.toml`, `README.md` (async runtime compatibility)
  - `adapters/unix/src/context.rs` (`get_or_init_messages`, `run_event_loop`, `activate_adapter`, `bus_after_status_change`)
  - `adapters/unix/src/executor.rs`, `adapters/unix/src/adapter.rs` (`Adapter::new`, `update_if_active`, `set_root_window_bounds`)
  - `adapters/unix/src/atspi/bus.rs` (`Bus::new`, `register_root_node`)
  - `adapters/atspi-common/src/node.rs` (`character_extents`, `offset_at_point`, `supports_text`, role map)
  - `adapters/atspi-common/src/adapter.rs` (`emit_text_change_if_needed_parent`, `emit_text_selection_change`)
  - `adapters/atspi-common/src/events.rs`
  - `accesskit_consumer/src/text.rs` (`supports_text_ranges`)
  - `accesskit/src/lib.rs` (`Role::Terminal`, `Role::TextRun`, `character_lengths`, `word_starts`, `character_positions`, `character_widths`)
  - `README-APPLICATION-DEVELOPERS.md`
- Crate pages: https://crates.io/crates/accesskit_unix (0.24.1, 2026-10-25), https://crates.io/crates/accesskit, https://crates.io/crates/accesskit_atspi_common, https://crates.io/crates/atspi (0.30.0 latest; accesskit_unix pins ^0.29 with `proxies` only), https://crates.io/crates/zbus (6.2.1 latest; accesskit_unix needs ^5.19 with `async-io`).
- zbus 5.19.0 source in the local cargo registry: `src/proxy/mod.rs` (property stream yields the current value first), `src/connection/builder.rs` (internal executor thread).
- AccessKit issues, read via the web:
  - https://github.com/AccessKit/accesskit/issues/314 (activation design, `IsEnabled`)
  - https://github.com/AccessKit/accesskit/issues/713 (Orca detection regression; GNOME 49, Orca 49.5 and 51.alpha; at-spi2-core #224)
  - https://github.com/AccessKit/accesskit/issues/854 (Orca 48, accesskit_atspi_common 0.19.1, Text interface limited to text inputs, Label, Document, Terminal)
  - Issue list: https://github.com/AccessKit/accesskit/issues?q=orca+is%3Aissue
- GNOME accessibility blog (Newton, limits of AccessKit text and GTK terminals): https://blogs.gnome.org/a11y/

libxkbcommon:
- https://xkbcommon.org/doc/current/group__compose.html (statuses, modifier keysyms ignored, search order, XDG extension)
- https://xkbcommon.org/doc/current/group__state.html (consumed modifiers, XKB and GTK modes)
- `/usr/include/xkbcommon/xkbcommon-compose.h` (iterator API since 1.6.0)
- `xkbcommon` crate 0.8.0 source in the local cargo registry (binds compose and state, not consumed-mods2 or the compose iterator) and `xkbcommon-dl` 0.4.2 (dlopen alternative, MIT).
- Host data used for sizing: `/usr/share/X11/xkb/rules/evdev.xml`, `/usr/share/X11/locale/`.

Unverified items:
- Whether kbvm's `consumed` equals libxkbcommon's XKB-mode consumed in the no-match and preserve cases (the harness decides).
- Which compositors send virtual-modifier bits in `wl_keyboard.modifiers`.
- Exact table-build times for kbvm Compose.
- Whether AT-SPI text attribute mapping (font, colour) landed in 0.24.
- Any Orca-with-terminal-on-AccessKit test report.
- Whether libxkbcommon's Compose modifier syntax handling is identical to kbvm's "ignored" behaviour.
- Licence terms of the X11 locale Compose data files if Apotheosis ever bundles them (today they come from the `libx11` package).
