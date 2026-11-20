<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# vendor/

`iced_layershell/`, `layershellev/`, `iced_sessionlock/` and `sessionlockev/` are 0.19.1 from crates.io (MIT,
third-party, so no SPDX headers added), copied verbatim, plus one hook so the
DE can reach a layer surface's `wl_surface` (for `eclipse_glass_v1`). Every
changed hunk is marked `// eclipse-patch:`; `rg eclipse-patch vendor/` lists
them. Upstreamable as "expose the wl_surface of a layer surface". Drop this
directory when upstream carries it.

1. `layershellev/src/lib.rs`: `WindowWrapper::wl_surface()` accessor, and
   `WlSurface` added to `reexport::wayland_client`. The field was private.
2. `iced_layershell/src/lib.rs` + `multi_window/window_manager.rs`:
   `SurfaceHook`, `set_surface_hook()`, called from `WindowManager::insert`
   with the iced window id and the `wl_surface`, before the first draw.
3. `#![allow(warnings, clippy::all)]` at the top of both `lib.rs` so our
   `-D warnings` gate does not lint upstream code.

4. `iced_sessionlock/` and `sessionlockev/` (0.19.1, MIT, verbatim) carry the same
   hook for the `ext_session_lock_v1` surface (ec-lock's glass, ADR 0129):
   `sessionlockev::WindowWrapper::wl_surface()` and `WlSurface` re-exported;
   `iced_sessionlock::{SurfaceHook, set_surface_hook}` called from
   `WindowManager::insert` with the iced window id and the `wl_surface`, before
   the first draw. `window::run` is not implemented by iced_sessionlock (it is
   dropped), so the xdg-window path cannot reach the surface. Upstreamable.
5. `sessionlockev/src/lib.rs`: a requested `wl_surface.frame` callback that has not
   answered in 50 ms frees the present slot (`slot_free`, `stall_wait`). abyss
   sends lock surfaces no frame callbacks (`send_frames` walks the space and
   layer map only), so without this the locker draws one frame and freezes: no
   clock tick, no typed dots, no entrance. Inert once the compositor sends them;
   drop it then.

`.cargo-ok` was removed from each directory; nothing else was.
