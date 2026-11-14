// SPDX-License-Identifier: AGPL-3.0-only
//! Pass 5: cursor shapes (T-07 §4; RENDERING-25, -26). The renderer draws the shape; blink
//! phase and focus come from the caller (`visible`, `focused`), so a static screen needs no
//! timer here.
//!
//! Cursor coordinates are grid pixels (not row-local): the cursor is not part of the
//! retained texture. Stroke width and thickness come from `CellMetrics::cursor_thickness`;
//! `cursor.thickness` scaling is the caller's (T-07 names the key but no default).
//! Unfocused block cursors become hollow; beam and underline are unchanged.

use crate::colour::Rgb;
use crate::scene::{
    BuildCtx, FLAG_BUILTIN, FLAG_COLOR, FLAG_CURSOR, GlyphInstance, GlyphLookup, GlyphProvider,
    GlyphRequest, Half, RectInstance,
};
use apoth_grid::{Attrs, SnapRow, Wide};

/// Cursor shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// Filled cell.
    Block,
    /// Vertical bar at the left of the cell.
    Beam,
    /// Bar at the bottom of the cell.
    Underline,
    /// Outline of the cell.
    Hollow,
}

/// What to draw.
#[derive(Clone, Copy, Debug)]
pub struct CursorState {
    /// Column.
    pub col: usize,
    /// Row.
    pub row: usize,
    /// Configured shape.
    pub shape: CursorShape,
    /// Blink phase / DECTCEM visibility.
    pub visible: bool,
    /// The window and pane have focus.
    pub focused: bool,
}

/// The cursor's instances for one frame.
#[derive(Clone, Debug, Default)]
pub struct CursorScene {
    /// Cursor rects (0, 1 or 4).
    pub rects: Vec<RectInstance>,
    /// The glyph redrawn over a block cursor in the cursor text colour.
    pub glyph: Option<GlyphInstance>,
}

/// The shape actually drawn.
#[must_use]
pub fn effective_shape(shape: CursorShape, focused: bool) -> CursorShape {
    if !focused && shape == CursorShape::Block {
        CursorShape::Hollow
    } else {
        shape
    }
}

