# Research dossier: video and audio inside a terminal (topic-media)

Date of research: 2026-11-12. Crate versions and dates are from the crates.io API on that day. Prior-art rule applies: this document describes techniques in prose. Escape-sequence syntax, key names and config key names are interface facts and are quoted.

## Summary

- Terminal video today is a set of hacks over image protocols. mpv `--vo=kitty` sends every frame as a raw RGB24 kitty-graphics image (optionally through POSIX shared memory). `--vo=sixel` scales, dithers and encodes every frame on the CPU with libsixel. `--vo=tct` prints half-block characters. None carries audio, and none gives the terminal any clock: the player owns timing.
- Apotheosis should do two things. (1) Be an excellent receiver of the existing paths: complete kitty graphics including shared memory (`t=s`), animation (`a=f`/`a=a`/`a=c`), Unicode placeholders and z-index; a fast sixel decoder; so `mpv --vo=kitty --vo-kitty-use-shm=yes` and `--vo=sixel` run smoothly. (2) Add a native media protocol where a sandboxed helper process demuxes and decodes, the terminal owns the clock, and audio goes to PipeWire.
- Under our rules the Rust media stack is thinner than it looks. Verified on 2026-11-12:
  - AV1 software: `rav1d` 1.1.0 (BSD-2-Clause, last release 2025-05-07) is the only credible pure-Rust AV1 decoder. Its fast path is x86/ARM assembly built by `nasm-rs` (needs an external `nasm`) and `cc`. That conflicts with "no C/C++ compiled by our build" in spirit (nasm is not C, but it is a non-Rust toolchain in the build). This needs an owner decision. Without the `asm` feature it runs the Rust fallback only (slow; no figure found, unverified).
  - VA-API: `cros-codecs` 0.0.6 (BSD-3-Clause) plus `cros-libva` 0.0.13 (BSD-3-Clause) decode H.264, H.265, VP8, VP9, AV1 through libva. Both are 0.0.x. `cros-libva` runs bindgen at build time (needs libclang). `cros-codecs` with its `backend` feature pulls `gbm` and `gbm-sys`, which link libgbm. libgbm is not on the allowed system-library list: flag.
  - Vulkan Video: `gpu-video` 0.5.0 (MIT, formerly `vk-video`) decodes H.264 straight into wgpu textures. H.265 and AV1 decode are "not supported yet, planned". Vulkan drivers are an allowed dependency, so this is the cleanest zero-copy path for H.264 only.
  - Audio decode: `symphonia` 0.6.1 (MPL-2.0, allowed) covers AAC-LC, Vorbis, FLAC, MP3, ALAC, PCM and the MKV/WebM, MP4 and Ogg demuxers. Its README lists Opus, but there is no `symphonia-codec-opus` on crates.io (404). Use `opus-decoder` 0.1.1 (MIT OR Apache-2.0, `forbid(unsafe)`, RFC 8251 vectors) instead.
  - Audio output: `pipewire` 0.10.1 and `libspa` (MIT). Their README says "work-in-progress... expect frequent breakage". They need libpipewire dev files and clang for bindgen.
- Recommended A/V sync: audio is the master clock, taken from PipeWire stream timing. Video frames are queued with presentation timestamps and presented on the Wayland frame/presentation feedback cadence. Late frames are dropped and early ones held.

## Architecture (how it is built)

### Existing terminal video paths

mpv kitty VO (source: mpv `video/out/vo_kitty.c`)
- mpv scales to the window pixel size, draws the OSD into the same buffer, and emits one graphics command per frame. It uses `a=T` (transmit and display), `f=24` (raw RGB), `s`/`v` pixel size, `C=1` (do not move the cursor), `q=2` (suppress replies). Before each frame it moves the cursor to the configured top/left.
- Normal path: base64 the whole frame and send it in 4096-byte slices with `m=1` on all but the last. mpv does not use zlib (`o=z`).
- Shared-memory path (`t=s`): creates a fresh POSIX shm object per frame, sends its base64-encoded name, waits for the terminal swapchain to drain, then unmaps and unlinks it. The terminal reads and unlinks the object per the kitty spec.
- Startup: hide cursor, optional alternate screen, enable mouse. Query terminal size in cells and pixels. On resize, delete all images (`a=d`), optionally clear, recompute rectangles. Shutdown deletes images and restores state.
- A multiplexer passthrough option wraps each command in the tmux or GNU screen DCS envelope when `TMUX` or `STY` is set.
- Observation from reading the source (my reading; unverified against real terminals): when a payload fits in one slice the first header carries `m=1` and no closing `m=0` packet is sent, so receivers should be lenient about unterminated single-chunk transfers. A tolerant parser costs nothing.

