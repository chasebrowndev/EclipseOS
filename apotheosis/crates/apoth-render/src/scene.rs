// SPDX-License-Identifier: AGPL-3.0-only
//! Snapshot -> instance lists (T-07 §4 pass 3; RENDERING-01, -03, -04, -17, -18, -24, -25;
//! PERFORMANCE-17).
//!
//! All coordinates in a [`RowScene`] are row-local pixels: the row's top-left is (0, 0). The
//! shader places the row through the ring (`damage::RowRing`). Colours are premultiplied
//! RGBA8. The per-cell build allocates nothing once a [`RowScene`]'s buffers have capacity.

use crate::colour::{MinContrast, Palette, Rgb, apply_min_contrast, dim};
use crate::damage::{Plan, Planner, RowRing, move_rows};
use crate::decor;
use apoth_font::metrics::CellMetrics;
use apoth_grid::{Attrs, Color, Damage, FrameSnapshot, SnapRow, Underline, Wide};
use std::fmt::Write as _;

/// A solid rectangle (decorations, cursor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectInstance {
    /// Left, row-local pixels.
    pub x: i32,
    /// Top, row-local pixels.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
    /// Premultiplied RGBA8.
    pub color: [u8; 4],
}

impl RectInstance {
    /// A rect.
    #[must_use]
    pub fn new(x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) -> Self {
        Self { x, y, w, h, color }
    }
}

/// Glyph uses the RGBA colour atlas and is not tinted.
pub const FLAG_COLOR: u16 = 1;
/// Glyph is a built-in sprite (exact-cell).
pub const FLAG_BUILTIN: u16 = 1 << 1;
/// Glyph is drawn by the cursor pass (recoloured under a block cursor).
pub const FLAG_CURSOR: u16 = 1 << 2;

/// One glyph quad (24 bytes; PERFORMANCE-17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct GlyphInstance {
    /// Grid cell (column, row).
    pub cell: [u16; 2],
    /// Pixel offset of the quad from the cell's top-left.
    pub offset: [i16; 2],
    /// Atlas rect origin.
    pub atlas_xy: [u16; 2],
    /// Atlas rect size (also the quad size).
    pub size: [u16; 2],
    /// Atlas page.
    pub page: u16,
    /// `FLAG_*`.
    pub flags: u16,
    /// Premultiplied RGBA8 tint.
    pub color: [u8; 4],
}

/// Which part of a wide glyph a cell shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Half {
    /// A one-cell glyph.
    Whole,
    /// Left cell of a wide glyph or ligature.
    Left,
    /// Right cell of a wide glyph or ligature.
    Right,
}

/// A glyph lookup.
#[derive(Clone, Copy, Debug)]
pub struct GlyphRequest<'a> {
    /// First code point.
    pub cp: u32,
    /// The whole cluster when the cell holds one.
    pub cluster: &'a [u32],
    /// Bold attribute.
    pub bold: bool,
    /// Italic attribute.
    pub italic: bool,
    /// Slice of a wide glyph.
    pub half: Half,
    /// The row answers a key event within `render.echo-window`: rasterise inline.
    pub echo: bool,
}

/// A glyph placed in an atlas page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasGlyph {
    /// Page.
    pub page: u16,
    /// Rect origin.
    pub x: u16,
    /// Rect origin.
    pub y: u16,
    /// Rect width.
    pub w: u16,
    /// Rect height.
    pub h: u16,
    /// Pixels from the cell's left edge to the quad's left edge.
    pub dx: i16,
    /// Pixels from the cell's top edge to the quad's top edge.
    pub dy: i16,
    /// From the colour atlas (emoji): never recoloured.
    pub color: bool,
    /// A built-in sprite.
    pub builtin: bool,
}

/// The answer to a [`GlyphRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphLookup {
    /// Nothing to draw (blank glyph).
    Blank,
    /// Not rasterised yet: draw the cell without it and keep the row pending.
    Pending,
    /// Ready in the atlas.
    Ready(AtlasGlyph),
}

