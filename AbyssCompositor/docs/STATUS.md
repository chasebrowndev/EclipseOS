# Abyss — implementation status

Last updated: 2026-09-11.

**Spec baseline: v2 + Appendix A applied (2026-09-08).** The amendment set
that previously sat unapplied at the end of VOL2 is now merged inline;
COMP-08 is at v0.2. What changed is mostly the contract the code is measured
against, and the new gaps that opens are recorded in the spec-gaps section;
the code-state rows below carry forward the 2026-09-07 verification pass,
with milestones 6 and 9b re-checked against the tree on 2026-09-08.

The COMP-16 table in `ECLIPSEOS_SPECS_v2_VOL1.md` is the *contract*: what each
milestone must contain and what its exit gate is. It deliberately carries no
status column — a spec that tracks progress stops being a spec. This file is
the progress record, and per F-07 §7 `docs/` is source of truth once code
exists.

Everything below was re-verified against the code on 2026-09-07, not against
the previous revision of this file. The 2026-09-10 revision folds in the first
**real-KMS boot** (see "Real-KMS boot record" below), which retires the single
largest unknown this document has carried since it was written.

---

## Start here (fresh session orientation)

`abyss` is the EclipseOS Wayland compositor: one crate, `crates/abyss`, built
on Smithay `=0.7.0` as a library (ADR 0002). A single `calloop` loop owns
`AbyssState`; there is no lock on the hot path and no `Rc<RefCell<_>>` scene
graph — children are `u64` handles into plain structs. Two backends sit behind
the `Backend` trait in `backend/`: `winit` (nested, the dev path) and `drm`
(KMS/udev/libinput/libseat/GBM/EGL/GLES).

Four crates sit around it. `crates/eclipse-ctl` is the human CLI over the
JSON-RPC socket. `crates/wlcs-abyss` is the conformance shim. As of 2026-09-11
there are two more, and they are the beginning of the DE userland (COMP-17,
F-01 §4): `crates/eclipse-ipc` is the client half of COMP-13 — a blocking,
calloop-friendly control-socket client with no async runtime — and
`crates/eclipse-ui` is the Eclipse design system over `iced` 0.14, which is
where every colour, radius and size in the DE is defined exactly once
(ADR 0038 chose native Rust over Quickshell; ADR 0039 records the four narrow
supply-chain exceptions iced cost). Neither is TCB. See
`docs/HANDOFF.md` for where that work stands and what comes next.

Phase 1 of COMP-16 is substantially built: milestones 1–9b all have code,
seven of them are exercised, three carry hardware gates that have never been
run. COMP-16 v0.2 added milestones 9c–9f (headless backend, node-level
redaction, window-rules engine, benchmark harness); only 9c has code.
Milestone 9a (COMP-06 §1 protocol completeness) is done: all fifteen protocols
are implemented and verified live on the socket. What is left in Phase 1 is
blocked on hardware or is 9c–9f; 9b (effects) is done and 9c has landed its
backend but not its `wlcs` gate.
Phase 2 (milestones 10–25, the agent protocol) has **no code at all** — the
`trusted_ui/`, `policy/`, `audit/`, `protocols/agent/` and `protocols/semantic/`
directories named in the root `CLAUDE.md` module map do not exist on disk. The
one exception is frame-level capture redaction, which milestones 9d and 22
specify but which landed early inside milestone 8.

**The DRM backend now runs on real KMS.** On 2026-09-10 `abyss --backend drm`
booted on this machine's own hardware (RTX 4060 Ti, `nvidia-open-dkms`), lit
both connected panels on their native modes, ran a real client on them and took
live keyboard input. That closes the question the previous revisions of this
file were organised around. What remains between the tree and Phase 1 exit is
now concrete and enumerable: the *multi-monitor* half of milestone 3's gate
(three panels, hotplug, dock/undock), milestone 4's gate (Firefox/mpv, scanout
actually taken, COMP-14 budgets), milestone 6's gate (suspend/resume, lid), and
9c–9f.

**Open vs. not, as of 2026-09-10.** Closed and needing nothing further:
milestones 1, 2, 5, 7, 9, 9a, 9b; spec gaps 1, 2, 3, 4 and 5. Partly hardware-verified, with the rest of the
gate still waiting on hardware this box does not have: milestones 3, 4
and 6, plus the multi-GPU half of COMP-01 §11's test plan — all listed under
"Deferred hardware verification", none of them a defect. Genuinely open work
that can be done today: an `ext-foreign-toplevel-list` consumer for milestone
9's bar gate, the four milestones COMP-16 v0.2 added to Phase 1 (9c, 9d, the
rest of 9e, 9f), and the whole of Phase 2 (milestones 10–25), whose
specification is settled but whose directories do not exist. Open and *not*
actionable: gap 6 (three COMP-07 requirements smithay 0.7.0 cannot express —
blocked upstream behind a pinned dependency) and stub 13's `agent-activity`
event, which has no source to fire from until milestone 11 lands.

---

## Phase 1 — daily-driver compositor