mpv sixel VO (source: mpv `video/out/vo_sixel.c`)
- Per frame: mpv scales into packed RGB24, composes the OSD, then libsixel quantises and encodes. The canvas is cut into horizontal slices aligned to cell rows and 6-pixel sixel bands. Each slice is dithered and encoded on a worker thread. Each slice is dithered with extra context rows above and below, which are discarded, to hide seams from error diffusion.
- Two palette modes: fixed XTERM256 (default) or a per-frame median-cut palette with a scene-change threshold. The canvas height is rounded down to a multiple of 6 and one row is kept free for status, so the image never exactly fills the window.
- Cost is a full scale plus a full dither plus an encode of the whole frame, per frame. No frame-rate figure is documented.
- mpv's sixel driver is LGPL 2.1+ (derived from an FFmpeg sixel device) and links libsixel: not usable as code here.

mpv tct VO
- Prints Unicode half blocks (`half-blocks`) or plain spaces (`plain`) with truecolor or 256-colour SGR. Works in any terminal and survives multiplexers and scrollback. `--vo-tct-256=yes` for 256-colour terminals.

timg, chafa, notcurses (image/video viewers)
- timg: auto-detects kitty, iTerm2 and WezTerm, supports sixel, falls back to half-blocks or quarter-blocks. Video decode is via ffmpeg/libav. It can skip frames on slow links (`TIMG_ALLOW_FRAME_SKIP=1`) and decodes on multiple threads. (Licence not read in this session: unverified.)
- chafa: C library plus CLI, LGPLv3+, animated GIF, optional codec libraries. Cannot be linked here (GPL-family, C).
- notcurses: Apache-2.0, C. Blitters: braille, sextants (Unicode 13), octants (Unicode 16), pixel via sixel or kitty. Uses a bitmap blitter only when the terminal positively reports support. `ncvisual_blit` degrades to the best available blitter unless forced. Rendered mode keeps screen state and emits minimal updates. Multimedia goes through FFmpeg.
- The shared idea worth copying: capability-ladder degradation (pixel protocol, then octants/sextants/quadrants/braille, then half-blocks) decided by positive detection, not guessing.

### Kitty graphics, the receive side (source: https://sw.kovidgoyal.net/kitty/graphics-protocol/)

- Transmission media: direct (base64 chunks of at most 4096 bytes, all but the last a multiple of 4, `m=1`/`m=0`), file `t=f`, temporary file `t=t`, shared memory `t=s` (POSIX shm name, `S` size and `O` offset keys, reader unlinks).
- Safety rules in the spec: only regular files, no named pipes or devices, may refuse `/proc`, `/sys`, `/dev`; `t=t` deletes only inside a known temp directory and only if the path contains `tty-graphics-protocol`; all file-read failures must return the same error response (no oracle).
- Compression: `o=z` RFC 1950 zlib, applied before base64.
- Query and ack: `a=q` with `i`; support detection by sending the query followed by a primary device attributes request; `e=<action>` feature query; placement ack `i=<id>;OK` or `ENOENT`.
- Z-index `z`: negative draws under text; below -1,073,741,824 draws under cells with non-default background; equal z tie-breaks by lower id.
- Unicode placeholders: `a=p` with `U=1`, `c`, `r`; the cell grid then carries U+10EEEE with row/column diacritics and the image id in the foreground colour (placement id in underline colour). Images therefore scroll, reflow and copy with text, and work inside tmux.
- Animation: frames added with `a=f` (keys `x`,`y`,`s`,`v` for sub-rectangle, `c` base frame, `r` edit frame, `X=1` overwrite instead of blend, `Y` background RGBA, `z` gap in ms; default gap 40 ms, gapless frames are skipped in display but usable as bases). Control with `a=a`: `s=1` stop, `s=2` run in loading mode (wait at last frame), `s=3` run normally; `v` loop count (`v=1` infinite, N plays N-1 loops); `r`+`z` change a frame's gap; `c` makes a frame current (client-driven animation). Compose with `a=c` (`r` source, `c` destination, `w`/`h`, `X`/`Y` source offset, `x`/`y` destination offset, `C=1` replace). Errors: ENOENT, EINVAL, ENOSPC. Kitty stores animation frame data on disk with a separate quota five times the base quota.

