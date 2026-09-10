# Session handoff

Written 2026-09-09, end of the COMP-16 m9c session. `docs/STATUS.md` is the
verified progress record; this file is the short-lived queue of what to pick
up next. If the two disagree, STATUS.md is right about the past and this file
is right about the intent.

Branch: `comp16-m9c-headless`, pushed, ~20 commits ahead of `main`. No PR open.

---

## What just landed

**m9c conformance is green.** Full wlcs suite on the test box: `RC=0, OK=555,
FAILED=0` against `ci/wlcs-skip.txt` (273 skip entries). The skip list is a
ratchet — entries only ever come out, and an unlisted failing test must turn
the gate red.

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

## Next dev step: touch injection in the harness

The biggest single ratchet win, and the only large item not blocked on a
design conversation. Roughly a day.

1. `WlcsEvent::TouchDown` / `TouchMove` / `TouchUp`.
2. `inject_touch_down` / `_move` / `_up` in `crates/abyss/src/input/inject.rs`
   — today that file has only the three pointer paths. Route through
   `AbyssState`, not the seat handle directly, for the reason in its module
   doc: idle activity, focus, clamping and lock suppression must all fire for
   an injected event too.
3. `Seat::add_touch()` in `state.rs`.
4. Real `create_touch()` in `crates/wlcs-abyss` in place of the stub.

**Resolve the sizing question first (~10 min).** I have been quoting "~170
tests unlocked" and cannot substantiate it. Firmly confirmed: **34** — the 16
`AllSurfaceTypes/TouchTest.*` plus 18 explicitly touch-named entries
elsewhere. The two large blocs, `RegionSurfaceInputCombinations` (122) and
`SurfaceInputCombinations` (50), are gtest-parameterized by device type with
bare numeric indices; the skipped ones skew heavily odd (153 odd vs 30 even),
which *suggests* odd = touch, but wlcs sources are not checked out locally so
it is unproven. Check the parameter list in wlcs's
`tests/surface_input_regions.cpp` before committing to a number. It is the
difference between 34 and ~190.

---

## Queue after that

- **`zwlr_virtual_pointer_v1`** — 12 tests, self-contained, no design call needed.
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
- **Open the PR** for `comp16-m9c-headless`. Body must cite the spec section
  (`Implements COMP-15 §1`) or the `spec-trail` job blocks it — that job is
  live now, not theoretical.
- **~230 remaining skip-listed failures** in 18 catalogued groups: future
  milestone work, not this session's.
- **First hardware KMS boot on cbbedroomdesktop** — explicitly sequenced last.
- Not started: 9d node-level redaction, 9e window-rules completion,
  9f benchmark harness.

---

## Traps that have already cost time

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
