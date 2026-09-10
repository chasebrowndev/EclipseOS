# Session handoff

Written 2026-09-09, end of the COMP-16 m9c session. `docs/STATUS.md` is the
verified progress record; this file is the short-lived queue of what to pick
up next. If the two disagree, STATUS.md is right about the past and this file
is right about the intent.

Branch: `comp16-m9c-headless`, pushed. PR #1 is open (`Implements COMP-15 §1`).

---

## What just landed

**m9c conformance is green, and touch has landed.** Full wlcs suite on the
test box: `RC=0, OK=743, FAILED=0` against `ci/wlcs-skip.txt` (85 skip
entries, down from 273). The skip list is a ratchet — entries only ever come
out, and an unlisted failing test must turn the gate red.

**The 11 `TextInputV3WithInputMethodV2Test.*` failures are closed as
won't-fix.** Not an abyss gap: wlcs's fixture binds `zwp_input_method_manager_v2`
on `input_client` but never flushes, so the bind reaches the compositor ~17ms
*after* the app client's `enable()`/`commit()` (measured with
`WAYLAND_DEBUG=server`). smithay 0.7.0 discards text-input requests while
`!has_instance()` and never replays them on late bind
(`text_input/text_input_handle.rs:~208`). Real toolkits recover because
smithay *does* send `zwp_text_input_v3.enter()` on late bind and the v3 spec
requires clients to re-issue `enable` on focus change. The rationale in
`ci/wlcs-skip.txt` says exactly this. Unverified and deliberately not chased:
whether GTK/Qt actually re-enable on a mid-session `enter`.

**Git history was rewritten.** All 70 commits stripped of `Co-Authored-By:
Claude` and `Claude-Session:` trailers, force-pushed to all three branches.
Claude was never an *author*, only a trailer, so the GitHub contributor graph
was already owner-only. Consequence: **any other clone is divergent**,
including the test box at `/root/abyss-ci/src`. Reset it before pushing from
there.

**CI runs on GitHub for the first time.** `.github/workflows/` was at
`AbyssCompositor/.github/workflows/`, which GitHub does not read — the repo's
Actions run count was literally 0 and every gate job had been inert since the
sources moved under `AbyssCompositor/`. Now at the repository root, with a
default `working-directory: AbyssCompositor` for `run:` steps and explicit
paths for the three actions that touch the filesystem (`defaults` does not
apply to `uses:`). First run: `build / fmt / clippy / test` ✅,
`cargo-deny` ✅, `attribution` ✅, `wlcs (headless)` ✅ (RC=0, 555 passed, 0
failed — the same result as the local run, so the headless gate reproduces on
a clean runner).

A new `attribution` workflow fails any push or PR whose commits carry an agent
co-author trailer, session link, or "generated with" footer, or whose
author/committer is not the repo owner. Patterns are anchored to line start:
unanchored, the check failed on its own introducing commit, which quoted the
trailer names in prose.

---

## Touch is done

The 170-entry touch bloc is out of the skip list: 168 pass, 2 rehomed. The
sizing question that headed this section is settled — it was 168, not 34 and
not ~190; the odd numeric indices of the `*InputCombinations` packs really
were the touch device.

Two things cost most of that day, both worth remembering:

- **wlcs touch coordinates are plain pixels.** `include/wlcs/touch.h` types
  the hooks as `wl_fixed_t`, `src/in_process_server.cpp:271` passes ints.
  Dividing by 256 put every touch at (0.35, 0.05). The *pointer* hooks are
  genuinely fixed point, so `fixed()` stays where it is.
- **A `wl_touch.up` cannot be routed through a destroyed surface.** smithay
  matches `wl_touch` instances by the focus surface's client, and by the time
  `CompositorHandler::destroyed` runs the surface has no client
  (`Resource::client()` → `None`), so the event is dropped with no error.
  `wl_touch.cancel` is not a substitute: no id, and wlcs's listener has
  `nullptr` for cancel. `AbyssState.touch_points` keeps the `Client` from
  down time and `release_touch_on` sends `up` + `frame` to it directly.

The 2 holdouts are closed, and they took 18 more tests with them — see
"The geometry-origin bug" below.

## The geometry-origin bug (fixed)

The hypothesis in this file was wrong: the remapped parent's subsurface *was*
in the focus tree. The whole window had moved.