### Native media: proposed architecture for Apotheosis (design, not prior art)

1. Terminal side: a media surface object owned by a pane, with a rectangle in cell coordinates, z-order relative to text, and a lifetime tied to the pane. Created by an escape sequence (new, namespaced under our own APC or OSC code) or by an Action (`media-open`).
2. Helper side: a separate `apotheosis-media` process per surface (or per terminal window), started under Landlock plus seccomp (the repo already has this pattern in `ec-agentd` sandbox.rs). It receives a file descriptor (opened by the terminal; the helper has no filesystem access) or a stream, demuxes, decodes, and returns frames and audio.
3. Frame hand-off: over a unix socket with SCM_RIGHTS. Hardware path: dmabuf plus DRM format modifier, imported into Vulkan. Software path: memfd with NV12/I420 planes. Compressed packets never reach the main process.
4. Rendering: wgpu samples NV12 planes (or imported dmabuf) with a YUV-to-RGB shader in the same pass as the grid, so video composes under or over glass UI with correct alpha. dmabuf import into wgpu is done through wgpu-hal Vulkan `texture_from_raw` with `VK_KHR_external_memory_fd` and `VK_EXT_image_drm_format_modifier` (recipe sketched in a gist; no verified end-to-end example found: unverified).
5. Audio: helper decodes to f32 PCM into a shared ring buffer; a PipeWire playback stream in the main process (or in the helper if sandboxing allows the PipeWire socket) drains it. The PipeWire stream clock is the master.
6. Control: pause, seek, rate, volume, track select, loop, all as Actions on a surface id; events (`ended`, `error`, `position`) feed the rules engine.

## Feature inventory (grouped by category)

Graphics (receive side)
- Kitty graphics: direct, file, temp file, shared memory transmission; PNG and raw RGB/RGBA (`f=24`, `f=32`, `f=100`); zlib; chunking; query; z-index; Unicode placeholders; animation; delete actions (`a=d`).
- Sixel: DCS decoding with raster attributes, palette registers, repeat introducer, 6-pixel bands. Used by `--vo=sixel`.
- iTerm2 inline images (OSC 1337 `File=`): used by timg and others (not verified in this session; listed as interface fact from memory: unverified).
- Cell-based blitters (half-block, quadrant, sextant, octant, braille): need pixel-perfect block-element glyph rendering in the terminal.

Media
- mpv `--vo=kitty`, `--vo=sixel`, `--vo=tct`, with `--vo-kitty-use-shm`.
- Native media protocol (this project), AV1 software decode (rav1d), VA-API hardware decode (cros-codecs), Vulkan Video decode (gpu-video, H.264), PipeWire audio.
- Containers: Matroska/WebM, ISO MP4, Ogg. Audio codecs: Opus, Vorbis, AAC-LC, FLAC, MP3.

Performance
- Zero-copy shm into a GPU staging buffer; dmabuf import; frame dropping; per-surface decode threads; bounded queues.

Security
- Sandboxed decoder helper; strict file/shm transmission policies; per-pane memory quotas; no-oracle error replies.

Mux
- DCS passthrough for tmux and screen (mpv `--vo-kitty-auto-multiplexer-passthrough`); Unicode placeholders as the multiplexer-safe image carrier.

## Control surface (config, actions, IPC)

Interface facts (mpv): `--vo-kitty-cols`, `--vo-kitty-rows`, `--vo-kitty-width`, `--vo-kitty-height`, `--vo-kitty-left`, `--vo-kitty-top`, `--vo-kitty-config-clear`, `--vo-kitty-alt-screen`, `--vo-kitty-use-shm` (default no; local only, "not over SSH"), `--vo-kitty-auto-multiplexer-passthrough`. Sixel: `--vo-sixel-dither` (auto, none, atkinson, fs, jajuni, stucki, burkes, arithmetic, xor), `--vo-sixel-fixedpalette`, `--vo-sixel-reqcolors` (256), `--vo-sixel-threshold`, `--vo-sixel-pad-x`, `--vo-sixel-pad-y`. tct: `--vo-tct-algo` (half-blocks, plain), `--vo-tct-256`.

