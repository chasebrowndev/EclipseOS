// SPDX-License-Identifier: AGPL-3.0-only
//! The glyph provider the scene build calls: `apoth-font` rasterises, `apoth-render`'s atlas
//! places, uploads are queued here and flushed to the renderer after the scene update.
//! Render thread only. First light: one glyph per cell (no shaping, so no ligatures yet).
//! Misses rasterise inline under the T-07 §4 budget (16 glyphs or 1 ms per frame, echo rows
//! exempt); the rest answer `Pending` and are retried next frame. The T-00 §4.2 worker pool
//! that would take them is not built yet (M11 gap).
//!
//! Atlas pages are never evicted piecemeal: scene rows bake page coordinates when built and
//! unchanged rows are not looked up again, so a silently reused page would show other
//! glyphs' texels. A full atlas clears everything and raises `flushed`, and the render
//! thread rebuilds every row.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use apoth_font::builtin::{SpriteCache, SpriteParams, is_sprite};
use apoth_font::config::Style;
use apoth_font::metrics::CellMetrics;
use apoth_font::raster::{GlyphBitmap, GlyphCache, RasterParams, Rasterizer};
use apoth_font::system::FontSystem;
use apoth_render::atlas::{Atlas, AtlasError, MissBudget};
use apoth_render::renderer::Renderer;
use apoth_render::scene::{AtlasGlyph, GlyphLookup, GlyphProvider, GlyphRequest, Half};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AKey {
    face: u64,
    glyph: u32,
    half: u8,
    kind: u8,
}

const KIND_OUTLINE: u8 = 0;
const KIND_SPRITE: u8 = 1;
const KIND_TOFU: u8 = 2;
const KIND_COLOR: u8 = 3;

struct Upload {
    color: bool,
    page: u16,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    data: Vec<u8>,
}

pub(crate) struct Glyphs {
    pub(crate) fonts: FontSystem,
    rast: Rasterizer,
    cache: GlyphCache,
    sprites: SpriteCache,
    mask: Atlas<AKey>,
    color: Atlas<AKey>,
    ready: HashMap<AKey, GlyphLookup>,
    /// (code point, style, half) to the resolved key and answer, for cluster-free requests.
    front: HashMap<u64, (AKey, GlyphLookup)>,
    metrics: CellMetrics,
    px: f32,
    params: RasterParams,
    builtin: bool,
    uploads: Vec<Upload>,
    /// An atlas ran out of room: everything was cleared, rows must be rebuilt.
    pub(crate) flushed: bool,
    /// Set for the last rebuild pass: a full atlas draws the glyph blank instead of clearing
    /// again, so the pass cannot invalidate rows it already built.
    pub(crate) degraded: bool,
    budget: MissBudget,
}

impl std::fmt::Debug for Glyphs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Glyphs")
            .field("cached", &self.ready.len())
            .finish_non_exhaustive()
    }
}

pub(crate) const PAGE: u32 = 1024;
pub(crate) const MASK_PAGES: usize = 4;
pub(crate) const COLOR_PAGES: usize = 2;

impl Glyphs {
    pub(crate) fn new(fonts: FontSystem, metrics: CellMetrics, px: f32) -> Glyphs {
        let (hinting, thicken, builtin) = {
            let c = fonts.config();
            (c.hinting, c.thicken, c.builtin_glyphs)
        };
        Glyphs {
            fonts,
            rast: Rasterizer::new(),
            cache: GlyphCache::new(),
            sprites: SpriteCache::new(),
            mask: Atlas::new(PAGE, MASK_PAGES),
            color: Atlas::new(PAGE, COLOR_PAGES),
            ready: HashMap::new(),
            front: HashMap::new(),
            metrics,
            px,
            params: RasterParams::grid(px, hinting, thicken),
            builtin,
            uploads: Vec::new(),
            flushed: false,
            degraded: false,
            budget: MissBudget::new(16, Duration::from_millis(1)),
        }
    }

    pub(crate) fn metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// New pixel size (scale change): metrics from the font system, all caches dropped.
    /// Returns false when the font system gives no metrics.
    pub(crate) fn set_px(&mut self, px: f32) -> bool {
        let Some(m) = self.fonts.cell_metrics(px) else {
            return false;
        };
        let (hinting, thicken) = {
            let c = self.fonts.config();
            (c.hinting, c.thicken)
        };
        self.metrics = m;
        self.px = px;
        self.params = RasterParams::grid(px, hinting, thicken);
        self.reset();
        true
    }

    /// Drop atlas contents (device rebuild, scale change).
    pub(crate) fn reset(&mut self) {
        self.mask.clear();
        self.color.clear();
        self.ready.clear();
        self.front.clear();
        self.uploads.clear();
        self.sprites = SpriteCache::new();
        self.flushed = true;
    }

    pub(crate) fn begin_frame(&mut self) {
        self.mask.begin_frame();
        self.color.begin_frame();
        self.budget.reset();
    }

