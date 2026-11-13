# Apotheosis research: font stack in Rust

Subject: topic-fonts. Scope: fontdb, harfrust vs rustybuzz, swash, skrifa/fontations, ligatures in a grid, fallback and emoji presentation, procedural box-drawing/powerline/sextant/octant, cell metrics, gamma/linear blending, subpixel positioning, crate licences.
Research date: 2026-11-12. Crate versions and licences are from the crates.io API on that date. Prior-art source was read in `/home/risk/.cache/apotheosis-prior-art/` (ghostty, kitty, foot, wezterm, alacritty, rio). Source findings are cited by upstream path and function and are described in prose. No source is reproduced.

## 1. Summary

- There is a complete, permissively licensed, pure-Rust font pipeline in 2026. Shaping is `harfrust` (MIT). Outlines, hinting, variable fonts and COLRv1 traversal are `skrifa` (MIT OR Apache-2.0), with `read-fonts` underneath. Rasterisation is `swash` (Apache-2.0 OR MIT) for gray and subpixel masks, or our own path rasteriser fed by skrifa. Discovery is `fontdb` (MIT) or `fontique` (Apache-2.0 OR MIT). None of these needs C, and every licence is on the allow-list.
- `rustybuzz` is effectively superseded. Its last release was 0.20.1 in Nov 2024, and it parses with `ttf-parser`, which was also last released in Nov 2024. `harfrust` began as a fork of rustybuzz, parses with `read-fonts`, matches HarfBuzz 15.1.0 and was updated on the research date. Use `harfrust`.
- `swash` 0.2.10 now depends on `skrifa` for outlines, so one parser family (fontations) serves both shaping and rasterisation. Its colour support is COLR v0 (colr+cpal layers), sbix and CBDT only. It has no COLRv1 paint graph, so COLRv1 needs our own painter on `skrifa::color::ColorPainter`.
- No existing terminal ships the whole stack we want in Rust. Rio is the closest (wgpu/Vulkan/Metal, swash, skrifa, own `sprite/` module), but it still uses `font-kit` and `ttf-parser`. Ghostty has the best procedural sprite coverage (box, block, braille, powerline, branch, geometric, sextants, octants, Symbols for Legacy Computing Supplement) and the best cell-metrics and blending code. Kitty has the best ligature-in-grid and text-sizing design. Foot has the best subpixel and overflow simplicity.
- The key design decisions are these. (1) Procedural cell-exact sprites for box, block, braille, powerline, sextants, octants, smooth mosaics and legacy-computing, ahead of any font lookup. (2) Shape per row run. Key the shape cache by position-independent run hash. Break runs at the cursor. (3) Compute integer cell metrics with explicit rounding rules and user modifiers. (4) Blend text in linear space with a luminance-based alpha correction, which is what Ghostty's `linear-corrected` does. (5) Handle emoji presentation through VS15/VS16 and fallback ordering, not by guessing from the font. (6) Write our own COLRv1 painter over skrifa and cache the result as RGBA atlas entries.

## 2. Architecture (how it is built)

### 2.1 Recommended layering for Apotheosis