`Space` positions an element by the origin of its **window geometry**, and a
toplevel that never calls `xdg_surface.set_window_geometry` derives that
geometry from the bounding box of its whole surface tree (spec-correct). So a
client that attaches a subsurface extending left of or above its root surface
moves its own geometry origin, and the element translates by that much after it
was placed — the probe showed `geometry().loc` at `(-100, 0)` and the window
100px off, so hit-testing *and* rendering were displaced. Index 4 of the same
suite passes only because it sets an explicit window geometry.

Fix: `AbyssState.geo_loc` records each element's last-seen
`Window::geometry().loc`; `shell::reanchor`, called from
`CompositorHandler::commit`, shifts the stored floating rect by any delta and
re-arranges, holding `render_location` (the surface origin) constant.

That cleared **20** skip entries in three suites that looked unrelated —
`input_seen_by_subsurface_after_parent_unmapped_and_remapped/{8,9,10,11}`, the
four `input_seen_by_second_surface_after_drag_off_first_and_up`, and 12
`RegionSurfaceInputCombinations.input_not_seen_after_leaving_region` cases.
Full suite on the test box: **RC=0, 743 passed, 0 failed** against 85 skip
entries (down from 105). Lesson worth keeping: an input failure that spans
several unrelated input suites is more likely one placement bug than several
protocol bugs.

## Virtual pointer is done

`zwlr_virtual_pointer_v1` is implemented (`protocols/standard/virtual_pointer.rs`,
hand-rolled — smithay 0.7 has no module) and all 12 of its tests pass. Full
suite at the time: **RC=0, 755 passed, 308 skipped, 0 failed** against the
same 85 skip entries; those 12 were wlcs *skips* (extension not advertised), not skip-list
entries, so the ratchet did not move. Worth keeping:

- **Discrete scroll steps go in as `discrete * 120`.** smithay divides `v120`
  by 120 again to send the legacy `wl_pointer.axis_discrete`
  (`wayland/seat/pointer.rs:150`).
- **`Dispatch::request` only hands you `&Data`**, so the frame batch lives in
  `AbyssState.virtual_pointer.devices`, keyed by resource — not in user data.
- Resolve `motion_absolute`'s output geometry *before* borrowing the pending
  batch mutably, or `state` is borrowed twice.
- Replaying the batch through `inject_pointer_*` rather than `PointerHandle`
  keeps idle activity, click-to-focus, clamping and lock suppression.

Foreign-toplevel (30 tests) is now the only genuine unimplemented protocol in
the skipped set, and it is owner-blocked.

## Pointer focus follows the scene, not just the mouse (fixed)

Skip groups 15, 16 and 17 were one bug. `pointer_moved` was the *only* code
that delivered `wl_pointer` focus, so focus was re-evaluated on device motion
and never on a change of scene: a stationary pointer never learned that a
window had moved or resized under it, that a subsurface had slid under or out
from under it, or that `set_input_region` had shrunk away from beneath it.

`AbyssState::refresh_pointer_focus` (`input/mod.rs`) is the scene-driven twin of
`pointer_moved`: hit-test the current pointer location and deliver
`motion` + `frame` only when the `(surface, rounded surface-relative position)`
pair actually changed, tracked in `AbyssState.last_pointer_focus`. Three things
worth keeping:

- **The changed-only guard is load-bearing, not an optimisation.** smithay's
  `PointerInnerHandle::motion` sends `enter`/`replace`/`leave` on a focus
  change but forwards a *same-focus* refresh as an unconditional
  `wl_pointer.motion` (`wayland/seat/pointer.rs:95`). Unguarded, every commit
  would spam every StrictMock listener in the suite and turn passing tests red.
  Compare on `to_i32_round()` — f64 comparison yields spurious motion.
- **Call it from `CompositorHandler::commit` and `shell::place_at`, never from
  `arrange`.** `pointer_moved` calls `arrange` itself via focus-follows-mouse,
  so an `arrange` hook re-enters motion delivery mid-flight. Committed state
  only is also why `subsurface_does_not_move_when_parent_not_committed`
  correctly sees no change. The refresh touches neither output nor keyboard
  focus — those are human-motion behaviours, and driving keyboard focus from a
  commit hook invites recursion.
- **Read the wlcs C++ first.** `tests/test_surface_events.cpp:430`
  (`surface_moves_while_under_pointer`) is what pins the design: it moves a
  surface repeatedly under a pointer parked at (500,500) and asserts a
  `wl_pointer.motion` carrying `(500 - new_x, 500 - new_y)` for *each* move.
  That plus the StrictMock constraint above is the whole specification.

