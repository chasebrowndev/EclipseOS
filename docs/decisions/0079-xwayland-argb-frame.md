# 0079 — Patch smithay's X11 frame window to depth 32 for ARGB clients
Status: accepted
Date: 2026-10-31
Deciders: chase (owner), Claude (advisory)

## Context
X11 clients with a 32-bit ARGB visual (Steam, GTK/Qt with an RGBA visual) are
composited fully opaque. smithay 0.7.0 `X11Wm` creates each client's frame
window with `COPY_DEPTH_FROM_PARENT` (depth 24) and reparents the client into
it (`xwayland/xwm/mod.rs`, MapRequest). Xwayland then tags the frame's
`wl_buffer` `XRGB8888` although the alpha bytes are present;
`buffer_has_alpha()` is false, so the opaque region is the whole surface and
the `NO_ALPHA` shader runs. A native Wayland ARGB window blends and blurs
correctly; the same GTK window over X11 does not. Nothing in abyss alters this.
It blocks real glass for the steam-glass add-on (ADR 0066); Weston solved the
same problem the same way.

## Options
1. **Do nothing.** Steam ships `backed` mode: the glass tints composited over
   `#0b0906`. Readable, not see-through.
2. **Patch smithay (`[patch.crates-io]`, vendored).** Frame windows of depth-32
   clients are created depth 32 with their own colormap. Depth-24 clients are
   unchanged. Needs an ADR and its own PR (root `CLAUDE.md`, Smithay pin).
3. **Custom texture import in abyss.** Re-derive alpha outside smithay's buffer
   path: large, touches the render hot path.
4. **Change Xwayland.** Out of our tree.

## Decision
Option 2, carried as `0079-smithay-argb-frame.patch` and applied by a vendored
copy or `[patch]` entry; upstream the change to smithay and drop the patch when
it lands.

## Evidence
Nested headless session, GPU (gles2) renderer, GTK RGBA test window
(red, alpha .35) over a (10,10,10) backdrop: before, pixel = (89,0,0) (alpha
ignored); with the patch, (96,7,7) = 89 + 10·.65, i.e. blended. The frame
window reports depth 32 under `xwininfo`.

An independent run over a white backdrop gave (255,166,166) patched against
(89,0,0) unpatched, and a depth-24 GTK client stayed unchanged.

Not yet tested: input and focus through the 32-bit frame, blur interaction,
non-ARGB 32-bit clients, and the abyss gate with the patch applied.

## Consequences
- The colormap is freed on the normal unmap path only; a client destroyed
  without an UnmapNotify leaks it, as the frame window already does.
- Every ARGB X11 client becomes translucent: right for Steam, but an X11 app
  with a stray RGBA visual and garbage alpha would show through. The
  `windowrule "blur off" { xwayland; }` default still gates blur.
- Colormap per ARGB frame, freed on destroy.
- `ec-steam-glass --glass` becomes usable; until then `backed` is the default.
- Smithay stays pinned at `=0.7.0`; the patch is carried until upstreamed.