    /// Send queued texels to the renderer's atlas textures.
    pub(crate) fn flush(&mut self, r: &Renderer) {
        for u in self.uploads.drain(..) {
            if u.color {
                r.upload_color(u.page, u.x, u.y, u.w, u.h, &u.data);
            } else {
                r.upload_mask(u.page, u.x, u.y, u.w, u.h, &u.data);
            }
        }
    }

    fn resident(&mut self, key: &AKey, color: bool) -> bool {
        if color {
            self.color.get(key).is_some()
        } else {
            self.mask.get(key).is_some()
        }
    }

    fn place(
        &mut self,
        key: AKey,
        w: u32,
        h: u32,
        color: bool,
    ) -> Result<apoth_render::atlas::Placement, AtlasError> {
        let atlas = if color {
            &mut self.color
        } else {
            &mut self.mask
        };
        // `oldest_in_flight` 0: no page is ever a victim; a full atlas reports `Full`.
        atlas.insert(key, w, h, 0)
    }

    #[allow(clippy::too_many_arguments)]
    fn put(
        &mut self,
        key: AKey,
        w: u32,
        h: u32,
        data: Vec<u8>,
        color: bool,
        dx: i32,
        dy: i32,
        builtin: bool,
    ) -> GlyphLookup {
        if w == 0 || h == 0 {
            return GlyphLookup::Blank;
        }
        if let Some(r @ GlyphLookup::Ready(_)) = self.ready.get(&key).copied()
            && self.resident(&key, color)
        {
            return r;
        }
        match self.place(key, w, h, color) {
            Ok(p) => {
                self.uploads.push(Upload {
                    color,
                    page: p.page,
                    x: u32::from(p.x),
                    y: u32::from(p.y),
                    w,
                    h,
                    data,
                });
                let g = GlyphLookup::Ready(AtlasGlyph {
                    page: p.page,
                    x: p.x,
                    y: p.y,
                    w: p.w,
                    h: p.h,
                    dx: dx.clamp(-32768, 32767) as i16,
                    dy: dy.clamp(-32768, 32767) as i16,
                    color,
                    builtin,
                });
                self.ready.insert(key, g);
                g
            }
            Err(AtlasError::TooLarge) => GlyphLookup::Blank,
            Err(AtlasError::Full) if self.degraded => GlyphLookup::Blank,
            Err(AtlasError::Full) => {
                self.reset();
                GlyphLookup::Pending
            }
        }
    }

    fn sprite(&mut self, cp: u32, tofu: bool) -> GlyphLookup {
        let (cw, ch) = (self.metrics.width as u32, self.metrics.height as u32);
        let key = AKey {
            face: 0,
            glyph: cp,
            half: 0,
            kind: if tofu { KIND_TOFU } else { KIND_SPRITE },
        };
        let bm = if tofu {
            Some(self.sprites.tofu(cp, cw, ch))
        } else {
            self.sprites.get(cp, &SpriteParams::new(cw, ch))
        };
        match bm {
            Some(b) => self.put(key, b.width, b.height, b.data.clone(), false, 0, 0, true),
            None => GlyphLookup::Blank,
        }
    }

    fn outline(&mut self, bm: &GlyphBitmap, key: AKey) -> GlyphLookup {
        if bm.is_blank() {
            return GlyphLookup::Blank;
        }
        let dy = self.metrics.ascent_from_top() - bm.top;
        self.put(
            key,
            bm.width,
            bm.height,
            bm.data.clone(),
            false,
            bm.left,
            dy,
            false,
        )
    }

    fn resolve_new(&mut self, req: &GlyphRequest<'_>) -> (AKey, GlyphLookup) {
        let cp = req.cp;
        let mut text = String::new();
        if req.cluster.is_empty() {
            text.extend(char::from_u32(cp));
        } else {
            text.extend(req.cluster.iter().filter_map(|c| char::from_u32(*c)));
        }
        let blank = AKey {
            face: 0,
            glyph: cp,
            half: 0,
            kind: KIND_TOFU,
        };
        if text.is_empty()
            || cp == 0
            || cp == 0x20
            || char::from_u32(cp).is_some_and(char::is_control)
        {
            return (blank, GlyphLookup::Blank);
        }
        if self.builtin && req.cluster.len() <= 1 && is_sprite(cp) {
            let key = AKey {
                face: 0,
                glyph: cp,
                half: 0,
                kind: KIND_SPRITE,
            };
            return (key, self.sprite(cp, false));
        }
        let style = Style::from_flags(req.bold, req.italic);
        let resolved = self.fonts.resolve(style, &text);
        let Some(inst) = resolved.face else {
            return (blank, self.sprite(cp, true));
        };
        let Some(gid) = inst.face().glyph_for(cp) else {
            return (blank, self.sprite(cp, true));
        };
        let half = match req.half {
            Half::Whole => 0u8,
            Half::Left => 1,
            Half::Right => 2,
        };
        let cw = self.metrics.width;
        if inst.face().color().drawable() {
            let key = AKey {
                face: inst.key(),
                glyph: gid,
                half,
                kind: KIND_COLOR,
            };
            if req.half == Half::Right {
                return (key, GlyphLookup::Blank);
            }
            let Some(bm) = self
                .cache
                .color(&mut self.rast, &inst, gid, self.px, [255; 4])
            else {
                return (key, GlyphLookup::Blank);
            };
            let dy = self.metrics.ascent_from_top() - bm.top;
            let r = self.put(
                key,
                bm.width,
                bm.height,
                bm.data.clone(),
                true,
                bm.left,
                dy,
                false,
            );
            return (key, r);
        }
        let key = AKey {
            face: inst.key(),
            glyph: gid,
            half,
            kind: KIND_OUTLINE,
        };
        let Some(bm) = self.cache.gray(&mut self.rast, &inst, gid, &self.params) else {
            return (key, GlyphLookup::Blank);
        };
        let r = match req.half {
            Half::Whole => self.outline(&bm, key),
            Half::Left => self.outline(&bm.slice(0, cw), key),
            Half::Right => self.outline(&bm.slice(cw, 2 * cw), key),
        };
        (key, r)
    }
}

