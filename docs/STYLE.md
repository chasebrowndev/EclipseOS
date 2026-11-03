# Eclipse OS — visual style spec

A dark (by default; see Light), warm-black desktop OS settings UI with balanced glassmorphism and a single
yellow accent. Use this as art direction; do not copy any specific screen.

## Colour
- Window / desktop base: #0b0906 (warm near-black — never blue-grey)
- Secondary surfaces behind glass: #12100b, #1a1712, #1c1913
- Glass panels: rgba(255,255,255,.035) to .06 fill, backdrop-filter: blur(28-56px) saturate(120-170%).
  The compositor draws the material (blur, saturation, hairline rim, shadow) under the
  surface (COMP-02 §9, C-14); a client only lays the light tint on top and must not paint
  an opaque ground while blur is on. Radii stay concentric with the compositor's mask.
  One material per window: the rail and the column of a Settings or console window are
  both the same tint, rgba(11,9,6,.70) by default, over the same blur (opaque #0b0906 with
  blur off), parted by a 1px hairline, not by a change of ground.
  The surface values are defaults, and users tune them live (the compositor's
  `decoration.surface.tint` .70, `card-fill` .035, `inset-fill` .03; sRGB intent, converted
  to linear alpha in the client, 0..1 and 0..0.30 respectively). Settings' Effects > Blur & glass page shows them as Window tint,
  Card fill and Inset fill. Every window re-reads them on a config reload.
  Settings cards with blur off or `blur` are hairline-only: a white `card-fill` fill, a 1px
  rim at about 3.4x that fill (.12 by default), no drop shadow. `glass` and `frost` keep the
  .045 glass panel above. Inset groups use `inset-fill` with the same rim ratio. A run that
  does not apply (a "not in use" group) is veiled with blur off: its content fades toward the
  ground and it keeps the inset fill and rim, so it reads as a quieter inset, not a grey slab.
  With blur on nothing is painted over it; only its labels dim.
- Panel borders: 1px solid rgba(255,255,255,.08-.16)
- Top edge highlight: inset 0 1px 0 rgba(255,255,255,.07-.2) — only on the blur-off fallback;
  on glass the compositor's rim lights the edge. Exception: the taskbar's folded strip
  (2-16px tall) keeps its painted top light on glass too, since the rim barely shows there
- Outer shadow: 0 24-40px 60-90px -20px rgba(0,0,0,.85-.92) — the window's, drawn by the
  compositor. Cards inside a window carry no drop shadow: white fill + hairline only
- Bar cells (task chips, pager tiles, launcher) are small glass lozenges: rgba(255,255,255,.05)
  fill, 1px inside hairline rgba(255,255,255,.10), 0 1px 6px rgba(0,0,0,.30). Hover .08 fill /
  .14 hairline; press .12 fill, no shadow. The focused chip, the active workspace and the
  launcher's selected row are the same glass a step clearer: a slightly brighter white fill
  (.12) and a crisp white hairline (.20), the neutral drop, and gold on the label only.
  No gold fill, no gold outline, no glow — nothing gold spills past the label, and nothing
  lights past the cell's edge: glassy, not glowy. A minimized chip has no fill, a .05 hairline, dimmed
  text. The grip is part of its cell: it paints no ground, only its line brightens on hover.
  A widget that reveals reserves no grip column while open: the grip draws over its leading
  pad and the cell eases 8px wider under the pointer. Alphas above are sRGB intent; iced
  blends in linear light, so `tokens.rs` carries the linear equivalents
- Accent: the theme's `accent` seed (#f2c33c gold in the built-in themes), user-overridable with
  `general.accent` (`#rrggbb`, live; Settings > Effects > Blur & glass > Colour scheme offers
  Gold, Coral, Rose, Violet, Azure, Teal, Green, a hex field and a picker). The accent text is
  re-derived from the override so contrast holds. Tints: rgba(242,195,60,.09) fill, .24-.32 border,
  #f5cf5c for text, all as the gold defaults; every pane reads them from the palette, never a literal
- Text: #fff primary, rgba(255,255,255,.64) secondary, rgba(255,255,255,.4) tertiary
- Neutral swatch grey: #96918a (warm, not #8f8f96)
- These values are the built-in dark seeds run through `derive` in `abyss/crates/ec-theme`
  (bit-exact, `dark_golden` test); the light alpha table lives beside them

## Light (ADR 0084)
Same material and same structure, inverted ink. Light glass is a bright frosted
tint with dark ink on it. It is never the dark tokens flipped. Values below are
starting points to tune in the per-pane visual pass. Every dark token has a
light twin, derived from the built-in light seeds. User themes override seeds and obey
the same rules (ADR 0084).
- Base: #f4f1ea (warm off-white, never blue-white). Surfaces: #ebe6dc, #e3ddd1, #ddd6c9
- Frost tint (compositor): #f4f1ea at .55. Glass fill on top: rgba(255,255,255,.35-.5)
- Ink: rgba(26,22,16,α) replaces rgba(255,255,255,α) for hairlines, borders and
  tracks, at roughly the same α (.06-.14). The rim stays white: on light glass it reads as
  a bright edge, not a highlight
