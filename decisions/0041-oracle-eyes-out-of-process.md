# 0041 — Oracle-Eyes runs out of process, on two revocable capabilities
Status: accepted
Date: 2026-09-15
Deciders: chase (owner), Claude (advisory)

## Context

`Oracle-Eyes/spec.md` §4 proposes shipping Oracle-Eyes as a shared object the
compositor `dlopen`s, talking to the host through a hand-rolled `#[repr(C)]`
`HostVtable` of five `extern "C"` function pointers (capture, damage, draw,
config, log).

That collides with the first line of `CLAUDE.md`: "No ambient
authority. Every operation requires a capability check." A vtable handed to a
loaded object *is* ambient authority — once the pointers are in its hands there
is no gate left, no `check()` to fail closed, no per-call decision to audit, and
nothing for the owner to revoke short of deleting the `.so`. It also puts
third-party code inside the address space of a single-threaded core that owns
`AbyssState`, where a segfault or a blocking read in the plugin is a compositor
freeze, and it invents a second unversioned ABI beside the two stable IPC
surfaces the system already has.

The spec half-concedes this: §4a already splits the heavy work (OCR, the model
call) into a separate daemon, leaving the in-process half as little more than a
forwarder. The question is whether the forwarder needs to exist at all.

Oracle-Eyes needs exactly two things from the compositor: pixels of a screen
region, and a way to put text on screen. Both already have gated transports.

- Pixels: `ext-image-copy-capture-v1`
  (`protocols/standard/image_copy_capture.rs`) and `zwlr_screencopy_v1` share
  one fail-closed `decide()` gate (`protocols/standard/screencopy.rs:81`, ADRs
  0027/0030) keyed on the client's process name.
- Control: the COMP-13 line-delimited JSON-RPC socket (ADR 0028), owner-uid
  checked via `SO_PEERCRED`, every method classified in a static fail-closed
  `TABLE` (`ipc/gate.rs:47`) where an absent method simply does not exist.

## Options

1. **`dlopen` plugin with the `HostVtable`, as specced.** One process, no
   serialisation, direct access to damage. Ambient authority; crash and hang
   surface shared with the core; a third ABI to keep stable; nothing to revoke.
2. **A new bespoke Wayland protocol for annotations.** Idiomatic and typed, and
   clients are already gated by process name. But it is a new protocol to design,
   version and maintain for one consumer, and it puts annotation content on the
   same path as client surfaces — the path ADR 0040 is trying to keep it off.
3. **Out of process over the surfaces that exist.** Control via new gated
   `annotation_*` methods on the COMP-13 socket; pixels via
   `ext-image-copy-capture-v1` behind the existing capture allowlist. No new ABI,
   no new protocol, two capabilities the owner grants and revokes independently.

## Decision

Option 3. Oracle-Eyes is an ordinary out-of-process daemon in `Oracle-Eyes/`,
outside the Abyss cargo workspace and outside its TCB. It holds two capabilities:

- **Capture** — one config line in `policy.kdl`: `capture { allow "oracle-eyes" }`.
  Absent, it reads nothing; the gate is fail-closed.
- **Control** — the owner-uid control socket, where it may call only the
  `annotation_create` / `annotation_update` / `annotation_destroy` /
  `annotation_clear` methods and subscribe to the `keybind` (and later `damage`)
  event kinds.

The spec's §4 plugin ABI is rejected outright and §4a's daemon becomes the whole
of Oracle-Eyes.

## Consequences

- Revocation is one line in a config file, per capability, hot-reloaded. An
  Oracle-Eyes that can draw but can no longer read the screen is a coherent,
  reachable state — which is the point.
- A crash, a hang, or a runaway allocation in OCR or the model call cannot stall
  the compositor. Oracle-Eyes may use threads and async freely; the
  single-threaded-core invariant is Abyss's, not its.
- Cost: a round trip and a pixel copy per query. Acceptable — the model call
  dominates by orders of magnitude.
- Damage-driven capture is harder from outside. DRM's damage lives inside
  smithay's `DrmCompositor` rather than an `OutputDamageTracker`
  (`backend/drm.rs:74`, `:1073`), so a `damage` event kind is real work. Automatic
  mode therefore ships first on a fixed poll, and damage-driven refresh comes
  last, behind it.
- The compositor, not the daemon, owns placement, collision avoidance, eviction
  and text sanitisation — only it knows output geometry, and the caller must not
  be able to influence anything but the glyphs.
- **Prompt injection is not solved by this decision, only contained.** OCR'd
  screen text is fully attacker-controlled: any page can render "ignore previous
  instructions" and have it read back and sent to the model. What this buys us is
  blast radius. The answerer runs a fresh `claude -p` per query with all tools
  disabled, one turn, and a system prompt framing its input as untrusted data; the
  reply can do nothing but become clamped, control-char-stripped glyphs in a pass
  that is visually distinct from Trusted UI (ADR 0040) and holds no phrase. A
  rendered "click allow" borrows no authority. Treat every future capability grant
  to Oracle-Eyes against this: it is a component that regularly executes text
  written by an adversary.
- We owe: `gate.rs` table entries and tests for the four methods (non-owner uid
  denied; an unlisted `annotation_*` name is method-not-found), and a COMP-18
  section in Volume 1 for PRs to cite.

## Revisit when

The round trip or the pixel copy shows up as a real cost in a profile, or a
second consumer wants annotations — at which point per-caller ownership, quotas
and a richer identity than the process-name allowlist all need revisiting.
