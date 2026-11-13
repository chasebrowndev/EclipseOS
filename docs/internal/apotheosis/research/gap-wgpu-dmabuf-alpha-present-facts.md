# gap-wgpu-dmabuf-alpha-present-facts: wgpu facts the Apotheosis architecture depends on

Prior-art rule observed: findings are prose; no source is transcribed. Vulkan/Wayland API names, extension names and config keys are interface facts and are quoted. Anything not confirmed is marked "unverified". Research date 2026-11-12.

## Summary

- **dmabuf import exists, export does not.** `wgpu_hal::vulkan::Device::texture_from_dmabuf_fd` is in the published wgpu-hal 30.0.0 / 30.0.2 (confirmed in the 30.0.2 crate tarball and in the CHANGELOG v30.0.0 "Vulkan" section, PR 9366). It is single-plane only. wgpu has no dmabuf export API and no way to present a wgpu texture as a `wl_buffer`. Presentation goes through the Vulkan swapchain (Mesa's Wayland WSI creates the dmabuf-backed `wl_buffer`s itself). The import is reachable from `wgpu::Device::as_hal` plus `create_texture_from_hal` (unsafe, needs an `initial_state` argument).
- **The 30.x import is gated by feature flags that main (31) removes.** In 30.x, request `Features::VULKAN_EXTERNAL_MEMORY_FD | VULKAN_EXTERNAL_MEMORY_DMA_BUF` at device creation. The unreleased CHANGELOG removes both flags and enables the extensions whenever the adapter supports them. Pin wgpu and plan the migration.
- **Transparency on Wayland works with `PreMultiplied` on Mesa.** Mesa's Wayland WSI advertises exactly `OPAQUE | PRE_MULTIPLIED` (Mesa `src/vulkan/wsi/wsi_common_wayland.c`, `wsi_wl_surface_get_capabilities`). It picks ARGB8888-class DRM formats only when `PRE_MULTIPLIED` is chosen, XRGB otherwise. wgpu's `CompositeAlphaMode::Auto` falls back only to `Opaque` then `Inherit`, never to `PreMultiplied`. Transparent surfaces therefore require an explicit `PreMultiplied` request, and the renderer must write premultiplied colour. NVIDIA proprietary on Wayland is unverified for current drivers; the only evidence found is a 2022 forum report of `supportedCompositeAlpha == OPAQUE` only.
- **Rio issue 1967 is not a wgpu bug.** It concerns Rio's native-Vulkan backend, which clears with unpremultiplied RGB and possibly negotiates `POST_MULTIPLIED`/`PRE_MULTIPLIED` ad hoc. The reporter's own wgpu path (which premultiplies the clear colour when the surface is `PreMultiplied`) renders correctly. The reporter says the diagnosis is unconfirmed. It supports "premultiply everything yourself", not "wgpu transparency is broken".
- **Device loss is notified, not recovered.** wgpu 30 offers `Device::set_device_lost_callback(Fn(DeviceLostReason, String))` and `CurrentSurfaceTexture::{Outdated, Lost, ...}`. `DeviceLostReason` has only `Unknown` and `Destroyed`. On `VK_ERROR_DEVICE_LOST` wgpu-hal returns `DeviceError::Lost`, wgpu-core marks the device invalid and fires the callback once. Recovery is entirely the application's job: new adapter, device, queue, surface configure, and re-upload every texture, buffer, atlas and pipeline. Apotheosis needs a renderer that can be rebuilt from CPU-side state.
- **No Vulkan = no adapter.** `Instance::request_adapter` returns `Err(RequestAdapterError::NotFound{..})`. `RequestAdapterOptions::force_fallback_adapter` selects a software adapter (`DeviceType::Cpu`, i.e. lavapipe/llvmpipe via Vulkan). The `gles` backend (EGL) would give a second software path but needs libEGL/libwayland-egl, which are not on the allowed system-library list. Recommendation: build wgpu with `vulkan` only, make lavapipe a supported-but-warned degraded mode, and have a non-wgpu last resort (shm CPU renderer) only if the owner wants "never fails to start". Acceptability of lavapipe performance is unmeasured (unverified).
- **Version, MSRV, licences are clean.** wgpu 30.0.2 / wgpu-hal 30.0.2 published 2026-11-12, MSRV 1.87 on crates.io (workspace main says 1.95). Licence `MIT OR Apache-2.0`. The upstream `.deny.toml` for the published library crates allows only Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, MIT, Unicode-3.0, Zlib, all inside our allowed set. ash, gpu-allocator, gpu-descriptor, renderdoc-sys, naga, half, petgraph: MIT OR Apache-2.0. libloading: ISC. spirv, codespan-reporting: Apache-2.0. The build pulls no GPL and no C/C++ (gpu-allocator and ash are pure Rust; Vulkan loaded via dlopen with libloading). Two things need a check in our own lockfile: optional `hexf-parse` is CC0-1.0 (allowed) and `wayland-sys` is MIT (only used by the gles feature, so avoid).

## Architecture (how it is built)

**wgpu stack.** `wgpu` (frontend) -> `wgpu-core` (validation, tracking, resource lifetime) -> `wgpu-hal` (per-API backend; `vulkan` uses `ash` for bindings and `gpu-allocator` for memory) -> loaded Vulkan driver via `libloading`. Shaders: WGSL/other -> `naga` IR -> SPIR-V, all in Rust. No C compiled by the build.

**Surface creation on Wayland (Vulkan).** `wgpu::Instance::create_surface` takes a raw-window-handle pair. For `Rwh::Wayland` + `Rdh::Wayland` the hal calls `vkCreateWaylandSurfaceKHR` (instance extension `VK_KHR_wayland_surface`), passing the app's `wl_display*` and `wl_surface*`. The Vulkan driver (Mesa WSI) then owns the swapchain: it allocates dmabuf-backed images, wraps them as `wl_buffer` via `zwp_linux_dmabuf_v1`, attaches/commits them to the app's surface on present, and uses `wp_linux_drm_syncobj` explicit sync when the compositor offers it. Consequence: the application and the driver share one `wl_surface`; the app must not attach its own buffers to it, and subsurfaces/viewport/fractional-scale/opaque-region calls are the app's responsibility.

**Alpha selection.** `SurfaceConfiguration.alpha_mode` (type `CompositeAlphaMode`: `Auto`, `Opaque`, `PreMultiplied`, `PostMultiplied`, `Inherit`) maps 1:1 onto `VkCompositeAlphaFlagBitsKHR`. `surface_config.rs` in wgpu-core validates the request against `SurfaceCapabilities.alpha_modes`; only `Auto` has a fallback chain (`Opaque`, then `Inherit`). An explicit unsupported mode returns `UnsupportedAlphaMode { requested, available }`. Mesa's Wayland WSI reports `OPAQUE | PRE_MULTIPLIED`: with PRE_MULTIPLIED it chooses ARGB DRM formats, otherwise XRGB (formats whose alpha and opaque variants both exist are required, `wl_drm_format_for_vk_format(.., alpha)`). The Mesa file contains no `wl_surface_set_opaque_region` call (grep), so opaque-region optimisation is up to the app.

**Present modes on Wayland (Mesa).** `MAILBOX` and `FIFO` always; `IMMEDIATE` only when the compositor has `wp_tearing_control_manager_v1`. Mesa also binds `wp_fifo_manager_v1`, `wp_commit_timing_manager_v1` and `wp_presentation` internally. Present modes were also covered in `topic-gpu-latency.md`, which says "Immediate unsupported, Mailbox only on Vulkan"; that is incomplete: Immediate appears when tearing-control exists (Mesa source, `wsi_wl_surface_get_present_modes`). Whether wgpu surfaces the mode depends on wgpu's own filtering (unverified).

**dmabuf import path.** `texture_from_dmabuf_fd(fd: OwnedFd, desc: &hal::TextureDescriptor, drm_modifier: u64, stride: u64, offset: u64) -> Result<vulkan::Texture, DeviceError>` (unsafe, `#[cfg(unix)]`). Technique: create a `VkImage` with `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`, chaining `VkExternalMemoryImageCreateInfo` (handle type DMA_BUF_EXT) and `VkImageDrmFormatModifierExplicitCreateInfoEXT` with one `VkSubresourceLayout` (offset, row pitch = stride); query `vkGetMemoryFdPropertiesKHR` for the fd's memory-type bits; intersect with the image's requirement bits; allocate with `VkImportMemoryFdInfoKHR` + `VkMemoryDedicatedAllocateInfo`; bind; wrap with `texture_from_raw` as a dedicated allocation. Ownership: on success Vulkan owns the fd; on failure wgpu closes it and destroys the image. Limits: single plane only (no NV12/P010 multi-plane import, so VA-API decoded surfaces need a per-plane or RGB path), only one explicit plane layout, no sync handling (see below).

**Feature gating (30.x).** `Features::VULKAN_EXTERNAL_MEMORY_FD` (bit 35, "wgpu-vulkan-external-memory-fd") and `Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF` (bit 63, "wgpu-vulkan-external-memory-dma-buf"). The adapter sets DMA_BUF only if `VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf` and `VK_EXT_image_drm_format_modifier` are all present. The function returns `DeviceError::Unexpected` when the feature was not enabled. In unreleased main the two flags are removed (CHANGELOG, PR 10635): extensions are enabled when supported, and the check is `Adapter::as_hal::<Vulkan>()` then `physical_device_capabilities().supports_extension(name)`.

**Wrapping into wgpu.** `Device::as_hal::<Vulkan>()` yields the hal device; the resulting hal texture goes through `wgpu::Device::create_texture_from_hal::<Vulkan>(hal_texture, &desc, initial_state)` (unsafe). `initial_state` must match the real image layout; passing `TextureUses::UNINITIALIZED` is a content-discarding transition, which CHANGELOG v29/30 notes broke zero-copy video imports with compressed modifiers. Pass a real "already shader-readable" state for imported frames.

**Sync for imported buffers.** wgpu does not implicitly wait on the dmabuf's producer. Related API: `vulkan::Queue::add_wait_semaphore`/`remove_wait_semaphore` (PR 9461) let an external producer's `VK_KHR_external_semaphore_*` semaphore be waited on at the next submit. Importing a sync_file as a semaphore from the dmabuf (`DMA_BUF_IOCTL_EXPORT_SYNC_FILE`) is the usual technique; wgpu has no helper for it (unverified whether the hal exposes the needed semaphore-import entry points).

**Export / present-as-wl_buffer.** Search of wgpu-hal Vulkan for `get_memory_fd`, export flags and `ExportMemory` finds only the import-side `get_memory_fd_properties`. There is no export API, so a wgpu texture cannot be turned into a `wl_buffer`. Two workable routes if ever needed: (a) let the WSI do it (normal swapchain); (b) allocate a `VkImage` yourself through `ash` with exportable dmabuf memory (`VkExportMemoryAllocateInfo`, `vkGetMemoryFdKHR`), wrap it with `texture_from_raw` + `create_texture_from_hal` to render into, and hand the fd to `zwp_linux_dmabuf_v1` with your own `wl_surface` buffer management. Route (b) is unverified in practice, needs `unsafe`, own swapchain logic, explicit sync via `wp_linux_drm_syncobj`, and loses the driver's latency/timing machinery. Not recommended for v1.

**Device loss.** Hal maps `VK_ERROR_DEVICE_LOST` to `DeviceError::Lost` in the paths that use `map_host_device_oom_and_lost_err` (submit, wait, etc.). `Device::lose(message)` in wgpu-core sets `valid=false`, calls the stored lost-closure once with `DeviceLostReason::Unknown`, and later work fails. A Cargo feature `device_lost_panic` on wgpu-hal turns loss into a panic (test use). `Surface` errors are separate: `CurrentSurfaceTexture::Outdated` (reconfigure), `Lost` (recreate surface, or recreate the device if the device-lost callback fired), `Timeout`, `Occluded`, `Validation`, plus `Suboptimal`. wgpu-core also treats `DeviceLost`-class errors as non-fatal inside some `Result`s (resource.rs lines near 4608, 4758) so many calls return Ok after loss; check `valid` via the callback, not return values.

**No-GPU path.** `request_adapter` goes through `wgpu_core::instance::Global::request_adapter`, filtering by power preference and `force_fallback_adapter`; if nothing matches, `RequestAdapterError::NotFound` (with per-backend reasons, e.g. "had no fallback adapters"). `DeviceType::Cpu` marks software adapters. lavapipe appears as a Vulkan adapter when installed (Mesa package `vulkan-swrast` on Arch), so a Vulkan-only build still has a software route without libEGL.

## Feature inventory (grouped by category)

Graphics / media:
- dmabuf single-plane import via hal; `create_texture_from_hal` wrap; `initial_state` argument; no export; external semaphore wait hook; `texture_from_raw` for caller-owned VkImage; `RawWindowHandle::Drm` support (feature `drm`).

Rendering:
- `CompositeAlphaMode` variants and their Vulkan mapping; Auto fallback chain; premultiplied expectations; `SurfaceColorSpace` (Srgb and HDR options, per-format capability reporting); `desired_maximum_frame_latency`.

Performance:
- Present modes via Mesa Wayland (MAILBOX, FIFO, IMMEDIATE only with tearing-control); explicit sync and fifo/commit-timing support inside Mesa WSI; hal-level Vulkan extension list is queryable.

Integration / reliability:
- `set_device_lost_callback`; `CurrentSurfaceTexture` status enum; `force_fallback_adapter`; `DeviceType::Cpu`; `device_lost_panic`; `on_uncaptured_error` and error scopes.

Packaging / licences:
- wgpu 30.0.2 MIT OR Apache-2.0; MSRV 1.87; upstream cargo-deny allow list; backend features selectable (turn off `gles`, `dx12`, `metal`, `webgpu`).

## Control surface (config, actions, IPC)

Proposed KDL keys (names are proposals, not upstream):
- `render { backend "vulkan"; adapter "auto|<name>|<index>"; power-preference "high|low"; allow-software #true|#false; present-mode "auto|fifo|mailbox|immediate"; frame-latency 1; surface-alpha "auto|opaque|premultiplied" }`.
- `window { opacity 0.85; blur #true }` implies `surface-alpha premultiplied` when opacity < 1 or any glass layer is transparent; settings GUI should show the negotiated mode read-only and warn when the driver offers only Opaque.
- `media { zero-copy-dmabuf #true }`: off automatically when the DMA_BUF feature/extension is missing.

Actions (shared vocabulary): `render-restart` (rebuild device and surface), `render-info` (copy adapter, backend, alpha mode, present mode, software flag, device-lost count to clipboard), `toggle-damage-overlay` (from the gpu-latency dossier).

IPC: `render.status` returns `{adapter, driver, device_type, backend, alpha_modes[], present_modes[], negotiated_alpha, negotiated_present, dmabuf_import, device_lost_count}`; event `render.device-lost {reason, message}` and `render.recovered`.

## Performance (numbers + method)

- No published wgpu-on-Wayland dmabuf-import or device-recovery timings were found (unverified; must be measured in-house).
- Software path (lavapipe) cost unmeasured. Plan a benchmark: fixed 200x50 grid, `vtebench`-style scroll workload at 1x and 2x scale, force `VK_ICD_FILENAMES` to lavapipe, compare frame time to a hardware adapter. Acceptance threshold to be set by owner (suggest: sustains 60 Hz with idle redraw below 5% CPU, and <33 ms worst frame at 1080p).
- Device-recovery budget: define as time from `lost` callback to first valid frame; target under 500 ms (proposal, not measured).
- dmabuf import cost per frame: expected sub-millisecond for single-plane (image create, memory import, bind); cache imported textures by (dev, ino, modifier) key to avoid per-frame import (technique; unverified quantitatively).

## What Apotheosis should take

1. Pin wgpu 30.x (30.0.2), `default-features = false`, features `vulkan` + `wgsl` (+ `std`); disable `gles`. Plan an ADR for the 31 migration (feature flags removed).
2. Request `VULKAN_EXTERNAL_MEMORY_FD | VULKAN_EXTERNAL_MEMORY_DMA_BUF` optionally: intersect with `adapter.features()`; run the media path only when present; fall back to CPU upload of decoded frames otherwise.
3. Explicitly request `CompositeAlphaMode::PreMultiplied` when any transparency is wanted; fall back to `Opaque` with a visible in-app notice. Never rely on `Auto` for transparency. Query `alpha_modes` at runtime and surface it in settings and `render.status`.
4. Premultiply every colour written to a transparent surface (clear colour, glass fill, atlas blending: premultiplied-alpha blend state `One, OneMinusSrcAlpha`), including the first clear. This is the Rio 1967 lesson.
5. Keep all GPU resources reconstructible: CPU-side glyph atlas source data, image cache keyed by id, pipeline descriptors as data. Install the device-lost callback at device creation; on loss, rebuild adapter/device/queue/surface and replay.
6. Handle every `CurrentSurfaceTexture` variant explicitly (Outdated/Suboptimal: reconfigure; Lost: recreate surface; Occluded/Timeout: skip frame).
7. Make software rendering an explicit, logged mode: first try hardware adapters, then `force_fallback_adapter`, then error with a clear message. Setting `allow-software` default true with a toast "software rendering, glass effects reduced".
8. Use imported dmabuf textures with a real `initial_state`, plus an external-semaphore wait for producer sync.
9. Query present modes from the surface and prefer Mailbox else Fifo (frame latency 1 as in the gpu-latency dossier); allow Immediate only when offered.
10. Keep Mesa/driver facts in an ADR with the Mesa path cited: Wayland WSI supports `OPAQUE | PRE_MULTIPLIED`.

## What it should avoid

- Do not plan on presenting a wgpu texture as a `wl_buffer` or exporting dmabuf from wgpu. If a design (e.g. nested video overlay subsurface) needs export, treat it as a separate, `unsafe` ash-level feature decision.
- Do not rely on `Auto` composite alpha, `PostMultiplied` or `Inherit` on Wayland: Mesa does not offer them; NVIDIA behaviour unverified.
- Do not assume multi-plane dmabuf import (NV12/P010); the hal function rejects anything but one plane by design.
- Do not enable the `gles` backend: pulls libEGL/wayland-egl (not on the allowed list) and `wayland-sys` for egl.
- Do not assume device-lost recovery is provided by wgpu; it is not. Do not use the `device_lost_panic` feature outside tests.
- Do not treat `Result::Ok` after a lost device as proof of health; rely on the callback.
- Do not trust the Rio diagnosis as a wgpu defect; it is native Vulkan and unconfirmed.
- Do not claim lavapipe is acceptable until measured.

## Sources

Source clone (shallow, main as of 2026-11-12, version 30.0.0 + unreleased): `/home/risk/.cache/apotheosis-prior-art/wgpu`. Published-crate checks used extracted `wgpu-hal-30.0.2` and `wgpu-types-30.0.2` tarballs from static.crates.io.

- wgpu `wgpu-hal/src/vulkan/device.rs` : `texture_from_dmabuf_fd`, `import_dmabuf_memory`, `texture_from_raw`.
- wgpu `wgpu-hal/src/vulkan/adapter.rs` : feature derivation for `VULKAN_EXTERNAL_MEMORY_FD/DMA_BUF`, optional extension enabling.
- wgpu `wgpu-hal/src/vulkan/instance.rs` : `create_surface_from_wayland` (`VK_KHR_wayland_surface`).
- wgpu `wgpu-hal/src/vulkan/conv.rs` : `map_composite_alpha_mode`, `map_vk_composite_alpha`.
- wgpu `wgpu-hal/src/vulkan/mod.rs` : `map_host_device_oom_and_lost_err`, `get_lost_err`.
- wgpu `wgpu-types/src/surface.rs` : `CompositeAlphaMode` docs; `wgpu-types/src/features.rs` : feature flags; `wgpu-types/src/adapter.rs` : `DeviceType`, `force_fallback_adapter`; `wgpu-types/src/lib.rs` : `DeviceLostReason`.
- wgpu `wgpu-core/src/device/surface_config.rs` : alpha-mode validation and Auto fallback; `wgpu-core/src/device/resource.rs` : `Device::lose`; `wgpu-core/src/instance.rs` : `request_adapter` / `NotFound`.
- wgpu `wgpu/src/api/surface_texture.rs` : `CurrentSurfaceTexture`; `wgpu/src/api/device.rs` : `set_device_lost_callback`, `create_texture_from_hal`.
- wgpu `CHANGELOG.md` : v30.0.0 Vulkan section (texture_from_dmabuf_fd PR 9366; add_wait_semaphore PR 9461), v30.0.1 (2026-08-21), Unreleased (feature flag removal PR 10635; `create_texture_from_hal` initial_state note). https://github.com/gfx-rs/wgpu/blob/trunk/CHANGELOG.md
- wgpu `.deny.toml` : licence allow list.
- Mesa `src/vulkan/wsi/wsi_common_wayland.c` : `wsi_wl_surface_get_capabilities` (supportedCompositeAlpha), `wsi_wl_surface_get_present_modes`, swapchain creation (`alpha = compositeAlpha == PRE_MULTIPLIED`), `wl_drm_format_for_vk_format`. Cloned from https://gitlab.freedesktop.org/mesa/mesa.git (sparse, `/home/risk/.cache/apotheosis-prior-art/mesa-wsi`).
- Rio issue 1967: https://github.com/raphamorim/rio/issues/1967 (native Vulkan backend, unpremultiplied clear colour suspected; open, unconfirmed).
- NVIDIA forum, "Wayland Vulkan ignores VkCompositeAlphaFlagsKHR" (2022, supportedCompositeAlpha 0x1): https://forums.developer.nvidia.com/t/wayland-vulkan-ignores-vkcompositealphaflagskhr/202227 (old; current drivers unverified).
- Mesa issue 2988 "compositeAlpha on Wayland": https://gitlab.freedesktop.org/mesa/mesa/-/issues/2988 (page blocked by bot protection; only a search snippet seen, unverified).
- wgpu issue 5661 (Opaque-only alpha lists on some drivers): https://github.com/gfx-rs/wgpu/issues/5661 (search snippet only).
- crates.io API for wgpu, wgpu-hal, ash, gpu-allocator, gpu-descriptor, libloading, renderdoc-sys, hexf-parse, naga, half, spirv, petgraph, codespan-reporting, wayland-sys, drm: licence and rust_version fields.
- Vulkan spec `VkCompositeAlphaFlagBitsKHR`: https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/VkCompositeAlphaFlagBitsKHR.html
- Prior dossier: `topic-gpu-latency.md` (present-mode claims corrected above).
