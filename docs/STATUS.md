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
| 8 | Screen sharing via xdg-desktop-portal | **in progress** | compositor half landed early: `zwlr_screencopy_v1` behind a fail-closed capture gate + frame redaction (ADR 0027). The portal half — running `xdg-desktop-portal-wlr` against us — is the open work |
| 9 | Human IPC, `eclipse-ctl`, metrics | **mostly done** | JSON-RPC socket, gate table and CLI exist; 10 gate rows are `implemented: false` (`resize`, `move_workspace_to_output`, `set_output`, the five agent/grant methods, `type_text`, `click_at`) — the agent ones are Phase 2 by design |
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
- Capture is `wl_shm` only, with no per-toplevel capture — that needs
  `ext-image-copy-capture-v1`.
- The capture indicator does not name the consumer; there is no text rendering
  yet.
- IPC subscribers cannot claim a principal until COMP-08.
- Client identity is a `/proc` stopgap (`TODO(COMP-05)` in
  `protocols/standard/data_control.rs`) until app identity exists.
