# 0084 — hybrid GPU: each GPU composites its own outputs, with a switchable mode

Status: accepted; the per-GPU compositing clause is superseded by 0086-secondary-gpu-copy-default.md
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

smithay 0.7.0 ships `GpuManager` / `MultiRenderer`. Every effect, blur and
animation shader in `ec-abyss-render` is written against `GlesRenderer`
directly (about 190 references). Moving them to the `MultiRenderer` generics
would be a large refactor of the render crate.

## Options

1. **Keep v1 scope.** Fix only auto-selection so the built-in panel always
   lights. The HDMI port on the dGPU stays dark until after v1.
2. **Adopt smithay `MultiRenderer`.** Upstream-shaped, with one GPU rendering
   and others receiving copies by default. It costs a rewrite of every
   render-crate signature and custom shader seam.
3. **One primary GPU composites everything, others scan out a copy.** This was
   built first. The owner rejected it: the dGPU contributes no compositing, and
   apps reach it only through `prime-run`.
4. **One `GlesRenderer` per GPU; each output is composited by the GPU that
   drives it.** Same Rust type throughout, so the render crate keeps its
   signatures. GPU objects cached on shared state become per renderer. Client
   buffers are shared across GPUs through dmabuf feedback, with a copy fallback.

## Decision

Option 4, plus automatic dGPU app launches and a user-selected mode. It pulls
COMP-01 §4's hybrid item forward ahead of v1.

**`misc.gpu-mode`** (`integrated`, `hybrid`, `dedicated`; default `hybrid`;
applied at next login):

| Mode | Compositing | Apps | dGPU |
|---|---|---|---|
| `integrated` | iGPU only; dGPU outputs stay dark | iGPU only | never opened, so it can power off |
| `hybrid` | each GPU composites its own outputs | iGPU unless the app asks for the dGPU | on while it drives an output or an app |
| `dedicated` | dGPU composites everything; iGPU outputs get a copy | all on the dGPU | always on |

**Primary GPU.** The primary is the dmabuf default-feedback `main_device`, the
explicit-sync device and the capture renderer. In `integrated` and `hybrid` it
is the device driving a connected internal panel, then the `boot_vga` device,
then the COMP-01 §4 ranking. In `dedicated` it is the discrete GPU.
`--render-device`, `ECLIPSE_RENDER_DEVICE` and `misc.render-device` still pin
it, and ADR 0033's refuse-on-miss rule is unchanged.

**Per-GPU compositing.** Each KMS device with connectors owns a `GlesRenderer`
and renders its own outputs. GPU objects are never shared between renderers:
compiled programs and textures are per EGL context. Capture keeps rendering on
the primary. A secondary that fails to initialise is logged and skipped. It
never stops startup.

**Client buffers across GPUs.** A surface's dmabuf feedback names the GPU of
the output it mostly occupies, and is resent when that changes. A fallback
tranche lists formats every active GPU can import (LINEAR in practice). If a
visible surface cannot be imported on an output's GPU, that output's frame is
rendered on the primary and copied across, and native compositing resumes once
the client reallocates.

**The copy path** from option 3 stays, for that fallback and for iGPU outputs
in `dedicated`. If the GPU receiving the copy cannot import the buffer, a CPU
copy is the last resort.

**Apps on the dGPU.** `ec-services` adds the PRIME offload environment at launch
when the desktop entry has `PrefersNonDefaultGPU` or `X-KDE-RunOnDiscreteGpu`,
when the app id is in `misc.discrete-gpu-apps`, when the launcher's one-shot
action asks for it, or in `dedicated`. Never in `integrated`. The choice has
to be made at launch: a GL or Vulkan driver is chosen when the process starts,
so a running window cannot be moved to another GPU.

This amends:

- COMP-01 §4's multi-GPU and hybrid text, and COMP-01 §11's hybrid test line.
- The last paragraph of ADR 0033. That paragraph says multi-GPU is out of
  scope and the resolver does no ranking. Ranking now lives in
  `backend/gpu.rs`, and auto mode picks a primary among several GPUs.

## Consequences

- Both GPUs' connectors light up, and the dGPU does real work on its own
  screens. Hotplug, VT switch, mode, VRR, gamma and power control route to each
  output's own device.
- A surface drawn on two GPUs is imported on each. Windows that span two GPUs,
  or have just moved, can pay for a copy until the client reallocates.
- The copy fallback only runs from the primary to another GPU. A buffer only a
  secondary can import draws nothing on a primary-driven output until the client
  reallocates on the new feedback, and a client that ignores feedback stays
  blank there. Rejecting such buffers at import would break native compositing
  on the secondary, so they are accepted. To check on the laptop.
- Trusted UI art is keyed by renderer as well as scale, so a prompt is uploaded
  once per GPU. This changes `trusted_ui/` (TCB) and needs owner review. What
  is drawn does not change.
- Runtime hot-add of a whole GPU (an eGPU) is not handled. It is logged and
  ignored.
- The installer adds `nvidia-prime` and enables NVIDIA runtime power management
  when a Turing+ NVIDIA GPU coexists with an Intel or AMD GPU.
- Owed: verification on the hybrid laptop once it is imaged, including whether
  the dGPU actually reaches D3 when idle in `hybrid`, and an update to the
  multi-GPU hardware gate in `docs/STATUS.md` (COMP-01 §11).

## Revisit when

- Import failures on real hardware make the copy fallback the common case.
- Compositing on the dGPU keeps it awake and battery life suffers in `hybrid`.
- The render crate moves off concrete `GlesRenderer` for another reason,
  which would make `MultiRenderer` cheap.
