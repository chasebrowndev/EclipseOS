# Abyss — implementation status

Last updated: 2026-09-07 (spec revision: Appendix A applied, COMP-08 v0.3).

The COMP-16 table in `ECLIPSEOS_SPECS_v2_VOL1.md` is the *contract*: what each
milestone must contain and what its exit gate is. It deliberately carries no
status column — a spec that tracks progress stops being a spec. This file is
the progress record, and per F-07 §7 `docs/` is source of truth once code
exists.

Everything below was re-verified against the code on 2026-09-07, not against
the previous revision of this file.

---

## Start here (fresh session orientation)

`abyss` is the EclipseOS Wayland compositor: one crate, `crates/abyss`, built
on Smithay `=0.7.0` as a library (ADR 0002). A single `calloop` loop owns
`AbyssState`; there is no lock on the hot path and no `Rc<RefCell<_>>` scene
graph — children are `u64` handles into plain structs. Two backends sit behind
the `Backend` trait in `backend/`: `winit` (nested, the dev path) and `drm`
(KMS/udev/libinput/libseat/GBM/EGL/GLES). The second crate,
`crates/eclipse-ctl`, is the human CLI over the JSON-RPC socket.

Phase 1 of COMP-16 is substantially built: milestones 1–9 all have code, seven
of them are exercised, three carry hardware gates that have never been run.
Milestone 9a (COMP-06 §1 protocol completeness) is done: all fifteen protocols
are implemented and verified live on the socket. What is left in Phase 1 is
blocked on hardware or is 9b (effects).

Phase 2's *specification* is now settled: Appendix A is applied inline across
both volumes and COMP-08 is at v0.3, so the wire signatures milestone 10
implements are fixed. Three v0.3 changes affect code or gate rows that already
exist: `button` gains a target handle and `expected_generation`;
`seat.pointer` splits into `seat.pointer.motion` and `seat.pointer.button`
(the bare name is retired, so any stale reference must fail rather than
match); and every acting request carries a trailing `provenance_ids`.
Phase 2 (milestones 10–18, the agent protocol) has **no code at all** — the
`trusted_ui/`, `policy/`, `audit/`, `protocols/agent/` and `protocols/semantic/`
directories named in the root `CLAUDE.md` module map do not exist on disk. The
one exception is frame-level capture redaction, which milestone 13 specifies
but which landed early inside milestone 8.

The single thing standing between the tree and Phase 1 exit is that the DRM
backend has never run on real KMS. Everything else in Phase 1 is either done
or a known, listed gap.

---

## Phase 1 — daily-driver compositor

