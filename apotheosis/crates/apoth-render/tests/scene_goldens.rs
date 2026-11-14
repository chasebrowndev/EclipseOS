// SPDX-License-Identifier: AGPL-3.0-only
//! Scene snapshot tests (the blocking, GPU-free gate: snapshot -> instance lists compared to
//! stored text) and offscreen pixel goldens drawn by the CPU reference renderer
//! (`apoth_render::reference`) at scale 1, compared to stored PNGs with a per-channel
//! tolerance plus a structural threshold.
//!
//! Stored files live in `tests/snapshots/*.txt` and `tests/golden/*.png`. A missing file
//! fails the test; create or refresh with `APOTH_BLESS=1 cargo test -p apoth-render` and
//! review the diff (read the PNGs) before committing. Glyphs are synthetic boxes, so the
//! files do not depend on any font or Mesa version.
//!
//! The GPU path (wgpu offscreen target on lavapipe) is not built yet: it will reuse these
//! scenes and skip with a message unless `APOTH_REQUIRE_GPU=1` when no adapter exists.

use apoth_font::metrics::CellMetrics;
use apoth_grid::{Attrs, Color, SnapCell, SnapRow, Style, Underline, Wide};
use apoth_render::colour::{Palette, Rgb, TextBlending};
use apoth_render::cursor::{self, CursorScene, CursorShape, CursorState};
use apoth_render::reference::{self, Image, MemAtlas};
use apoth_render::scene::{
    AtlasGlyph, BuildCtx, GlyphLookup, GlyphProvider, GlyphRequest, Highlight, RowScene,
    SceneConfig, build_row,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Per-channel tolerance and the fraction of pixels allowed to exceed it.
const TOLERANCE: u8 = 2;
const STRUCTURAL_FRACTION: f64 = 0.001;

fn metrics() -> CellMetrics {
    CellMetrics {
        width: 8,
        height: 16,
        baseline_from_bottom: 4,
        underline_position: 13,
        underline_thickness: 1,
        strikethrough_position: 8,
        strikethrough_thickness: 1,
        overline_position: 0,
        overline_thickness: 1,
        cursor_thickness: 2,
    }
}

/// Gives each code point a 6x10 synthetic glyph: a box whose interior varies with the code
/// point, so placement errors are visible.
struct Synth {
    atlas: MemAtlas,
    glyphs: HashMap<u32, AtlasGlyph>,
    next_x: u32,
}

impl Synth {
    fn new() -> Self {
        Self {
            atlas: MemAtlas::new(256, 1),
            glyphs: HashMap::new(),
            next_x: 0,
        }
    }
}

impl GlyphProvider for Synth {
    fn lookup(&mut self, req: &GlyphRequest<'_>) -> GlyphLookup {
        if let Some(g) = self.glyphs.get(&req.cp) {
            return GlyphLookup::Ready(*g);
        }
        let (w, h) = (6u32, 10u32);
        let mut data = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let edge = x == 0 || y == 0 || x == w - 1 || y == h - 1;
                let diag = req.cp.is_multiple_of(2) && x == y * w / h;
                let cov = if edge {
                    255
                } else if diag {
                    160
                } else {
                    0
                };
                data[(y * w + x) as usize] = cov;
            }
        }
        let x = self.next_x;
        self.next_x += w + 1;
        self.atlas.write_mask(0, x, 0, w, h, &data);
        let g = AtlasGlyph {
            page: 0,
            x: x as u16,
            y: 0,
            w: w as u16,
            h: h as u16,
            dx: 1,
            dy: 2,
            color: false,
            builtin: false,
        };
        self.glyphs.insert(req.cp, g);
        GlyphLookup::Ready(g)
    }
}

fn cell(ch: char, style: Style) -> SnapCell {
    SnapCell {
        cp: ch as u32,
        cluster_start: 0,
        cluster_len: 0,
        wide: Wide::Narrow,
        style,
        link: false,
        protected: false,
        semantic: apoth_grid::Semantic::default(),
    }
}

fn row(cells: Vec<SnapCell>) -> SnapRow {
    SnapRow {
        cells,
        ..SnapRow::default()
    }
}

fn underlined(u: Underline) -> Style {
    Style {
        underline: u,
        fg: Color::Palette(2),
        ..Style::DEFAULT
    }
}