Terminal size queries mpv-style players rely on: cell and pixel size reporting via XTWINOPS (`CSI 14 t`, `CSI 16 t`, `CSI 18 t`) and the ioctl `TIOCGWINSZ` pixel fields. Apotheosis must report accurate pixel sizes, including under fractional scaling.

Proposed Apotheosis surface (proposal only; names are not decided):
- KDL: `media { graphics-protocols "kitty" "sixel" "iterm2"; shm-transmission "local-only"; max-image-memory "512MiB"; animation-quota "2560MiB"; helper-sandbox true; hwdec "auto" /* vaapi | vulkan | none */; av1-software true; audio-output "pipewire"; sync-master "audio"; frame-drop "late" }`, plus per-rule overrides.
- Actions: `media-open`, `media-play-pause`, `media-seek`, `media-rate`, `media-volume`, `media-mute`, `media-track`, `media-close`, `graphics-clear`, `graphics-toggle` (all with a surface selector).
- IPC and CLI: `media.list`, `media.open`, `media.control`, `media.state`, event stream `media.position`, `media.ended`, `media.error`; `apotheosis msg media ...`.
- Rule events: `media-started`, `media-ended`, `media-error`, `graphics-quota-exceeded`.

## Performance (numbers + method)

Measured by others (verified from sources, method stated):
- rav1d vs dav1d: "under 6%" slower on x86_64 now, peak overhead was about 11% when optimisation began; initial c2rust output was 3.8% slower with all assembly enabled, executing about 7% more instructions, of which bounds checks were about 2%. Rust code accounts for "almost exactly half" of decode time even with every assembly routine enabled. Method: hyperfine on the dav1d CLI, Chimera 8-bit 1080p clip, 8 threads, Ryzen 7700X, dav1d built with Clang 18. Source: https://www.memorysafety.org/blog/rav1d-performance-optimization/. A separate May 2025 bounty post reports about 5%; an independent M3 test reports about 5% on average and 9% on one file.
- Symphonia: decoders generally within about ±15% of FFmpeg, single thread, FFmpeg libs statically linked via a purpose-built `ffbench`; SIMD flags `opt-simd-sse`, `opt-simd-avx`, `opt-simd-neon` pull in `rustfft`. Benchmark file last updated 2023-02-03.
- mpv vo=kitty and vo=sixel frame rates: no published numbers found (unverified).

Arithmetic you can rely on (computed here, not measured):
- 1920x1080 RGB24 is 6,220,800 bytes per frame; base64 expands it by 4/3 to 8,294,400 bytes. At 30 fps that is about 187 MB/s raw, about 249 MB/s base64. At 60 fps double that. This is why the pty path is the bottleneck and why `t=s` matters.
- 4096-byte slices mean about 2,025 APC commands per 1080p frame on the direct path, each with a header. The parser needs an APC fast path that does not allocate per chunk.
- NV12 1080p is 3,110,400 bytes (half of RGB24), I420 the same.

Targets Apotheosis should set (proposals, to be measured): 1080p30 shm kitty playback with under 15% of one core in the terminal process; zero per-frame heap allocation on the media render path; dmabuf path with no CPU copy; A/V offset within 20 ms steady state.

Method to measure: replay a fixed clip through `mpv --vo=kitty --vo-kitty-use-shm=yes` in a headless Abyss session, record terminal frame-present times (`wp_presentation` feedback), PTY bytes/s, and CPU via `perf stat`; compare against kitty, foot and Ghostty on the same host.

## What Apotheosis should take

1. Full kitty graphics, including shm and animation, with the spec's safety rules (regular files only, no-oracle errors, temp-dir restriction).
2. A quota model for images and animation frames (kitty uses a 5x quota for animation storage on disk). Do it in GPU/host memory with an explicit KDL setting and LRU eviction.
3. A fast, allocation-free APC and DCS path with a lenient chunk parser (see the mpv single-chunk observation).
4. A sixel decoder that works in 6-row bands, uploads to a texture incrementally, and handles palette registers; mpv's slice-with-context-rows trick informs encoders, not decoders.
5. Positive-detection capability ladder for apps (XTWINOPS pixel sizes, kitty `a=q`, DA1 sentinel).
6. Native media protocol with sandboxed helper (see Architecture), exposed through the shared Action vocabulary.
7. Decode stack (see crate list below), hardware first, `rav1d` as software AV1 fallback.
8. Audio-master A/V sync on the PipeWire stream clock, video dropped when late.
9. Unicode placeholders so images behave in splits, scrollback and multiplexers.

