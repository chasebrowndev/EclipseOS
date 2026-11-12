# 0102 — Panic containment for non-essential features
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
`ec-abyss` is one process on one `calloop` thread (COMP-01 §3, ADR 0018). The
overview, annotations, IPC, config reload, XWayland glue and the enforcing
paths all share it. Until now nothing caught a panic: there is no
`panic = "abort"` in `abyss/Cargo.toml` or any `.cargo/config.toml`, and no
panic hook, so a panic printed the std default message (payload included) to
stderr and unwound out of `calloop::EventLoop::run` and `main`. Unwinding ran
the destructors, so on DRM the libseat session closed and logind/seatd took
the VT back, and the process exited 101 — taking every client with it. One
`unwrap` in the Alt-Tab switcher ended the session.

The only existing `catch_unwind` is the syncobj eventfd probe in
`backend/drm.rs` (a startup capability probe, unrelated).

What a contained panic must never do: continue past a panic in the TCB
(`policy/`, `trusted_ui/`, `audit/`, `render/capture.rs`), the session lock,
input delivery to clients, or the frame path that applies redaction. Those
keep today's behaviour.

### The boundaries surveyed
For each candidate: can an unwind leave state half-mutated, and is there an
"off" state to reset to?

| Candidate | Half-mutation | Clean off state | Verdict |
|---|---|---|---|
| Overview / switcher / hot corners / preview, input side (`overview/`) | Own fields (`switcher`, `overview`, `preview`, `corners`, `thumbs`, `overlay_cache`); calls `shell::` and smithay's seat between them | `overview::close_all` — already what the session lock calls as it engages | **contained** |
| Annotation socket methods (`annotation_*`) | `AnnotationStore` only, plus `damage_all` | A fresh `AnnotationStore` (drawn as nothing) | **contained** |
| Read-only socket queries (`get_*`, `dump_state`, `get_theme`) | `IpcState` window-handle map (append-only, consistent at every step); the reply is built on the stack | Nothing to reset; refuse further calls of that method | **contained** |
| Other IPC commands (`focus_window`, `move_to_workspace`, `set_output`, …) | Layout, outputs, focus — shared with every other path | None: the half-done move *is* session state | not contained |
| Config hot-reload apply (`config::apply_loaded`) | Touches binds, outputs, layout, theme, xwayland, idle, policy file presence | None: "the old config" is not recoverable once half-applied | not contained (follow-up needs a two-phase apply) |
| Window rules evaluation (`shell::rules`) | Runs inside map/commit, mid-`shell` state change | None | not contained |
| Effects / animations / blur / decorations | Run in the frame path, which also applies redaction | Would need a "draw without effects" frame that is provably still redacted | not contained (follow-up) |
| Annotation and overview *drawing* | Frame path | — | not contained: never behind a boundary |
| Screenshot UI / region selector | Interleaves with `render/capture.rs` | — | not contained |
| Optional protocol handlers | Run inside wayland-server dispatch, under smithay's internal state | Not ours to reset | not contained |
| XWayland glue (`xwayland/`) | X11 window identity feeds the scene filter | Restarting Xwayland is the reset; not built | not contained (follow-up) |

## Options
1. **Leave it.** Simple, and the TCB argument is trivial. Every small bug in a
   feature nobody needs to stay logged in still ends the session.
2. **`panic = "abort"` everywhere.** Makes the failure explicit, but takes the
   VT-restoring unwind away and contains nothing.
3. **`catch_unwind` at the event-loop edge for everything.** Survives
   everything, and continues after a half-done TCB or seat operation. Unsound.
4. **A small, named list of contained features, each with a named reset,
   admitting only panics raised in that feature's own source.** Chosen.

## Decision
`abyss/crates/ec-abyss/src/contain.rs` adds one helper,
`contain(state, Feature, |state| …) -> Option<R>`, and a `Contained` bitset in
`AbyssState`. A boundary checks the feature's bit first (off = `None`, the
entry point is not run), then runs the closure under
`catch_unwind(AssertUnwindSafe(..))`.