/// Three rows: underline styles; line attributes and colours; selection.
fn decoration_rows() -> Vec<SnapRow> {
    let mut r0 = Vec::new();
    for u in [
        Underline::Single,
        Underline::Double,
        Underline::Curly,
        Underline::Dotted,
        Underline::Dashed,
    ] {
        r0.push(cell('a', underlined(u)));
        r0.push(cell('b', underlined(u)));
    }
    let strike = Style {
        attrs: Attrs(Attrs::STRIKE),
        ..Style::DEFAULT
    };
    let over = Style {
        attrs: Attrs(Attrs::OVERLINE),
        ..Style::DEFAULT
    };
    let coloured = Style {
        underline: Underline::Curly,
        ul_color: Color::Rgb(255, 80, 80),
        ..Style::DEFAULT
    };
    let inverse = Style {
        attrs: Attrs(Attrs::INVERSE),
        fg: Color::Palette(4),
        ..Style::DEFAULT
    };
    let faint = Style {
        attrs: Attrs(Attrs::FAINT),
        ..Style::DEFAULT
    };
    let bold = Style {
        attrs: Attrs(Attrs::BOLD),
        fg: Color::Palette(1),
        ..Style::DEFAULT
    };
    let r1 = vec![
        cell('s', strike),
        cell('t', strike),
        cell('o', over),
        cell('p', over),
        cell('c', coloured),
        cell('d', coloured),
        cell('i', inverse),
        cell('j', inverse),
        cell('f', faint),
        cell('g', faint),
        cell('B', bold),
        cell('C', bold),
    ];
    let r2 = "select this text"
        .chars()
        .map(|c| cell(c, Style::DEFAULT))
        .collect();
    vec![row(r0), row(r1), row(r2)]
}

fn build_all(rows: &[SnapRow], highlights: &[Highlight], synth: &mut Synth) -> Vec<RowScene> {
    let cfg = SceneConfig {
        bold_bright: true,
        ..SceneConfig::default()
    };
    let pal = Palette::xterm();
    let m = metrics();
    let ctx = BuildCtx {
        cfg: &cfg,
        palette: &pal,
        metrics: &m,
        highlights,
    };
    rows.iter()
        .enumerate()
        .map(|(y, r)| {
            let mut out = RowScene::default();
            build_row(&ctx, y, r, false, synth, &mut out);
            out
        })
        .collect()
}

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn blessing() -> bool {
    std::env::var_os("APOTH_BLESS").is_some_and(|v| v == "1")
}

fn check_text(name: &str, actual: &str) {
    let path = snapshot_dir().join("snapshots").join(format!("{name}.txt"));
    if blessing() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; run with APOTH_BLESS=1 and review",
            path.display()
        )
    });
    assert_eq!(
        want, actual,
        "scene snapshot {name} changed (APOTH_BLESS=1 to refresh after review)"
    );
}

fn check_png(name: &str, img: &Image) {
    let path = snapshot_dir().join("golden").join(format!("{name}.png"));
    let straight = img.to_straight();
    if blessing() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let f = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let mut enc = png::Encoder::new(f, img.w as u32, img.h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&straight)
            .unwrap();
        return;
    }
    let f = std::fs::File::open(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with APOTH_BLESS=1 and read it",
            path.display()
        )
    });
    let mut reader = png::Decoder::new(std::io::BufReader::new(f))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(
        (info.color_type, info.bit_depth),
        (png::ColorType::Rgba, png::BitDepth::Eight)
    );
    buf.truncate(info.line_size * info.height as usize);
    let want = Image {
        w: info.width as usize,
        h: info.height as usize,
        data: buf,
    };
    let got = Image {
        w: img.w,
        h: img.h,
        data: straight,
    };
    assert_eq!(
        (want.w, want.h),
        (got.w, got.h),
        "golden {name}: size changed"
    );
    let d = reference::diff(&want, &got, TOLERANCE);
    let allowed = (d.total as f64 * STRUCTURAL_FRACTION) as usize;
    assert!(
        d.differing <= allowed,
        "golden {name}: {} of {} pixels differ by more than {TOLERANCE} (max channel diff {})",
        d.differing,
        d.total,
        d.max_channel
    );
}