- Bar cells: rgba(26,22,16,.07) fill (darker than the frost: stacked glass moves away from the backdrop), rgba(26,22,16,.12) hairline. Hover darkens: .11 fill / .18 hairline;
  press .15, no shadow. A focused cell thins to .58 white on hover and .46 on press.
  Compositor glass does the same: `Look::shade` mixes toward ink. Focused / selected: .70 white and a crisp rgba(26,22,16,.18) hairline,
  gold on the label only. The same rules as dark: glassy, not glowy
- Shadow: warm and soft, rgba(60,45,20,.18-.28). A light window must not sit in a black halo.
  Compositor pane glass casts the cell's contact shadow (a pixel down, 6 px fall-off) at rest,
  on hover and on focus; none pressed, none minimized or latent at rest
- Accent: #f2c33c for fills, toggles, slider fill and marks; knob and glyph ink on it #1a1610.
  Accent *text* is #856000 (≥ 4.5:1 on the base and on a selected `accent_fill` pill). Tints: rgba(242,195,60,.18) fill, .45 border
- Text: #1a1610 primary, rgba(26,22,16,.62) secondary, rgba(26,22,16,.42) tertiary
- Settings content column: rgba(255,255,255,.30) tint on the shared blur (the light twin
  of the dark .28 black)
- Depth runs toward the ink on light. A light card on the column is ink at the `lift_soft`
  step (rgba(26,22,16,.04)); an inset on a card is the bar cell's rest step (`cell`,
  rgba(26,22,16,.07)). Both scale with `card-fill` / `inset-fill` against their dark
  defaults (.035 / .03), so an inset is always darker than its card. Never white over
  frost: white lifts a card above the column it sits on. Toast buttons are insets and
  follow the inset step. Selection is the one lift: the focused row is white .70 with its
  crisp ink rim.
- Light ink and fill alphas are specified as sRGB intent and converted to linear-light alphas
  over the base (`ec_theme::linear_alpha`), because iced blends in linear light. `danger` is
  #bd3a26 and `neutral` is #6f6a62. Controls take their grounds from palette tokens
  (`nav_active`, `pill`, `input_fill`, `scroller`, `control_off_hover`, `knob_off`,
  `highlight_rim`, `on_accent`), never from `Color::WHITE`. Selection is a brighter fill plus a
  `highlight_rim` border
- Compositor glass (`eclipse_glass_v1` panes, layer and window glass) in light has no dark smoke
  and no highlight knee. The backdrop is saturated lightly (1.25) and mixed 70% toward warm white
  (.985,.975,.955). The bezel seam is .04 and the rim stays white. `Appearance::glass_light()`
  drives this
- Trusted-UI prompts use the built-in dark or light palette matching the theme's
  brightness, never a user theme's colours (ADR 0084)

## Accent discipline
The accent (yellow by default) marks state and one live value per screen: the active nav item, the selected
option, the current slider fill, an "in use" status. Never decorative, never two
competing yellow areas in one pane, no gradients of it.

## Type
- Instrument Sans (400/500/600) for UI labels and titles; -0.01em tracking on headings
- JetBrains Mono (400/500) for all data, units, paths, timestamps, device identifiers,
  and small uppercase section labels (9.5-10px, letter-spacing .12-.2em)
- Sizes: pane title 24px/600, card title 12.5-13.5px/500, body 12-13.5px/400,
  mono data 10.5-12px, big numbers 19-38px

## Structure
- Window: 1120x720, radius 16px, 1px light border. No traffic-light dots — this is a
  window manager, so the titlebar is either absent or a thin label row.
- Left sidebar 214px: the window tint (rgba(11,9,6,.70) by default) + blur(30px), the same tint as the content column, text nav items each with a small
  rounded placeholder icon square; active item = translucent fill + 3px yellow left bar
- Sidebar footer: small mono status block (uptime, daemon version, device count)
- Content column: 26/30px padding, 18px gap between blocks
- Header row: title + one-line subtitle (containing one yellow phrase) on the left,
  1-3 pill controls or a toggle on the right
- Then a hero block unique to the pane (arrangement canvas, level meter, focus banner,
  charge bar + 24h histogram, status metric grid)
- Then one or two inset list panels: header row, hairline rows
  (rgba(255,255,255,.05-.06)), label left, mono value right
- Radii: window 16, cards 13-14, inset chips 9-11, pills 99px

## Controls
- Toggle: 44x25 pill, yellow with a #0b0906 knob when on; rgba(255,255,255,.13) with a
  translucent white knob when off
- Slider: 4-6px track rgba(255,255,255,.12), yellow fill, 16-18px yellow round knob
- Radio: 11px circle, yellow ring + yellow dot when selected
- Segmented choice: pill buttons; selected gets yellow tint fill, yellow border, yellow text
- Bar charts: flat rects, 2-3px top radius, rgba(255,255,255,.13) for history,
  #f2c33c only for the current or peak bar

## Content rules
- Fictional but plausible system data: device names, config paths (~/.config/wm/theme.conf),
  daemon versions, dBm, Hz, W, GB, timestamps. No brand or trademark names.
- No card-in-card-in-card; one level of inset inside a glass panel is the limit
- No lorem, no decorative radial glow blobs, no emoji, no hand-drawn SVG icons —
  small rounded rects stand in for icons
- Every pane shows real state somewhere (what is connected, in use, or queued)

## What to build
One settings pane per screen at 1120x720, sidebar + detail, inline styles only.
Candidate panes: appearance, network, display, sound, input, notifications, power,
privacy, storage, keybinds, autostart, accounts.