A panic hook (installed by `main`, `contain::install_hook`) records where every
panic was raised. A caught panic is **kept only if it was raised in one of the
feature's own source files** (`Feature::sources`); otherwise it is re-raised
with `resume_unwind` and unwinds out of the loop exactly as before. A kept
panic sets the off bit, runs the feature's reset, logs one
`tracing::error!` with the feature name and `file:line:col`, emits the
`feature-disabled` IPC event, and the loop continues. The bit is never
cleared: there is no setter that turns a feature back on, and every boundary
checks it before running, so a second panic in the same feature cannot happen.
A panic while a reset runs aborts the process (the hook sees the reset flag
and calls `std::process::abort`).

### Contained features and their reset

| Feature | Boundaries | Reset | Off means |
|---|---|---|---|
| `overview` | `run_action` arms `Switcher`, `ToggleOverview`, `Overlay`; the switcher commit on modifier release; `pointer_moved` (hot corners, overlay motion); `pointer_button` and touch-down while an overlay holds the pointer; the hot-corner dwell timer; the `show_window_preview` / `hide_window_preview` socket methods | `overview::close_all`: switcher, overview and preview closed, corner timer removed, thumbnails and overlay cache dropped, damage | `overview::allowed` is false; the binds are consumed and do nothing; preview methods are refused |
| `annotations` | `annotation_create`, `_update`, `_destroy`, `_clear` | `state.annotations` replaced with an empty store (fading ones and GPU art dropped), damage | the four methods are refused (`DENIED`) |
| `queries` (per method) | `get_workspaces`, `get_desktop_grid`, `get_outputs`, `get_windows`, `get_focused`, `get_metrics`, `dump_state`, `get_theme`, each with its own off bit (`contain::Query`, a `u16` bitset, no allocation) | none | only the method that panicked is refused (`DENIED`); the other queries keep answering |

The source lists: `overview` = `ec-abyss/src/overview/`,
`ec-abyss-render/src/overview.rs`, `ec-abyss-render/src/thumb.rs`;
`annotations` = `ec-abyss/src/ipc/methods.rs`,
`ec-abyss-render/src/annotation.rs`, `ec-abyss-render/src/text.rs`;
`queries` = `ec-abyss/src/ipc/methods.rs`, `ec-abyss/src/theme.rs`.

### Never contained
No boundary may wrap, and no source list may name, any of:

- `policy/`, `trusted_ui/`, `audit/`, `render/capture.rs` (TCB), and
  `ec-policyd`, `ec-policy-eval`, `ec-brokerd`.
- The session lock (`protocols/standard/session_lock.rs`, `state.lock`).
- Input delivery to clients: `keyboard_key` up to and including
  `input_forward`, the pointer/touch/tablet paths that send to a surface, the
  protected-surface gate, the agent compat lock, smithay's seat.
- The per-frame render path of every backend (`backend/{drm,winit,headless}.rs`
  frame functions), which applies redaction, draws trusted UI, and draws the
  overview and annotations from their state.
- Wayland protocol dispatch, `xwayland/`, config apply, window rules, layout,
  workspace and output management.

A panic raised in any of these, even when it happens inside a contained
feature's boundary (the overview calling `shell::focus_window`, a query calling
`render::capture::classify`), is not raised in the feature's sources and so
propagates.

### Why `AssertUnwindSafe` over `&mut AbyssState` is sound here
`catch_unwind` needs the closure to be `UnwindSafe`; `&mut AbyssState` is not,
because an unwind may leave it mid-update. The assertion is justified per
boundary by two facts together:

1. **Origin filter.** A kept panic was raised in the feature's own code. Every
   other module the feature called had returned (or had not yet been called)
   when the unwind began, so its invariants held at the panic point — the
   feature only ever reaches other state through those modules' functions.
   Smithay's and `std`'s internals, `shell/`, `outputs/` and the TCB therefore
   cannot be mid-update when a kept panic unwinds.
2. **Reset.** What *can* be half-written is the feature's own state. The reset
   discards all of it (`close_all`; a fresh `AnnotationStore`; nothing for a
   read-only query whose reply lived on the unwound stack).

A partially executed *sequence* of other-module calls can remain (the overview
switched desktop but had not yet focused the window). Each step left state
consistent, so this is an unexpected but valid state, the same as a human
pressing the two binds separately.

