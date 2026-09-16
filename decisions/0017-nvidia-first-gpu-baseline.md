# 0017 — NVIDIA-first GPU baseline; explicit sync mandatory
Status: accepted
Date: 2026-09-05
Deciders: chase (owner), Claude (advisory)

## Context
F-04. The reference machine is an RTX 4060 Ti (8 GB) on `nvidia-open`.
NVIDIA-on-Wayland's reputation comes from compositors that designed for the
Intel/AMD path and retrofitted NVIDIA later.

## Options
1. Mesa/Intel-first, NVIDIA later — the usual path, the usual result.
2. NVIDIA-first, other vendors validated after.

## Decision
Design against NVIDIA as the primary target. `linux-drm-syncobj-v1` explicit
sync is **mandatory** — no implicit-sync fallback path is designed for or
relied on. Assume neither direct scanout nor overlay planes: composition must
be correct and fast without them, and both are runtime-detected optimizations.
Buffers come from GBM with modifier negotiation; nothing vendor-specific in the
code. Intel is tested regularly on the reference machine's iGPU so the path
does not rot; AMD is a v1 target validated before release.

## Consequences
- 8 GB VRAM shared with the desktop is the binding constraint on the inference
  design (~2 GB for the compositor at 3× 1440p).
- Damage tracking is a requirement, not an optimization.
- We will not see AMD VRR or older-Intel plane quirks during development; they
  are release-blocking work, budgeted late.

## Revisit when
An AMD test machine is acquired, or explicit sync proves unavailable on a
target we must support.