/// Builds the cursor instances into `out` (cleared first).
pub fn build<P: GlyphProvider>(
    st: &CursorState,
    row: &SnapRow,
    ctx: &BuildCtx<'_>,
    echo: bool,
    provider: &mut P,
    out: &mut CursorScene,
) {
    out.rects.clear();
    out.glyph = None;
    if !st.visible {
        return;
    }
    let Some(cell) = row.cells.get(st.col) else {
        return;
    };
    let m = ctx.metrics;
    let pal = ctx.palette;

    let attrs = cell.style.attrs;
    let mut fg = pal.resolve_fg(
        cell.style.fg,
        ctx.cfg.bold_bright && attrs.contains(Attrs::BOLD),
    );
    let mut bg = pal.resolve_bg(cell.style.bg);
    if attrs.contains(Attrs::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    let colour: Rgb = pal.cursor.unwrap_or(fg);
    let text: Rgb = pal.cursor_text.unwrap_or(bg);
    let c = colour.premul(255);

    let x = st.col as i32 * m.width;
    let y = st.row as i32 * m.height;
    let w = if cell.wide == Wide::Wide {
        2 * m.width
    } else {
        m.width
    };
    let h = m.height;
    let t = m.cursor_thickness.max(1);

    match effective_shape(st.shape, st.focused) {
        CursorShape::Block => out.rects.push(RectInstance::new(x, y, w, h, c)),
        CursorShape::Beam => out.rects.push(RectInstance::new(x, y, t.min(w), h, c)),
        CursorShape::Underline => {
            let t = t.min(h);
            out.rects.push(RectInstance::new(x, y + h - t, w, t, c));
        }
        CursorShape::Hollow => {
            let t = t.min(w / 2).min(h / 2);
            if t < 1 {
                out.rects.push(RectInstance::new(x, y, w, h, c));
            } else {
                out.rects.push(RectInstance::new(x, y, w, t, c));
                out.rects.push(RectInstance::new(x, y + h - t, w, t, c));
                if h - 2 * t > 0 {
                    out.rects.push(RectInstance::new(x, y + t, t, h - 2 * t, c));
                    out.rects
                        .push(RectInstance::new(x + w - t, y + t, t, h - 2 * t, c));
                }
            }
        }
    }

    // The glyph under a (focused) block cursor is redrawn in the cursor text colour.
    if effective_shape(st.shape, st.focused) != CursorShape::Block
        || attrs.contains(Attrs::INVISIBLE)
    {
        return;
    }
    let (src, half) = match cell.wide {
        Wide::SpacerTail if st.col > 0 => (&row.cells[st.col - 1], Half::Right),
        Wide::Wide => (cell, Half::Left),
        Wide::SpacerTail | Wide::SpacerHead => return,
        Wide::Narrow => (cell, Half::Whole),
    };
    if src.cp == 0 || src.cp == 0x20 {
        return;
    }
    let cluster: &[u32] = if src.cluster_len > 0 {
        row.cluster(src)
    } else {
        &[]
    };
    let req = GlyphRequest {
        cp: src.cp,
        cluster,
        bold: attrs.contains(Attrs::BOLD),
        italic: attrs.contains(Attrs::ITALIC),
        half,
        echo,
    };
    if let GlyphLookup::Ready(g) = provider.lookup(&req) {
        let mut flags = FLAG_CURSOR;
        if g.color {
            flags |= FLAG_COLOR;
        }
        if g.builtin {
            flags |= FLAG_BUILTIN;
        }
        out.glyph = Some(GlyphInstance {
            cell: [st.col as u16, st.row as u16],
            offset: [g.dx, g.dy],
            atlas_xy: [g.x, g.y],
            size: [g.w, g.h],
            page: g.page,
            flags,
            color: if g.color {
                [255, 255, 255, 255]
            } else {
                text.premul(255)
            },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colour::Palette;
    use crate::decor::test_metrics;
    use crate::scene::{AtlasGlyph, SceneConfig};
    use apoth_grid::{SnapCell, Style};

    struct Ready;
    impl GlyphProvider for Ready {
        fn lookup(&mut self, _: &GlyphRequest<'_>) -> GlyphLookup {
            GlyphLookup::Ready(AtlasGlyph {
                page: 0,
                x: 5,
                y: 6,
                w: 4,
                h: 8,
                dx: 1,
                dy: 2,
                color: false,
                builtin: false,
            })
        }
    }

    fn row() -> SnapRow {
        let c = SnapCell {
            cp: 'a' as u32,
            cluster_start: 0,
            cluster_len: 0,
            wide: Wide::Narrow,
            style: Style::DEFAULT,
            link: false,
            protected: false,
            semantic: apoth_grid::Semantic::default(),
        };
        SnapRow {
            cells: vec![c; 4],
            ..SnapRow::default()
        }
    }

    fn run(shape: CursorShape, focused: bool, visible: bool) -> CursorScene {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let ctx = BuildCtx {
            cfg: &cfg,
            palette: &pal,
            metrics: &m,
            highlights: &[],
        };
        let st = CursorState {
            col: 2,
            row: 3,
            shape,
            visible,
            focused,
        };
        let mut out = CursorScene::default();
        build(&st, &row(), &ctx, false, &mut Ready, &mut out);
        out
    }

    const FG: [u8; 4] = [229, 229, 229, 255];

    #[test]
    fn block_fills_cell_and_recolours_glyph() {
        let o = run(CursorShape::Block, true, true);
        assert_eq!(o.rects, vec![RectInstance::new(16, 48, 8, 16, FG)]);
        let g = o.glyph.expect("glyph recoloured");
        assert_eq!(
            g.color,
            [0, 0, 0, 255],
            "cell background is the text colour"
        );
        assert_eq!(g.cell, [2, 3]);
        assert_ne!(g.flags & FLAG_CURSOR, 0);
    }

    #[test]
    fn beam_and_underline() {
        let o = run(CursorShape::Beam, true, true);
        assert_eq!(o.rects, vec![RectInstance::new(16, 48, 2, 16, FG)]);
        assert!(o.glyph.is_none());
        let o = run(CursorShape::Underline, true, true);
        assert_eq!(o.rects, vec![RectInstance::new(16, 62, 8, 2, FG)]);
    }

    #[test]
    fn hollow_is_four_strokes_inside_the_cell() {
        let o = run(CursorShape::Hollow, true, true);
        assert_eq!(o.rects.len(), 4);
        for r in &o.rects {
            assert!(
                r.x >= 16 && r.x + r.w <= 24 && r.y >= 48 && r.y + r.h <= 64,
                "{r:?}"
            );
        }
        // Strokes do not overlap: total area = outline area.
        let area: i32 = o.rects.iter().map(|r| r.w * r.h).sum();
        assert_eq!(area, 8 * 16 - 4 * 12);
        assert!(o.glyph.is_none());
    }

    #[test]
    fn unfocused_block_is_hollow_beam_unchanged() {
        assert_eq!(run(CursorShape::Block, false, true).rects.len(), 4);
        assert_eq!(run(CursorShape::Beam, false, true).rects.len(), 1);
        assert_eq!(
            effective_shape(CursorShape::Underline, false),
            CursorShape::Underline
        );
    }

    #[test]
    fn invisible_cursor_draws_nothing() {
        let o = run(CursorShape::Block, true, false);
        assert!(o.rects.is_empty() && o.glyph.is_none());
    }

    #[test]
    fn wide_cell_doubles_width() {
        let (cfg, pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        let ctx = BuildCtx {
            cfg: &cfg,
            palette: &pal,
            metrics: &m,
            highlights: &[],
        };
        let mut r = row();
        r.cells[0].wide = Wide::Wide;
        let st = CursorState {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            focused: true,
        };
        let mut out = CursorScene::default();
        build(&st, &r, &ctx, false, &mut Ready, &mut out);
        assert_eq!(out.rects[0].w, 16);
    }

    #[test]
    fn palette_overrides_colours() {
        let (cfg, mut pal, m) = (SceneConfig::default(), Palette::xterm(), test_metrics());
        pal.cursor = Some(Rgb(1, 2, 3));
        pal.cursor_text = Some(Rgb(9, 8, 7));
        let ctx = BuildCtx {
            cfg: &cfg,
            palette: &pal,
            metrics: &m,
            highlights: &[],
        };
        let st = CursorState {
            col: 0,
            row: 0,
            shape: CursorShape::Block,
            visible: true,
            focused: true,
        };
        let mut out = CursorScene::default();
        build(&st, &row(), &ctx, false, &mut Ready, &mut out);
        assert_eq!(out.rects[0].color, [1, 2, 3, 255]);
        assert_eq!(out.glyph.unwrap().color, [9, 8, 7, 255]);
    }
}
