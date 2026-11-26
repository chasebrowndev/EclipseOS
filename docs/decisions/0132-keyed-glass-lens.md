# 0132 — Keyed glass lens
Status: proposed
Date: 2026-11-25
Deciders: chase (owner), Claude (advisory)
Amends: 0080 (keyed glass for opaque clients)

## Context
ADR 0080 made the pixels a client declares as a key colour translucent, with
the blur behind them. A keyed surface (the URL field, the selected tab, the
toolbar and tab strip, a popup, a Home card) is therefore a flat tint of the
backdrop at the key's alpha. The compositor's glass, the frosted and lensed
surfaces of COMP-02 §9, is richer than that: it bends and disperses what lies
behind an edge. Keyed surfaces should read as the same material.

The add-on (ADR 0079) decides which pixels are keyed and at what alpha. It
should not decide how the glass looks. The look already has a home: the
compositor's `decoration.blur.glass.lens.*` settings (refraction, bevel,
dispersion, rim): the family `docs/CONFIG.md` assigns to small controls, which
a keyed surface is.

## Options
1. **A lens per keyed surface, optics from the compositor's glass settings.**
   The add-on declares one on/off `lens` flag per key. The compositor draws the
   matched pixels with the Lens glass preset, bent by a distance field of the
   key mask, at the key's alpha. Pros: the add-on stays a colour declaration,
   and the look is the one the shell's glass panes already use. Cons: a label and a
   distance pass per keyed window per commit; a larger GPU budget for keyed
   windows only.
2. **The add-on sends the lens parameters.** Pros: per-app tuning. Cons:
   a client decides how glass looks, which the appearance settings exist to
   prevent, and every add-on version would carry its own optics.
3. **Keep the flat key.** No cost, no change. The keyed surfaces stay tints.

## Decision
Option 1. `set_client_strips` keys gain an optional `lens` boolean (default
`false`). It is parsed in `ec-abyss/src/ipc/client_strips.rs`; a non-boolean is
`invalid_params`. It is stored as `Keys::lens` in `ec-abyss-render`.

A key with `lens` true is drawn in three steps, for the window it matches:

1. **Label.** A pass over the client's own pixels writes, per texel, the
   matched key's index (0 for none) into the blue channel as `label / 4`. The
   first matching key decides, as it does for the flat tint. The pixel test is
   the same one the flat key uses (within 1/255 per channel).
2. **Distance.** A pass over the label map writes, into the red channel, the
   distance to the nearest texel of another label, over `KEYED_LENS_DIST_MAX`
   (32 px), clamped to 1 on the plateau.
3. **Glass.** The Lens preset's glass body draws with a `KEYED_LENS` variant:
   the backdrop is the blurred texture (the rim reads the lightly blurred
   level, as native lens glass does), the lens edge and its normal come from
   the distance field, and the output is the glass at the key's alpha
   (premultiplied), cut at the window's rounded corners. The key's own colour
   is composited at its alpha over the bent backdrop, so the hue survives. The strength, bevel
   and dispersion come from `decoration.blur.glass.lens.*` (the Lens preset's
   look, as shaped layer glass and glass panes read it), clamped to the
   field's reach. The window-glass keys (`refraction`, `bevel`, `dispersion`,
   `rim`) are not read.

The packed texture (`[blurred backdrop | lightly blurred backdrop | key mask]`,
three wide) is built per keyed window. A custom shader samples one texture, so
the three are copied into their thirds in separate passes. A key stays flat if
the window's backdrop is not packed (glass mode only). The rim and edge shading
treat a keyed surface as a small control, not as the size of the lens region.

Only texels the label pass matched to a lens key are touched: every other
texel of the client's content is drawn unchanged (C-21 still holds: content is
never faded to obtain glass). A key without `lens` is drawn exactly as before. A window with no lens built
(no lens key, or a failed compile or pass) uses the flat program, so its pixels
do not change. Live and capture draw the same: `render/capture.rs` reaches the
keyed draw through `Still`, and `still.rs` mirrors the lens. This ADR amends
C-23 and COMP-02 §9: a lens key's pixels take that key's alpha *and* the
Lens glass optics, where a flat key takes the alpha and the blur only.