| # | Milestone | State | Evidence / what the gate needs |
|---|---|---|---|
| 1 | winit backend; one xdg toplevel; keyboard + pointer; quit binding | **done** | `backend/winit.rs`, `protocols/standard/xdg_shell.rs`, `input/`. Gate run: terminal clients open, type and close cleanly nested under Hyprland (`WAYLAND_DISPLAY=wayland-1 ./target/debug/abyss --backend winit`). |
| 2 | Config (KDL), dwindle + master layouts, workspaces, floating, bindings, layer-shell | **done** | `config/mod.rs` (1027 lines, inotify hot-reload, ADR 0016), `shell/` layouts, `protocols/standard/layer_shell.rs`. Gate run nested: layer-shell bar + launcher + notifier clients run; layouts usable by hand. Config blocks `decoration`, `animations`, `input`, `input` and `xwayland` parse and are then **ignored**; `decoration`,
`animations` and `windowrule` now apply in part — see stubs. |
| 3 | DRM backend, multi-output, hotplug, fractional scale, output persistence | **code complete, gate never run** | `backend/drm.rs` (1013 lines), `outputs/` (hotplug, layout, persistence), `protocols/standard/fractional_scale.rs`. Gate needs three physical monitors on a real TTY plus a dock/undock cycle. Cannot be run from inside the nested session this work happens in; requires a VT login. |
| 4 | dmabuf + explicit sync + direct scanout + VRR; damage tracking complete | **code complete, gate never run** | `protocols/standard/dmabuf.rs`, `drm_syncobj.rs` (registered only when the driver reports `supports_syncobj_eventfd`, else a warning and no global), `render/` damage + `scanout_candidate` (`backend/drm.rs:856`), VRR via `VrrSupport::Supported` + per-output `vrr` config. Gate needs Firefox and mpv on real KMS and the COMP-14 frame benchmarks, which have never been collected. |
| 5 | Clipboard, primary selection, data-control, DnD, IME | **done** | `data_device.rs`, `primary_selection.rs`, `data_control.rs` (allowlisted per ADR 0027), `text_input.rs`, `input_method.rs`. Gate run nested: copy/paste across clients including primary; `wl-clipboard` via data-control honours the allowlist. |
| 6 | Session lock, idle, DPMS, power, lid | **code complete, gate never run** | `session_lock.rs`, `idle_notify.rs`, `idle_inhibit.rs`, `output_power.rs`, `outputs/power.rs`. Lock/unlock is exercised nested; suspend/resume and lid handling need real hardware. `lid-close "suspend"` now spawns `systemctl suspend` (`outputs/power.rs`), untested for want of a lid. |
| 7 | XWayland | **done, with two spec deviations** | `xwayland/mod.rs` (167 lines) + `xwayland_shell.rs`; rootless, one untrusted trust domain (ADR 0026). Gate run nested with X11 clients. Deviations: started **eagerly at compositor start**, not lazily, and without `-noTouchPointerEmulation` / MIT-SHM off — smithay 0.7.0's `XWayland::spawn` exposes no lazy entry point and no flag parameter (verified in the vendored source, `src/xwayland/xserver.rs:112`). Both are blocked upstream, not oversights. |
| 8 | Screen sharing via xdg-desktop-portal | **partial** | Two capture protocols behind **one shared fail-closed gate**: `zwlr_screencopy_v1` (`screencopy.rs`) and `ext_image_copy_capture_v1` + `ext_image_capture_source_v1` (`image_copy_capture.rs`), ADRs 0027/0029/0030. Measured against `xdg-desktop-portal-wlr` 0.8.3: 60 frames in 1.18 s. Frame-level redaction verified across 860,343 pixels of a redacted surface, all exactly opaque black. Remaining for the gate: an actual video call. Cursor capture is refused (session answered `stopped`/`leave`, `image_copy_capture.rs:333`). |
| 9 | Human IPC, `eclipse-ctl`, metrics | **done (Phase 1 scope)** | `ipc/` (JSON-RPC 2.0 line-delimited over `$XDG_RUNTIME_DIR/eclipse/abyss.sock`, `SO_PEERCRED` owner-uid gating, ADR 0028), `ipc/gate.rs` authorisation table, `ipc/methods.rs` (17 methods), `crates/eclipse-ctl` (327 lines). 7 of the 24 gate rows are deliberately `implemented: false` — all Phase 2 surface, listed below and asserted by the `phase_two_rows_stay_unimplemented` test. Gate "waybar driven by our IPC" is met in the weaker sense that `eclipse-ctl watch` streams workspace/window/output/focus events; a bar cannot yet enumerate windows it does not own because there is no `ext-foreign-toplevel-list`. |
| 9a | COMP-06 §1 protocol completeness | **done** | All fifteen protocols implemented and confirmed advertised on a live socket: `xdg_decoration`, `xdg_activation`, `wp_single_pixel_buffer`, `zwp_pointer_constraints`, `zwp_relative_pointer`, `zwp_pointer_gestures`, `ext_foreign_toplevel_list`, `wp_security_context`, `zwp_tablet_v2`, `wlr_output_management` (v4), `xdg_foreign` (exporter+importer v2), `wlr_gamma_control`, `content_type`, `wp_alpha_modifier`, `cursor_shape`. Smithay 0.7 has no module for `wlr_output_management` or `wlr_gamma_control`, so those two are hand-written dispatch following `output_power.rs`. Output configuration from the wlr protocol and from the human IPC now share one apply path (`outputs::apply_change`), so the COMP-03 §4 "never disable the last enabled output" refusal cannot be routed around. Remaining gate items are behavioural, not code: a third-party bar listing windows, mouse-look in a Proton game. |
| 9b | *(stretch)* animations, rounding, shadows, dim, blur | **partial** | Borders, `active-opacity`/`inactive-opacity`, `dim-inactive` and `rounding` draw (`render/mod.rs`, `render/effects.rs`); rounding is a fragment-shader mask in framebuffer space, confirmed visually under the winit backend. Animations, `shadow` and `blur` are parsed and validated but not drawn. Every effect is off by default, so an unconfigured frame is still the single `space_render_elements` call — damage tracking and direct scanout unchanged. Rounding a window necessarily makes it non-opaque, so a rounded window cannot take a scanout plane; that is inherent, not a regression. |
| — | **PHASE 1 EXIT** — 14 consecutive days as the only compositor | **not started** | Blocked on milestone 3's gate. Abyss is now *launchable* from any greeter: `abyss --session` plus `dist/abyss.desktop`, `dist/abyss-session`, `dist/abyss-session.target` (ADR 0032). What has never happened is a real KMS boot. |