### Concrete crate list

| Purpose | Crate | Version (2026-11-12) | Licence | Notes |
|---|---|---|---|---|
| Matroska/WebM demux | `matroska-demuxer` | 0.8.1 | Zlib OR MIT OR Apache-2.0 | updated 2026-08-07; pure Rust |
| MP4 demux | `symphonia-format-isomp4` | 0.6.1 | MPL-2.0 | maturity "Great"; whether it exposes video packets is unverified, check before relying on it |
| MP4 demux (alt) | `mp4` | 0.14.0 | MIT | last release 2023-08-01, stale |
| MP4 demux (alt) | `mp4parse` | 0.17.0 | MPL-2.0 | last release 2023-05-29, parser only |
| Audio demux and decode | `symphonia` (features `aac`, `isomp4`, `mkv`, `ogg`, `vorbis`, `flac`, `mp3`) | 0.6.1 | MPL-2.0 | "100% safe Rust" per README; HE-AAC unclassified |
| Opus decode | `opus-decoder` | 0.1.1 | MIT OR Apache-2.0 | `forbid(unsafe_code)`, 12 RFC 8251 vectors; port of libopus algorithms |
| Resampling | `rubato` | 5.1.0 | MIT OR Apache-2.0 | |
| PipeWire output | `pipewire` + `libspa` | 0.10.1 | MIT | needs libpipewire dev files and clang (bindgen) |
| AV1 software | `rav1d` | 1.1.0 | BSD-2-Clause | `asm` feature needs `nasm`; owner decision |
| VA-API decode | `cros-codecs` (+ `cros-libva`) | 0.0.6 (+ 0.0.13) | BSD-3-Clause | H.264, H.265, VP8, VP9, AV1; libva at least 1.20; bindgen; pulls gbm |
| Vulkan Video decode | `gpu-video` | 0.5.0 | MIT | H.264 only; wgpu textures |
| GPU | `wgpu` | 30.0.2 | MIT OR Apache-2.0 | |
| fd and mmap plumbing | `rustix`, `memmap2` | 1.1.6, 0.9.12 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT; MIT OR Apache-2.0 | |
| Wayland | `wayland-client`, `wayland-protocols` | 0.31.15, 0.32.13 | MIT | for linux-dmabuf, presentation-time |

Decisions needed from the owner:
- rav1d assembly: allow `nasm-rs` in the build, or accept Rust-only AV1 software decode (slow), or restrict AV1 to hardware (VA-API / future Vulkan Video).
- libgbm: cros-codecs `backend` feature pulls `gbm`/`gbm-sys`; libgbm is not on the allowed list. Either extend the list or fork the decoder to avoid gbm (it uses it for buffer allocation).
- bindgen and libclang at build time for `cros-libva`, `libspa-sys`, `pipewire-sys`: bindgen is Rust, but libclang is a build-time C++ dependency. Generated bindings could be vendored and committed to avoid libclang in the build.

## What it should avoid

- `rav1d-safe` 0.6.0: licence is AGPL-3.0-only OR a commercial licence: not in our allowed list.
- `dav1d` bindings, `ffmpeg-next`, `libsixel`, `libopus` bindings (`audiopus`): C, disallowed.
- chafa (LGPLv3+, C) and mpv's sixel code (LGPL 2.1+): do not link or transliterate.
- `symphonia-codec-opus`: not published on crates.io as of today despite the README row; do not plan on it.
- Decoding untrusted media in the terminal's main process: decoders are the highest-risk parsers we ship.
- Per-frame fresh shm objects as the internal design (mpv does this for kitty compatibility; for our own protocol use a persistent ring of memfds).
- Over-trusting `pipewire-rs` API stability: pin the version and wrap it behind a small audio trait.
- Treating `cros-codecs` 0.0.x as stable: wrap it behind our own decoder trait and keep a software fallback.
- Allowing `t=f`/`t=t`/`t=s` from remote (SSH) sessions by default: shm and local files are meaningless remotely, and a hostile remote could probe the local file system.