/// Resolves cells to atlas glyphs. Implemented over `apoth-font` + [`crate::atlas::Atlas`]
/// by the GPU executor; tests use fakes.
pub trait GlyphProvider {
    /// Looks the glyph up, rasterising or queuing a miss as the budget allows.
    fn lookup(&mut self, req: &GlyphRequest<'_>) -> GlyphLookup;
}

/// Renderer configuration the scene build reads.
#[derive(Clone, Copy, Debug)]
pub struct SceneConfig {
    /// `colors.minimum-contrast`.
    pub min_contrast: MinContrast,
    /// `colors.minimum-contrast-builtin-glyphs`.
    pub min_contrast_builtin: bool,
    /// Bold text uses bright palette colours.
    pub bold_bright: bool,
    /// Alpha of default-background cells (window opacity), 0.0..=1.0.
    pub bg_alpha: f32,
}

impl Default for SceneConfig {
    fn default() -> Self {
        Self {
            min_contrast: MinContrast::Off,
            min_contrast_builtin: false,
            bold_bright: false,
            bg_alpha: 1.0,
        }
    }
}

/// A colour span folded into a row on the CPU (selection, search; RENDERING-24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Highlight {
    /// Row.
    pub row: usize,
    /// First column.
    pub start: usize,
    /// One past the last column.
    pub end: usize,
    /// Background.
    pub bg: Rgb,
    /// Foreground; `None` keeps the cell's.
    pub fg: Option<Rgb>,
}

/// Everything the row build reads besides the row.
#[derive(Clone, Copy, Debug)]
pub struct BuildCtx<'a> {
    /// Configuration.
    pub cfg: &'a SceneConfig,
    /// Colours.
    pub palette: &'a Palette,
    /// Integer cell metrics.
    pub metrics: &'a CellMetrics,
    /// Highlight spans for any row (filtered by `row`).
    pub highlights: &'a [Highlight],
}

/// One row's instance lists.
#[derive(Clone, Debug, Default)]
pub struct RowScene {
    /// One premultiplied RGBA8 background texel per column.
    pub bg: Vec<[u8; 4]>,
    /// Glyph instances.
    pub glyphs: Vec<GlyphInstance>,
    /// Decoration rects.
    pub rects: Vec<RectInstance>,
    /// A glyph was still pending: the row stays in the pending set.
    pub pending: bool,
}

impl RowScene {
    fn reset(&mut self, cols: usize) {
        self.bg.clear();
        self.bg.resize(cols, [0; 4]);
        self.glyphs.clear();
        self.rects.clear();
        self.pending = false;
    }

    /// Writes the stored-text form used by the scene snapshot tests.
    pub fn dump(&self, row: usize, out: &mut String) {
        let _ = writeln!(
            out,
            "row {row}{}",
            if self.pending { " pending" } else { "" }
        );
        let _ = write!(out, "  bg");
        let mut i = 0;
        while i < self.bg.len() {
            let mut j = i + 1;
            while j < self.bg.len() && self.bg[j] == self.bg[i] {
                j += 1;
            }
            let c = self.bg[i];
            let _ = write!(
                out,
                " {i}..{j}={:02x}{:02x}{:02x}{:02x}",
                c[0], c[1], c[2], c[3]
            );
            i = j;
        }
        let _ = writeln!(out);
        for g in &self.glyphs {
            let c = g.color;
            let _ = writeln!(
                out,
                "  glyph cell={},{} off={},{} atlas={},{} {}x{} page={} flags={} color={:02x}{:02x}{:02x}{:02x}",
                g.cell[0],
                g.cell[1],
                g.offset[0],
                g.offset[1],
                g.atlas_xy[0],
                g.atlas_xy[1],
                g.size[0],
                g.size[1],
                g.page,
                g.flags,
                c[0],
                c[1],
                c[2],
                c[3]
            );
        }
        for r in &self.rects {
            let c = r.color;
            let _ = writeln!(
                out,
                "  rect {},{} {}x{} color={:02x}{:02x}{:02x}{:02x}",
                r.x, r.y, r.w, r.h, c[0], c[1], c[2], c[3]
            );
        }
    }
}

