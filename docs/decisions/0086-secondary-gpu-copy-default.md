# 0086 — Outputs on a secondary GPU get a copy by default; native compositing is opt-in
Status: accepted
Date: 2026-11-04
Deciders: chase (owner), Claude (advisory)

## Context

ADR 0084-hybrid-gpu-per-gpu-compositing.md has each GPU composite its own
outputs and copies only as a per-frame fallback. On an Intel + NVIDIA laptop
with HDMI wired to the NVIDIA card, abyss logged
`output composited on card0; render-on-primary copy kept as the fallback`, then
`GL_INVALID_OPERATION ... <image> and <target> are incompatible`, and the HDMI
screen showed random colours and lines. The NVIDIA context accepted the import
probe for Intel client dmabufs (likely a tiled modifier) but could not sample
them, so the fallback never triggered.

Mutter, KWin, wlroots, niri and Hyprland all render on the primary GPU and
copy or blit to outputs on another GPU. Native compositing on a secondary
depends on that driver sampling another vendor's buffers, which is the least
reliable path in the stack.

## Options

1. Keep 0084 and harden the import probe. Cannot be verified without every
   driver pair, and the probe already lies.
2. Render on the primary and copy by default; keep native compositing as an
   opt-in for hardware where it works.
3. Remove native compositing. Loses the dGPU-does-real-work case for people who
   want it and whose drivers cope.

## Decision

Option 2. In `integrated` and `hybrid`, an output on a secondary GPU is
rendered by the primary into a transfer buffer both GPUs understand and
blitted by the output's own GPU, exactly as `dedicated` already does. The new
`misc.secondary-compositing` (bool, default `#false`, restart) restores 0084's
native compositing and its per-frame copy fallback. If no transfer format is
shared with the primary, the output falls back to native compositing with a
warning rather than staying dark. Single-GPU systems and primary-GPU outputs
are unchanged.

This supersedes the clause of ADR 0084 (the file
0084-hybrid-gpu-per-gpu-compositing.md, not 0084-themes.md) that says each
GPU composites its own outputs, and the COMP-01 §4 text that repeated it. The
rest of 0084 (modes, primary selection, app offload) stands.

Client buffers: an output that is copied never samples client buffers on the
secondary, so its surfaces are advertised the primary's formats only, with no
scanout tranche. Where native compositing is on, a surface keeps the
shared-format (LINEAR in practice) tranche, built only from formats every
active GPU can import. A secondary's failure to import the transfer buffer is
sticky for the output (CPU copy), logged once at warn.

## Consequences

- HDMI on the NVIDIA card works without depending on cross-vendor sampling.
- The dGPU no longer composites by default, which also helps its idle power.
- Each secondary frame costs a copy; no direct scanout on those outputs (as
  already the case in `dedicated`).
- Not verified on the Intel + NVIDIA laptop; the dev box has one GPU.
- A native frame that falls back to copy remains per-frame, not sticky, so it
  resumes once the client reallocates (0084 behaviour, opt-in only).

## Revisit when

- Copy cost shows up in frame times on real hardware.
- smithay's `MultiRenderer` becomes cheap to adopt (0084's revisit trigger).

## Amendment 2026-11-05 — import-scanout first, no EGLImage on the secondary

Hardware report (Intel iGPU primary, NVIDIA secondary, HDMI on the NVIDIA):
every secondary frame logged `rendering secondary frame` followed by
`GL_INVALID_OPERATION ... EGLImage not supported`. The copy step imported the
primary's transfer dmabuf as a texture on the secondary's GL context
(`import_dmabuf` -> `glEGLImageTargetTexture2DOES`). smithay 0.7.0 does not
check `glGetError` after that call (and none after
`glEGLImageTargetRenderbufferStorageOES` when binding a buffer), so
`import_dmabuf` returned `Ok`, the "sticky CPU copy" trigger (an `Err` from the
import) could never fire, and the bad texture was drawn or the frame failed
every time.

A secondary output now takes one of three routes, decided once at bring-up and
logged once per output at info (`secondary output route`, field `route`):

1. `import-scanout` (preferred). The primary allocates a LINEAR buffer on its
   own GBM, renders the whole output into it with its own renderer, and the
   secondary's KMS imports it (PRIME fd import, `drmModeAddFB2`) for scanout.
   The secondary's GL context is never used. Implemented as a `DrmCompositor`
   whose allocator is the primary's and whose framebuffer exporter is the
   secondary's (no import node, so every buffer is foreign). Constructing it
   allocates, imports and test-commits a buffer: that is the probe. The
   intersection of formats is LINEAR ARGB/XRGB/ABGR/XBGR8888 the primary can
   render and the plane takes. Client buffers are never scanned out directly
   on such an output and dmabuf feedback carries the primary's formats only.
2. `cpu-copy`. The primary renders, `glReadPixels` reads the frame back, the
   secondary uploads it as a plain texture. Chosen when the probe fails (one
   warn with the reason), and sticky-demoted to at runtime after 3 consecutive
   frames that cannot be queued (one warn). Never promoted back until restart.
3. `own-gpu`. Primary-GPU outputs (so every single-GPU output, unchanged), a
   secondary with `secondary-compositing` on, or a secondary with no usable
   shared format outside `dedicated`.

The dmabuf-texture copy variant is removed: no EGLImage is created on a
secondary GL context. `dedicated` follows the same table. No config key
changes. Unverified on the cross-GPU laptop; see the hardware test plan in the
change that introduced this amendment.
