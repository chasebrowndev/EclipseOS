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
  `decoration.surface.tint` .70, `card-fill` .035, `inset-fill` .05; sRGB intent, 0..1 and
  0..0.30 respectively. The tint is written as the buffer's alpha as is, because the compositor
  blends it in sRGB like a terminal's `alpha=`; card and inset fills are converted to linear alpha
  because iced blends those itself). Settings' Effects > Blur & glass page shows them as Window tint,
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
- Base: #faf9f6 (paper white, barely warm, never cream or blue-white). Surfaces: #f1efea, #e9e6df, #e2dfd7
- Frost tint (compositor): #faf9f6 at .35. Glass fill on top: rgba(255,255,255,.22-.32);
  sheets #faf9f6 at .36, the bar at .30
- Window tint: the `decoration.surface.tint` intent times .50 (`LIGHT_TINT_SCALE`), so the
  default .70 is .35. A white veil flattens the backdrop far more than a dark one at the
  same alpha; one knob still drives both schemes
- Ink: rgba(26,22,16,α) replaces rgba(255,255,255,α) for hairlines, borders and
  tracks, at roughly the same α (.06-.14). The compositor rim of a window or layer stays white
  but is no longer alone: white on near-white frost is no edge, so light adds an ink hairline
  on the outermost pixel (α .45) and a white specular (.85) on the lit top-left lobe just
  inside it. Panes already rim in ink and take neither
- Bar cells: rgba(26,22,16,.05) fill (darker than the frost: stacked glass moves away from the backdrop), rgba(26,22,16,.12) hairline. Hover darkens: .09 fill / .18 hairline;
  press .13, no shadow. A focused cell thins to .58 white on hover and .46 on press.
  Compositor glass does the same: `Look::shade` mixes toward ink. Focused / selected: .70 white and a crisp rgba(26,22,16,.18) hairline,
  gold on the label only. The same rules as dark: glassy, not glowy
- Shadow: warm and soft, rgba(60,45,20,.18-.28). A light window must not sit in a black halo.
  Compositor pane glass casts the cell's contact shadow (a pixel down, 6 px fall-off) at rest,
  on hover and on focus; none pressed, none minimized or latent at rest
- Accent: #f2c33c for marks, rims, knobs and slider handles. **Nothing has a gold ground**
  (owner, 2026-11-15): a selected pill, chip, row or default action is the neutral inset (the
  same fill and rim as any inset cell) with a gold label and a gold-tinted rim, a step crisper
  on hover. A toggle is an inset track; on is a gold rim and gold knob. A slider is an inset
  rail whose filled run and rim ramp from faint to full accent as the value nears max.
  Accent *text* is #856000 (≥ 4.5:1 on the base and on the inset). Rim tint: rgba(242,195,60,.45)
- Text: #1a1610 primary, rgba(26,22,16,.62) secondary, rgba(26,22,16,.42) tertiary
- Settings content column: rgba(255,255,255,.12) tint on the shared blur (the light twin
  of the dark .28 black)
- Depth runs toward the ink on light. A light card on the column is ink at the `lift_soft`
  step (rgba(26,22,16,.04)); an inset on a card is the bar cell's rest step (`cell`,
  rgba(26,22,16,.05)). Both scale with `card-fill` / `inset-fill` against their dark
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
- The window tint is premultiplied by iced in linear light, the compositor blends it in sRGB. A
  near-black base hides the difference; a light base does not (Settings read near-opaque white). The
  tint colour is therefore pre-compensated per channel (`surface::tint`: `enc(dec(c*a)/a)`), so the
  buffer carries `c*a` and alpha `a`, the same effective .70-.72 as a terminal's `alpha=0.72` in either scheme
- Compositor glass (`eclipse_glass_v1` panes, layer and window glass) in light has no dark smoke
  and no highlight knee. The backdrop is saturated (1.6) and veiled 12% toward paper white
  (.992,.989,.980), the light twin of dark's 28% smoke, thin enough that the wallpaper's colour and contrast survive. Whitening happens once, in the window
  tint: the shader only veils, or the backdrop is whitened twice and the glass reads opaque.
  The bezel seam is .08 and the rim stays white. `Appearance::glass_light()` drives this