impl GlyphProvider for Glyphs {
    fn lookup(&mut self, req: &GlyphRequest<'_>) -> GlyphLookup {
        let half = match req.half {
            Half::Whole => 0u64,
            Half::Left => 1,
            Half::Right => 2,
        };
        let fkey = u64::from(req.cp)
            | (u64::from(req.bold) << 32)
            | (u64::from(req.italic) << 33)
            | (half << 34);
        let front = req.cluster.is_empty();
        if front && let Some((key, r)) = self.front.get(&fkey).copied() {
            match r {
                GlyphLookup::Ready(g) if self.resident(&key, g.color) => return r,
                GlyphLookup::Ready(_) => {
                    self.front.remove(&fkey);
                }
                _ => return r,
            }
        }
        if !self.budget.allow(req.echo) {
            return GlyphLookup::Pending;
        }
        let t = Instant::now();
        let (key, r) = self.resolve_new(req);
        if matches!(r, GlyphLookup::Ready(_)) {
            self.budget.record(t.elapsed());
        }
        if front && r != GlyphLookup::Pending {
            self.front.insert(fkey, (key, r));
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_font::config::FontConfig;
    use apoth_font::discovery::FontIndex;
    use apoth_font::system::EmbeddedFont;
    use std::sync::Arc;

    fn glyphs() -> Glyphs {
        let embedded = EmbeddedFont {
            name: "FiraMono-Medium.ttf".into(),
            data: Arc::from(
                &include_bytes!("../../apoth-font/tests/fixtures/FiraMono-Medium.ttf")[..],
            ),
        };
        let mut fs = FontSystem::new(FontIndex::default(), vec![embedded], FontConfig::default());
        let px = fs.config().pixel_size(1.0);
        let m = fs.cell_metrics(px).expect("metrics");
        Glyphs::new(fs, m, px)
    }

    fn req(cp: u32, echo: bool) -> GlyphRequest<'static> {
        GlyphRequest {
            cp,
            cluster: &[],
            bold: false,
            italic: false,
            half: Half::Whole,
            echo,
        }
    }

    #[test]
    fn misses_beyond_the_budget_are_pending_and_resolve_next_frame() {
        let mut g = glyphs();
        g.begin_frame();
        let mut pending = 0;
        for cp in 0x41..0x41 + 40u32 {
            if g.lookup(&req(cp, false)) == GlyphLookup::Pending {
                pending += 1;
            }
        }
        assert!(pending > 0, "40 distinct glyphs exceed 16 per frame");
        // Echo rows are exempt.
        assert!(matches!(g.lookup(&req(0x7a, true)), GlyphLookup::Ready(_)));
        g.begin_frame();
        let again = (0x41..0x41 + 40u32)
            .filter(|cp| g.lookup(&req(*cp, false)) == GlyphLookup::Pending)
            .count();
        assert!(again < pending, "the next frame makes progress");
    }

    #[test]
    fn full_atlas_clears_once_and_degraded_draws_blank() {
        let mut g = glyphs();
        let key = |n: u32| AKey {
            face: 1,
            glyph: n,
            half: 0,
            kind: KIND_OUTLINE,
        };
        // 1024x1024 pages, 4 of them: big bitmaps fill it with no eviction.
        let big = 500;
        let mut n = 0;
        while !g.flushed {
            let r = g.put(
                key(n),
                big,
                big,
                vec![0; (big * big) as usize],
                false,
                0,
                0,
                false,
            );
            assert!(n < 64, "atlas must fill");
            if r == GlyphLookup::Pending {
                break;
            }
            n += 1;
        }
        assert!(g.flushed, "a full atlas clears everything and says so");
        assert!(g.ready.is_empty());
        g.flushed = false;
        g.degraded = true;
        let mut blank = false;
        for i in 100..140 {
            if g.put(
                key(i),
                big,
                big,
                vec![0; (big * big) as usize],
                false,
                0,
                0,
                false,
            ) == GlyphLookup::Blank
            {
                blank = true;
                break;
            }
        }
        assert!(blank, "degraded: full means blank");
        assert!(!g.flushed, "degraded never clears again");
    }
}
