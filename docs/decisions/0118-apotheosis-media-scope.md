# 0118 — Media: three tiers, audio over the PulseAudio protocol, nasm at build time
Status: accepted
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
The owner asked for video in three tiers plus audio. Research findings: pure-Rust AV1 decoding (`rav1d`) is usable but needs its x86-64 assembly to be fast, which `nasm` assembles; `pipewire-rs` compiles C and runs bindgen; a pure-Rust `pulseaudio` protocol crate exists and `pipewire-pulse` is standard; `cros-libva` needs bindgen unless bindings are committed; software H.264/HEVC in pure Rust is immature.

## Options
1. Tier A only (kitty animation; mpv does the decoding). No audio, no native placements.
2. Link FFmpeg. Everything plays; violates the language rule wholesale.
3. Three tiers under the rule: A kitty animation with a named mpv gate; B native placements with a sandboxed helper, `rav1d`, and audio through a pure-Rust PulseAudio client; C VA-API through vendored `cros-libva`/`cros-codecs` with committed bindings.

## Decision
Option 3 (T-16). `nasm` is permitted at **build time only** (`makedepends`), solely to assemble `rav1d`'s x86-64 routines in the helper crate behind the `av1-asm` feature (on in packaging, off in dev and default CI). PipeWire comes off the system-library allow-list; `libva-drm` joins it. All untrusted demux and decode runs in `apoth-media-helper` under Landlock and seccomp: the helper initialises first (vaInitialize on the passed render fd), then sets NO_NEW_PRIVS, an empty-view Landlock ruleset and seccomp, with a per-driver-family DRM ioctl table; it fails closed (`sandbox-unavailable`) if Landlock is not fully enforced or seccomp fails; frame metadata goes over the socket, never the ring (T-16 §4). Software H.264 is admitted only after passing the helper's fuzz and conformance gates; software VP9/HEVC are not planned.

## Consequences
Without VA-API, H.264/HEVC/VP9 play only through Tier A. Two vendored forks to maintain (`VENDOR.md` records the diffs). MP4 demux needs a spike at M34. Media is R2: it does not gate the default switch.

## Revisit when
A pure-Rust decoder for H.264/HEVC matures, or `rav1d` gains competitive pure-Rust SIMD (then the `nasm` exception is deleted).