---

## Phase 2 — agent protocol

| # | Milestone | State | Evidence |
|---|---|---|---|
| 10 | Privileged socket; `agentd` skeleton; grants; `list_toplevels`; audit | **not started** | No `protocols/agent/`, no `audit/`. `get_agents` answers "not implemented". |
| 11 | Agent seats; injection; focus arbitration; override chord | **not started** | No agent seats. `type_text`/`click_at` are gated `implemented: false`. `Action::AgentOverride` now exists and is bound by default to Super+Escape, but its handler is a stub until the trusted UI has something to show. |
| 12 | Atomic batches, `click`, `wait_for`, dedupe, generations | **not started** | — |
| 13 | Region-level redaction; policy-driven sensitivity classes | **partially landed early** | Frame-level redaction and the capture gate shipped in milestone 8 (`render/capture.rs`, opaque-black `PLACEHOLDER`). Region-level redaction and policy-driven classification do not exist: sensitivity is a manual flag on the surface (`state.rs:110`, "Stub until the policy engine"). |
| 14 | Trusted UI: indicator, prompt, emergency panel, phrase | **indicator only** | `render::capture::indicator()` draws the compositor-drawn capture indicator (COMP-10 §3.6) in both backends. No prompt, no emergency panel, no phrase, no `trusted_ui/`. |
| 15 | Policy table enforcement; prompt and defer paths | **not started** | No `policy/`. The IPC gate in `ipc/gate.rs` is a separate, narrower mechanism (COMP-13 §2) and must not be mistaken for COMP-11's enforcement table. |
| 16 | `eclipse_semantic_v1` server + reference client | **not started** | No `protocols/semantic/`. |
| 17 | Launcher with cgroup attribution; agent workspaces; virtual outputs | **not started** | Virtual outputs are unimplemented; `outputs/` handles physical outputs only. |
| 18 | MCP surface in `agentd`; SDK; reference agent | **not started** | Out of this repo. |
| — | **PHASE 2 EXIT** | **not started** | Also depends on `policyd` (S-01..S-04) and `agentd` (A-01, A-02), neither of which exists in any repo. |

---

## Component map (COMP-01..COMP-16)