1. **Discovery.** `fontdb` or `fontique` enumerates faces by directory scan. `fontdb` default features are `std, fs, memmap, fontconfig`. Its `fontconfig` feature uses `fontconfig-parser` (MIT, pure Rust) to read fontconfig config, with no libfontconfig. The README says it does not touch system APIs and that codepoint-level querying is a non-goal. Codepoint fallback therefore needs our own coverage index: a per-face charmap cached as ranges. `fontique` adds generic families, script/locale fallback and optional system backends, and can `dlopen` fontconfig. Apotheosis's allowed-library list does not include fontconfig, so we should use the pure scan path and avoid the dlopen feature. Whether fontique's default backend links libfontconfig is unverified from its README. Check the Cargo features before adopting it.
2. **Parsing.** `read-fonts` (zero-copy, no allocation) via `skrifa`. It is `#![forbid(unsafe_code)]` in skrifa 0.44 (verified in `src/lib.rs`).
3. **Shaping.** `harfrust::Shaper` built from `ShaperData`/`ShaperBuilder`. Variations come in through `ShaperInstance`. `ShapePlan` and `ShapePlanKey` allow plan caching per (font, script, language, features). harfrust has no size concept. It shapes in font units and the caller scales advances.
4. **Outline and hinting.** `skrifa::outline` offers `HintingInstance` with `HintingOptions { engine, target }`. Engines are `Interpreter` (TrueType or PostScript hints), `Auto` (FreeType-style autohinter, optional precomputed `GlyphStyles`) and `AutoFallback` (FreeType's selection rule). Targets are monochrome, normal grayscale, light, and LCD horizontal or vertical subpixel (`SmoothMode`, `LcdLayout`).
5. **Rasterisation.** Either `swash::scale::{ScaleContext, Render}` (formats Alpha, subpixel and colour bitmap, with embolden and fractional offset, as Rio does) or our own scanline coverage over skrifa outlines. The second path is needed anyway for COLRv1 and for sprites. One of `zeno` (Apache-2.0 OR MIT, swash's rasteriser), `yazi` (Apache-2.0 OR MIT) or `tiny-skia` (BSD-3-Clause) could serve as the path filler.
6. **Atlas.** Glyph atlases in wgpu: R8 for masks, RGBA8 for colour glyphs and COLRv1, and an RGB mask for LCD if we support it. Rio's `grid/atlas.rs` and wezterm's `window/src/bitmaps/atlas.rs` are shelf or skyline packers. Ghostty's `font/Atlas.zig` is a square rectangle-bin atlas with separate modification and resize counters so the renderer knows when to re-upload.
7. **Sprite layer.** A `Sprite` module draws procedural glyphs per exact cell size into an R8 bitmap and stores them in the same atlas. It is queried before the font fallback chain.

### 2.2 How prior-art terminals do it

- **Ghostty** (`src/font/`). Per-face `Collection`, `CodepointResolver` (a codepoint-to-font map plus fallback), `SharedGrid` (font set + atlases, shared between surfaces via `SharedGridSet` and keyed on config), `shaper/run.zig` (text-run iterator), `sprite/` (procedural glyphs through a software canvas), `Metrics.zig` (`calc` for cell geometry), and `nerd_font_attributes.zig` (per-codepoint constraint table to scale and align Nerd Font icons). Shaping uses HarfBuzz (CoreText on macOS), rasterisation uses FreeType.
- **Kitty** (`kitty/fonts.c`, `decorations.c`). Fonts are grouped per size (`FontGroup`). Each cell gets a sprite position, and a multi-cell ligature is represented as N sprite slices indexed by `ligature_index`, so the GPU shader treats each cell uniformly. `decorations.c` (also box drawing) draws underline variants (straight, double, dotted, dashed, curl) with integer geometry, not from font data.
- **WezTerm** (`wezterm-font/`). HarfBuzz via `shaper/harfbuzz.rs`, FreeType rasteriser, and a separate `rasterizer/colr.rs`. A `ClearShapeCache` error signals cache invalidation when fallback fonts change. The shaper takes a `Presentation` (Text or Emoji) and a `PresentationWidth` to choose among fallback fonts. Custom glyphs live in `wezterm-gui/src/customglyph.rs` (`BlockKey::from_char`, gated by the config option `custom_block_glyphs`). It uses poly-with-custom-metrics paths for lines.
- **Foot** (`box-drawing.c`, fcft). Box drawing is done in the terminal, not by the font, and is controlled by `box-drawings-uses-font-glyphs`. Foot draws on an `a8` buffer at 4x supersampling for curves and diagonals (via pixman), and draws pure lines and rectangles at pixel precision. Shaping is optional through fcft+HarfBuzz and grapheme clustering through utf8proc (`grapheme-shaping`, toggle at runtime with DECSET 2027).
- **Alacritty** (`renderer/text/builtin_font.rs`). Builtin box-drawing and powerline drawn into a `Canvas` with a manual coordinate system. There is no shaping, so no ligatures.
- **Rio** (`sugarloaf/src/sprite/`, `text.rs`, `grid/`). Sprites are CPU-rasterised once per cell size into an R8 bitmap and uploaded to the grid atlas with `bearing_x = 0`, `bearing_y = cell_h`. Modules are `block, box_drawing, braille, branch, geometric, legacy, powerline`. Text uses a swash `ScaleContext` with `Render::new(...).format(Format::Alpha)`, hinting taken from the font library setting, and synthetic bold as an embolden amount. It also has a "glyph protocol" (see 3.14).

### 2.3 Cell metrics

Ghostty's `Metrics.calc` (verified by reading the comments) works as follows. The face supplies ascent, descent and line gap, plus underline, strikethrough and overline geometry (from `post` and `OS/2`, with fallbacks) and the cap or ASCII height. Cell width is the rounded advance width. Cell height is the rounded line height. Ghostty explicitly chooses round over ceiling so error is at most 0.5 px and spacing matches across DPIs. It accepts that a zero-bearing glyph can overflow by one pixel. Descenders that touch the font's descent limit are left to the user, who adjusts with `adjust-cell-height`. The line gap is split in half, above and below. The baseline is computed so the face is vertically centred, and it is stored relative to the bottom of the cell. Every metric has a minimum of 1 so a modifier cannot yield a zero-thickness line. The unrounded `face_width`, `face_height` and `face_y` are kept for scaling glyph constraints. The `Metrics` struct also carries `box_thickness`, `cursor_thickness`, `cursor_height`, `icon_height` and `icon_height_single` (the Nerd Font constraint heights).

User modifiers in Ghostty are absolute, additive or percentage (`1`, `-1`, `20%`) on `adjust-cell-width/height`, `adjust-font-baseline`, and `adjust-{underline,strikethrough,overline}-{position,thickness}`, `adjust-cursor-thickness`, `adjust-cursor-height`, `adjust-box-thickness`, `adjust-icon-height`. Kitty uses the config key `modify_font` with suffixes `px` and `%` (no suffix means points) on `underline_position`, `underline_thickness`, `strikethrough_*`, `cell_width`, `cell_height` and `baseline`. A baseline change in kitty moves underline and strikethrough by the same amount.

### 2.4 Shaping runs and ligatures in a grid

- **Run formation.** Ghostty's `shaper/run.zig` never builds a run across rows. It splits runs when the style, font, or comparable attributes differ (`comparableStyle` strips attributes that do not matter). The run hash is position-independent because it uses relative cluster positions, so identical text on different columns hits the same shaper cache entry (`shaper/Cache.zig`). With the config `font-shaping-break = cursor` (default) the iterator breaks around the cursor, so any row with a cursor has at least three runs: before, the cursor cell, and after. This makes ligatures un-fuse under the cursor so editing is visible.
- **Fallback in a run.** For a grapheme cluster, Ghostty asks each codepoint for its supporting font and picks the font common to all candidates (`RunIterator` helper that finds a font index for a grapheme).
- **Ligature to cells.** Shaped glyphs map back to cells by cluster index. A ligature glyph is wider than one cell. Kitty slices it into per-cell sprites (`sprite_position_for(..., ligature_index, cell_count)`), and it detects "infinite ligatures" (runs of repeated glyphs longer than 9 cells, as in `-----` or `====` rails in fonts with such features) and reuses the same sprite for repeated middle cells (`is_infinite_ligature`, `is_repeat_sprite`). Kitty's `disable_ligatures` option is `never`, `cursor` or `always`, and it can be changed per window, tab or all via the action `disable_ligatures_in`.
- **Features.** Kitty `font_features` takes per-font OpenType feature strings (for example `+ss01`, `-liga`). Ghostty's `font-feature` is the equivalent. Ghostty `shaper/feature.zig` parses the feature syntax.
- **Variation.** Ghostty has `font-variation`, `-bold`, `-italic`, `-bold-italic`. Rio and others do the same through their instance builders.

### 2.5 Fallback and emoji presentation

- Kitty's `has_emoji_presentation` checks the first codepoint's emoji property in its own char-property table and handles multi-cell text. The fallback is created with an `emoji_presentation` flag (`create_fallback_face`, `load_fallback_font`) so font selection asks for a colour face, and the fallback key includes the style (`'a'` for emoji versus `'A'`). If the cell is not emoji-presentation, a face with the codepoint wins first. Kitty also marks colour glyphs (`colored`) for the shader.
- WezTerm passes a `Presentation` hint to the shaper and has `assume_emoji_presentation` on a font handle.
- Foot relies on grapheme clustering (DECSET 2027) before shaping. Without it, ZWJ, flag, keycap, modifier and tag sequences do not shape. `grapheme-width-method` choices are `wcswidth`, `double-width` (default, cap at 2) and `max`.
- Rules we must pin (Unicode, not terminal-specific): VS15 (U+FE0E) forces text presentation, VS16 (U+FE0F) forces emoji presentation and, in terminals that follow Unicode 9+ width, widens to 2 cells. That is a policy decision in the VT core (see the unicode topic). The font layer has to respond by choosing a colour face (COLR/CPAL, sbix, CBDT, SVG) versus a monochrome face.

### 2.6 Procedural drawing

- **Which characters.** Box Drawing U+2500-257F, Block Elements U+2580-259F (including shade characters), Braille U+2800-28FF, Geometric Shapes (arcs, triangles, circles), Powerline and Powerline Extra symbols, git-branch glyphs, Symbols for Legacy Computing U+1FB00-1FBFF (sextants U+1FB00-1FB3B, smooth mosaics, etc.), and Symbols for Legacy Computing Supplement U+1CC00-1CEBF (octants U+1CD00-1CDE5, twelfth and quarter ellipse pieces U+1CE.. as ghostty's comments list, separated mosaics).
- **Ghostty.** Each family has a `draw*` function in `font/sprite/draw/`. Octants use a generated table (`octants.txt`) mapping codepoint to a set of eight cells (2 columns, 4 rows). The canvas is a z2d surface (`sprite/canvas.zig`) with direct-pixel paths where speed matters. Box thickness is a metric (`box_thickness`) so lines scale with the font. Joins of light, heavy and double lines meet at corners with careful overlap. Ghostty documents unresolved Supplement characters (1CC1F and 1CC20, double diagonals).
- **Foot.** `box-drawing.c` has one function per character, a `_thickness(base, LIGHT|HEAVY)` helper, horizontal and vertical line primitives, and 4x supersampling (only when format is a8) for arcs and diagonals.
- **Kitty.** `decorations.c` draws all underline styles in integer maths, including a curl via per-column intensity with anti-aliasing (`add_intensity`) and dotted/dashed with distributed gaps (`distribute_dots`, which spreads the remainder pixels so the dots tile the cell width exactly). It also has `add_missing_glyph` for the replacement box.
- **Rio.** CPU sprite rasteriser per family, uploaded to the shared atlas.
- **Powerline.** The arrow and rounded-separator glyphs (U+E0B0-E0B7 etc.) are drawn as triangles and half-ellipses filling the full cell and extending slightly past the edges to avoid seams between differently coloured backgrounds. Thin variants use stroke thickness metrics. Foot and Alacritty (`powerline_drawing`) do this. Kitty and Ghostty do too.
- **Nerd Fonts.** Private-use icons are not fixed width. Ghostty keeps a generated constraint table (`nerd_font_attributes.zig`, `nerd_font_tables.zig`) giving, per codepoint, scale, alignment and whether to constrain to one cell. Kitty (`is_nerd_font_codepoint`) knows the PUA ranges of Nerd Fonts and supports `symbol_map` (codepoint range to font) and `narrow_symbols` (force N cells wide). Foot has `overflowing-glyphs` (default yes) that lets a glyph spill into one extra neighbouring cell.

### 2.7 Blending and gamma

- **The problem.** Anti-aliased coverage `a` blended in sRGB space gives text that looks the way type designers and platform renderers tuned it. Blending in linear light is physically right but makes light-on-dark text bolder and dark-on-light thinner, and produces visible fringes for coloured text on coloured backgrounds when done in gamma space.
- **Ghostty `alpha-blending`.** Values `native`, `linear` and `linear-corrected`. `linear-corrected` is the default on Linux. The fragment shader (`renderer/shaders/glsl/cell_text.f.glsl`) takes foreground and background luminance in linear light, blends their gamma-encoded luminances by coverage, converts back to linear to get the target luminance, and solves for the alpha that yields that luminance in a linear lerp from bg to fg. It skips this when the luminances differ by less than 0.001. It then outputs premultiplied colour. The result looks like native blending without darkening artefacts.
- **Kitty `text_composition_strategy`.** `platform` (default), `legacy` (pre-0.28), or up to two numbers: a gamma-like thickness adjustment (default 1.0 on Linux, 1.7 on macOS) scaled by the luminance difference so dark-on-light is affected most, and a second multiplicative contrast percentage. Kitty has an older obsolete option for subpixel-like thickening.
- **Foot `gamma-correct-blending`.** Boolean, default no. It needs a 10-bit surface (`surface-bit-depth`) to avoid banding, and falls back to 8-bit on request when speed matters.
- For the glass UI, wgpu surface formats matter. A `*Srgb` swapchain makes the hardware blend and encode in linear. A non-sRGB format with our own shader maths lets us do the corrected blend. Apotheosis composites over a translucent background (glass), so alpha must also stay premultiplied through to the compositor.

### 2.8 Subpixel positioning and hinting

- A terminal grid positions glyphs at integer cell origins, so horizontal subpixel positioning matters only inside a cell (side bearings and overflow) and for non-grid text (UI, tabs, palette). In the grid, rasterise glyphs once per (glyph, size, style) with zero fractional offset. This keeps the atlas small and the grid sharp.
- swash supports fractional offsets, so it can serve the UI text path. LCD (RGB subpixel) text is a mask with three coverage channels. It requires an opaque background to be correct, and it is wrong on translucent glass surfaces because the compositor blends channels uniformly. Recommend grayscale AA by default, with LCD as an opt-in that is automatically disabled while the window background is translucent.
- skrifa hinting: `Target::Mono`, `Target::Smooth{mode, symmetric_rendering, preserve_linear_metrics}` and `LcdLayout`. Light hinting (vertical only, `SmoothMode::Light`) is what FreeType's light autohint does and is the usual terminal default. Ghostty's `freetype-load-flags` exposes `hinting`, `force-autohint`, `monochrome`, `autohint`, `light`.
- Synthetic styles. Ghostty `font-synthetic-style` and `font-thicken` (with `font-thicken-strength`, 0 to 255, macOS-oriented). Rio synthesises bold via an embolden amount in swash and italics via a skew.

### 2.9 Colour glyph formats

- COLR v0 (layered flat colour), COLR v1 (paint graph with gradients, transforms, composites), sbix (PNG), CBDT/CBLC (PNG bitmap, Noto Color Emoji legacy), SVG-in-OpenType (needs an SVG renderer, which is a large scope; skip).
- skrifa `color` module has a `ColorPainter` trait (verified in `src/color/mod.rs`, with `push_transform` etc.) and `traversal.rs`, `instance.rs`, `transform.rs`. It walks the paint graph and calls our painter. We implement the painter to fill clip paths with solid, linear, radial and sweep gradients and composite modes onto an RGBA bitmap, then cache the bitmap per (glyph, size) in the colour atlas. Noto Color Emoji ships as COLRv1 in recent versions and as CBDT in older ones, so both paths matter.
- WezTerm has a dedicated `rasterizer/colr.rs`. Rio's glyph protocol decodes COLR v0 and v1 payloads.

## 3. Feature inventory (grouped by category)

3.1 Discovery and fallback: family search by directory scan (fontdb), fontconfig config parsing (fontconfig-parser, pure Rust), CSS-like query by family, weight, stretch, style (fontdb), per-face charmap coverage cache, fallback chain with a user-configurable list, per-range font mapping (`symbol_map`), narrow-symbol override, platform fallback (fontique).

3.2 Shaping: harfrust with GSUB/GPOS, complex scripts, variations, plan cache, per-font feature strings, cluster to cell mapping, cursor-aware run breaking, position-independent run hash.

3.3 Rasterisation: hinted outline (skrifa), gray/LCD masks, synthetic bold and italic, variable instance rendering, colour bitmaps.

3.4 Colour: COLR v0/v1 via our painter, sbix, CBDT, SVG (skip), palette selection (CPAL) with dark and light palettes.

3.5 Procedural glyphs: families above, with thickness from metrics and cell-exact edges.

3.6 Metrics: integer cell geometry, rounding policy, per-metric user modifiers, minimums, decorations (underline styles, strikethrough, overline), cursor shapes.

3.7 Blending: native, linear, linear-corrected, user contrast curve.

3.8 Text sizing: kitty OSC 66 (see 3.13).

3.9 Custom glyph protocol: rio glyph protocol (see 3.14).

## 4. Control surface (config, actions, IPC)

Proposed KDL keys (every one editable in the settings GUI, and by hand). Names are ours. Other terminals' names are listed for reference.

| Concern | Apotheosis KDL (proposed) | Prior art key |
| --- | --- | --- |
| Families | `font { family "..." bold "..." italic "..." bold-italic "..." fallback "..." "..." }` | ghostty `font-family*`, kitty `font_family`, foot `font*` |
| Size | `size 13.0` | kitty `font_size` |
| Features | `features "+ss01" "-calt"` and per-family blocks | kitty `font_features`, ghostty `font-feature` |
| Variations | `variations { wght 450 }` plus per-style | ghostty `font-variation*` |
| Ligatures | `ligatures "always" "cursor" "never"` | kitty `disable_ligatures`, ghostty `font-shaping-break` |
| Symbol map | `symbol-map { range "U+E000-U+F8FF" family "Symbols Nerd Font Mono" }` | kitty `symbol_map` |
| Narrow symbols | `narrow-symbols { range ... cells 1 }` | kitty `narrow_symbols` |
| Metric modifiers | `metrics { cell-width "0%"; cell-height "+2px"; baseline "1px"; underline-position ...; underline-thickness ...; box-thickness ... }` | ghostty `adjust-*`, kitty `modify_font` |
| Procedural glyphs | `builtin-glyphs { box true; block true; braille true; powerline true; legacy true; octants true }` | foot `box-drawings-uses-font-glyphs`, wezterm `custom_block_glyphs`, kitty `box_drawing_scale` |
| Hinting | `hinting "none" "light" "full"`; `engine "auto" "interpreter"` | ghostty `freetype-load-flags` |
| AA | `antialias "gray" "lcd-rgb" "lcd-bgr" "mono"` (lcd auto-off on translucent) | foot/fcft |
| Blending | `blending "native" "linear" "linear-corrected"`; `contrast 1.0` | ghostty `alpha-blending`, kitty `text_composition_strategy` |
| Overflow | `overflow-glyphs true` | foot `overflowing-glyphs` |
| Undercurl | `undercurl-style "smooth" "sharp"` | kitty `undercurl_style` |
| Grapheme | `grapheme-width "double-width"` | foot `grapheme-width-method` |
| Emoji | `emoji { family "Noto Color Emoji"; presentation "unicode" "emoji-first" }` | ours |

Actions (shared vocabulary): `font-size-increase`, `font-size-decrease`, `font-size-reset`, `font-size-set <pt>`, `ligatures-set <mode> [scope]`, `font-feature-toggle <tag>`, `font-family-set <style> <name>`, `font-reload`, `font-debug-fallback` (toggle overlay or log), `font-clear-caches`, `blending-set <mode>`. Kitty's per-scope `disable_ligatures_in active|tab|all` is the pattern to copy.

IPC and CLI: `apo font list`, `apo font query <codepoint>` (returns the resolving face and why: primary, symbol map, fallback), `apo font metrics` (cell size, baseline, decorations, box thickness), `apo font set key value`. Escape sequences: OSC 66 text sizing (kitty), and the Glyph Protocol APC (rio), both as optional features.

## 5. Performance (numbers + method)

Published figures are sparse. All of these are as-reported, and none was reproduced here (nothing was built or run).

- harfrust vs HarfBuzz: "less than 25% slower than HarfBuzz on most common fonts", per the harfrust README, with a linked spreadsheet and `cargo bench` for local runs. Method: comparison on common fonts, no exact numbers on the page. Source: https://github.com/harfbuzz/harfrust
- Atlas design budget: no published number. Keep glyph raster misses off the frame path. Rasterise on a worker, upload with a staging belt, and show a blank or previous glyph until ready is one option, but a blocking miss on first draw of a new codepoint is what Alacritty/Foot do.
- Shape cache: Ghostty keys by position-independent run hash, so repeated lines (a scrolling log with the same prefix, prompts) skip the shaper. This is the main CPU saver for ligature fonts.
- Sprite cost: sprites are rendered once per (cell size, codepoint) and live in the atlas, so draw cost is zero at steady state.
- Overflow glyphs can cost: foot notes "might impact performance depending on the font used", especially at small sizes.
- Foot gamma-correct blending needs 10-bit surfaces, which it labels slow, so it is off by default. In our design the blend is a few ALU ops per fragment in the shader, which is cheap on any GPU. (This is our reasoning, not a published measurement.)

## 6. What Apotheosis should take

1. `harfrust` for shaping, `skrifa` for outlines/hinting/colour, `swash` or own rasteriser for masks, `fontdb` for discovery.
2. Cell-exact procedural sprites for all legacy and box families, ahead of the font lookup, with thickness from a metric and a user toggle.
3. Ghostty's metrics approach: round advance and line height, centre the baseline, split line gap, min 1 on everything, and expose the additive and percentage modifiers.
4. Position-independent shape-run hash, per-row runs, cursor-aware run breaks, kitty's per-cell ligature slicing and infinite-ligature reuse.
5. Linear-corrected blending as default, with the native and linear alternatives and a contrast knob.
6. Grapheme clustering and VS15/VS16 handled in the VT core, with the font layer selecting colour faces by presentation.
7. Own COLRv1 painter on skrifa's `ColorPainter`, caching to the RGBA atlas.
8. Per-codepoint Nerd Font constraints (generated table) plus `symbol_map` and `narrow_symbols`.
9. `apo font query` explaining which face a codepoint resolved to. This is cheap and removes a whole class of support questions.
10. Kitty OSC 66 text sizing, because it also drives multi-cell glyph layout that the sprite and ligature code needs anyway.

## 7. What it should avoid

- `rustybuzz` and `ttf-parser` (unmaintained since late 2024, duplicate parsers next to fontations).
- `fontconfig` C bindings, `freetype` and `harfbuzz` C bindings (the build rules forbid compiled C/C++, and fontconfig is not on the allowed library list).
- `cosmic-text` as the grid engine. It is a paragraph layout engine and the cell grid needs exact integer placement. Fine for UI text only (MIT OR Apache-2.0).
- LCD subpixel text on translucent glass.
- Trusting font-reported monospace and metrics blindly. Fonts with a descender at the descent limit, or an advance that differs from ASCII glyphs, need the modifiers.
- Per-frame shaping of unchanged rows.
- SVG-in-OpenType colour support (needs a full SVG renderer, low payoff).
- Transliterating prior-art source. This dossier describes only the techniques.

## 8. Crate licence and maturity table (crates.io API, 2026-11-12)

| Crate | Version | Licence | Last update | Notes |
| --- | --- | --- | --- | --- |
| fontdb | 0.24.0 | MIT | 2026-07-29 | features std, fs, memmap, fontconfig (fontconfig-parser MIT) |
| fontconfig-parser | 0.5.8 | MIT | 2025-05-12 | pure Rust |
| fontique | 0.13.0 | Apache-2.0 OR MIT | 2026-11-12 | linebender, enumeration and fallback |
| parley | 0.13.0 | Apache-2.0 OR MIT | 2026-11-12 | layout, UI text candidate |
| harfrust | 0.16.0 | MIT | 2026-11-12 | HarfBuzz 15.1.0 port |
| rustybuzz | 0.20.1 | MIT | 2024-11-12 | stale |
| ttf-parser | 0.25.1 | MIT OR Apache-2.0 | 2024-11-29 | stale |
| read-fonts | 0.47.0 | MIT OR Apache-2.0 | 2026-11-10 | zero-copy parser |
| skrifa | 0.50.0 | MIT OR Apache-2.0 | 2026-11-10 | outlines, hinting, colour |
| font-types | 0.12.7 | MIT OR Apache-2.0 | 2026-10-15 | |
| swash | 0.2.10 | Apache-2.0 OR MIT | 2026-07-17 | depends on skrifa, zeno, yazi, core_maths |
| zeno | 0.3.3 | Apache-2.0 OR MIT | 2025-05-12 | path rasteriser |
| yazi | 0.2.2 | Apache-2.0 OR MIT | 2026-11-01 | |
| tiny-skia | 0.12.0 | BSD-3-Clause | 2026-02-02 | CPU 2D |
| vello_cpu | 0.4.0 | Apache-2.0 OR MIT | 2026-11-11 | CPU renderer |
| cosmic-text | 0.19.0 | MIT OR Apache-2.0 | 2026-04-22 | |
| glyphon | 0.12.0 | MIT OR Apache-2.0 OR Zlib | 2026-07-09 | wgpu text |
| allsorts | 0.17.0 | Apache-2.0 | 2026-05-13 | |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 | 2026-06-01 | |
| unicode-width | 0.2.3 | MIT OR Apache-2.0 | 2026-10-28 | |
| icu_properties | 2.4.0 | Unicode-3.0 | 2026-10-22 | emoji and script properties |
| wgpu | 30.0.2 | MIT OR Apache-2.0 | 2026-11-12 | |

All entries are on the allowed licence list. `Apache-2.0 OR MIT` means we may pick either, so no GPL concerns. Unverified: the transitive dependency set of each, which should be checked with `cargo deny` once chosen.

## 9. Sources

Docs and metadata
- https://github.com/harfbuzz/harfrust (README: relationship to rustybuzz, "less than 25% slower", limitations, MIT)
- https://github.com/dfrg/swash (README: scaling, hinting, variable fonts, colour, emoji)
- https://github.com/googlefonts/fontations (crate roles)
- https://docs.rs/skrifa/latest/skrifa/ (outline, hinting, metrics)
- https://github.com/RazrFalcon/fontdb
- https://github.com/linebender/parley/tree/main/fontique
- https://sw.kovidgoyal.net/kitty/text-sizing-protocol/ (OSC 66)
- https://crates.io/api/v1/crates/<name> for versions, dates, licences, fontdb features and dependencies
- `rio/specs/glyph-protocol.md` and https://rapha.land/introducing-glyph-protocol-for-terminals/ (Glyph Protocol, APC `s`/`q`/`r`/`c`, PUA only, glyf, COLR v0/v1)

Source (upstream path and function)
- ghostty `src/font/Metrics.zig` (`calc`, `Minimums`, `FaceMetrics`); `src/font/shaper/run.zig` (`RunIterator`, `comparableStyle`, cursor break); `src/font/shaper/Cache.zig`; `src/font/sprite/draw/*.zig` (`box.zig`, `block.zig`, `braille.zig`, `powerline.zig`, `branch.zig`, `geometric_shapes.zig`, `symbols_for_legacy_computing*.zig`, `octants.txt`); `src/font/sprite/canvas.zig`; `src/font/nerd_font_attributes.zig`; `src/font/Atlas.zig`; `src/font/SharedGridSet.zig`; `src/renderer/shaders/glsl/cell_text.f.glsl` and `common.glsl` (`linearize`, `unlinearize`, `luminance`); `src/config/Config.zig` (`alpha-blending`, `font-shaping-break`, `adjust-*`, `font-variation*`, `font-thicken`, `font-synthetic-style`)
- kitty `kitty/fonts.c` (`has_emoji_presentation`, `load_fallback_font`, `sprite_position_for`, ligature handling, `is_nerd_font_codepoint`, `symbol_maps`); `kitty/decorations.c` (`add_straight_underline`, `add_curl_underline`, `distribute_dots`, `add_missing_glyph`); `kitty/options/definition.py` (`text_composition_strategy`, `modify_font`, `disable_ligatures`, `font_features`, `symbol_map`, `narrow_symbols`)
- foot `box-drawing.c` (supersample 4 on a8, `_thickness`); `doc/foot.ini.5.scd` (`gamma-correct-blending`, `surface-bit-depth`, `overflowing-glyphs`, `grapheme-shaping`, `grapheme-width-method`)
- wezterm `wezterm-font/src/lib.rs`, `shaper/harfbuzz.rs` (`Presentation`, `PresentationWidth`, `assume_emoji_presentation`), `rasterizer/colr.rs`, `wezterm-gui/src/customglyph.rs` (`BlockKey::from_char`)
- alacritty `alacritty/src/renderer/text/builtin_font.rs` (`builtin_glyph`, `box_drawing`, `powerline_drawing`, `Canvas`)
- rio `sugarloaf/src/sprite/mod.rs` and siblings; `sugarloaf/src/text.rs` (swash `ScaleContext`, `Render`, embolden); `sugarloaf/src/grid/atlas.rs`; `sugarloaf/Cargo.toml`
- skrifa 0.44 `src/color/mod.rs` (`ColorPainter`), `src/outline/hint.rs` (`HintingOptions`, `Engine`, `Target`), `src/lib.rs` (`forbid(unsafe_code)`); swash 0.2.10 `Cargo.toml` (skrifa dependency) and `src/scale/mod.rs` (colr/cpal gate; no `ColorPainter` use)

Unverified
- fontique's default build linking libfontconfig; harfrust exact benchmark numbers; fontdb 0.24 parser dependency (its crates.io dependency list did not show `ttf-parser`); swash maintenance cadence beyond crates.io dates; Noto Color Emoji format shipped on the user's system.
