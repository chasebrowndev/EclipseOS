# apoth-font

Governing spec: T-06 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). AGPL-3.0-only, `#![forbid(unsafe_code)]`,
no GPU dependency. Produces coverage bitmaps and glyph placement; atlas packing and upload are
`apoth-render`'s (T-07). Pure Rust (ADR 0110): skrifa + harfrust + swash/zeno; one skrifa and one
read-fonts in the lock (keep it so: `cargo tree -p apoth-font -d` must print nothing).

## Public API for apoth-render (M10)

Flow per frame: `FontSystem` (owns faces/fallback) -> `RowShaper::shape_row` -> per
`PlacedGlyph` either `GlyphCache::gray/color` (outline/colour) or `SpriteCache::get/tofu`
(built-ins) -> upload to atlas -> draw quads.

- `system::FontSystem::new(FontIndex, Vec<EmbeddedFont>, FontConfig)`; `set_config` bumps
  `generation()` (renderer must call `GlyphCache::sync_generation`, `SpriteCache::clear`).
  `cell_metrics(px_size) -> Option<CellMetrics>` (integer cell w/h, baseline, underline and
  strike positions; `ascent_from_top()`), `primary(Style)`, `resolve(Style, &str) -> Resolved`
  (face `None` = draw tofu; `partial` = diagnostic), `load`, `family_faces`, `info`, `path`.
- `discovery::FontIndex`: `build(dirs, previous)`, `load_or_build(dirs, cache_file)` (run off
  the render thread), `from_memory`, `font_dirs`, `default_cache_file`, `faces_covering(cp)`.
- `shape::RowShaper::new()/with_cache(sets, ways)`;
  `shape_row(&mut FontSystem, px_size, &[RowCluster], cursor_col, &mut ShapedRow)`. `ShapedRow`
  is reusable (cleared first; no per-row allocation in steady state). Output: `runs`
  (`ShapedRun`, `run_glyphs`) of `PlacedGlyph { glyph, source: GlyphSource, .. }`; `source`
  says font face vs built-in sprite vs tofu. Shape cache is flushed automatically on a
  `FontSystem` generation change.
- `raster::RasterParams::grid(px_size, Hinting, thicken_pct)`; `GlyphCache::new()`,
  `gray(&mut Rasterizer, &FaceInstance, glyph, &RasterParams) -> Option<Arc<GlyphBitmap>>`
  (8-bit coverage + placement `left/top`; `is_blank`, `slice(x0,x1)` for wide glyph cell
  slices), `color(.., px_size, foreground_rgba) -> Option<Arc<ColorBitmap>>` (premultiplied
  RGBA; COLRv0/v1 via `colr.rs`, CBDT/sbix via swash; OT-SVG not drawn). Failed glyphs are
  cached as missing once. `GlyphKey` is the stable atlas key.
- `builtin::SpriteParams::new(cell_w, cell_h).with_thickness(factor)`;
  `SpriteCache::get(cp, &SpriteParams) -> Option<Arc<SpriteBitmap>>` (exact-cell, box
  drawing/blocks/braille/sextants/octants/Powerline-adjacent; `is_sprite(cp)` to test),
  `SpriteCache::tofu(cp, w, h)`. Degenerate params (0 size, non-finite thickness) are safe.
- `ranges::CodepointRanges` (`from_ranges`, `from_codepoints`, `parse`, `contains`, `count`):
  clamped to U+10FFFF; used for `font.map` and coverage.
- `text`: `presentation`, `script_of`, `segment_clusters` (diagnostic approximation only; grid
  owns real UAX #29 segmentation via apoth-unicode).

## Limits and invariants

- COLRv1 painting is bounded by a time budget (64M pixel-ops) and `MAX_LIVE` (8) simultaneous
  layers/clip masks; exhaustion yields `None` (render falls back to the monochrome glyph).
- Never log text content. Rasterisation is on the render thread; discovery cache build is not.
- Tests use `tests/fixtures/FiraMono-Medium.ttf` (SIL OFL 1.1, see `tests/fixtures/README.md`);
  no host-font dependency.