| Spec | Where the code is | State |
|---|---|---|
| COMP-01 backends | `crates/abyss/src/backend/{mod,winit,drm}.rs` | winit exercised; DRM unverified on hardware. `mod.rs` holds the trait; nothing outside it touches winit/DRM/libinput/GBM types. |
| COMP-02 render | `crates/abyss/src/render/{mod,capture}.rs` | Damage tracking, direct-scanout candidate selection, frame-level redaction, capture indicator. Region-level redaction absent. |
| COMP-03 outputs | `crates/abyss/src/outputs/{mod,power}.rs` | Hotplug, layout, persistence, per-output rules (mode/position/scale/transform/enabled/vrr/lid-close). No virtual outputs. |
| COMP-04 input | `crates/abyss/src/input/mod.rs` (327 lines) | One human seat, keyboard/pointer, VT-switch intercept, bindings. No agent seats, no injection, no override chord. |
| COMP-05 shell | `crates/abyss/src/shell/mod.rs` | dwindle + master, workspaces, floating, focus. App identity is a `/proc` stopgap (`data_control.rs:14` TODO) — the provenance record is Phase 2. Window rules parse but do nothing. |
| COMP-06 standard protocols | `crates/abyss/src/protocols/standard/` (20 modules) | compositor, data_control, data_device, dmabuf, drm_syncobj, fractional_scale, idle_inhibit, idle_notify, image_copy_capture, input_method, layer_shell, output_power, presentation, primary_selection, screencopy, seat, session_lock, shm, text_input, xdg_shell. See spec gaps for what COMP-06 §1 lists and this set lacks. |
| COMP-07 XWayland | `crates/abyss/src/xwayland/mod.rs` | Rootless, eager start. |
| COMP-08 agent protocol | — | Does not exist. |
| COMP-09 semantic protocol | — | Does not exist. |
| COMP-10 trusted UI | `render/capture.rs::indicator` only | Indicator done; prompts/panel/phrase absent. |
| COMP-11 policy | — | Does not exist. |
| COMP-12 audit | — | Does not exist. |
| COMP-13 human IPC + config | `crates/abyss/src/ipc/`, `crates/abyss/src/config/`, `crates/eclipse-ctl` | Socket, gate table, 17 methods, event stream, KDL parse + hot-reload. |
| COMP-14 performance | — | No benchmark harness. The §COMP-14 frame budgets referenced by milestone 4's gate have never been measured. |
| COMP-15 testing | `cargo test --workspace` | 46 tests, all passing. Unit-level; no compat matrix, no redaction suite as a suite (redaction was verified by hand once). |
| COMP-16 milestones | this file | — |

---

## Known stubs and deliberate omissions

Every `TODO`/`FIXME`/`unimplemented!`/`todo!`/`stub`/`implemented: false` hit
in the tree is accounted for here. There are no `unimplemented!()` or `todo!()`
macros anywhere in the workspace.

1. **7 IPC gate rows are `implemented: false`** — `get_agents`, `pause_agent`,
   `resume_agent`, `terminate_agent`, `revoke_grants`, `type_text`,
   `click_at` (`ipc/gate.rs`). They answer
   `"{method} is specified but not implemented yet"` (`ipc/mod.rs:484`).
   Deliberate: all are Phase 2 surface, and the test
   `phase_two_rows_stay_unimplemented` pins the exact list so it cannot drift.
   *Unblocked by:* milestones 10–11.