## A/V sync technique (design)

- Master clock: audio. Position equals the stream's presentation timestamp of the last sample actually played, derived from PipeWire stream timing (`pw_stream_get_time_n`, which returns a `pw_time` that includes graph-level delay, queued and buffered amounts). The PipeWire docs only point to these functions without describing fields (https://docs.pipewire.org/page_streams.html); the usage of `delay` and `queued` as latency terms is from the struct's documentation and is unverified here.
- Video: each decoded frame carries a PTS. On every Wayland frame callback, compute the target PTS as audio clock plus expected display latency (from `wp_presentation` feedback or the frame callback time). Present the latest frame whose PTS is at or below the target; drop earlier ones; hold if none.
- Drift handling: if the video queue runs empty, hold the last frame; if audio underruns, insert silence and re-anchor. Small steady drift is corrected by gently adjusting the audio resampler ratio (rubato supports ratio changes) rather than skipping samples.
- Seek: flush both queues, decode from the previous keyframe, discard output until PTS reaches the seek target, then start the audio and video clocks together.
- mpv's display-sync mode (`--video-sync=display-resample`) resamples audio to match the display refresh instead; the manual excerpt fetched here only showed the example profile, not the mode list: unverified in detail.
- Real-time rule: the PipeWire `process` callback with `PW_STREAM_FLAG_RT_PROCESS` runs on a realtime thread where non-realtime calls are unsafe (docs.pipewire.org). Keep it to a lock-free ring-buffer read.

## Sources

- mpv option reference (vo): https://raw.githubusercontent.com/mpv-player/mpv/master/DOCS/man/vo.rst
- mpv upstream path `video/out/vo_kitty.c` (function-level behaviour: draw_frame, flip_page, reconfig, uninit) and `video/out/vo_sixel.c` (slice and dither worker logic)
- kitty graphics protocol: https://sw.kovidgoyal.net/kitty/graphics-protocol/
- rav1d repository README and Cargo.toml: https://github.com/memorysafety/rav1d ; build script `build.rs` (nasm-rs, cc); `src/rust_api.rs`
- rav1d performance article: https://www.memorysafety.org/blog/rav1d-performance-optimization/ ; bounty: https://www.memorysafety.org/blog/rav1d-perf-bounty/ ; M3 test: https://ohadravid.github.io/posts/2025-05-rav1d-faster
- crates.io API entries for rav1d, cros-codecs, cros-libva, symphonia, matroska-demuxer, mp4, mp4parse, pipewire, libspa, cpal, wgpu, opus-decoder, opus-rs, ruopus, rav1d-safe, gpu-video, rubato, rustix, memmap2, wayland-client, wayland-protocols
- cros-codecs README and Cargo.toml: https://github.com/chromeos/cros-codecs ; cros-libva README, `lib/Cargo.toml`, `lib/build.rs`: https://github.com/chromeos/cros-libva
- Symphonia README and BENCHMARKS.md: https://github.com/pdeljanov/Symphonia
- pipewire-rs README, `libspa-sys/Cargo.toml`, `pipewire-sys/Cargo.toml`: https://gitlab.freedesktop.org/pipewire/pipewire-rs
- PipeWire stream docs: https://docs.pipewire.org/page_streams.html
- gpu-video README: https://github.com/software-mansion/smelter/tree/master/gpu-video
- opus-decoder docs: https://docs.rs/crate/opus-decoder/0.1.1
- timg: https://github.com/hzeller/timg ; chafa: https://github.com/hpjansson/chafa ; notcurses: https://github.com/dankamongmen/notcurses
- wgpu dmabuf import sketch: https://gist.github.com/i509VCB/649d3b62f23a458ef18d3f2f4731eca7 ; Dawn dmabuf wrapping tests (Vulkan): https://dawn.googlesource.com/dawn
- Not verified in this session: timg licence, iTerm2 OSC 1337 details, mpv `--video-sync` mode list, `symphonia-format-isomp4` video packet exposure, rav1d Rust-only fallback speed, mpv kitty/sixel frame rates, end-to-end VA-API surface to wgpu texture recipe.