| # | Milestone | State | Evidence / what the gate needs |
|---|---|---|---|
| 1 | winit backend; one xdg toplevel; keyboard + pointer; quit binding | **done** | `backend/winit.rs`, `protocols/standard/xdg_shell.rs`, `input/`. Gate run: terminal clients open, type and close cleanly nested under Hyprland (`WAYLAND_DISPLAY=wayland-1 ./target/debug/abyss --backend winit`). |
| 2 | Config (KDL), dwindle + master layouts, workspaces, floating, bindings, layer-shell | **done** | `config/mod.rs` (1027 lines, inotify hot-reload, ADR 0016), `shell/` layouts, `protocols/standard/layer_shell.rs`. Gate run nested: layer-shell bar + launcher + notifier clients run; layouts usable by hand. Config blocks `decoration`, `animations`, `input` and `xwayland` all parse and validate; `decoration` and `animations` now apply in full (milestone 9b), `windowrule` applies except `launching-principal` (stub 7), and `xwayland` is still parsed and ignored. |
| 3 | DRM backend, multi-output, hotplug, fractional scale, output persistence | **runs on real KMS; gate partially met** | `backend/drm.rs`, `outputs/` (hotplug, layout, persistence), `protocols/standard/fractional_scale.rs`. **Booted on real KMS 2026-09-10**: two connectors, each on its own native mode (HDMI-A-1 1360x768 on crtc 198/plane 52; DP-3 1280x720 on crtc 390/plane 244), a kitty client rendered and typed into, `grim` capture 2640x768. Multi-output composition and per-output modeset are therefore verified. **Still unmet:** three physical monitors, a hotplug event, and a dock/undock cycle restoring a saved layout — this box has two panels and no dock. |
| 4 | dmabuf + explicit sync + direct scanout + VRR; damage tracking complete | **code complete, gate never run** | `protocols/standard/dmabuf.rs`, `drm_syncobj.rs` (registered only when the driver reports `supports_syncobj_eventfd`, else a warning and no global), `render/` damage + `scanout_candidate` (`backend/drm.rs:856`), VRR via `VrrSupport::Supported` + per-output `vrr` config. Gate needs Firefox and mpv on real KMS and the COMP-14 frame benchmarks, which have never been collected. The 2026-09-10 KMS boot produced the first real frame timings from any backend (`--stats`: 465 frames, ~1 fps idle rising to 38.1 fps under input, render p50 ~500 us, submit p50 ~166 us) — that is the damage tracker and the submit path behaving, not a COMP-14 measurement, and direct scanout was not confirmed taken. |
| 5 | Clipboard, primary selection, data-control, DnD, IME | **done** | `data_device.rs`, `primary_selection.rs`, `data_control.rs` (allowlisted per ADR 0027), `text_input.rs`, `input_method.rs`. Gate run nested: copy/paste across clients including primary; `wl-clipboard` via data-control honours the allowlist. |
| 6 | Session lock, idle, DPMS, power, lid | **code complete, gate never run** | `session_lock.rs`, `idle_notify.rs`, `idle_inhibit.rs`, `output_power.rs`, `outputs/power.rs`. Lock/unlock is exercised nested; suspend/resume and lid handling need real hardware. `lid-close "suspend"` now spawns `systemctl suspend` (`outputs/power.rs`), untested for want of a lid. |
| 7 | XWayland | **done, with two spec deviations** | `xwayland/mod.rs` (167 lines) + `xwayland_shell.rs`; rootless, one untrusted trust domain (ADR 0026). Gate run nested with X11 clients. Deviations: started **eagerly at compositor start**, not lazily, and without `-noTouchPointerEmulation` / MIT-SHM off — smithay 0.7.0's `XWayland::spawn` exposes no lazy entry point and no flag parameter (verified in the vendored source, `src/xwayland/xserver.rs:112`). Both are blocked upstream, not oversights. |
| 8 | Screen sharing via xdg-desktop-portal | **partial** | Two capture protocols behind **one shared fail-closed gate**: `zwlr_screencopy_v1` (`screencopy.rs`) and `ext_image_copy_capture_v1` + `ext_image_capture_source_v1` (`image_copy_capture.rs`), ADRs 0027/0029/0030. Measured against `xdg-desktop-portal-wlr` 0.8.3: 60 frames in 1.18 s. Frame-level redaction verified across 860,343 pixels of a redacted surface, all exactly opaque black. Remaining for the gate: an actual video call. Cursor capture is refused (session answered `stopped`/`leave`, `image_copy_capture.rs:333`). |
| 9 | Human IPC, `eclipse-ctl`, metrics | **done (Phase 1 scope)** | `ipc/` (JSON-RPC 2.0 line-delimited over `$XDG_RUNTIME_DIR/eclipse/abyss.sock`, `SO_PEERCRED` owner-uid gating, ADR 0028), `ipc/gate.rs` authorisation table, `ipc/methods.rs` (17 methods), `crates/eclipse-ctl` (327 lines). 7 of the 24 gate rows are deliberately `implemented: false` — all Phase 2 surface, listed below and asserted by the `phase_two_rows_stay_unimplemented` test. Gate "waybar driven by our IPC" is met in the weaker sense that `eclipse-ctl watch` streams workspace/window/output/focus events; a bar cannot yet enumerate windows it does not own because there is no `ext-foreign-toplevel-list`. |
| 9a | COMP-06 §1 protocol completeness | **done** | All fifteen protocols implemented and confirmed advertised on a live socket: `xdg_decoration`, `xdg_activation`, `wp_single_pixel_buffer`, `zwp_pointer_constraints`, `zwp_relative_pointer`, `zwp_pointer_gestures`, `ext_foreign_toplevel_list`, `wp_security_context`, `zwp_tablet_v2`, `wlr_output_management` (v4), `xdg_foreign` (exporter+importer v2), `wlr_gamma_control`, `content_type`, `wp_alpha_modifier`, `cursor_shape`. Smithay 0.7 has no module for `wlr_output_management` or `wlr_gamma_control`, so those two are hand-written dispatch following `output_power.rs`. Output configuration from the wlr protocol and from the human IPC now share one apply path (`outputs::apply_change`), so the COMP-03 §4 "never disable the last enabled output" refusal cannot be routed around. Remaining gate items are behavioural, not code: a third-party bar listing windows, mouse-look in a Proton game. |
| 9b | *(stretch)* animations, rounding, shadows, dim, blur | **done** | Borders, `active-opacity`/`inactive-opacity`, `dim-inactive`, `rounding` and `shadow` draw (`render/mod.rs`, `render/effects.rs`); rounding is a fragment-shader mask in framebuffer space and the shadow an SDF pixel shader over the grown window rect, both confirmed visually under the winit backend. The `windows` animation interpolates window position from the frame clock (`render/anim.rs`), also confirmed visually, as are `fade` (a newly mapped window's alpha ramps from zero; there is no fade-out, since a closing window is out of the space before the next frame) and `border` (the border colour crossfades on focus change). `workspaces` slides the arriving workspace's windows in from the edge the switch came from (there is no outgoing half — the old workspace's windows are unmapped before the frame is drawn), also confirmed visually. `blur` is a dual-Kawase chain (`render/blur.rs`): the element list below a translucent window is rendered into an offscreen buffer, downsampled `passes` times and upsampled back, and the result spliced in directly beneath that window's surfaces; it is skipped entirely for opaque windows, and any enabled effect (blur included) disqualifies direct scanout. Every effect is off by default, so an unconfigured frame is still the single `space_render_elements` call — damage tracking and direct scanout unchanged. Rounding a window necessarily makes it non-opaque, so a rounded window cannot take a scanout plane; that is inherent, not a regression. |
| 9c | `headless` backend (COMP-01 §10) | **landed; CI run pending** | `backend/headless.rs` runs: EGL device (hardware render node, else Mesa software), offscreen GLES target, synthetic 60 Hz frame clock, no KMS and no host display server. `abyss --backend headless [--size WxH]` serves a real socket and composites for real, so redaction and pixel comparisons have something to assert on. The conformance harness now exists too: `abyss` is a lib + bin, `backend::headless::run_wlcs` drives a compositor from a `WlcsEvent` channel, `input/inject.rs` feeds synthetic pointer events through abyss's own focus and clamping path (COMP-04 §6), and `crates/wlcs-abyss` is a `cdylib` exporting `wlcs_server_integration` (verified with `nm -D`). `gate.yml` has a blocking `conformance` job that builds MirServer/wlcs at the pinned SHA and runs it against the library, skipping only what `ci/wlcs-skip.txt` lists. The suite has now been executed end to end against the library: **RC=0, 555 passed, 319 skipped, 0 failed**, at the CI-default `ulimit -n 1024` and with no raised limit. Getting there required fixing a per-compositor fd leak: a `DisplayHandle` stored in a global’s user data or bind filter is owned by the `Display`, forming a strong reference cycle, so the backend `Arc` never dropped and every fd it owned (epoll, two eventfds, timerfd, the seat’s keymap memfd) leaked — about five per test. wlcs builds one compositor per test inside one process, so the run exhausted the fd table and SIGSEGV’d around test 165. Bind-time code now holds a `WeakDh` (`Weak<DisplayHandle>`) upgraded through an `Arc<DisplayHandle>` owned by `AbyssState`, fail-closed: a handle that no longer upgrades reads as unknown client identity, i.e. a denial. **Missing:** the `conformance` job has not yet run the suite on CI’s llvmpipe runner — the green run above was on a local test box. The milestone closes when CI goes green. **Touch has since landed and the suite is at RC=0, 743 passed, 319 skipped, 0 failed against 85 skip entries** (`ci/wlcs-skip.txt`, down from 273 — the ratchet only ever loosens). What group 1 (≈170 touch tests) had been called a harness gap really was one, not a COMP-04 fault — `create_touch()` returns `None`, the wlcs FFI passes a null `WlcsTouch*`, and wlcs’s own `Touch::Impl` constructor dereferences it before any event is sent (confirmed empirically: a single touch case run with no skip filter exits 139/SIGSEGV at `[ RUN ]`, before any assertion or compositor code). Closing it took `WlcsEvent::TouchDown/TouchMove/TouchUp`, `Seat::add_touch()`, `inject_touch_*` in `input/inject.rs`, and a real `create_touch()`, plus two non-obvious fixes. First, **wlcs touch coordinates are plain pixels, not 24.8 fixed point**: `include/wlcs/touch.h` declares the hooks as taking `wl_fixed_t` but `src/in_process_server.cpp:271` passes ints, so dividing by 256 put every touch at (0.35, 0.05) and 12 `TouchTest`s missed every surface. The pointer hooks really are fixed point. Second, **a `wl_touch.up` cannot be routed through a destroyed surface**: smithay's `for_each_focused_touch` matches `wl_touch` instances by the focus surface's client, and by the time `CompositorHandler::destroyed` runs the surface is already dead and `Resource::client()` returns `None`, so the event is dropped silently (`wl_touch.cancel` is not a substitute — it carries no id, and wlcs registers no cancel listener). `AbyssState.touch_points` therefore records the `Client` alongside the surface at down time and `release_touch_on` sends `up`+`frame` straight to that client's `wl_touch`, then clears smithay's slot state through the normal `up` path. 168 of the 170 pass; the 2 that do not are the touch twins of `input_seen_by_subsurface_after_parent_unmapped_and_remapped`, whose pointer cases fail identically, so they are rehomed with those. The 319 skips are wlcs declining to run tests whose extensions we do not advertise, and only **42** of them are worth chasing: 244 are `wl_shell`/`zxdg_shell_v6` (dead protocols), 5 `gtk_primary_selection` (superseded), 4 are wlcs’s own `SelfTest`, 24 are the touch gap above — leaving foreign-toplevel (30) and `zwlr_virtual_pointer_v1` (12) as the only genuine unimplemented protocols. 120 of the 244 hide behind numeric `*InputCombinations` names that `Combine()` flattens to a bare integer; decoding `(index / 2) % 6` against `SurfaceBuilder::all_surface_types()` puts all 120 on `WlShellSurfaceBuilder` or `XdgV6SurfaceBuilder`, so they are not the input-region gap the suite names imply. Foreign-toplevel exposes the full window list to any client, so it wants a policy check (COMP-08/COMP-11) rather than a straight implementation. A 20-test bloc then came out in one fix: `Space` positions an element by the origin of its *window geometry*, and a toplevel that never calls `xdg_surface.set_window_geometry` derives that geometry from the bounding box of its whole surface tree, so a client attaching a subsurface that extends left of or above its root silently moves its own geometry origin — and with it the whole window, by up to the subsurface offset. The window jumped +100px after placement, so hit-testing *and* rendering were displaced, which is why the failures looked like input-region and focus-tree bugs across three unrelated suites (`input_seen_by_subsurface_after_parent_unmapped_and_remapped`, `input_seen_by_second_surface_after_drag_off_first_and_up`, and 12 `RegionSurfaceInputCombinations.input_not_seen_after_leaving_region` cases). `AbyssState.geo_loc` now records each element's last-seen `Window::geometry().loc` and `shell::reanchor` (called from `CompositorHandler::commit`) shifts the stored floating rect by any delta, holding `render_location` — the surface origin — constant. Suite: **RC=0, 743 passed, 0 failed against 85 skip entries**. `zwlr_virtual_pointer_v1` then closed the smaller of the two remaining protocol gaps: smithay 0.7 has no module for it, so `protocols/standard/virtual_pointer.rs` writes the dispatches out against the wlr bindings smithay re-exports, following `output_power.rs`. The protocol is frame-batched by design — motion, buttons and axis state queue and nothing is delivered until `frame` — so each device carries a pending batch in `AbyssState.virtual_pointer` (the resource's user data is `&`-only in `Dispatch::request`, so the batch cannot live there), and `frame` replays it through the existing `inject_pointer_*` entry points, keeping idle activity, click-to-focus, cursor clamping and lock suppression. Two details were not guessable: discrete scroll steps must be stored as `discrete * 120` because smithay re-divides by 120 for the legacy `wl_pointer.axis_discrete` event (`wayland/seat/pointer.rs:150`), and `motion_absolute`'s output geometry has to be resolved *before* the pending batch is mutably borrowed or `state` is borrowed twice. Suite: **RC=0, 755 passed, 308 skipped, 0 failed against the same 85 skip entries** — the 12 tests were wlcs skips, not skip-list entries, so the ratchet did not move; foreign-toplevel (30) is now the only genuine unimplemented protocol left. A second bloc of 20 then came out for the same reason the first did — one missing path, not one bug per suite. `pointer_moved` was the *only* code that delivered `wl_pointer` focus, so focus was re-evaluated on device motion and never on a change of scene: a stationary pointer never learned that a window had moved or resized under it, that a subsurface had slid under or out from under it, or that `wl_surface.set_input_region` had shrunk away from beneath it. `AbyssState::refresh_pointer_focus` is the scene-driven twin — hit-test the current pointer location and deliver `motion`+`frame` only when the (surface, rounded surface-relative position) pair actually changed. That guard is load-bearing rather than an optimisation: smithay's `PointerInnerHandle::motion` forwards a same-focus refresh as an unconditional `wl_pointer.motion` (`wayland/seat/pointer.rs:95`), so an unguarded refresh on every commit would spam every strict listener in the suite and turn passing tests red. It is called from `CompositorHandler::commit` — committed state only, which is exactly why `subsurface_does_not_move_when_parent_not_committed` correctly sees nothing — and from `shell::place_at`, deliberately *not* from `arrange`, since `pointer_moved` itself calls `arrange` via focus-follows-mouse and would re-enter motion delivery mid-flight. It changes neither output nor keyboard focus; those are human-motion behaviours, and driving keyboard focus from a commit hook invites recursion. Retired 12 `RegionSurfaceInputCombinations`, 4 `SubsurfaceTest` and 4 `ClientSurfaceEventsTest` entries. Suite: **RC=0, 775 passed, 308 skipped, 0 failed against 65 skip entries**. |
| 9d | Node-level redaction, fail-closed on stale/absent tree | **not started** | `render/capture.rs` is surface-level only. Implementable now: COMP-02 §7 defines the absent-tree case, so the fail-closed arm needs no semantic tree. |
| 9e | Window-rules engine; `class_source`, `irreversible_capable` | **partially landed** | `shell/rules.rs` matches at map time and re-evaluates on commit; all COMP-05 §4 actions and matchers apply except the `launching-principal` matcher (stub 7). Neither `class_source` nor `irreversible_capable` exists in `shell/`. |
| 9f | Benchmark harness (COMP-14 §2 + A-14 budgets) | **not started** | No harness. Milestone 4's gate has cited COMP-14 budgets since v0.1 with nothing able to produce them. |
| — | **PHASE 1 EXIT** — 14 consecutive days as the only compositor on real hardware | **not started** | Blocked on the rest of milestone 3's gate, on 4 and 6, and on 9c–9f. Abyss is now *launchable* from any greeter: `abyss --session` plus `dist/abyss.desktop`, `dist/abyss-session`, `dist/abyss-session.target` (ADR 0032). The DRM backend has now booted on real KMS (2026-09-10) as well as under VM/virtio-gpu, but that boot was launched by hand on a VT via `openvt`, not from a greeter, and ran for 60 s under a `timeout`. Nobody has yet used abyss as their session for a whole day, let alone fourteen. |

---

## Phase 2 — agent protocol

Renumbered by COMP-16 v0.2. Old → new: 10→11, 11→13, 12→14, 13→split
across 9d/17/22, 14→15, 15→16, 16→22, 17→24, 18→25. Milestones 10, 12, 19,
20, 21 and 23 are new.

| # | Milestone | State | Evidence |
|---|---|---|---|
| 10 | `policyd` skeleton; task store; grant compilation, issue, revocation | **not started** | No `crates/policyd`. The task object (A-04) exists in no code. |
| 11 | Privileged socket; `agentd` skeleton; grant verification; `list_toplevels` | **not started** | No `protocols/agent/`. `get_agents` answers "not implemented". |
| 12 | Audit spine: append-only journal, req-id chaining, `trace` | **not started** | No `audit/`. |
| 13 | Agent seats; injection; focus arbitration; `agent-override` chord | **not started** | No agent seats. `type_text`/`click_at` are gated `implemented: false`. The `agent-override` bind reserved by COMP-13 §1.1 has no `Action` variant. |
| 14 | Atomic batches, `click`, `wait_for`, dedupe, generations | **not started** | — |
| 15 | Trusted UI: prompt, emergency panel, phrase | **indicator landed early** | `render::capture::indicator()` draws the compositor-drawn capture indicator (COMP-10 §3.6) in both backends, from milestone 8. No prompt, no emergency panel, no phrase, no `trusted_ui/`. |
| 16 | Policy table enforcement; prompt and defer paths | **not started** | No `policy/`. The IPC gate in `ipc/gate.rs` is a separate, narrower mechanism (COMP-13 §2) and must not be mistaken for COMP-11's enforcement table. |
| 17 | Policy-driven sensitivity classes; classification races | **not started** | Sensitivity is a manual flag on the surface (`state.rs:110`, "Stub until the policy engine"); nothing classifies automatically. |
| 18 | Provenance chain; irreversible matcher | **not started** | Blocked on F-03 (defect 13) for the S-07 §5 `stamper` enum. |
| 19 | `brokerd` | **not started** | Does not exist in any repo. |
| 20 | Per-agent egress proxy; netns + pasta; stub resolver | **not started** | Does not exist in any repo. |
| 21 | `cataclysm-pub` | **not started** | Does not exist (defect 12). |
| 22 | `eclipse_semantic_v1` server + reference client | **not started** | No `protocols/semantic/`. |
| 23 | `cataclysm` foot fork | **not started** | Does not exist; vendoring undecided (defect 12, F-07 §1 VERIFY). |
| 24 | Launcher with cgroup attribution; agent workspaces; virtual outputs | **not started** | Virtual outputs are unimplemented; `outputs/` handles physical outputs only. Gated on C-00 §17 open item 1. |
| 25 | MCP surface in `agentd`; SDK; reference agent | **not started** | Out of this repo. |
| — | **PHASE 2 EXIT** | **not started** | Also depends on `policyd` (S-01..S-04) and `agentd` (A-01, A-02), neither of which exists in any repo. |

---

## DE userland — COMP-17, DP-2, F-01 §4

Started 2026-09-11 against the plan at
`~/.claude/plans/ok-claude-today-we-inherited-cook.md`, which holds the owner's
settled decisions and the build order. This is the human-facing surface abyss
has never had: today everything the user sees is a Quickshell/QML config living
outside the repo at `~/.config/quickshell/eclipse/`. Appendix B's F-01 §4
requires every setting to be reachable from a GUI, with the files still the
source of truth and the GUI writing only through the COMP-13 §1.4 API — never a
parallel one.

| Unit | State | Evidence |
|---|---|---|
| A0 KDL round-trip spike | **done** | verdict at the foot of `docs/HANDOFF.md`: byte-splice edits round-trip, the `value_repr` trap is real and is handled |
| A1 declarative schema | **done** | `config/schema.rs`, three-sided anti-drift over `schema::KEYS` (`ac3cb4f`) |
| A2 `abyss.kdl` / `policy.kdl` split | **done** | `a95d657`, ADR 0037. Ownership is refused in both directions at parse time, naming the other file |
| A3 config over the socket | **done** | `ipc/config_rpc.rs`, per-file gate rows, `Policy` rows default-closed (`b63b6ac`) |
| A4 self-write vs. hot-reload | **done** | one `apply_loaded`, no `config-error` storm, a genuine external rewrite still reloads (`6d8f5a8`) |
| A5 `eclipse-ctl config` + coverage ratchet | **done** | `18b97cf`; `ci/gui-coverage-exceptions.txt` may only shrink |
| `docs/CONFIG.md` | **done** | generated from the schema (`fab0eb3`) |
| B2 `crates/eclipse-ipc` | **done** | `5ebe0a9`. Raw fd exposed, non-blocking, no async runtime — it drops into a calloop client loop |
| B1 `crates/eclipse-ui` | **done** | `a501576`. `tokens.rs` (the style spec's only transcription), `theme.rs` (style fns over iced's stock widgets), `widget/` (the 44×25 toggle and the bar chart, written from scratch because the spec fixes their geometry), vendored instanced typefaces under `assets/fonts/` |
| B5 settings app | **next** | controls generated from `get_config {schema:true}`; `policy.kdl` keys read-only; the Display pane closes stub 15(a) by driving `calibrate_output` |
| B3 bar | not started | `wlr_foreign_toplevel_management` stays absent — the bar lists windows over Wayland and acts over `eclipse-ipc` |
| B4 control center + services | not started | notifications in-house over zbus; tray, audio, network/BT/battery, session, launcher and clipboard crates already chosen in the plan |
| B6 policy viewer | not started | a separate binary reading `policy.kdl` from disk. **Not** over the socket — `Policy/Read` stays closed |

Out of scope by decision (plan B7): desktop icons (DP-6), `mode wm|de`
profiles (DP-3), the compositor-drawn policy editor (that stays milestone 15),
the first-boot overscan offer, and bind/windowrule editing.

The toolkit is iced 0.14 + `iced_layershell` 0.19.1, all Rust, in house — no
Quickshell and no QML (ADR 0038). Blur under a translucent client is abyss's
own `decoration { blur }`, not the toolkit's.

---

## Component map (COMP-01..COMP-16)

| Spec | Where the code is | State |
|---|---|---|
| COMP-01 backends | `crates/abyss/src/backend/{mod,winit,drm,gpu}.rs` | winit exercised; headless exercised by wlcs; DRM verified on real KMS 2026-09-10 (two outputs, native modes, live client). `mod.rs` holds the trait; nothing outside it touches winit/DRM/libinput/GBM types. `gpu.rs` is §4's device ranking, unit-tested over synthetic candidates and observable live with `abyss --list-gpus`. |
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
| COMP-13 human IPC + config | `crates/abyss/src/ipc/`, `crates/abyss/src/config/`, `crates/eclipse-ctl`, `crates/eclipse-ipc` | Socket, gate table, event stream, KDL parse + hot-reload, plus (2026-09-11) the §1.4 write API: byte-splice in-place edits, a declarative schema over every key, per-file gate rows after the `policy.kdl` split, `eclipse-ctl config` verbs, and `crates/eclipse-ipc` as the client half. |
| COMP-14 performance | — | No benchmark harness. The §COMP-14 frame budgets referenced by milestone 4's gate have never been measured. `--stats` now reports frames/fps and render/submit percentiles from a live KMS run, which is a diagnostic, not the harness 9f specifies. |
| COMP-15 testing | `cargo test --workspace`, `.github/workflows/gate.yml` | 128 tests, all passing, now enforced by CI. Unit-level. Zero of the twelve COMP-15 §2 security suites exist. No compat matrix. |
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
   *Unblocked by:* milestones 11–13.
2. **Sensitivity flag is manual** (`state.rs:110`, "Stub until the policy
   engine"). Surfaces can be flagged sensitive and are then redacted, but
   nothing classifies them automatically. *Unblocked by:* milestone 17.
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
6. **Effects all draw; `xwayland` config block still ignored**: `decoration` and `animations` now parse
   and validate in full (`config/mod.rs`), and `active-opacity` /
   `inactive-opacity` / `dim-inactive` / `rounding` render (`render/mod.rs`,
   `window_elements`; the rounded-corner mask itself is
   `render/effects.rs`, a custom texture program bound for the duration of
   each surface's draw, so every subsurface of one window is cut by the same
   rectangle and the window rounds as a single shape — verified nested under
   winit). `shadow` draws as one signed-distance-field pixel-shader element
   per window behind the borders, following the corner radius and discarding
   the region under the window so a translucent window is not darkened by its
   own shadow (also verified nested under winit). Window move animations are
   in `render/anim.rs`: the shell always maps a window at its target, and the
   store hands the render path a shrinking offset, so `scene`/`get_tree`
   never report an interpolated position. The same store drives `fade` (map-in
   alpha ramp, no fade-out) and `border` (focus-change colour crossfade).
   `workspaces` slides an arriving workspace in through the same store.
   `blur` is a dual-Kawase down/upsample chain (`render/blur.rs`) spliced in
   beneath each translucent window; every 9b effect now draws. Agents see target
   geometry, never an interpolated value (COMP-08's rule). All are off by default, so
   the default frame path is the single `space_render_elements` call it was
   before — damage and direct scanout unchanged. `xwayland` is still parsed and
   ignored. *Unblocked by:* nothing for the effects themselves — 9b is done; the
   `xwayland` block waits on COMP-07 work.
7. **`windowrule` is complete except `launching-principal`**
   (COMP-05 §4). `shell/rules.rs` matches at map time and re-evaluates on every
   commit; `float`, `tile`, `workspace N`, `size WxH`, `position X,Y`,
   `output NAME`, `opacity F`, `fullscreen`, `sensitivity secret|private` (raise-only),
   `app-trust`, `seat-compat`, `idle-inhibit`, `no-focus-steal` and `no-agent`
   all parse and apply. `no-agent`, `app-trust` and `seat-compat` set state
   nothing reads yet — COMP-08 `list_toplevels` and the COMP-04 §8 focus locks
   consume them; the COMP-07 clamps (X11 never above `standard`, always
   seat-locked) are applied where the rule is stored, not where it is read.
   Matchers `app-id`, `title`, `pid`, `cgroup`, `output`, `workspace` and
   `xwayland` all work; `app-id`/`title`/`cgroup` are full regexes (the `regex`
   crate, linear-time by construction because titles are client-controlled) and
   a pattern that does not compile is refused at parse time with the rule
   dropped whole, so a rule never applies in part. Placement actions get one
   deferred pass: most clients have no `app_id`/`title` at map time, so a
   window without an identity is re-placed on the first commit that carries
   one (`rules::Placed`), and placement is frozen after that. The `fullscreen`
   action fires last of all, so the rectangle it restores to on unfullscreen is
   the one the other rules just chose. Still unimplemented: the
   `launching-principal` matcher (needs COMP-08 launch tracking); it is refused
   at parse time. *Unblocked by:* COMP-08.
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
   to open. *Unblocked by:* milestone 15.
10. **No lease state** — COMP-08 §4.1 specifies interaction leases and
    enforcement step 6d. Nothing in the tree holds a `handle → LeaseHolder`
    map. *Unblocked by:* milestone 14.
11. **No press-time hit-test resolution** — COMP-08 §10 step 6e requires
    resolving an agent button press to `(handle, node)` before policy
    evaluation. `hit_test` exists in spec only, and `click_at` is gated
    `implemented: false`. *Unblocked by:* milestones 13 and 16.
12. **XWayland eager start, no hardening flags** — see milestone 7. Blocked on
    smithay upstream, verified in the vendored source.
13. **`agent-activity` is the one IPC event kind still never emitted.**
    `window`, `output`, `focus` and workspace events fire from
    `outputs/mod.rs`, `shell/mod.rs` and `ipc/methods.rs`; `config-error` now
    fires from `config/watch.rs::reload_now` when a reload is refused.
    **Stub 13 is therefore only partially closed** — `config-error` is done,
    `agent-activity` is not, so this entry stays open. It closes when
    COMP-08 `eclipse_agent_v1` lands (Phase 2, milestone 11): the event has no
    source to fire from until an agent client can attach, so there is nothing
    to implement before then. Nothing else blocks it.
14. **No `crates/policyd`, `crates/agentd`, `crates/sandbox`** — the TCB crates
    named in the root `CLAUDE.md` do not exist. The module map in that file
    describes the intended end state, not the tree.
15. **Overscan compensation has no frontend.** The backend is complete
    (COMP-03 §2): `outputs/overscan.rs` (per-edge insets, clamping, the
    inverse map), `render/overscan.rs` (the scale-and-pad wrap plus the
    corner markers), the wrap wired into both `backend/drm.rs` and
    `backend/winit.rs`, the absolute-pointer inverse in `input/mod.rs`, the
    `calibrate.rs` seat-grabbing state machine, per-panel persistence keyed
    by EDID identity with the hand-written config winning over saved state,
    and both IPC methods (`set_output` with `overscan`, `calibrate_output`
    with `start`/`commit`/`cancel`) plus the `eclipse-ctl output ID overscan`
    and `eclipse-ctl output ID calibrate` verbs. **What is missing is UI**,
    and it is deliberately batched with the rest of the frontend work:
    - a settings-GUI panel that can start calibration on *any* output the
      user picks, including one plugged in long after first boot;
    - a first-boot/setup step that *offers* calibration rather than forcing
      it — the user's explicit call.
    Nothing is cropped by this feature at any point: the scene is scaled
    down into an inset rect and the margins are left black. The calibration
    overlay is compositor-drawn, not a layer-shell client, because trusted
    UI has to be.

---

## Real-KMS boot record

**2026-09-10.** `abyss --backend drm --stats` booted on this machine's own
hardware for the first time — RTX 4060 Ti, `nvidia-open-dkms`, launched on a
free VT with `openvt -sw` as root, `LIBSEAT_BACKEND=seatd`. Two runs, both
successful. Harness: `/tmp/abyss-kms/run.sh`.

What it proved:

- The libseat session, DRM master acquisition, GBM/EGL/GLES setup, atomic
  modeset, page-flip-driven frame scheduling and libinput keyboard path all
  work on real KMS, on the NVIDIA open driver.
- Both connectors came up on their **own** native modes and stayed there —
  HDMI-A-1 (connector 829) at 1360x768 on crtc 198 / plane 52, DP-3
  (connector 832) at 1280x720 on crtc 390 / plane 244. `Setting new mode`
  fires once per output and sticks.
- A kitty client mapped, rendered, and accepted live keyboard input: the
  second run's screenshot shows `ls` typed at the prompt with its output.
  `grim` exited 0 with a 2640x768 capture spanning both outputs.
- First frame timings from any backend: 465 frames over the run, ~1.0 fps
  idle rising to 38.1 fps while typing, render p50 ~500 us, submit p50
  ~166 us. The variance is damage tracking working, not a stall.

### The bug it found and fixed (COMP-03 §2/§4, commit `e024947`)

Both panels on this box advertise **byte-identical EDIDs**. Output identity is
derived from EDID, and `persist::set_key()` dedupes, so the pair collapsed into
a single `SavedOutput`: the second output was restored onto the first one's
saved mode. The `DrmCompositor` surface was then built for one mode while the
smithay `Output` claimed another, so the primary plane got `SRC_W/H` from one
and `CRTC_W/H` from the other. **NVIDIA primary planes cannot scale**, so every
atomic commit returned `EINVAL` and the backend retried forever — a log that
grew to 2.4 MB in 60 s. The fix keeps the scanout mode and the output mode in
sync via `use_mode`, so `src == dst` on the primary plane. After it, the same
run's deduped log is ~11 KB and the only repeated line is `[x4] config loaded`.

### Two logging traps, both of which have cost a session

- `stats::maybe_report()` calls `tracing::info!(frames, fps, ...)` with **no
  message string**. The journald layer stores those as `F_`-prefixed fields
  (`F_FPS`, `F_FRAMES`, `F_RENDER_P50_US`, `F_SUBMIT_P50_US`) with an *empty*
  `MESSAGE`, so they are invisible to `journalctl -o cat` and to any grep for
  "fps". Read them with
  `journalctl -t abyss -o json | jq 'select(.F_FPS)'`. Their absence from a
  plain log dump is **not** evidence that no frames were submitted.
- `"queueing frame"` (`backend/drm.rs:1083`) is a `warn!` on the **error**
  branch only. A silent journal there means every queue succeeded.

### Cosmetic issues seen during the boot, all still open

None of these stopped the compositor; all are worth fixing before daily-driving.

- `EINVAL` when dropping DRM master on VT-switch-away.
- `ENOENT` reading a mode property blob just after a connector add.
- Duplicate modes in the connector mode list (HDMI-A-1 lists 1920x1080@60 and
  1280x720@60 twice; DP-3 lists 1920x1080@60 and 640x480@60 twice). *Not* an
  ambiguous-PREFERRED bug: `underscan_probe` shows exactly one PREFERRED mode
  per connector. The duplication is the real source of the earlier report.
- `vram=256MiB` in the GPU probe is the PCI BAR size, not real VRAM
  (already recorded as spec gap 2, COMP-01 §4).
- `refresh_mhz` is truncated rather than rounded.

---

## Deferred hardware verification

None of these can run inside the nested dev session; each needs a real TTY
login on the target machine. The first such login happened on 2026-09-10 and
retired part of this list — see "Real-KMS boot record" below for what it
actually covered.

**Done as of 2026-09-10:** the DRM/KMS boot itself (libseat session, DRM
master, GBM/EGL, atomic modeset, page flips, libinput keyboard, VT switch back
out), multi-output composition across two connectors on their own native modes,
and output persistence writing and re-reading `outputs.kdl`.

Still deferred:

- **Milestone 3 gate, remainder** — three monitors, hotplug, dock/undock
  restoring saved layouts. Two panels and no dock on this box.
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

1. **Boot it from the greeter.** The compositor itself now starts on real KMS
   and paints (2026-09-10), but only when launched by hand on a VT. The
   session path — `dist/abyss.desktop` in `/usr/share/wayland-sessions/`,
   `dist/abyss-session` in `/usr/bin/`, the target in the user's systemd
   units, `abyss --session` doing the D-Bus/systemd handoff — has still never
   executed end to end from greetd.
2. **Milestone 3, 4 and 6 gates**, most of which want the same session as
   step 1 plus hardware this box does not have (a third panel, a dock, a lid).
3. **Exercise the milestone 9a protocols against real clients** — the globals
   are advertised and the handlers are written, but a third-party bar over
   `ext_foreign_toplevel_list` and mouse-look in a Proton game over
   `pointer_constraints`/`relative_pointer` have not been run.
4. **A real xcursor theme** on DRM, replacing the built-in amber arrow used
   for named cursor shapes.
5. **Effects (9b)** — optional by COMP-16 Open Decision 1. Borders, opacity,
   dim-inactive, rounding, shadows, blur and all four animations are in;
   nothing of 9b remains unimplemented.
6. **The last COMP-05 §4 rule** — the `launching-principal` matcher (gap 7);
   everything else in §4 now applies.

---

## Spec gaps found

Per the root `CLAUDE.md`: "specs and code disagreeing is a bug in one of them —
do not silently pick." These are recorded, not resolved. None of them has been
fixed by editing either side.

1. ~~**COMP-01 §5 says nothing about the user session manager.**~~ *Closed.*
   The gap was recorded against a stale reading — §5 runs to 14 steps, not 9 —
   but the substance stood: `session.rs` hands off to the systemd user session
   and its docstring calls itself *additive to the spec, not an implementation
   of it* (ADR 0032). §5 now states that session-manager integration is
   deliberately out of scope and points at ADR 0032, so a compositor that never
   talks to one still satisfies the section. Steps 10, 11 and 13 remain
   Phase 2 work with no counterpart in code yet.
2. ~~**COMP-01 §4 GPU ranking.**~~ *Closed — the spec was right and the code
   was taking a shortcut.* `backend/drm.rs` used to call smithay's
   `primary_gpu(&seat_name)`, a different and simpler policy. `backend/gpu.rs`
   now implements §4's ranking directly over every DRM device udev reports for
   the seat: render-capable first, then discrete > integrated > virtual, then
   VRAM, then PCI domain:bus:device.function ascending. The full ranked list
   and the reason are logged at startup.

   Two judgement calls worth knowing about:
   - **Discreteness is decided by PCI topology, not `boot_vga`.** On this
     single-GPU desktop the 4060 Ti is *also* the boot VGA device, so
     `boot_vga` alone would classify it integrated. An integrated GPU hangs
     directly off the host bridge; a discrete card sits behind a PCIe port
     bridge. `boot_vga` is recorded and logged as corroboration only.
   - **VRAM is a hint.** `mem_info_vram_total` where the driver exposes it
     (amdgpu), else the largest prefetchable PCI BAR. The proprietary NVIDIA
     driver publishes neither, and without resizable BAR the aperture reads
     256 MiB on a 8 GiB card. It only ever orders two cards of the same class,
     and it is deterministic.

   §4's hybrid-graphics paragraph is also enforced now: a selected GPU with no
   connectors while another device has them refuses to start rather than
   coming up blind.

   The ordering is proven by unit tests over synthetic candidates (hybrid
   laptop, virtual device, VRAM tiebreak, PCI tiebreak, display-only card), so
   it is verified without a second GPU. `abyss --list-gpus` prints the live
   ranking without taking over the display; on this machine it reports
   `1. /dev/dri/card1 [0000:01:00.0 discrete vram=256MiB connectors=4
   render=yes boot_vga=true]`.

   The ordering was also checked against a real second DRM device: loading the
   `vkms` module produced `/dev/dri/card0`, which sorts *first* by path and so
   would beat the 4060 Ti under any enumeration-order policy. The ranking put
   it second, on two independent criteria (no render node, no PCI parent).
   `vkms` cannot be unloaded while a session is running — logind and the
   running compositor both hold the card open — so it stays resident until the
   next logout; nothing autoloads it. **Still unverified on real multi-GPU
   hardware** — that is COMP-01 §11's test plan and needs a machine with a
   second card, where the discrete-vs-integrated and VRAM comparisons actually
   fire.
3. **COMP-06 §1 protocol list vs. the implemented modules.** *Closed.* The
   fifteen missing protocols are implemented (milestone 9a). COMP-06 §1 also
   gained a note that
   `wl_subcompositor` and `xdg_output` are not separate items — both are
   already advertised, `wl_subcompositor` by `CompositorState::new` and
   `xdg_output` by `OutputManagerState::new_with_xdg_output`
   (`state.rs:212`).
4. ~~**COMP-13 §1.2 config validation.**~~ *Closed — the spec was right and
   the code was wrong.* `Config::load` used to log and carry on, always
   returning a usable config. That was worse than either behaviour §1.2
   contemplates: because an unparseable file was skipped and the config was
   built from `Config::default()`, a typo saved mid-edit made hot-reload
   replace the live config with **built-in defaults** — the opposite of "keep
   the last good config. Never half-apply." Validation is now total: every
   refusal is recorded in `Config::errors` as a `ConfigError` carrying
   `file:line:col` and the offending token. Startup (COMP-01 §5 step 3) prints
   them and exits non-zero; `config/watch.rs::reload_now` keeps `state.config`
   untouched, logs, and emits the `config-error` IPC event the spec names.
   **User-visible behaviour change: abyss now refuses to start on an invalid
   config.**
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
7. **COMP-16 v0.2 splits the old milestone 13** three ways: the fail-closed
   node-level redaction arm to Phase 1 milestone 9d, policy-driven classes
   to milestone 17, and live-tree region redaction to milestone 22. Frame-
   level redaction landing early in milestone 8 was verified and remains
   accurate.

New as of the 2026-09-08 spec baseline. These are gaps the amendments
opened; none is a code regression, and none was introduced by a change to
the tree:

8. **COMP-02 §7 now mandates node-level redaction** (A-10), including the
   fail-closed rule that a stale or absent semantic tree redacts the *whole*
   surface. `render/capture.rs` implements surface-level redaction only.
   Previously this was a milestone 13 item; it is now a requirement of a
   Phase 1 document, which makes the gap visible against COMP-02 rather than
   only against COMP-16. COMP-16 v0.2 sequences it as milestone 9d. The code is not wrong, but the document it is
   measured against changed underneath it.
9. **COMP-05 §1 `Toplevel` gains two fields** (A-12):
   `irreversible_capable: bool` and `class_source: u8`. Neither exists in
   `shell/`. `irreversible_capable` is rule-derived and therefore blocked on
   the window-rules engine, which parses and does nothing today.
10. **COMP-15 §2's twelve blocking suites do not exist.** Twelve, not ten:
    redaction, seat isolation, trusted UI, enforcement, scope leakage, audit
    completeness, X11 posture, classification races, irreversible matching,
    provenance, secrets, egress — the count in the gate table below was
    always right and this entry was wrong. The five added by A-15 joined
    seven, not five. CI now exists (ADR 0035), so these are *unimplemented*
    rather than *unenforceable* — but the harness cannot run any of the ones
    asserting on pixels or a live socket until milestone 9c lands a headless
    backend. Each is now attached to a COMP-16 v0.2 milestone.
11. **COMP-14 gained five budgets** (A-14) covering `classify()`, the
    irreversible matcher, provenance resolution, and egress proxy latency.
    They join the existing §2 targets in never having been measured: there
    is still no benchmark harness. Unchanged by ADR 0035 — CI has a slot
    for this gate and nothing to put in it. COMP-16 v0.2 sequences the
    harness as milestone 9f; before that, milestone 4's exit gate is
    unclosable as written, and has been since v0.1.
12. **`cataclysm` and `cataclysm-pub` do not exist** (ADR 0034). Neither
    appears in the workspace, and the foot fork's vendoring and build
    integration are undecided (F-07 §1 VERIFY). Blocks P-04 entirely.
13. **F-03 is marked DONE in the planning index and has no body.** It is the
    document that would have settled the `abyss`/`helios` name fork, which
    was instead resolved by normalization during Appendix A application.
    Until F-03 exists, that resolution rests on an editorial judgment
    recorded only in Appendix A's application record — and the S-07 §5
    `stamper` enum, a protocol-visible string literal, depends on it.

---

## Open merge — PR #6

`fix(abyss): keep the scanout mode and the output mode in sync`
(`comp16-output-identity-modeset` -> `main`) is **green and unmerged**.
GitHub reports `MERGEABLE` / `mergeStateStatus: CLEAN`, with every check
SUCCESS: gate build/fmt/clippy/test, cargo-deny, wlcs (headless), TCB touch
check, spec-citation, owner-only authorship. It is the first CI run of the
`conformance` job, so it is also the evidence for the wlcs row below.

It stays open only because the local Claude Code auto-mode classifier refuses
to run `gh pr merge`. The repo owner merges it by hand:

```
cd /home/chase/syncedprojects/EclipseOS && gh pr merge 6 --merge
```

Deliberately **without** `--delete-branch`: `comp16-output-identity-modeset` is
the local base of `comp03-overscan-compensation`. Once #6 lands, that branch
rebases onto `main`, pushes, and opens its own PR citing COMP-03 §2.

---

## Test and gate status

CI runs on every push (`.github/workflows/gate.yml`, cloud runner, no GPU —
ADR 0035). The `gate` job is a required status check.

**Live and blocking:** `cargo fmt --all --check`; `cargo clippy --workspace
--all-targets --all-features -- -D warnings`; `cargo build --workspace
--all-targets`; `cargo test --workspace` (128 tests); `cargo deny check
advisories bans licenses sources`; spec-citation check (F-07 §5).

**Live and advisory:** TCB-touch warning (F-07 §4).

**Live and blocking on PRs:** the `gui-coverage` ratchet (`gate.yml:174`,
landed `18b97cf`) — `ci/gui-coverage-exceptions.txt` may not grow and may not
gain an entry even at an unchanged length, so a swap cannot smuggle one in.

The other half of F-01 §4's coverage claim — every `schema::TABLE` path that is
not a collection and not on the exception list has a rendered control — is
**not** asserted anywhere, because there is no settings app to enumerate yet.
It is owed with B5, and it belongs in that crate as a unit test over its own
control registry rather than as a workflow step: a CI step cannot see the
registry without linking the crate, and `cargo test --workspace` already runs.

**Specified and absent.** Every one of these has a CI slot waiting and no
suite to put in it:

| Gate | Required by | Blocked on |
|---|---|---|
| `wlcs` headless conformance | COMP-15 §1 | harness and the blocking `conformance` job exist; the suite runs green off-CI (775 passed / 308 skipped / 0 failed at `ulimit -n 1024`, against 65 skip entries; of the skips only foreign-toplevel's 30 are a real gap, the rest being dead protocols and wlcs self-tests); the first CI run of the job is in flight on PR #6 and has not yet reported |
| `cargo-fuzz` smoke | COMP-15 §3 | proto crates existing |
| Redaction suite | COMP-15 §2 | suite does not exist |
| Seat isolation, trusted UI, enforcement, scope leakage, audit completeness, X11 posture | COMP-15 §2 | suites do not exist |
| S-05 race harness, S-06 matcher corpus, S-07 algebra, S-08 broker, S-09 leak matrix | COMP-15 §2 (A-15) | suites do not exist |
| Benchmark regression gate | COMP-14 §5 | milestone 9f — no benchmark harness (defect 11) |
| Golden decision suite | F-07 §3 | S-02 implemented |
| Red team (S-10) | F-07 §3 | S-01..S-07 implemented |
| Client compat matrix | F-07 §3 | self-hosted runner |

The 128 tests are unit-level. Nothing in the security suite of COMP-15 §2 is
asserted by anything today; redaction was verified by hand, once. **The
presence of CI must not be read as coverage.**