2. **Sensitivity flag is manual** (`state.rs:110`, "Stub until the policy
   engine"). Surfaces can be flagged sensitive and are then redacted, but
   nothing classifies them automatically. *Unblocked by:* milestone 15.
3. **App identity is a `/proc` read** (`protocols/standard/data_control.rs:14`,
   `TODO(COMP-05)`) — to be replaced by the app identity/provenance record.
   *Unblocked by:* COMP-05 §6 provenance work in Phase 2.
4. **Cursor capture refused** (`image_copy_capture.rs:333`) — a session asking
   for cursor capture is handed back stopped. Matches the `capture.cursor`
   default of `no`; a real implementation waits on the capability model.
5. **DRM cursor has no xcursor theme** (`render/cursor.rs`) — client-set
   cursor surfaces composite correctly at their hotspot, but named
   `wp_cursor_shape_v1` shapes all fall back to one built-in amber arrow
   rather than loading the user's theme.
6. **Effects parsed but not drawn**: `decoration` and `animations` now parse
   and validate in full (`config/mod.rs`), and `active-opacity` /
   `inactive-opacity` / `dim-inactive` / `rounding` render (`render/mod.rs`,
   `window_elements`; the rounded-corner mask itself is
   `render/effects.rs`, a custom texture program bound for the duration of
   each surface's draw, so every subsurface of one window is cut by the same
   rectangle and the window rounds as a single shape — verified nested under
   winit). Still not drawn: `shadow` (needs a pre-blurred nine-slice
   texture), `blur` (needs multi-pass framebuffers) and animation
   interpolation (needs the frame clock, plus COMP-08's rule that agents see
   target geometry, never the interpolated value). All are off by default, so
   the default frame path is the single `space_render_elements` call it was
   before — damage and direct scanout unchanged. `xwayland` is still parsed and
   ignored. *Unblocked by:* the rest of 9b.
7. **`windowrule` actions are partial** (COMP-05 §4). `shell/rules.rs` matches
   at map time and re-evaluates on title change; `float`, `tile`,
   `workspace N`, `opacity F`, `sensitivity secret|private` (raise-only),
   `no-focus-steal` and `no-agent` all parse and apply — except `no-agent`,
   which sets a flag nothing reads until COMP-08 ships `list_toplevels`.
   `size`, `position`, `fullscreen`, `app-trust`, `seat-compat` and
   `idle-inhibit` actions, and the `cgroup` / `launching-principal` matchers,
   are unimplemented. `app-id` and `title` are full regexes (the `regex` crate,
   linear-time by construction because titles are client-controlled); a pattern
   that does not compile is refused at parse time and the rule is dropped
   whole, so a rule never applies in part. *Unblocked by:* the COMP-05 actions
   above.
8. **Custom modes are refused by `wlr_output_management`** — a
   `set_custom_mode` request with valid dimensions fails the whole
   configuration rather than modesetting outside the connector's own mode
   list. Deliberate (a silent landing on a neighbouring mode is worse), but it
   is a real limitation for anyone using `wlr-randr` with a custom mode.
9. **`agent-override` is bound but inert** — Super+Escape is a built-in
   default bind carrying `Action::AgentOverride` and the config parser still
   refuses to let anyone rebind it, but the action itself only logs. It gets
   its behaviour with the trusted UI in milestone 11. `agent-attention`
   (`SUPER+space`, COMP-13 §1.1 / COMP-10 §3.10) is in exactly the same state:
   a built-in default bind carrying `Action::AgentAttention`, reserved against
   rebinding, whose handler only logs until there is a pending decision queue
   to open. *Unblocked by:* milestone 14.
10. **No lease state** — COMP-08 §4.1 specifies interaction leases and
    enforcement step 6d. Nothing in the tree holds a `handle → LeaseHolder`
    map. *Unblocked by:* milestone 12.
11. **No press-time hit-test resolution** — COMP-08 §10 step 6e requires
    resolving an agent button press to `(handle, node)` before policy
    evaluation. `hit_test` exists in spec only, and `click_at` is gated
    `implemented: false`. *Unblocked by:* milestones 11 and 15.
12. **XWayland eager start, no hardening flags** — see milestone 7. Blocked on
    smithay upstream, verified in the vendored source.
13. **Only two IPC event kinds are emitted for shell changes** — `window`,
    `output`, plus `focus` and workspace events (9 `emit` call sites across
    `outputs/mod.rs`, `shell/mod.rs`, `ipc/methods.rs`). `agent-activity` and
    `config-error` are named by COMP-13 and never emitted.
14. **No `crates/policyd`, `crates/agentd`, `crates/sandbox`** — the TCB crates
    named in the root `CLAUDE.md` do not exist. The module map in that file
    describes the intended end state, not the tree.

---

## Deferred hardware verification

None of these can run inside the nested dev session; each needs a real TTY
login on the target machine.

- **Milestone 3 gate** — three monitors, hotplug, dock/undock restoring saved
  layouts.
- **Milestone 4 gate** — Firefox and mpv on KMS; direct scanout actually taken;
  explicit sync path exercised on a driver that reports syncobj eventfd
  support; COMP-14 frame benchmarks collected for the first time.
- **Milestone 6 gate** — suspend/resume across a real logind cycle, laptop lid
  open/close (this machine has no lid; needs different hardware or a synthetic
  ACPI event).
- **VRR** — `vrr_capable`/`vrr_enabled` paths have never seen a VRR panel.
- **NVIDIA modeset refusal** — `check_nvidia_modeset` (`backend/drm.rs:141`)
  reads `/sys/module/nvidia_drm/parameters/modeset` and refuses to start when
  it is `N` (F-04 §2). The refusal path is untested on the failing
  configuration.
- **`--session` handoff** — `session::import()` /`teardown()` shell out to
  `dbus-update-activation-environment`, `systemctl --user import-environment`
  and `systemctl --user start abyss-session.target`, all best-effort. Never
  run from a greeter.

---

## What it takes to daily-drive Abyss

In rough order:

1. **Boot it on real KMS.** Install `dist/abyss.desktop` to
   `/usr/share/wayland-sessions/`, `dist/abyss-session` to `/usr/bin/`, the
   target to the user's systemd units, and log in from the greeter. Nothing
   about this path has ever executed. Everything below assumes it works.
2. **Milestone 3 and 4 gates**, which are the same session as step 1.
3. **Exercise the milestone 9a protocols against real clients** — the globals
   are advertised and the handlers are written, but a third-party bar over
   `ext_foreign_toplevel_list` and mouse-look in a Proton game over
   `pointer_constraints`/`relative_pointer` have not been run.
4. **A real xcursor theme** on DRM, replacing the built-in amber arrow used
   for named cursor shapes.
5. **Effects (9b)** — optional by COMP-16 Open Decision 1. Borders, opacity,
   dim-inactive and rounding are in; "no animation, no shadows, no blur" is
   what remains of the visible gap against the current Hyprland setup.
6. **Full COMP-05 §4 matchers** — `size`, `position`, `fullscreen`,
   `app-trust`, `seat-compat`, `idle-inhibit`, `cgroup` and
   `launching-principal`, plus real regex patterns (gap 7).

---

## Spec gaps found

Per the root `CLAUDE.md`: "specs and code disagreeing is a bug in one of them —
do not silently pick." These are recorded, not resolved. None of them has been
fixed by editing either side.

1. **COMP-01 §5 stops at step 9** ("export `$WAYLAND_DISPLAY`") and says
   nothing about handing off to the user session manager. `session.rs` does
   exactly that and its own docstring says the module is *additive to the
   spec, not an implementation of it* (ADR 0032). The spec needs a step 10 or
   an explicit statement that session integration is out of scope. Steps 10,
   11 and 13 of that startup sequence likewise have no counterpart in code.
2. **COMP-01 §4 GPU selection.** The spec describes a ranking for choosing the
   primary GPU; `backend/drm.rs:503` calls smithay's `primary_gpu(&seat_name)`
   instead, which is a different (simpler) policy. The `render-device` config
   key that would let a user override it parses and is discarded
   (`config/mod.rs:586`). Either the spec's ranking should be implemented or
   §4 should adopt smithay's.
3. **COMP-06 §1 protocol list vs. the implemented modules.** *Closed.* The
   fifteen missing protocols are implemented (milestone 9a). COMP-06 §1 also
   gained a note that
   `wl_subcompositor` and `xdg_output` are not separate items — both are
   already advertised, `wl_subcompositor` by `CompositorState::new` and
   `xdg_output` by `OutputManagerState::new_with_xdg_output`
   (`state.rs:212`).
4. **COMP-13 §1.2 config validation.** The spec calls for total validation —
   a bad config is an error. `Config::load` (`config/mod.rs:382`) logs
   `"config unreadable, using defaults"` or `"config parse failed, file
   ignored"` and **always returns a usable config**, warning on unknown nodes
   rather than rejecting them. This is a deliberate choice for hot-reload (a
   typo mid-edit should not kill the session) but it contradicts the spec as
   written. One of the two needs to change; the `config-error` IPC event the
   spec names would be the natural bridge, and it is never emitted.
5. ~~**KDL v2 booleans.**~~ *Resolved.* The `kdl` crate is v2, where bare
   `true` and `false` are identifiers rather than values; the spec's examples
   were written in v1 syntax and would not have parsed. The examples were
   swept to `#true` / `#false` and COMP-13 §1.2 now states the rule
   explicitly. The parser was left strict — accepting the bare identifiers
   would have meant blessing a non-KDL syntax in the one place the spec asks
   for total validation.
6. **COMP-07 §1 requires lazy XWayland start**, §5 requires
   `-noTouchPointerEmulation` and no MIT-SHM, and §6's test plan asserts
   "XWayland is not running with no X11 clients". Smithay 0.7.0 cannot express
   any of the three. The spec is right and the code is constrained; the pin
   note in `CLAUDE.md` applies (a version bump is its own PR with its own ADR).
7. **COMP-16 milestone 13** is described as region-level redaction plus
   policy-driven classes, and the table itself now notes that frame-level
   redaction landed early in milestone 8 — that parenthetical is accurate and
   was verified.

---

## Test and gate status

`cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo fmt --check` and `cargo test --workspace` (46 tests) are all
green as of this revision. There is no CI; the gate is run by hand.