### Fail-closed, never wider authority
- Nothing inside a boundary produces pixels or a reply that escapes the
  boundary on unwind. The backends draw the overview and annotations from
  state, outside any boundary; after a reset that state is "closed" / empty,
  so a contained panic can only leave *less* on screen. A query's reply is
  built on the stack and is never sent if the query panics; the client gets
  `DENIED`.
- Redaction, capture, the lock and trusted UI are never behind a boundary, so
  a panic in them still ends the process. Under the lock or a trusted prompt
  the overview cannot open (`allowed`), and both resets touch neither
  `state.lock` nor `state.trusted_ui`.
- Turning a feature off removes capability; it never adds one. No reset
  changes the gate, a grant, a hook or a policy table.
- A miss in the source match (a `std` method that reports its own location, a
  file renamed without updating `sources`) re-raises: the failure mode of the
  filter is today's behaviour.

### The panic hook
`contain::install_hook` logs every panic it does not contain at `error` with
its location, thread and the boundary it happened inside (if any), plus a
backtrace (symbols and paths only). **It never logs the payload**: std's
`str` slicing panic quotes the string, which may be a window title or typed
text (root invariant on human input). In a release build the std default hook
is no longer chained, so the payload no longer reaches stderr either; under
`cfg(test)` it is chained for uncontained panics so failing tests stay
readable. The process still unwinds after the hook, so the DRM session and VT
are restored as before.

### `feature-disabled`
New subscribable event, `{"feature": "overview" | "annotations"}`, or
`{"feature": "queries", "method": "<method>"}` when one query method went off
(the extra key is additive; consumers reading only `feature` still work),
in `ec-abyss-wire`'s `EVENTS` and `ec_ipc::EventKind::FeatureDisabled`. Emitted
once when a feature or query method turns off and replayed on every `subscribe` while it
stands (as `config-error` is, ADR 0064), so a toast service that connects
later still hears it. No gate row: it is an event, not a method.

## Consequences
- A bug in the overview, the annotation methods or a query costs that feature
  for the session instead of the session.
- The happy path costs two thread-local writes and an unwind landing pad per
  boundary; no allocation, no lock. `pointer_moved` crosses the hot-corner
  boundary once per motion.
- Containment depends on the hook: a process that never calls
  `install_hook` (the wlcs shim, a library user) contains nothing — `contain`
  keeps only panics the hook vouched for.
- Fault injection is `contain::fault(Feature)`, a no-op outside `cfg(test)`,
  placed in `overview::motion`, `annotation_create` (after the store took the
  annotation) and `get_workspaces`. Tests: one per feature (for queries: a panic in `get_workspaces` leaves `get_windows` answering) (the reset, the
  off state on a second call, session state untouched, the lock left alone),
  one that a panic raised outside the feature's sources propagates through a
  boundary, one that feature code reached without a boundary propagates.
- Adding a feature is an amendment to this ADR with its reset and a test.
- Toast surfacing is the frontend's half: `ec-toasts` subscribing to
  `feature-disabled` is not part of this change.

### Residual risk
- **Smithay locks.** A panic raised in feature code *inside a closure smithay
  runs under one of its `std::sync::Mutex`es* (`with_surface_tree_downward` in
  `overview/thumbs.rs::commit_key`, `with_states`) is kept by the origin filter
  but poisons that mutex. Smithay `unwrap`s its locks, so the next access to
  that surface panics outside any boundary and the session ends — later, not
  open. Fail-closed but not contained.
- **Callbacks into a feature from another module.** If a contained feature's
  code ran as a callback inside another module's half-done operation, the
  origin filter would keep a panic that the reset cannot repair. No such path
  exists today among the three features (verified by reading the call graph,
  not enforced by a test).
- **Location attribution.** `#[track_caller]` makes `unwrap`/`expect`/index
  panics report the feature's line. A panic reported at a `std` or dependency
  location propagates even when the bug is in feature code (fails closed).

## Revisit when
- A new feature is proposed for containment (effects, config apply, XWayland
  restart) — each needs its own reset argument here.
- Smithay's lock behaviour changes (poison-free locks would turn the first
  residual risk from "late crash" into "continues on torn state").
- The frame path is split so effects can be dropped from a frame without
  touching redaction; then effects become a candidate.
