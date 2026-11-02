# 0084 — hybrid GPU: one primary renders, secondary-GPU outputs get a copy

Status: accepted
Date: 2026-11-02
Deciders: chase (owner), Claude (advisory)

## Context

COMP-01 §4 makes multi-GPU a v1 non-goal. Outputs on a non-selected device
stay dark, and hybrid dGPU laptops are deferred to an "experimental later"
release. The spec's v1 fallback for a hybrid laptop is a refusal to start
when the connectors are only on another device. That refusal does not fire
on a real hybrid laptop. The target machine is an Acer with an Intel Raptor
Lake iGPU (boot VGA, eDP-1 built-in panel, HDMI-A-1) and an NVIDIA RTX 4050
Mobile (HDMI-A-2):

- The ranking puts the discrete GPU first.
- The NVIDIA card has a connector (HDMI), so the zero-connector bail never fires.
- abyss came up on NVIDIA with the built-in panel silently dark.
- Pinning Intel with `misc.render-device` lights the panel but loses the
  NVIDIA HDMI port.

The owner needs EclipseOS working on this laptop now, and v1 is months
away. There is precedent for pulling a deferred item forward by ADR: ADR 0038
(part of Z-01) and ADR 0046 (the audit spine).

smithay 0.7.0 ships `GpuManager` / `MultiRenderer` for this. Every effect,
blur and animation shader in `ec-abyss-render` is written against
`GlesRenderer` directly (about 190 references). Moving them to the
`MultiRenderer` generics would be a large refactor of the render crate.

## Options

1. **Keep v1 scope.** Fix only auto-selection so the built-in panel always
   lights. The HDMI port on the dGPU stays dark until after v1.
2. **Adopt smithay `MultiRenderer`.** This is the upstream-shaped design. It
   costs a rewrite of every render-crate signature and custom shader seam.
3. **One primary render GPU, copy to secondary outputs.** The primary GPU
   renders every output's scene with the existing `GlesRenderer` code. An
   output on another GPU is rendered into an offscreen dmabuf on the primary
   (a format and modifier both devices accept, LINEAR in practice). That
   buffer is imported into a small blit `GlesRenderer` on the secondary
   device and scanned out there. A CPU copy is the fallback if the import
   fails.

## Decision

Option 3, on by default. It pulls COMP-01 §4's hybrid item forward ahead of
v1.

- **Primary GPU.** In auto mode the primary is the device driving a connected
  internal panel (eDP, LVDS, DSI). If no device has one, the primary is the
  `boot_vga` device. If neither tells them apart, the COMP-01 §4 ranking
  decides (discrete, then VRAM, then BDF). The primary is the dmabuf feedback
  `main_device`, the explicit-sync device and the capture renderer, so
  clients allocate on the iGPU by default. `prime-run` offloads a client to
  the dGPU, whose buffers the primary imports.
- **Secondary GPUs.** Every other KMS device with connectors is opened as a
  secondary. A secondary that fails to initialise is logged and skipped. It
  never stops startup.
- **Override chain.** `--render-device`, `ECLIPSE_RENDER_DEVICE` and
  `misc.render-device` still pin the primary. ADR 0033's refuse-on-miss rule
  is unchanged.

This amends:

- COMP-01 §4's hybrid paragraph.
- The last paragraph of ADR 0033. That paragraph says multi-GPU is out of
  scope and the resolver does no ranking. Ranking now lives in
  `backend/gpu.rs`, and auto mode picks a primary among several GPUs.

## Consequences

- Both GPUs' connectors light up. Hotplug, VT switch, mode, VRR, gamma and
  power control route to each output's own device.
- Every frame on a secondary-GPU output costs one extra full-output copy, and
  client buffers are never scanned out directly there. On a laptop's
  external port this cost is acceptable.
- With the iGPU as primary, the dGPU does no rendering, but abyss keeps its
  device open while it is a secondary. Whether it reaches runtime D3 with
  nothing on HDMI has not been measured.
- Runtime hot-add of a whole GPU (an eGPU) is not handled. It is logged and
  ignored.
- The installer adds `nvidia-prime` when a Turing+ NVIDIA GPU coexists with
  an Intel or AMD GPU.
- Owed: verification on the hybrid laptop once it is imaged, and an update
  to the multi-GPU hardware gate in `docs/STATUS.md` (COMP-01 §11).

## Revisit when

- Copying to secondary outputs is measurably too slow (missed frames on the
  dGPU output).
- dGPU power draw with nothing on HDMI matters.
- The render crate moves off concrete `GlesRenderer` for another reason,
  which would make `MultiRenderer` cheap.
