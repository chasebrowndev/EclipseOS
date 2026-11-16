# apoth-wl

Governing spec: T-09 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). AGPL-3.0-only. Not TCB.
`unsafe` is allowed here (FFI only, `rawhandle.rs`), each block with a `// SAFETY:` comment.

## Layout
- `source.rs`: calloop `WlSource` + `WlHandle`; `state.rs`: the one state struct and binds;
  `dispatch.rs`: `Dispatch` impls; `seat.rs`, `window.rs`, `scale.rs`, `cursor.rs`,
  `transfer.rs`/`mime.rs` (clipboard, DnD), `repeat.rs`, `gesture.rs`, `caps.rs` (§3 table).
- No xkbcommon, no winit, one wayland backend (system libwayland).

## Tests
- Unit tests in each module (80+ and the protocol-absence matrix in `caps.rs`).
- `tests/session.rs`: two entry points sharing one child body, run against headless
  `ec-abyss` (private runtime dir, pid-captured, never a visible window). The child checks
  the capability table (fatal globals bound, no optional global left at "present"), maps a
  toplevel, receives configure, requests an activation token, attaches a CPU buffer, waits for
  keyboard enter, round-trips the clipboard through the compositor (set, offered back, receive
  plus send transfers) and runs a 1000-step resize storm.
  - `against_spawned_headless_abyss`: spawns `$APOTH_EC_ABYSS` or `ec-abyss` from `$PATH`.
    Skips without it unless `APOTH_REQUIRE_SESSION=1` (the gate job must set it).
  - `against_given_display`: connects to `$APOTH_TEST_WAYLAND_DISPLAY` (runtime dir
    `$APOTH_TEST_XDG_RUNTIME_DIR`, default `$XDG_RUNTIME_DIR`) when set; otherwise skips.
- Not yet covered end to end: pointer/key delivery with injected input, IME round trip,
  output hot-unplug while mapped, pixel-crisp scale screenshots (need an input-injecting test
  compositor, T-25 §6, owed to apoth-testkit).
- The §1 `cargo tree` check (one wayland backend, no xkbcommon) belongs in CI
  (`cargo tree -p apoth-wl -e features -i xkbcommon` must fail); it is not wired here.

## Optional globals (`extras.rs`)
`xdg_activation_v1` (token, urgency, activate), `xdg_system_bell_v1`,
`xdg_toplevel_icon_manager_v1` (icon by name), `xdg_toplevel_tag_manager_v1`,
`wp_content_type_manager_v1`, `zwp_idle_inhibit_manager_v1` and
`zwp_input_timestamps_manager_v1` are bound when advertised (also when they appear late) and
exposed on `WlHandle` (`request_activation_token`, `request_attention`, `activate_with_token`,
`ring_bell`, `set_icon_name`, `set_toplevel_tag`, `set_content_type`, `set_idle_inhibit`);
every call returns `false` when the global is absent. Events: `WlEvent::ActivationToken`,
`WlEvent::InputTimestamp` (informative latency series, never a gate input). Icon by pixel
buffer is not implemented (name only). The app must call these (urgency on bell/notification,
content type and idle inhibit from media playback, tag from the profile, timestamps into the
T-23 latency series).

## Limits
- `repeat_info` rate is clamped to `repeat::MAX_RATE_HZ` (1000 Hz).

## Render-thread link (`pace.rs`, M11)
`WlHandle::surface_handle(id)` gives a `Send + Sync` `WlSurfaceHandle` (raw-window-handle 0.6;
the only unsafe pointer wrapping, `rawhandle.rs`) for `wgpu::Instance::create_surface`.
`WlHandle::pacer(id)` gives a `Send` `Pacer`: it issues `request_frame` /
`request_presentation_feedback` itself and receives `PaceEvent::{Frame, Presented, Discarded,
Surface}` on a channel (`Surface` = logical size, buffer size, scale, suspended; sent first on
creation and after every relayout/scale change). The same events still flow through `WlEvent`.
Keyboard: `InputKind::Keymap(k)` -> `k.text()` -> `KbvmBackend::load_keymap_bytes`;
`Modifiers{..}` -> `apoth_input::Components`; `Key{key, pressed, repeat, ..}` -> `lookup(key)`.

## Drag and drop in (M16)
`WlEvent::Dnd(DndEvent::{Enter, Motion, Leave, Dropped})`: the crate accepts text and
`text/uri-list` offers (copy action) and refuses the rest. After `Dropped`, call
`WlHandle::receive_drop(seat)` (a `Transfer`, size cap and timeout as for the clipboard; its
done event has `kind == Clipboard`, track it by id), convert with `DropContent::from_bytes`
(`paste_bytes()` = text, or shell-quoted paths joined by spaces) and send that through the
paste guard, then `finish_drop(seat, ok)`. Not covered end to end (needs an input-injecting
compositor, T-25 §6).
