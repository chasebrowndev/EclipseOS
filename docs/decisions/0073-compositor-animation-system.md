# 0073 — Compositor animation system: events, presets, ghosts, transition shaders
Status: accepted
Date: 2026-10-04
Deciders: chase (owner), Claude (advisory)

## Context
Milestone 9b shipped four animations: window glide, workspace slide-in, fade on
open, and border crossfade. They sit behind one `animations.enabled` switch that
is off by default (C-01). Settings → Animations is a single toggle. Several
transitions have no animation at all: close (no buffer survives the unmap), the
outgoing workspace half, minimize, fullscreen and layer surfaces. The bar chips
animate from a separate `bar.motion` setting. Toasts, the center and the
launcher do not animate at all.

The owner wants one animation system covering every bundled component, with
several styles per event, from minimal to extra, and presets that work out of
the box:
- the default is on;
- cheap 2D styles are built in;
- expensive effects ship as an add-on pack that can be installed at any time.

Forces:
- COMP-02 §9 requires that animation never change what an agent sees.
  `scene`/`get_tree` must report target geometry.
- Capture is in the TCB (`render/capture.rs`), and animation must stay out of
  it.
- Idle outputs must render nothing, and direct scanout must survive when
  nothing is moving.
- ADR 0066 and ADR 0041 keep add-on code out of abyss's process.

## Options
1. **Keep the 9b shape and add names.** This is the smallest change. It has no
   presets, no close or outgoing animations, and nothing for Settings to show
   besides raw curves.
2. **An event model with presets in the compositor; panes animate only their
   own internals.** It needs close snapshots and an outgoing ghost pass. Layer
   surfaces are animated once, for every client.
3. **Each pane animates itself.** iced panes cannot animate their own map or
   unmap without fighting layer-shell, every pane would carry its own copy, and
   third-party layer clients would get nothing.

For expensive effects:
- (a) compile every effect into abyss and keep it inert until a hook enables it;
- (b) the pack ships GLSL as data, and abyss compiles it behind a hook.

## Decision
Option 2 with (b).

**Events.** The events are:
`window-open`, `window-close`, `window-move`, `workspace-switch`,
`window-to-workspace`, `minimize`, `unminimize`, `fullscreen`, `layer-open`,
`layer-close`, `focus`, `chip-add`, `chip-remove`, `bar-layout` and `toast`.

**Config.** `animations { preset; speed; reduce-motion; <event> { style;
duration-ms; curve } }` resolves each event in this order: preset, then
per-event override, then speed, then reduce-motion. The function lives in
`ec-abyss-config`, so abyss, Settings and the panes share it.
- Presets are `off`, `subtle`, `smooth` and `lively`. **The default is `smooth`.**
- "Custom" is derived (some override is present), not stored.
- Curves add `spring` and `bounce`.

**Built-in styles.** Built-in styles use only offset, scale and alpha. The
catalog per event is in `docs/CONFIG.md`.

**Animation is render-only:**
- The shell maps every window at its target geometry. `get_tree`, `scene` and
  hit-testing never see a position mid-flight.
- Leaving and closing windows are drawn as **ghosts**. A ghost is either:
  - a live surface no longer in the `Space` (the outgoing workspace half,
    minimize), or
  - a snapshot of the last committed textures, taken before the buffer is
    released (close).
- Ghosts are drawn only by `collect_elements`. They are never hit-tested,
  listed or captured.
- `capture.rs` builds its own pass from the `Space` and never reads animation
  state, so screencopy always shows target geometry. A source-scan test pins
  this.

**Retile hold.** A tiled window closing on an active workspace with motion on
holds its neighbours at their current geometry until its close ghost is done
(or its armed-clock grace runs out); any other layout change ends the hold and
retiles at once. The neighbours really stay put, so `get_tree` and hit-testing
still match what is drawn. A shader-driven ghost or open draws no compositor
decoration (border, shadow, dim): the shader owns the whole look.

**Never animated:** session lock, Trusted UI and full-output scrims.

**Under load,** COMP-14 §6 shedding steps animations down after blur and
shadows: add-on styles first fall back to built-in, then motion becomes a fade,
then everything turns off.