/// Builds one row's instance lists into `out` (cleared first).
pub fn build_row<P: GlyphProvider>(
    ctx: &BuildCtx<'_>,
    y: usize,
    row: &SnapRow,
    echo: bool,
    provider: &mut P,
    out: &mut RowScene,
) {
    let m = ctx.metrics;
    let pal = ctx.palette;
    let cols = row.cells.len();
    out.reset(cols);
    let default_alpha = (ctx.cfg.bg_alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let min_target = ctx.cfg.min_contrast.target();

    for (col, cell) in row.cells.iter().enumerate() {
        let st = &cell.style;
        let bold = st.attrs.contains(Attrs::BOLD);
        let inverse = st.attrs.contains(Attrs::INVERSE);
        let mut fg = pal.resolve_fg(st.fg, ctx.cfg.bold_bright && bold);
        let mut bg = pal.resolve_bg(st.bg);
        let mut opaque = st.bg != Color::Default;
        if inverse {
            std::mem::swap(&mut fg, &mut bg);
            opaque = true;
        }
        if st.attrs.contains(Attrs::FAINT) {
            fg = dim(fg, bg);
        }
        for h in ctx.highlights {
            if h.row == y && col >= h.start && col < h.end {
                bg = h.bg;
                if let Some(f) = h.fg {
                    fg = f;
                }
                opaque = true;
            }
        }
        out.bg[col] = bg.premul(if opaque { 255 } else { default_alpha });

        let x0 = col as i32 * m.width;
        let mut text_fg = fg;
        let mut drawn_fg = fg;

        // Glyph: wide glyphs are looked up from their left cell and sliced per cell.
        let hidden = st.attrs.contains(Attrs::INVISIBLE);
        let (src, half) = match cell.wide {
            Wide::SpacerTail if col > 0 => (Some(&row.cells[col - 1]), Half::Right),
            Wide::Wide => (Some(cell), Half::Left),
            Wide::SpacerTail | Wide::SpacerHead => (None, Half::Whole),
            Wide::Narrow => (Some(cell), Half::Whole),
        };
        if let Some(s) = src.filter(|s| s.cp != 0 && s.cp != 0x20 && !hidden) {
            let cluster: &[u32] = if s.cluster_len > 0 {
                row.cluster(s)
            } else {
                &[]
            };
            let req = GlyphRequest {
                cp: s.cp,
                cluster,
                bold,
                italic: st.attrs.contains(Attrs::ITALIC),
                half,
                echo,
            };
            match provider.lookup(&req) {
                GlyphLookup::Blank => {}
                GlyphLookup::Pending => out.pending = true,
                GlyphLookup::Ready(g) => {
                    let mut flags = 0u16;
                    if g.color {
                        flags |= FLAG_COLOR;
                    }
                    if g.builtin {
                        flags |= FLAG_BUILTIN;
                    }
                    if let Some(t) = min_target {
                        let skip = g.color || (g.builtin && !ctx.cfg.min_contrast_builtin);
                        if !skip {
                            text_fg = apply_min_contrast(fg, bg, t);
                        }
                    }
                    drawn_fg = text_fg;
                    out.glyphs.push(GlyphInstance {
                        cell: [col as u16, y as u16],
                        offset: [g.dx, g.dy],
                        atlas_xy: [g.x, g.y],
                        size: [g.w, g.h],
                        page: g.page,
                        flags,
                        color: if g.color {
                            [255, 255, 255, 255]
                        } else {
                            text_fg.premul(255)
                        },
                    });
                }
            }
        }

        // Decorations use the final foreground.
        if st.underline != Underline::None {
            let c = if st.ul_color == Color::Default {
                drawn_fg
            } else {
                pal.resolve_bg(st.ul_color)
            };
            decor::underline(st.underline, x0, 0, m, c.premul(255), &mut out.rects);
        }
        if st.attrs.contains(Attrs::STRIKE) {
            decor::strikethrough(x0, 0, m, drawn_fg.premul(255), &mut out.rects);
        }
        if st.attrs.contains(Attrs::OVERLINE) {
            decor::overline(x0, 0, m, drawn_fg.premul(255), &mut out.rects);
        }
    }
}

/// What one [`GridScene::update`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameWork {
    /// A frame must be rendered and presented.
    pub present: bool,
    /// Rows rebuilt.
    pub rebuilt: usize,
    /// Rows still waiting for a glyph.
    pub pending: usize,
}

/// The retained grid's CPU state: per-row instance lists that move with their rows.
#[derive(Debug, Default)]
pub struct GridScene {
    /// Per-row instance lists, logical order.
    pub rows: Vec<RowScene>,
    /// The texture ring.
    pub ring: RowRing,
    planner: Planner,
    force: Vec<u32>,
    extra: Vec<u32>,
    redrawn: Vec<u32>,
    cols: usize,
}

impl GridScene {
    /// An empty scene; its first update must be `Full`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forces `row` to be rebuilt by the next update (selection or search changed).
    pub fn invalidate(&mut self, row: u32) {
        self.force.push(row);
    }

    /// The plan of the last update (ops for the executor).
    #[must_use]
    pub fn plan(&self) -> &Plan {
        self.planner.last()
    }

    /// Every logical row the last update rebuilt: the plan's redraw set plus pending rows
    /// retried. The executor redraws exactly these rows in the retained texture.
    #[must_use]
    pub fn redrawn(&self) -> &[u32] {
        &self.redrawn
    }