- Compositor titlebars (`mode "de"`) follow the scheme: #faf9f6 (GROUND_L, .980 .976 .965) at the light window tint
  (.35 focused, .26 idle) plus the white focus lift, ink title and glyphs at the text steps,
  an ink seam (.10 / .06), and bar-cell ink steps on hovered buttons. Close keeps its danger cell
- Trusted-UI prompts use the built-in dark or light palette matching the theme's
  brightness, never a user theme's colours (ADR 0084)

## Accent discipline
The accent (yellow by default) marks state and one live value per screen: the active nav item, the selected
option, the current slider fill, an "in use" status. Never decorative, never two
competing yellow areas in one pane, no gradients of it.

## Type
- **Onest is the one UI face** (SIL OFL, `assets/fonts/Onest-*.ttf`): labels, titles, data, units,
  paths, timestamps, device identifiers, key caps and the logo wordmark. No second family.
  Weights 400/500/600 for UI (`font::UI`, `UI_MEDIUM`, `UI_SEMIBOLD`); `font::DATA`/`DATA_MEDIUM`
  are the same family at 400/500, kept as names so a data run stays greppable. -0.01em tracking on headings
- Digits are tabular: the vendored statics make `tnum` the default figures (`assets/fonts/gen_onest.py`),
  because iced cannot switch OpenType features on. Columns of numbers align without a mono face
- Small uppercase section labels (9.5-10px, letter-spacing .12-.2em) are Onest Medium
- Monospace survives only where a cell grid is the point: terminal cells (apotheosis/foot) and the
  compositor's HUD/trusted bitmap faces, which are not shell chrome
- Sizes: pane title 24px/600, card title 12.5-13.5px/500, body 12-13.5px/400,
  data 10.5-12px, big numbers 19-38px

## Structure
- Window: 1120x720, radius 16px, 1px light border. No traffic-light dots — this is a
  window manager, so the titlebar is either absent or a thin label row.
- Left sidebar 214px: the window tint (rgba(11,9,6,.70) by default) + blur(30px), the same tint as the content column, nav items each with a drawn
  icon tile (see Art); active item = translucent fill + 3px yellow left bar. Settings' sidebar is the search field pinned on top, the
  Overview, then the sections under five small uppercase group headings (Look & feel, Devices, System, Privacy & accounts, Apps & add-ons)
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
- No lorem, no decorative radial glow blobs, no emoji, no SVG or bitmap icon files. Icons are
  drawn in-process (`ec_ui::widget::art`), see Art
- Every pane shows real state somewhere (what is connected, in use, or queued)

## Art (Settings)
Drawn in-process, no asset and no icon font: `ec_ui::widget::art` holds the glyphs (a handful of rounded rects, arcs and
one bezier in a unit square) and `tokens::art` every size, alpha and hue.
- **Icon tile**: a 22px glass squircle (corner .27 of the side), a hue at .22 fill and .38 rim, the glyph in line (stroke .075 of
  the side) in the hue pulled .58 toward the text ink, so it is pastel on dark and deep on light. The current section's tile is
  a brighter fill (.34) and a crisper rim (.70). Never gold, never a glow.
- **One hue per sidebar group**, restrained: Look & feel violet, Devices azure, System slate, Privacy & accounts sage, Apps &
  add-ons clay, Overview the neutral swatch grey. The accent is not a hue of the art.
- **Vignette**: the page's header art, 92x64, left of the title. The group's hue at .16 over the glass, two quiet rings, and the
  page's glyph drawn large with translucent glass fills. Hidden in a narrow window.
- **Previews** are diagrams drawn from the live values (window set: layout, gaps, border, rounding; glass: rim, refraction,
  dispersion, bevel, tint; scheme cards: the built-in dark and light palettes). A backdrop gradient of two group hues is the one
  gradient the art allows, because glass needs something to bend; it is not chrome and not the accent.
- Art never carries meaning alone (a tile sits by a label, a vignette by a title), does no work while the window is idle, and
  follows the scheme.
- Pages with several groups are one titled card per group (title 13px/500, a one-line caption); a settings few people change sit
  behind an "Advanced" disclosure at the foot of the page's last card (opened by a search jump). Below 640px wide the sidebar is
  a list that replaces the page, with a back control in the header.

## What to build
One settings pane per screen at 1120x720, sidebar + detail, inline styles only.
Candidate panes: appearance, network, display, sound, input, notifications, power,
privacy, storage, keybinds, autostart, accounts.
