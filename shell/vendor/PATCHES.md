<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# vendor/

`iced_layershell/` and `layershellev/` are 0.19.1 from crates.io (MIT,
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

`.cargo-ok` was removed from each directory; nothing else was.