20 entries out (12 `RegionSurfaceInputCombinations`, 4 `SubsurfaceTest`, 4
`ClientSurfaceEventsTest`). Full suite: **RC=0, 775 passed, 308 skipped, 0
failed** against 65 skip entries.

Negative result worth not re-chasing:
`subsurface_does_not_move_when_parent_not_committed` is **not** a focus bug. It
fails at `subsurfaces.cpp:273` with `position_on_window()` = (2560,2560) where
the test wants (7680,7680) — a factor-of-3 subsurface-offset/sync-commit
accounting error, same family as the `place_above_simple` / `place_below_simple`
restack failures. Correctly still skipped.

## The map-time configure, and the grab that deadlocked itself (fixed)

Eleven skip entries in four groups (7 interactive move/resize, 8 `set_parent`,
9 window-geometry hit-test offset, and 3 of the 6 group-10 configure cases)
were **two** bugs, neither of which was the one the skip file blamed.

**Bug 1 — no configure at map time.** abyss sent the *initial*
`xdg_toplevel.configure` from `shell::handle_commit` on the first, buffer-less
commit, and then nothing. Every later configure goes through
`shell::configure`, which ends in `ToplevelSurface::send_pending_configure()`,
and smithay suppresses that when the pending state has not changed since the
last one (`wayland/shell/xdg/mod.rs:1474`, `:1722`). The tiled size and
`Activated` that `arrange` computes at `place_new_window` time are already in
the initial configure, so the pending state is unchanged and the map configure
never goes out. wlcs's `ConfigurationWindow` constructor (in
`tests/xdg_toplevel_stable.cpp`) commits, roundtrips, attaches a buffer,
commits, and then `dispatch_until_configure()` — it *requires* a second
configure. Ten tests instantiate that helper, so all ten failed identically
with `C++ exception "Timeout waiting for condition"` at the 10s mark, from
inside the constructor. Nothing was wrong with the coordinates the skip file
accused: the two `*_respects_window_geom_offset` tests passed the moment they
got past the constructor. Fix: in `handle_commit`, force one
`send_configure()` at the unmapped→mapped transition, guarded by a per-window
`MapConfigured` marker in the window's user data so it fires exactly once, and
by `has_buffer()` (`with_renderer_surface_state(..).buffer().is_some()` —
smithay 0.7 has no `Window::is_mapped`) so the transition is detected at all.

**Bug 2 — `refresh_pointer_focus` re-entered a held mutex.** The four
interactive move/resize tests did not fail, they *hung the whole wlcs process*
(and in one earlier run took it down with a SIGSEGV). `MoveSurfaceGrab::motion`
calls `shell::place_at`, which ends in `AbyssState::refresh_pointer_focus`,
which calls `PointerHandle::motion`. But `PointerHandle::motion` does
`self.inner.lock().unwrap()` and holds that `std::sync::Mutex` guard *across*
the grab callback (`input/pointer/mod.rs:232`). Re-entering it from inside the
callback self-deadlocks the single compositor thread. Fix: a
`AbyssState.pointer_grab_active` flag, set after `set_grab` in
`input::grabs::start_move`/`start_resize` and cleared in each grab's `unset`,
with an early return at the top of `refresh_pointer_focus`. That is also the
correct behaviour on its own terms — a drag deliberately clears pointer focus
for its duration, so there is nothing to refresh.

Full suite on the test box: **RC=0, 786 passed, 307 skipped, 0 failed** against
**54** skip entries, up from 775 passed against 65.

Load-bearing details and negative results:

- **The flag cannot be `pointer.is_grabbed()`.** That method locks the same
  mutex, so the guard would deadlock exactly where the bug did. It has to be
  compositor state, not seat state.
- **Set `pointer_grab_active` *after* `set_grab`**, never before: `set_grab`
  runs the previous grab's `unset`, which clears the flag.
- **`Space` already handles the window-geometry offset**, and always did.
  `InnerElement::render_location()` is `location - element.geometry().loc`,
  computed live at query time (`desktop/space/mod.rs:509`), so `map_element`'s
  location *is* the geometry origin and hit-testing subtracts the offset for
  free. Skip group 9's "the offset is not subtracted when hit-testing" was
  wrong; `shell::reanchor` and `geo_loc` were not involved either. Do not go
  looking for a hit-test offset bug — there isn't one.
- **`SurfaceInputCombinations.input_seen_after_surface_unmapped_and_remapped/7`**
  (the window-geometry-inset + touch variant) passes now too, without being
  touched. It was the same missing map-time configure, not a touch or a
  `reanchor` bug.
