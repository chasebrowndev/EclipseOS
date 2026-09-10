# Session handoff

Written 2026-09-09, last refreshed **2026-09-10 (late session)**.
`docs/STATUS.md` is the verified progress record; this file is the short-lived
queue of what to pick up next. If the two disagree, STATUS.md is right about
the past and this file is right about the intent.

**Everything below the `--- history ---` divider is closed root-cause notes
kept so they are not re-chased. Read the two sections above it first.**

---

## State as of 2026-09-10 late

Three branches, none merged into each other:

| Branch | PR | State |
|---|---|---|
| `main` @ `7a15947` | — | PR #1 merged. Ruleset `main protection` (id `22774955`) is active: **direct pushes to `main` are blocked, every change needs a PR.** |
| `comp16-m9e-fullscreen` @ `214fe86` | [#2](https://github.com/chasebrowndev/EclipseOS/pull/2) | xdg_toplevel fullscreen (COMP-05 §4). All checks green except `wlcs (headless)`, which was still running at handoff — **it is the only verification of the two unskipped fullscreen tests**, since wlcs cannot be built locally (needs cmake + boost + gtest, an unrequested package install). Check it before merging. |
| `comp16-drm-preboot` | [#3](https://github.com/chasebrowndev/EclipseOS/pull/3) | Branched off `main`, **not** off the fullscreen branch. Three pre-boot `drm.rs` fixes + `docs/BUILDING.md`. |

### What fullscreen (PR #2) actually does

`state.fullscreen: HashMap<Window, Option<Rectangle>>` mirrors `maximized`.
`shell::fullscreen_toplevel` / `unfullscreen_toplevel` model themselves on the
maximize pair, with three deliberate differences:

- Fullscreen takes **`space.output_geometry()`**, not `usable_area` — it
  ignores layer-shell exclusive zones by design.
- **Fullscreen wins over maximized.** A maximized window that goes fullscreen
  keeps its `maximized` entry, so unfullscreen drops it back to maximized
  rather than to its pre-maximize rect. That is what xdg-shell asks for.
- The `Top` layer (the bar) must render **below** the window stack while a
  fullscreen surface owns the output, or the bar draws over it. `Overlay`
  stays above — that layer is reserved for things that outrank fullscreen,
  such as a lock screen. `collect_elements` has no access to `AbyssState`, so
  the flag is a new `fullscreen: bool` parameter threaded in from all three
  backend call sites. In `drm.rs` it **must** be computed before
  `state.drm.as_mut()` or borrowck rejects it.

Also fixes a pre-existing leak: `unmap_window` never removed the `maximized`
entry.

Two plan premises turned out to be wrong and were corrected in the branch:
the `windowrule "fullscreen"` fixture at `config/mod.rs:1902` lived inside
`rejects_unhonourable_windowrules` (asserting the rule is *thrown out*), so it
was moved into `parses_windowrules` as a positive case; and
`XdgToplevelStableConfigurationTest.defaults` is a **separate** deliberate
divergence (abyss tiles by default, so reporting a 0x0 configure would be a lie
about a real rectangle) — it stays skipped, unrelated to fullscreen.

### What PR #3 fixes, and why it gates the boot

`Duration::from_secs_f64(1_000f64 / m.refresh as f64 / 1_000f64)` in the VBlank
handler was wrong twice. `m.refresh` is **mHz**, so the frame period is
`1000 / refresh` seconds; the trailing divide made every `wp_presentation`
refresh report 1000x too short (16.6 us instead of a 16.6 ms frame at 60 Hz).
And `from_secs_f64` **panics** on a non-finite or negative argument. It runs on
every page flip, so a mode reporting 0 mHz takes the compositor down the moment
the screen lights up. Now `frame_period()`, which falls back to 16 ms.
`refresh_mhz` saturates instead of truncating with `as i32` — a negative result
was the other route into that panic.

The primary-plane format list was `[Abgr8888, Argb8888]`. A driver advertising
only the opaque `XRGB8888` on its primary plane fails the modeset outright,
surfacing as **one `warn` and a black screen**. `Xbgr8888`/`Xrgb8888` are now
appended after the alpha variants, so behaviour is unchanged where they already
worked.

**Do not attempt the first boot without PR #3's branch.** Unfixed, the most
likely outcome is a process that dies about a second in and teaches you
nothing.

---

## Plan: first KMS boot, on `chase-pc`

`cbbedroomdesktop` has been offline for hours (`tailscale ping` times out), and
the VM pass is **cancelled by the owner** — "we've done enough VM testing".
So the first real KMS boot happens on `chase-pc` itself, from a second VT.

### Why this is safe for a running session

Only the **active** VT's logind session holds DRM master. `Ctrl+Alt+F2` makes
tty2 active, logind revokes Hyprland's master, and Hyprland is *suspended*, not
killed — every process inside it survives, terminals and long-lived shells
included. `Ctrl+Alt+F1` brings it all back. Nothing is `pkill`ed.

### Preconditions, verified on chase-pc 2026-09-10

- One GPU: `/dev/dri/card1` + `/dev/dri/renderD128` (nvidia). Render node
  present, so `gpu.rs` has something to find. **No `card0`** — anything
  assuming `card0` is wrong, though `drm.rs` hardcodes no path.
- logind session 4 on `seat0`/`tty1`. `seatd.service` is also active, but
  **libseat prefers logind**, so the missing `seat` group does not matter.
  `chase` is in `video` and `input`, which is what is needed.
- `nvidia-drm.modeset=1` is **not** on the kernel cmdline, but recent
  `nvidia-open-dkms` defaults it on and Hyprland could not run otherwise.
  Note `/sys/module/nvidia_drm/parameters/modeset` is mode 0400, so the guard
  at `drm.rs:149` cannot read it and silently no-ops — a `modeset=0` boot would
  surface as an opaque EGL error from `EGLDisplay::new(gbm)` at `drm.rs:600`,
  not as the clear message that guard intends.

### Before switching VTs

**Open the out-of-band recovery path first.** `chase-laptop`
(`100.124.173.22`) is online on the tailnet. From it:

```
tailscale ssh chase-pc
pkill -x abyss        # exact name only. NEVER pkill -f.
```

A compositor that wedges holding DRM master can make the VT switch back fail,
leaving a black screen with the desktop alive underneath. There is no keyboard
recovery from that. `cbbedroomdesktop` is not available as the recovery box.

### The run

```
git checkout comp16-drm-preboot
cargo build
```

Then `Ctrl+Alt+F2`, log in as `chase`, and:

```
cd ~/syncedprojects/EclipseOS/AbyssCompositor
XDG_RUNTIME_DIR=/run/user/1000 ./target/debug/abyss --backend drm
```

A Claude session left running under Hyprland survives the switch and can tail
`journalctl --user -t abyss -f` from tty1 while you are on tty2.

### What to verify, per milestone

This one boot is the gate for three milestones. Verify each explicitly rather
than declaring victory on "it started".

- **M3** — multi-output, hotplug (unplug/replug a DP cable), fractional scale,
  output persistence across a replug. Never pin an output by name; DRM
  connector names shift across driver upgrades on this hardware.
- **M4** — dmabuf, explicit sync, direct scanout, damage tracking, and **VRR**.
  VRR "auto" (`drm.rs:393`) only engages while a fullscreen surface owns the
  output, so it has *never* engaged — M4 cannot close without PR #2's
  fullscreen also being present. Confirm from the log whether `drm_syncobj`
  registered or warned; `supports_syncobj_eventfd` (`drm.rs:629`) probes a DRM
  *core* ioctl and logs on both branches, so it takes the true branch on any
  current Arch kernel regardless of driver — it is **not** the nvidia risk.
- **M6** — session lock, idle, DPMS, and **suspend/resume and lid**, which have
  never run anywhere.

Budget this as debugging, not verification. `drm.rs` is the largest
never-executed surface in the tree.

### Ranked list of what is most likely to bite

Items 1-2 are fixed in PR #3. The rest are unfixed and worth recognising fast
rather than re-deriving at a wedged console.

3. `drm.rs:600` `EGLDisplay::new(gbm)` on nvidia-open, with the unreadable
   modeset guard above making a `modeset=0` failure opaque.
4. `refresh_mhz` truncation — fixed in PR #3, but it also feeds the advertised
   mode list (`drm.rs:334`/`:339`) and the config mode-match (`:311`).
5. **`scan_connectors` ordering** (`drm.rs:252-274`) + `outputs/mod.rs:563`:
   unplugging the **last** monitor calls `unregister` before `sync_fallback` at
   `:274`, so `fallback_id()` is `None` and `removed.windows` is silently
   dropped. Client loss, not a panic. Fix is to hoist `sync_fallback` above the
   departure loop.
6. `drm.rs:786-804` — `UdevEvent::Removed` for our own GPU re-scans against a
   dead fd instead of tearing down `DrmData`. Warn-spam, then a fallback-only
   compositor.
7. `outputs/mod.rs:566` — `target.workspaces.len() - 1` underflow, latent on
   the hotplug-removal path.
8. smithay `drm_syncobj/mod.rs:73` `unreachable!()` is reachable via
   `drm.rs:632`. Silent abort, zero diagnostic, if it ever trips.
9. `drm.rs:305` `info.modes()[0]` and `drm.rs:854` cached-`n` indexing —
   invariant-safe today, both refactor hazards.

Other hardcodes worth knowing: `drm.rs:69` `FALLBACK_SIZE (1920,1080)`;
`outputs/mod.rs:298` `refresh: 60_000` (fabricated); `drm.rs:443` `"HEADLESS-1"`
(collides across boots for user config rules); `drm.rs:352`
`GbmBufferFlags::RENDERING|SCANOUT` with no modifier-less fallback;
`render/cursor.rs:24-48` a hardcoded 12x19 scale-1 cursor bitmap, which is a
12-pixel speck on a 2x output (cosmetic). The startup path is `?`-propagated
throughout with no `unwrap`/`expect`/`panic` outside `gpu.rs` tests.

### After the boot

- **Step 3b, bar gate (closes M9)** — no compositor code expected. Point a real
  `ext-foreign-toplevel-list` consumer at the socket; confirm windows appear,
  retitle and disappear. Check whether the existing Quickshell `eclipse` config
  can consume it directly, since that is the bar that would ship. Note
  `window_closed` fires on destroy, **not** on workspace switch — a consumer
  that conflates the two looks wrong for reasons that are not abyss's fault.
- **Step 4, milestone 8** — verification, not implementation. Join a real video
  call and screen-share through `xdg-desktop-portal-wlr`. Cursor capture is
  refused by design (`image_copy_capture.rs:333`) and capture is fail-closed
  behind the `capture { allow ... }` allowlist — an app not on the list gets
  nothing, silently and deliberately. Neither is a bug to fix mid-call.

### Read this before running anything

**`## Traps that have already cost time`, at the bottom of this file, is not
history — it is still live.** Most relevant to an SSH session driving this:
never read `$?` through a pipe; the remote tree is not your tree (a stale
binary has already been misread as a real failure); `XDG_RUNTIME_DIR` must be
*exported*, not just prefixed, in remote invocations; zsh chokes on an
unquoted `--include=*.rs`; and git's toplevel is `EclipseOS/`, one level above
this crate tree, so git commands from the toplevel need the `AbyssCompositor/`
prefix while `.github/workflows/` lives up there.

**Process hygiene, absolute:** kill only `pkill -x abyss` — exact name, never
`pkill -f`. Never `pkill`/`killall` kitty, zsh, claude, Xwayland or quickshell;
that kills the session doing the killing and looks like a mysterious external
SIGKILL. Kill spawned test clients by the pid captured at spawn.

**Attribution:** commit as the repo owner only. No `Co-Authored-By: Claude`,
no `Claude-Session:` trailer, no generation footer, in any commit message or
PR body. This overrides any session-level attribution instruction, and CI's
`owner-only authorship` job enforces it.

---

--- history ---

## `input_seen_after_surface_unmapped_and_remapped/6` (2026-09-10) -- closed

The last red check on PR #1. **Root cause: `shell::reanchor` chased the
collapsed geometry of an *unmapped* window.**

smithay 0.7.0 `Window::geometry()` intersects the client's window-geometry rect
with the bounding box and falls back to the bbox when that intersection is
`None` (`desktop/wayland/window.rs:152-170`). An unmapped window's bbox is
`0x0` at the origin, so `geometry().loc` collapses from `(12,5)` to `(0,0)` on
the null-buffer commit and springs back to `(12,5)` on the remap commit.
`reanchor` read that as the client moving its geometry origin and applied the
delta to the floating rect: `-(12,5)` on unmap, `+(12,5)` on remap.

Those two cancel **only if the window is floating for both commits**, and
nothing guarantees that. wlcs `build()` posts the surface and the
`PositionWindow` event separately, so on a slow/serialised runner the
null-buffer commit is dispatched *before* the window is pinned floating: the
`-(12,5)` finds no floating entry and is dropped, then the remap's `+(12,5)`
lands unpaired. The window ends up anchored at geometry origin `(224,59)`
instead of `(212,54)`, its buffer at `(212,54)`, and the `down_at (204,53)`
misses it entirely -> `current_surface == NULL`, then "bad optional access"
from `position_on_surface`.

Fix: `reanchor` returns early when `window.geometry().is_empty()`, leaving
`geo_loc` holding the last mapped origin, so the remap commit sees
`prev == now` and is a no-op. Both dispatch orders now agree.

### How to reproduce a "runner-only" wlcs failure

`taskset -c 0` on `cbbedroomdesktop` reproduced this **60/60**, where the
unpinned run passed 100%. Pin to one core before concluding a wlcs failure is
GitHub-runner-specific; `--gtest_repeat` alone did not surface it.

### Negative results -- do not re-chase

- **Not a lost/late `refresh_pointer_focus`.** Probes showed the remap commit
  (and its `refresh_pointer_focus`) landing *before* `pointer_moved`, with
  `surface_under` returning `None` for a pointer that was geometrically
  outside the window. The focus plumbing was never at fault; the window was in
  the wrong place.
- **Not the pointer-vs-touch input path.** `/7` (same inset, touch) passed for
  a timing reason, not a behavioural one: the misplacement is in the shell, so
  whichever input method asks gets the same wrong answer.
  `WlcsEvent::PointerButtonDown` bypassing `process_input_event` is real (see
  the popup section) but irrelevant here.
- **Not `place_at`/`Space` window-geometry handling.** As recorded earlier,
  `InnerElement::render_location` subtracts the window-geometry offset live;
  that half is correct. The bug was `reanchor` *also* adjusting the stored
  rect, from a geometry value that was not a real one.

---

## Where the 2026-09-10 session left off

- **Head is the popup-cluster commit** on `comp16-m9c-headless` (parent
  `028ad7c`). The substantive commits of the session are `6dd1ea8` (map-time
  configure + grab deadlock, see its own section below) and the popup-cluster
  commit (popup grabs, reactive popups, reposition geometry, popup pointer
  leave); the rest are docs and the `spec-trail` working-directory fix.
- **Suite: `RC=0, 797 passed / 307 skipped / 0 failed` against 43 skip
  entries**, measured on `cbbedroomdesktop` after the popup-cluster commit.
  Previous baselines: 786 passed / 54 entries, and 775 / 65. Local gate green
  on all five.
- **PR #1 CI was re-running on `6dd1ea8` when the session paused.** `wlcs
  (headless)` was the only red job before that commit and the fix cleared it
  locally on the box, but the *CI* run had not finished — check
  `gh pr checks 1` before assuming PR #1 is green.
- **The popup cluster is closed, 11 of 12.** It was four independent root
  causes, not one shared bug (see "Popup cluster" below). The only entry left
  skipped is `XdgPopupTest.zero_size_anchor_rect_stable`, and it is an upstream
  smithay 0.7.0 blocker, not an abyss bug -- its rationale in
  `ci/wlcs-skip.txt` now says so.
- **Two hypotheses were burned on `/7` before `6dd1ea8` and must not be
  re-chased**: that it was touch-specific, and that the
  `XdgStableSurfaceBuilder(12,5,20,6)` window-geometry inset shifted the
  hit test through `shell::reanchor`. Both wrong. `Space` subtracts the
  window-geometry offset live in `InnerElement::render_location`, and `/7` was
  the same missing map-time configure as the rest of its cluster.

---

## Popup cluster (2026-09-10) -- four root causes, 11 of 12 unskipped

The 12 skipped `XdgPopupTest` entries across the three parameterizations were
**not** one shared bug the way the previous ~15-entry cluster was. Four causes:

1. `popup_can_be_repositioned` -- smithay's `PopupSurface::post_commit_hook`
   sets `attributes.current = last_acked` on **every** popup commit once the
   initial configure is sent, so a one-shot `publish_popup_geometry` at the
   initial configure was silently undone by the next commit and
   `PopupKind::location()` read `{0,0}`. Fixed by republishing the geometry on
   every popup commit in `shell::handle_commit`.
2. `when_parent_surface_is_moved_a_reactive_popup_is_moved` -- nothing re-ran
   `unconstrain_popup` + configure when the parent moved. Fixed with
   `shell::refresh_reactive_popups`, called from `shell::arrange` (and so from
   `place_at`). It deliberately sends a plain `send_configure()` and **no**
   `repositioned` token, because the test asserts zero `repositioned` calls;
   that is legal only because the positioner is reactive (smithay's
   `send_configure` errors for a non-reactive popup after the initial one).
3. `popup_gives_up_pointer_focus_when_gone` (x2) -- no `wl_pointer.leave` when
   the popup died, so wlcs failed at `in_process_server.cpp:1222`. Fixed with
   `XdgShellHandler::popup_destroyed` -> `shell::popup_gone`, which sends
   `pointer.motion(None)` + `frame` while the `wl_surface` is still alive, then
   `refresh_pointer_focus()` to deliver the enter to the toplevel. Safe because
   `PopupTree::iter_popups()` filters out non-`alive()` nodes, so the dying
   popup is no longer hit-tested.
4. The three grab tests (x2 params) -- a hand-rolled grab stack
   (`AbyssState::popup_grabs`). smithay's own `PopupGrab` is unusable here: it
   requires `KeyboardFocus: WaylandFocus + From<PopupKind>` and the orphan rule
   forbids `impl From<PopupKind> for WlSurface`, which is abyss's
   `SeatHandler::KeyboardFocus`. `grab()` pushes and takes keyboard focus;
   `new_toplevel` dismisses; an outside press dismisses via
   `PopupManager::dismiss_popup` (child-then-parent `popup_done` ordering comes
   free from `PopupNode::send_done`) followed by `refocus_topmost`. Dismissal
   runs **after** the button is delivered, since xdg-shell forbids `popup_done`
   preceding its cause, and `grab_root_of` scopes "outside" by comparing popup
   roots -- which is why `does_not_get_popup_done_event_before_button_press`
   (a click inside the parent toplevel) correctly does not dismiss.

### Negative results -- do not re-chase

- **"Wrong geometry store."** Disproved. `publish_popup_geometry` already
  writes `XdgPopupSurfaceData.current.geometry`, which is exactly the store
  `PopupKind::location()` reads. The bug was the timing (cause 1), not the
  store. The other store, `SurfaceCachedState.current().geometry`, drives
  `PopupKind::geometry()` and is not involved.
- **`reposition_request` needed a second configure.** No --
  `PopupSurface::send_repositioned(token)` is
  `send_configure_internal(Some(token))` and already emits the configure; the
  extra `send_configure` was doubling it.
- **wlcs pointer input does not flow through `AbyssState::process_input_event`.**
  Buttons arrive as `WlcsEvent::PointerButtonDown/Up`, handled in
  `backend/headless.rs` (~line 538), which calls
  `input/inject.rs::inject_pointer_button`. Any input-path behaviour that must
  be visible to wlcs has to be added in **both** `input/mod.rs::on_pointer_button`
  and `inject_pointer_button`. The first grab-dismissal attempt only patched the
  former and looked like the grab hook was never firing at all.
- **`XdgPopupTest.zero_size_anchor_rect_stable` is not an abyss geometry bug.**
  The `xdg_positioner` global is delegated wholly to smithay, and smithay
  0.7.0's `handlers/positioner.rs` posts `xdg_positioner::Error::InvalidInput`
  ("Invalid size for positioner's anchor rectangle.") for `width < 1 ||
  height < 1`, while xdg-shell only mandates an error for *negative* values.
  wlcs sets a 0x0 anchor rect, so the client is killed before abyss places
  anything; abyss's own math yields the expected (170, 230). Needs a smithay
  bump (its own PR under the `=0.7.0` pin) or taking the positioner handler
  over locally.

### Next

- Layer-shell keyboard interactivity (skip section 13) and the subsurface
  placement cluster (section 15) are the two largest remaining blocks.

---

## What just landed

**m9c conformance is green, and touch has landed.** Full wlcs suite on the
test box, as of `6dd1ea8`: `RC=0, 786 passed / 307 skipped / 0 failed` against
`ci/wlcs-skip.txt` (54 skip entries, down from 273 and from 85 earlier in this
session). The skip list is a ratchet — entries only ever come
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

## Queue after that (2026-09-09) -- SUPERSEDED, kept for the unblocked items

Stale as of 2026-09-10: the CI cost tightening landed in `b782848`, branch
protection is done (ruleset `22774955`), PR #1 is merged, and fullscreen is
implemented in PR #2. The foreign-toplevel / IME COMP-11 blocks and the wlcs
skill clusters below are still accurate. The live queue is the two sections at
the top of this file.


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
- **PR #1** is open. `wlcs (headless)` was the last red job; the missing
  map-time configure fixed in `6dd1ea8` cleared it along with the whole
  `XdgToplevelStable*` cluster, so re-check the run before assuming anything
  here is still true. The long-chased
  `SurfaceInputRegions/SurfaceInputCombinations.input_seen_after_surface_unmapped_and_remapped/7`
  had nothing to do with touch, hit-test offsets, `reanchor` or window-geometry
  insets — every one of those hypotheses was wrong (see the negative results
  above). It was the same missing configure, and it passes untouched.
- **54 remaining skip-listed failures** (down from 65). Actual counts from
  `ci/wlcs-skip.txt`, best breakthrough candidates first:
  - Popups, 12 — `XdgPopupTest` 4, `XdgPopupStable/XdgPopupTest` 4,
    `LayerShellPopup/XdgPopupTest` 4. Now the largest live cluster and three
    parameterizations of the same suite, so the single-bug odds are good.
  - Subsurfaces, 6 — `XdgShellStableSubsurfaces/SubsurfaceTest` 4 and
    `SubsurfaceMultilevelTest` 2; already localized to a factor-of-3 offset
    error at `subsurfaces.cpp:273` (see above).
  - `PointerConstraints` 5 — cheap retest, the pointer-focus refresh now calls
    `update_pointer_constraint_focus` on scene changes.
  - `XdgSurfaceStableTest` 4; `LayerSurfaceTest` 4; `PrimarySelection` 4 +
    `CopyCutPaste` 1; `XdgToplevelStableConfigurationTest` 3 and
    `XdgToplevelStableTest` 2 (what survived `6dd1ea8`; `defaults` and the two
    fullscreen cases are deliberate, fullscreen is an unimplemented COMP-05
    feature); `ClientSurfaceEventsTest.frame_timestamp_increases` 1 (COMP-02);
    `BadBufferTest.test_truncated_shm_file` 1.
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
