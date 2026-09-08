# Milestone status

The COMP-16 table in `ECLIPSEOS_SPECS_v2_VOL1.md` is the *contract* — what has
to be built and what proves it. It deliberately carries no status column: a
spec that tracks progress stops being a spec. This file is the progress record,
and per F-07 §7 `docs/` is source of truth once code exists.

Exit gates marked **unverified** are gates that need hardware or a human at a
real TTY. Code for them is written; the gate has not been run. Those are
batched deliberately (see *Deferred hardware verification* below) rather than
claimed.

Last updated: 2026-09-07.

## Phase 1

| # | Milestone | State | Notes |
|---|---|---|---|
| 1 | winit backend, one toplevel, input, quit | **done** | |
| 2 | Config, layouts, workspaces, bindings, layer-shell | **done** | KDL parse + validate + hot reload |
| 3 | DRM backend, multi-output, hotplug, scale, persistence | **code complete, gate unverified** | needs 3 monitors and a dock |
| 4 | dmabuf, explicit sync, direct scanout, VRR, damage | **code complete, gate unverified** | scanout/VRR/sync need real KMS on the 4060 Ti |
| 5 | Clipboard, primary, data-control, DnD, IME | **done** | data-control is allowlist-gated (ADR 0022) |
| 6 | Session lock, idle, DPMS, power, lid | **code complete, gate unverified** | `lid-close "suspend"` falls through to `"off"`; suspend is logind's business and is not wired |
| 7 | XWayland | **done** | rootless, untrusted trust domain; lazy start and the hardening flags are blocked on smithay upstream |
| 8 | Screen sharing via xdg-desktop-portal | **partial** | Two capture protocols behind one shared fail-closed gate (ADR 0027, ADR 0030): `zwlr_screencopy_v1` and `ext_image_copy_capture_v1` + `ext_image_capture_source_v1` (output source, `wl_shm` only). `xdg-desktop-portal-wlr` 0.8.3, allowlisted by the user (ADR 0029), now takes its `ext_image_copy_capture` path against helios and streams continuously — measured end to end over D-Bus and PipeWire at 60 frames in 1.18 s, no stall (the one-frame stall on its wlr path is an xdpw bug and is simply not on this path any more). Redaction verified in pixels on the new path: with `redact-app-id "kitty"` a captured frame is uniformly (0,0,0) across all 860,343 pixels, and the same scene without the entry comes back in real colour. Outstanding: dmabuf capture (readback per frame), per-toplevel capture (blocked on `ext-foreign-toplevel-list`, not on the protocol), the COMP-10 consent prompt (M14) — today the allowlist entry is the only gate and cannot tell portal clients apart — and lock-denies-capture is unit-tested only, since no locker available here will lock a nested session |
| 9 | Human IPC, `eclipse-ctl`, metrics | **done (Phase 1)** | JSON-RPC socket, gate table and CLI complete for Phase 1: `resize`, `move_workspace_to_output` and `set_output` landed (COMP-13 §2.1, ADR 0031). 7 gate rows stay `implemented: false` by design — the five agent/grant methods (`get_agents`, `pause_agent`, `resume_agent`, `terminate_agent`, `revoke_grants`) wait on the agent protocol (COMP-08, Phase 2), and `type_text` / `click_at` wait on input injection (COMP-04) |
| 9b | *(stretch)* animations, rounding, shadows, dim, blur | **not started** | Open Decision #1 proposes after Phase 1 exit |
| — | **PHASE 1 EXIT** — 14 days as the only compositor | **not started** | |

## Phase 2

Milestones 10–18: **not started**, except milestone 13, whose frame-level
redaction and capture gate landed early inside milestone 8. The spec row was
narrowed to the remainder (region-level redaction, policy-driven sensitivity
classes) in `aa4f4b5`.

## Deferred hardware verification

Batched for a session at a real TTY on the RTX 4060 Ti: DRM page-flip
presentation, direct scanout planes, VRR, dmabuf and explicit sync on NVIDIA,
DPMS on real KMS, lid switch.

## Known stubs

- `--stats` FPS is meaningless under winit.
- The sensitive-surface set is a stub until the policy engine (COMP-11).
- Trusted UI is not yet prepended in the render path (COMP-10).
- The DRM cursor is an amber placeholder pending a themed cursor.
- Tablet-mode switches are logged and ignored.
- Capture is `wl_shm` only; every frame is a readback. Per-toplevel capture is
  blocked on `ext-foreign-toplevel-list`, not on the capture protocol.
- `ext_image_copy_capture_v1` cursor sessions are stubbed: the session is
  answered `stopped`/`leave` from the start rather than silently ignored.
- The capture indicator does not name the consumer; there is no text rendering
  yet.
- IPC subscribers cannot claim a principal until COMP-08.
- Client identity is a `/proc` stopgap (`TODO(COMP-05)` in
  `protocols/standard/data_control.rs`) until app identity exists.