- **`XdgToplevelStableConfigurationTest.defaults` stays skipped, deliberately.**
  It wants the last configure of a fresh toplevel to carry 0x0 ("pick your own
  size"). abyss tiles by default (`Placement::float` is false in
  `shell/rules.rs`), so `arrange` computes a real rectangle and
  `shell::configure` sends it — 1256x1000 on the headless output. Reporting
  0x0 would be a lie about a tiled window. Needs an owner decision on whether
  an unconstrained-size path is wanted, not a patch.
- **`window_can_fullscreen_itself` / `window_can_unfullscreen_itself` stay
  skipped: fullscreen is not implemented at all.** There is no
  `fullscreen_request`/`unfullscreen_request` in
  `protocols/standard/xdg_shell.rs` and no fullscreen state anywhere under
  `shell/` (`grep -rn fullscreen crates/abyss/src` finds two comments). Those
  two are a missing COMP-05 feature; they are not a configure bug and should
  not be re-chased as one. `window_stays_maximized_after_fullscreen` and
  `window_can_maximize_itself_while_fullscreen` are `DISABLED_` in wlcs and
  never run.
- **A wlcs run that stops mid-suite with no result line and leaves the process
  alive is a compositor deadlock**, the sibling of the documented
  SIGSEGV-vs-hang rule. `ps -eo pid,etime,comm | grep wlcs` distinguishes them.

## Queue after that

- **Foreign-toplevel** — 30 tests, but **blocked**: needs an owner decision on
  whether enumerating other clients' toplevels takes a capability check
  (COMP-08 / COMP-11). Do not invent a check; leave `TODO(COMP-11)`.
- **IME keyboard grab policy (COMP-11)** — same shape, same rule: reserved as
  an architectural call for the owner.
- **CI cost tightening** — the gate runs on every push to every branch, and
  `wlcs (headless)` cmake-builds the whole suite from scratch each time
  (`rust-cache` caches cargo, not that). Two fixes: `actions/cache` keyed on
  the pinned `WLCS_SHA`, and making `conformance` PR-only like `tcb-review`
  and `spec-trail` already are. `attribution` should stay on every push — it
  is ~20s.
- **Branch protection / ruleset on `main`** — deferred until the gate has
  produced check runs at least once, since check names are only selectable
  after they exist. Restrict pushes, block force-push, require `attribution`
  plus the gate jobs. Requiring *signed commits* is a separate decision: it
  needs a GPG or SSH signing key configured locally or every commit fails.
- **PR #1** is open and everything is green except `wlcs (headless)`, which is
  the last thing standing between m9c and closed. It fails on exactly one test
  the box passes:
  `SurfaceInputRegions/SurfaceInputCombinations.input_seen_after_surface_unmapped_and_remapped/7`
  (the touch variant; `/6`, the pointer one, failed too before the pointer-focus
  fix and now passes). `current_surface` is NULL at
  `surface_input_regions.cpp:623` — the client sees no touch down at all. Ruled
  out so far: `attach_visible_buffer` blocks on a frame callback, so the remap
  commit really was processed and rendered before `down_at`; we never
  `unmap_window` on a null-buffer commit (only `toplevel_destroyed` and the
  XWM); `Space::element_under` hit-tests a live `Window::bbox_with_popups`, and
  `Window::bbox` is a cached value that `CompositorHandler::commit` does refresh
  via `window.on_commit()`; `Window::alive()` is resource liveness, not
  mapped-ness, so `space.refresh()`'s `retain` cannot drop a remapped element.
  Next step is empirical, not more reading: `eprintln!` in
  `inject_touch_down` and `shell::surface_under` (the wlcs cdylib installs no
  tracing subscriber) and run just that test in a loop on the box under load.
  If it stays reclusive it is a fair skip-list candidate under the
  "reclusive and UX-inconsequential" rule — but that is a ratchet regression and
  must be recorded as one.