    /// Applies one frame's damage: plans, moves per-row ranges with their rows, rebuilds
    /// the rows to redraw (damaged, forced and pending) and reports the work done.
    /// `Damage::None` with nothing pending does no work.
    pub fn update<P: GlyphProvider>(
        &mut self,
        snap: &FrameSnapshot,
        damage: Damage,
        ctx: &BuildCtx<'_>,
        echo: bool,
        provider: &mut P,
    ) -> FrameWork {
        self.redrawn.clear();
        let nrows = snap.rows.len();
        let cols = snap.rows.first().map_or(0, |r| r.cells.len());
        let mut damage = damage;
        if nrows != self.rows.len() || cols != self.cols {
            self.rows.resize_with(nrows, RowScene::default);
            self.ring = RowRing::new(nrows);
            self.cols = cols;
            damage = Damage::Full;
        }
        self.extra.clear();
        self.extra.extend_from_slice(&self.force);
        self.force.clear();
        if damage == Damage::None && self.extra.is_empty() && !self.rows.iter().any(|r| r.pending) {
            return FrameWork::default();
        }
        let plan = self.planner.plan(
            damage,
            &snap.changed,
            &self.extra,
            nrows,
            cols,
            &mut self.ring,
        );
        for op in &plan.ops {
            move_rows(&mut self.rows, *op);
        }
        let mut rebuilt = 0;
        for &y in &self.planner.last().redraw {
            let y = y as usize;
            build_row(ctx, y, &snap.rows[y], echo, provider, &mut self.rows[y]);
            self.redrawn.push(y as u32);
            rebuilt += 1;
        }
        // Pending rows (their flags moved with their rows) retry unless just rebuilt.
        for (y, row) in self.rows.iter_mut().enumerate() {
            if row.pending
                && self
                    .planner
                    .last()
                    .redraw
                    .binary_search(&(y as u32))
                    .is_err()
            {
                build_row(ctx, y, &snap.rows[y], echo, provider, row);
                self.redrawn.push(y as u32);
                rebuilt += 1;
            }
        }
        let pending = self.rows.iter().filter(|r| r.pending).count();
        FrameWork {
            present: self.planner.last().present || rebuilt > 0,
            rebuilt,
            pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decor::test_metrics;
    use apoth_grid::{SnapCell, Style};

    /// Gives every letter a 6x10 glyph at atlas (cp*8, 0); digits are pending the first time.
    #[derive(Default)]
    struct Fake {
        seen_pending: std::collections::HashSet<u32>,
        lookups: usize,
        last_echo: bool,
    }

    impl GlyphProvider for Fake {
        fn lookup(&mut self, req: &GlyphRequest<'_>) -> GlyphLookup {
            self.lookups += 1;
            self.last_echo = req.echo;
            let c = req.cp;
            if (0x30..0x3a).contains(&c) && self.seen_pending.insert(c) {
                return GlyphLookup::Pending;
            }
            GlyphLookup::Ready(AtlasGlyph {
                page: 0,
                x: (c * 8) as u16,
                y: 0,
                w: 6,
                h: 10,
                dx: 1,
                dy: 2,
                color: c == 0x1f600,
                builtin: (0x2500..0x2580).contains(&c),
            })
        }
    }

    fn cell(cp: char, style: Style) -> SnapCell {
        SnapCell {
            cp: cp as u32,
            cluster_start: 0,
            cluster_len: 0,
            wide: Wide::Narrow,
            style,
            link: false,
            protected: false,
            semantic: apoth_grid::Semantic::default(),
        }
    }

    fn row(text: &str, style: Style) -> SnapRow {
        SnapRow {
            cells: text.chars().map(|c| cell(c, style)).collect(),
            ..SnapRow::default()
        }
    }

    fn ctx<'a>(
        cfg: &'a SceneConfig,
        pal: &'a Palette,
        m: &'a CellMetrics,
        h: &'a [Highlight],
    ) -> BuildCtx<'a> {
        BuildCtx {
            cfg,
            palette: pal,
            metrics: m,
            highlights: h,
        }
    }