**Transition shaders.** A new hook, `transition-shaders`, gates them, and this
ADR amends ADR 0066's hook table.
- **Catalog.** While the hook is on, abyss reads
  `/usr/share/eclipse/transitions/<pack>/<style>.kdl` and `.frag` from that
  constant, package-owned directory. Styles are named `pack:style`.
- **Pack presets.** A pack may also ship `<pack>/presets/<name>.kdl`: `label`,
  `base` (a built-in preset key, not `off`) and one `style "<event>" "<style>"`
  per event, where `<style>` is a bare name from the same pack that serves the
  event. The id is `pack:name`. The same limits apply (64 KiB, no symlinks, no
  dot-files); a preset with an unknown event or base, a duplicate event, or a
  missing or non-serving style is dropped alone and logged like a rejected
  style. The `presets/` directory is not scanned as styles, and presets exist
  only while the hook is on. `get_config.animations.pack_presets` lists them
  (`[]` when none) as `{id, label, base, styles: {event: "pack:style"}}`, so
  Settings can show a card next to Off/Subtle/Smooth/Lively.
- **Shader contract.** A shader is a GLSL ES 1.00 fragment body appended to
  abyss's header, with fixed uniforms: `progress`, `eased`, `direction`,
  `kind`, `size`, `content`, `travel`, `side`, `seed`, `time`. It samples only
  an offscreen copy of its own window. There is no backdrop sampler.
- **Limits:**
  - at most 64 KiB per file;
  - no `#extension` and no `while`;
  - the styles that the resolved config and the pack presets select are
    compiled ahead of use, on the frame after the catalog or `animations`
    config changes, so a run never compiles mid-animation;
  - a compile failure, or repeated budget overruns, drops the style back to its
    built-in fallback. The frame that compiles a style, or first renders a run,
    does not count as an overrun.
- **Armed clock.** A shader run's clock starts on its first successfully drawn
  frame, so that frame is `progress == 0`. A run that is never drawn expires
  after a 1 s grace. The snapshot of a ghost run (close, minimize) is rendered
  offscreen once and reused; open runs reuse one damage tracker.
- **`reach`.** A style may set `reach #true`. Its quad then grows toward
  `travel` until the target lies inside it, with `margin` still applied on the
  other sides, so `content` may be off-centre. The size limit still applies.
- **Chip target.** `set_window_chip_rect` (a command, owner-only) lets the
  taskbar report where it drew a window's chip, as an output-local rect or
  `null`. abyss keeps it per window and forgets it on unmap. It is render-only:
  minimize and unminimize aim `travel` at the chip centre, falling back to the
  dock. It moves nothing and grants nothing.
- **Missing pack.** A `pack:style` named in config whose pack is not installed
  falls back with one warning. It is not a config error.

## Consequences
- **Process boundary.** This is the one exception to "add-on code runs in its
  own process". The driver's GLSL compiler runs inside abyss on files from an
  installed package, at the trust level of an installed package. The output is
  confined to its own window's pixels and never reaches capture. Nothing else
  from an add-on runs in-process.
- **Default on.** The default flips to on. C-01's "animations off" is amended
  by C-18. COMP-02 §9 widens from "geometry-only" to "render-only: offset,
  scale, alpha, and add-on transition shaders".
- **Config migration.** `animations.enabled` and `animation "<name>"` are
  legacy. They still load, and `ec-ctl config migrate` rewrites them.
  `bar.motion` gains `animations.bar-layout` as its successor.
- **New IPC.** `set_config_values` (one commit, one `config` event) and
  `value: null` (remove a key) exist so Settings can apply a preset in one
  write.
- **We owe these tests:**
  - target geometry from `get_tree` mid-animation (COMP-02 §11);
  - ghosts not listed and not hit-tested;
  - `capture.rs` never references animation state;
  - idle renders zero frames and keeps the scanout path;
  - an animating bench scenario within the M9f budget.

## Revisit when
- Third-party (unsigned) transition packs are wanted. This also triggers ADR
  0066's signing revisit.
- An agent-facing need arises to see animated geometry.
- Capture is asked to show animations.