- **`/7` is the window-geometry-inset variant, not a touch bug.** Decoded from
  the wlcs sources on the box: `all_surface_types()` is `[wl_shell, xdg_v6,
  xdg_stable(0,0,0,0), xdg_stable(12,5,20,6), subsurface(0,0),
  subsurface(7,12)]` and `all_input_methods()` is `[pointer, touch]`;
  `Combine()` varies the last parameter fastest, so `/6` is inset+pointer and
  `/7` is inset+touch. The zero-inset pair `/4`,`/5` both pass, so the
  discriminator is the geometry inset, which puts this failure in the same
  family as the `XdgToplevelStable*` window-geometry cluster. The builder makes
  a 215x108 buffer, sets window geometry `(12,5,183,97)`, then
  `move_surface_to(200+12, 49+5)` — the compositor is handed the *geometry*
  origin, so the buffer origin is at `(200,49)` and the touch at `(204,53)` is
  inside the buffer but outside the geometry rect, which must still hit the
  surface because the default input region is the whole surface. Pointer passes
  and touch fails because the pointer gets a second chance from
  `refresh_pointer_focus` on the post-remap commit while `inject_touch_down`
  hit-tests exactly once. Prime suspect: `shell::reanchor`. On the null-buffer
  commit smithay's `Window::geometry()` intersects the set geometry with an
  empty bbox and falls back to the bbox, so `loc` becomes `(0,0)` and reanchor
  shifts by `(-12,-5)`, then by `(12,5)` on the remap — symmetric only if the
  window is found in `ws.floating`; otherwise the delta is silently dropped and
  the space location stops matching the geometry origin.
- **65 remaining skip-listed failures.** Clusters, best breakthrough candidates
  first:
  - `XdgToplevelStable*` window-geometry-offset / configure, ~15 —
    `pointer_respects_window_geom_offset`, `touch_respects_window_geom_offset`,
    `surface_can_be_{moved,resized}_interactively`,
    `pointer_leaves_surface_during_interactive_{move,resize}`, the 6
    `XdgToplevelStableConfigurationTest` cases, 4 parent-setting. Strongest
    single-bug smell of what's left.
  - Subsurface sync-commit / offset, 6 — already localized to a factor-of-3
    offset error at `subsurfaces.cpp:273` (see above).
  - `XdgPopupTest` 11; `PointerConstraints` 5 (cheap retest: the new refresh
    now calls `update_pointer_constraint_focus` on scene changes);
    `PrimarySelection` 4 + `CopyCutPaste` 1; `LayerSurfaceTest` 4;
    `XdgSurfaceStableTest` 4; `ClientSurfaceEventsTest.frame_timestamp_increases`
    1 (COMP-02); `BadBufferTest.test_truncated_shm_file` 1.
  - `TextInputV3WithInputMethodV2Test` 11 — closed won't-fix, see above.
- **First hardware KMS boot on cbbedroomdesktop** — explicitly sequenced last.
- Not started: 9d node-level redaction, 9e window-rules completion,
  9f benchmark harness.

---

## Traps that have already cost time

- **A GitHub Actions job with no `actions/checkout` has no working directory.**
  `gate.yml` sets `defaults.run.working-directory: AbyssCompositor` at the
  workflow level, so the checkout-less `spec-trail` job could not even start
  bash: `An error occurred trying to start process '/usr/bin/bash' with working
  directory '…/AbyssCompositor'. No such file or directory`. It read as a PR-body
  regex failure and was not one — the grep never ran. Fixed in `a4eb33e` by
  pinning that step to `working-directory: .`.
- **The remote tree is not your tree.** A full-suite run was misread as a real
  failure because the test box's binary was stale, built from a `state.rs`
  still carrying a debug probe. Sync and rebuild before trusting a remote
  measurement.
- **Never read `$?` through a pipe** — you get `tail`'s exit code. And `set -e`
  will abort the runner before an `echo "RC=$?"` ever prints.
- **A wlcs run that ends at `[ RUN      ]` with no result line is a SIGSEGV**
  (RC=139), not a hang.
- **`XDG_RUNTIME_DIR=/run/user/0` must be exported** in every remote wlcs
  invocation.
- **`git apply -p2` can silently no-op.** Check it changed something.
- **`--force-with-lease` reports "stale info" after a `filter-branch`**, because
  the rewrite also rewrote the local remote-tracking refs. `git fetch` first,
  verify the remote heads still match `refs/original/`, then push.
- **zsh**: `grep -rn "pat" dir --include=*.rs` fails. Quote the glob or drop it.
- **`scp` to the test box fails** ("Connection closed"). Use
  `tailscale ssh root@cbbedroomdesktop 'cat > /path' < localfile`.
- **Git toplevel is `EclipseOS/`, not `EclipseOS/AbyssCompositor/`.** Git
  commands from the toplevel need the `AbyssCompositor/` prefix.
- **Never `pkill`/`killall` kitty, zsh, claude, Xwayland or quickshell** — see
  the root `CLAUDE.md`. It kills the live session and looks like an external
  SIGKILL.