#[test]
fn decorations_scene_and_pixels() {
    let mut synth = Synth::new();
    let hl = [Highlight {
        row: 2,
        start: 7,
        end: 11,
        bg: Rgb(60, 60, 140),
        fg: Some(Rgb(255, 255, 255)),
    }];
    let rows = build_all(&decoration_rows(), &hl, &mut synth);
    let mut text = String::new();
    for (y, r) in rows.iter().enumerate() {
        r.dump(y, &mut text);
    }
    check_text("decorations", &text);
    let img = reference::render(
        &rows,
        None,
        &metrics(),
        &synth.atlas,
        TextBlending::LinearCorrected,
    );
    check_png("decorations", &img);
}

#[test]
fn cursor_shapes_scene_and_pixels() {
    let mut synth = Synth::new();
    let rows = build_all(
        &[row("cursor"
            .chars()
            .map(|c| cell(c, Style::DEFAULT))
            .collect())],
        &[],
        &mut synth,
    );
    let cfg = SceneConfig::default();
    let pal = Palette::xterm();
    let m = metrics();
    let ctx = BuildCtx {
        cfg: &cfg,
        palette: &pal,
        metrics: &m,
        highlights: &[],
    };
    let snap_row = row("cursor".chars().map(|c| cell(c, Style::DEFAULT)).collect());
    let mut text = String::new();
    for (name, shape, focused) in [
        ("block", CursorShape::Block, true),
        ("beam", CursorShape::Beam, true),
        ("underline", CursorShape::Underline, true),
        ("hollow", CursorShape::Hollow, false),
    ] {
        let st = CursorState {
            col: 2,
            row: 0,
            shape,
            visible: true,
            focused,
        };
        let mut cs = CursorScene::default();
        cursor::build(&st, &snap_row, &ctx, false, &mut synth, &mut cs);
        text.push_str(&format!("{name}\n"));
        for r in &cs.rects {
            text.push_str(&format!("  {r:?}\n"));
        }
        if let Some(g) = &cs.glyph {
            text.push_str(&format!("  {g:?}\n"));
        }
        let img = reference::render(&rows, Some(&cs), &m, &synth.atlas, TextBlending::Native);
        check_png(&format!("cursor_{name}"), &img);
    }
    check_text("cursors", &text);
}

#[test]
fn glyph_quads_sit_on_integer_origins_for_every_cell_metrics() {
    // The quad of cell (c, r) starts at (c * w + dx, r * h + dy): grid glyphs sit on integer
    // pixel origins at any cell metrics, so there is no sub-pixel drift (T-06 §3). The cell
    // sizes below are what scale 1, 1.25, 1.5 and 2 give for an 8x16 base cell.
    for (w, h) in [(8, 16), (10, 20), (12, 24), (16, 32)] {
        let m = CellMetrics {
            width: w,
            height: h,
            ..metrics()
        };
        let cfg = SceneConfig::default();
        let pal = Palette::xterm();
        let ctx = BuildCtx {
            cfg: &cfg,
            palette: &pal,
            metrics: &m,
            highlights: &[],
        };
        let r = row("abcdef".chars().map(|c| cell(c, Style::DEFAULT)).collect());
        let mut synth = Synth::new();
        let mut out = RowScene::default();
        build_row(&ctx, 0, &r, false, &mut synth, &mut out);
        let rows = [out];
        let img = reference::render(&rows, None, &m, &synth.atlas, TextBlending::Native);
        assert_eq!(img.w, 6 * w as usize);
        for (i, g) in rows[0].glyphs.iter().enumerate() {
            assert_eq!(
                i32::from(g.cell[0]) * w + i32::from(g.offset[0]),
                i as i32 * w + 1
            );
        }
        // Edge pixels of the first glyph's box: top-left corner of the quad is lit.
        let (x0, y0) = (1usize, 2usize);
        assert_eq!(
            img.data[(y0 * img.w + x0) * 4],
            229,
            "quad origin pixel at cell size {w}x{h}"
        );
        assert_eq!(
            img.data[((y0 + 9) * img.w + x0 + 5) * 4],
            229,
            "quad far corner at {w}x{h}"
        );
    }
}