The `password` role, human input and TCB paths are not involved.

### Status of the implementation
Landed: the `lens` key field and parser (`ec-abyss`, `Keys`); the label,
distance and glass shader variants (`effects.rs`), compiled on demand through
`Programs::get` (a compile failure logs once and the key stays flat); the pure
label, distance, clamp and containment arithmetic (`keyed_lens.rs`); the GPU
label, distance and pack passes; the `window_elements` and `insert_blur`
wiring (`lib.rs`); the still-frame mirror (`still.rs`); timed spans for the
three passes (`Span::LensLabel`, `LensDistance`, `LensPack`). The lens sits
directly above its backdrop and covers the strip or rect the backdrop covers,
so the backdrop may be larger than the lens.

Verified on a real GL driver (nested headless session, Mesa, AMD render node;
one session at a time, its pid captured and checked gone) with a synthetic
client painting a key colour, over a checker and stripe wallpaper:
- the lens bends the checker across a bevel inside each keyed region, with a
  lit rim, and keeps the key's hue (a brown key stays brown, a blue one blue);
- a second `everywhere` key (a field in the body) lenses too;
- with `lens` off the same keys draw as flat tints, and non-key pixels are
  identical in both.

Second verification round, with real Firefox 160 and the add-on in the nested
session (the helper reaching the nested socket through `ECLIPSE_SOCKET_DIR`),
on the owner's wallpaper and `decoration` config: a first build packed only the
blurred backdrop beside the mask, so the rim had no crisp content to bend and
the shading faded with the size of a whole-window region; the lens then read as
a brightening only. Packing the lightly blurred level too, and the small-control
size rule, made the bevel, rim and wallpaper bend visible at the window edge.
The add-on keeps the sidebar and cards key flat, because a lens shows the
backdrop through them and they stopped reading as cards.

Two defects found while verifying, both in the pass plumbing and now fixed:
the label shader read the alpha byte of an X-format buffer (undefined), and the
pack pass gave `render_texture_from_to` an absolute damage rectangle for the
mask half. Smithay's damage is relative to the destination and is clamped into
it, so the mask half was never written and every lens was empty. Debug output
of nested sessions goes to the user journal under the live session's
identifier; filter by the nested pid (`_PID=`) when reading it.

Measured, `EC_GLASS_PROFILE`, 400x300 region, two lens keys, mean GPU time per
frame: label 21 to 29 us, distance 147 to 239 us, pack 17 to 20 us, glass
39 to 44 us (flat: glass 33 to 38 us). The distance pass dominates and scales
with the region's area, so a whole-window key on a 1080p window is about 17
times that.

Brightness: a lens fill reads about 2x the luma of the flat key (strip 32/40/49
flat against 79/87/95; field 58/39/34 against 115/96/89), because it shows the
bent backdrop directly where the flat key sits over the smoked glass.

Owed before this leaves `proposed`:
- a cost review of the distance pass on a full-size keyed window and on the
  Pi-class target (COMP-02 §10 budget);
- the lens is rebuilt every frame a keyed window draws; cache the packed
  texture by the window's surface commit and the key set. Each build allocates
  two offscreen textures in `mask()` and two in `pack()`, plus uniform and
  element vectors;
- a bevel also forms around a non-key cut-out inside a keyed region (the
  label inside the strip); decide whether that is wanted;
- the brightness difference above, an owner call.

## Consequences
- Easier: a keyed surface can read as real glass with one desktop setting; the
  add-on stays a colour declaration.
- Harder: a keyed window with a lens costs a label pass, a distance pass and a
  pack pass per commit, and a distance pass reads up to 32 texels in eight
  directions per texel. The cost is paid only by windows that declare a lens.
- A lensed key reads brighter than the flat tint did (see Status).
- Forbidden: a client-supplied lens strength, bevel or dispersion. The lens
  takes them only from `decoration.blur.glass.lens.*`.

## Revisit when
- The distance pass is more than a few percent of a frame on a keyed window
  (`EC_GLASS_PROFILE`, `Span::LensDistance`).
- Two keyed surfaces overlap and the packed textures no longer fit the blur
  pyramid's budget.
- A second client wants a lens with its own optics, which would reopen option 2.