    #[test]
    fn plain_row_builds_glyphs_and_bg() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let r = row("ab ", Style::DEFAULT);
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            3,
            &r,
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert_eq!(out.glyphs.len(), 2, "space draws no glyph");
        assert_eq!(out.glyphs[1].cell, [1, 3]);
        assert_eq!(out.glyphs[0].atlas_xy, [0x61 * 8, 0]);
        assert_eq!(out.bg, vec![[0, 0, 0, 255]; 3]);
        assert!(out.rects.is_empty() && !out.pending);
        assert_eq!(out.glyphs[0].color, [229, 229, 229, 255]);
    }

    #[test]
    fn inverse_swaps_and_is_opaque_even_with_translucent_default() {
        let cfg = SceneConfig {
            bg_alpha: 0.5,
            ..SceneConfig::default()
        };
        let (pal, m) = (Palette::xterm(), test_metrics());
        let st = Style {
            attrs: Attrs(Attrs::INVERSE),
            ..Style::DEFAULT
        };
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &row("a", st),
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert_eq!(out.bg[0], [229, 229, 229, 255]);
        assert_eq!(out.glyphs[0].color, [0, 0, 0, 255]);
        let mut out2 = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &row("a", Style::DEFAULT),
            false,
            &mut Fake::default(),
            &mut out2,
        );
        assert_eq!(out2.bg[0][3], 128);
    }

    #[test]
    fn highlight_folds_into_bg_and_fg() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let h = [Highlight {
            row: 0,
            start: 1,
            end: 2,
            bg: Rgb(1, 2, 3),
            fg: Some(Rgb(9, 9, 9)),
        }];
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &h),
            0,
            &row("abc", Style::DEFAULT),
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert_eq!(out.bg[1], [1, 2, 3, 255]);
        assert_eq!(out.bg[0], [0, 0, 0, 255]);
        assert_eq!(out.glyphs[1].color, [9, 9, 9, 255]);
        // Different row: untouched.
        let mut out2 = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &h),
            1,
            &row("abc", Style::DEFAULT),
            false,
            &mut Fake::default(),
            &mut out2,
        );
        assert_eq!(out2.bg[1], [0, 0, 0, 255]);
    }

    #[test]
    fn decorations_use_fg_or_underline_colour() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let st = Style {
            underline: Underline::Single,
            attrs: Attrs(Attrs::STRIKE | Attrs::OVERLINE),
            ul_color: Color::Rgb(255, 0, 0),
            ..Style::DEFAULT
        };
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &row(" a", st),
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert_eq!(
            out.rects.len(),
            6,
            "underline, strike, overline per cell, spaces included"
        );
        assert_eq!(out.rects[0].color, [255, 0, 0, 255]);
        assert_eq!(out.rects[1].color, [229, 229, 229, 255]);
        assert_eq!(out.rects[3].x, 8);
    }

    #[test]
    fn invisible_hides_glyph_only() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let st = Style {
            attrs: Attrs(Attrs::INVISIBLE | Attrs::OVERLINE),
            ..Style::DEFAULT
        };
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &row("a", st),
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert!(out.glyphs.is_empty());
        assert_eq!(out.rects.len(), 1);
    }

    #[test]
    fn wide_glyph_requests_left_then_right_slice() {
        struct Rec(Vec<Half>);
        impl GlyphProvider for Rec {
            fn lookup(&mut self, r: &GlyphRequest<'_>) -> GlyphLookup {
                self.0.push(r.half);
                GlyphLookup::Blank
            }
        }
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let mut r = row("x", Style::DEFAULT);
        r.cells[0].wide = Wide::Wide;
        let mut tail = cell(' ', Style::DEFAULT);
        tail.cp = 0;
        tail.wide = Wide::SpacerTail;
        r.cells.push(tail);
        let mut rec = Rec(Vec::new());
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &r,
            false,
            &mut rec,
            &mut RowScene::default(),
        );
        assert_eq!(rec.0, vec![Half::Left, Half::Right]);
    }

    #[test]
    fn min_contrast_skips_color_and_builtin_unless_enabled() {
        let pal = Palette::xterm();
        let m = test_metrics();
        let st = Style {
            fg: Color::Rgb(20, 20, 20),
            ..Style::DEFAULT
        };
        let mut cfg = SceneConfig {
            min_contrast: MinContrast::Ratio(4.5),
            ..SceneConfig::default()
        };
        let r = SnapRow {
            cells: vec![cell('a', st), cell('\u{2500}', st), cell('\u{1f600}', st)],
            ..SnapRow::default()
        };
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &r,
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert_ne!(out.glyphs[0].color, [20, 20, 20, 255], "text raised");
        assert_eq!(out.glyphs[1].color, [20, 20, 20, 255], "builtin skipped");
        assert_eq!(out.glyphs[2].color, [255, 255, 255, 255], "emoji untinted");
        cfg.min_contrast_builtin = true;
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &r,
            false,
            &mut Fake::default(),
            &mut out,
        );
        assert_ne!(
            out.glyphs[1].color,
            [20, 20, 20, 255],
            "builtin raised when enabled"
        );
    }

    #[test]
    fn rebuild_reuses_capacity() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let r = row("hello world", Style::DEFAULT);
        let mut out = RowScene::default();
        let mut f = Fake::default();
        build_row(&ctx(&cfg, &pal, &m, &[]), 0, &r, false, &mut f, &mut out);
        let caps = (
            out.bg.capacity(),
            out.glyphs.capacity(),
            out.rects.capacity(),
        );
        let ptrs = (out.bg.as_ptr(), out.glyphs.as_ptr());
        build_row(&ctx(&cfg, &pal, &m, &[]), 0, &r, false, &mut f, &mut out);
        assert_eq!(
            caps,
            (
                out.bg.capacity(),
                out.glyphs.capacity(),
                out.rects.capacity()
            )
        );
        assert_eq!(ptrs, (out.bg.as_ptr(), out.glyphs.as_ptr()));
    }

    fn snap(rows: &[&str]) -> FrameSnapshot {
        let mut s = FrameSnapshot::new();
        s.rows = rows.iter().map(|t| row(t, Style::DEFAULT)).collect();
        s
    }

    #[test]
    fn idle_does_no_work_and_pending_row_is_redrawn_next_frame() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let mut gs = GridScene::new();
        let mut f = Fake::default();
        let mut s = snap(&["ab", "1x"]);
        s.changed = vec![0, 1];
        let w = gs.update(&s, Damage::Full, &ctx(&cfg, &pal, &m, &[]), false, &mut f);
        assert_eq!((w.rebuilt, w.pending, w.present), (2, 1, true));
        assert!(gs.rows[1].pending);
        s.changed.clear();
        // No damage, but the pending row is retried and now resolves.
        let before = f.lookups;
        let w = gs.update(&s, Damage::None, &ctx(&cfg, &pal, &m, &[]), false, &mut f);
        assert_eq!((w.rebuilt, w.pending), (1, 0));
        assert!(f.lookups > before);
        assert_eq!(gs.rows[1].glyphs.len(), 2);
        // Now truly idle: no lookups, no work.
        let before = f.lookups;
        let w = gs.update(&s, Damage::None, &ctx(&cfg, &pal, &m, &[]), false, &mut f);
        assert_eq!(w, FrameWork::default());
        assert_eq!(f.lookups, before);
    }

    #[test]
    fn scroll_moves_row_scenes_with_rows() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let mut gs = GridScene::new();
        let mut f = Fake::default();
        let mut s = snap(&["aa", "bb", "cc"]);
        gs.update(&s, Damage::Full, &ctx(&cfg, &pal, &m, &[]), false, &mut f);
        // Scroll up one: rows become bb, cc, dd; the grid lists the exposed row.
        s = snap(&["bb", "cc", "dd"]);
        s.changed = vec![2];
        let d = Damage::Scroll {
            top: 0,
            bottom: 2,
            left: 0,
            right: 1,
            delta: 1,
        };
        let before = f.lookups;
        let w = gs.update(&s, d, &ctx(&cfg, &pal, &m, &[]), false, &mut f);
        assert_eq!(w.rebuilt, 1);
        assert_eq!(f.lookups - before, 2, "only the exposed row is rebuilt");
        assert_eq!(gs.rows[0].glyphs[0].atlas_xy[0], 0x62 * 8);
        assert_eq!(gs.rows[2].glyphs[0].atlas_xy[0], 0x64 * 8);
        assert_eq!(gs.ring.offset(), 1);
    }

    #[test]
    fn echo_flag_reaches_the_provider() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let mut f = Fake::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &row("a", Style::DEFAULT),
            true,
            &mut f,
            &mut RowScene::default(),
        );
        assert!(f.last_echo);
    }

    #[test]
    fn dump_is_stable_text() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let mut out = RowScene::default();
        build_row(
            &ctx(&cfg, &pal, &m, &[]),
            0,
            &row("a", Style::DEFAULT),
            false,
            &mut Fake::default(),
            &mut out,
        );
        let mut s = String::new();
        out.dump(0, &mut s);
        assert_eq!(
            s,
            "row 0\n  bg 0..1=000000ff\n  glyph cell=0,0 off=1,2 atlas=776,0 6x10 page=0 flags=0 color=e5e5e5ff\n"
        );
    }
}
