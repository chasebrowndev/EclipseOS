// SPDX-License-Identifier: AGPL-3.0-only
//! GPU goldens (T-25 §5, T-07 §6): the reference scenes drawn by the wgpu executor into an
//! offscreen target on whatever Vulkan adapter exists (lavapipe counts), read back and
//! compared to the CPU reference renderer (whose output is the stored PNG golden of
//! `scene_goldens.rs`) with a per-channel tolerance plus a structural threshold.
//!
//! With no adapter the tests print a skip message and pass, unless `APOTH_REQUIRE_GPU=1`, which
//! turns the absence into a failure. These are class CI-N: informative when the Mesa version
//! changes, never a reason to bless a scene snapshot.

use apoth_font::metrics::CellMetrics;
use apoth_grid::{Attrs, Color, Damage, FrameSnapshot, SnapCell, SnapRow, Style, Underline, Wide};
use apoth_render::colour::{Palette, Rgb, TextBlending};
use apoth_render::cursor::{self, CursorScene, CursorShape, CursorState};
use apoth_render::gpu::{Gpu, GpuOptions};
use apoth_render::image::ImageQuad;
use apoth_render::presenter::{OffscreenPresenter, PresentOutcome, Presenter};
use apoth_render::reference::{self, Image, MemAtlas};
use apoth_render::renderer::{FrameInput, PaneDraw, Renderer, RendererConfig};
use apoth_render::scene::{
    AtlasGlyph, BuildCtx, GlyphLookup, GlyphProvider, GlyphRequest, GridScene, Highlight,
    SceneConfig,
};
use std::collections::HashMap;

const PAGE: u32 = 256;

fn metrics(w: i32, h: i32) -> CellMetrics {
    CellMetrics {
        width: w,
        height: h,
        baseline_from_bottom: h / 4,
        underline_position: h - 3,
        underline_thickness: 1,
        strikethrough_position: h / 2,
        strikethrough_thickness: 1,
        overline_position: 0,
        overline_thickness: 1,
        cursor_thickness: 2,
    }
}

fn gpu() -> Option<Gpu> {
    match Gpu::headless(&GpuOptions {
        allow_software: true,
        compositor_hint: None,
    }) {
        Ok(g) => Some(g),
        Err(e) => {
            assert!(
                std::env::var_os("APOTH_REQUIRE_GPU").is_none_or(|v| v != "1"),
                "APOTH_REQUIRE_GPU=1 and no Vulkan adapter: {e}"
            );
            eprintln!("SKIP gpu_goldens: no Vulkan adapter ({e}); set APOTH_REQUIRE_GPU=1 to fail");
            None
        }
    }
}

/// Synthetic 6x10 glyph boxes, as in `scene_goldens.rs`.
struct Synth {
    atlas: MemAtlas,
    glyphs: HashMap<u32, AtlasGlyph>,
    next_x: u32,
}

impl Synth {
    fn new() -> Self {
        Self {
            atlas: MemAtlas::new(PAGE, 1),
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
                data[(y * w + x) as usize] = if edge {
                    255
                } else if diag {
                    160
                } else {
                    0
                };
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

fn text_row(s: &str, style: Style) -> SnapRow {
    SnapRow {
        cells: s.chars().map(|c| cell(c, style)).collect(),
        ..SnapRow::default()
    }
}

fn decoration_rows() -> Vec<SnapRow> {
    let mut r0 = Vec::new();
    for u in [
        Underline::Single,
        Underline::Double,
        Underline::Curly,
        Underline::Dotted,
        Underline::Dashed,
    ] {
        let st = Style {
            underline: u,
            fg: Color::Palette(2),
            ..Style::DEFAULT
        };
        r0.push(cell('a', st));
        r0.push(cell('b', st));
    }
    let strike = Style {
        attrs: Attrs(Attrs::STRIKE),
        ..Style::DEFAULT
    };
    let over = Style {
        attrs: Attrs(Attrs::OVERLINE),
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
        cell('i', inverse),
        cell('j', inverse),
        cell('f', faint),
        cell('g', faint),
        cell('B', bold),
        cell('C', bold),
    ];
    let r1r = SnapRow {
        cells: r1,
        ..SnapRow::default()
    };
    let r2 = text_row("select this text", Style::DEFAULT);
    let r0r = SnapRow {
        cells: r0,
        ..SnapRow::default()
    };
    // Rows must share a width: pad to 16 columns.
    let mut rows = vec![r0r, r1r, r2];
    for r in &mut rows {
        while r.cells.len() < 16 {
            r.cells.push(cell(' ', Style::DEFAULT));
        }
    }
    rows
}

struct Harness {
    synth: Synth,
    scene: GridScene,
    snap: FrameSnapshot,
    cfg: SceneConfig,
    pal: Palette,
    m: CellMetrics,
    hl: Vec<Highlight>,
    renderer: Renderer,
    pres: OffscreenPresenter,
    mode: TextBlending,
    uploaded: u32,
}

impl Harness {
    fn new(gpu: &Gpu, m: CellMetrics, cols: usize, rows: usize, mode: TextBlending) -> Self {
        let size = (
            (cols as i32 * m.width) as u32,
            (rows as i32 * m.height) as u32,
        );
        let rcfg = RendererConfig {
            cell: (m.width as u32, m.height as u32),
            page_size: PAGE,
            mask_pages: 1,
            color_pages: 1,
            text_blending: mode,
            ..RendererConfig::default()
        };
        Self {
            synth: Synth::new(),
            scene: GridScene::new(),
            snap: FrameSnapshot::new(),
            cfg: SceneConfig {
                bold_bright: true,
                ..SceneConfig::default()
            },
            pal: Palette::xterm(),
            m,
            hl: Vec::new(),
            renderer: Renderer::new(gpu, &rcfg),
            pres: OffscreenPresenter::new(gpu, size),
            mode,
            uploaded: 0,
        }
    }

    /// Applies one frame (snapshot rows, changed rows, damage), uploads new atlas texels,
    /// draws and returns the readback.
    fn frame(
        &mut self,
        rows: Vec<SnapRow>,
        changed: Vec<u32>,
        damage: Damage,
        cur: Option<&CursorScene>,
    ) -> Image {
        self.snap.rows = rows;
        self.snap.changed = changed;
        let ctx = BuildCtx {
            cfg: &self.cfg,
            palette: &self.pal,
            metrics: &self.m,
            highlights: &self.hl,
        };
        self.scene
            .update(&self.snap, damage, &ctx, false, &mut self.synth);
        if self.synth.next_x != self.uploaded {
            self.renderer
                .upload_mask(0, 0, 0, PAGE, PAGE, self.synth.atlas.mask_page(0));
            self.uploaded = self.synth.next_x;
        }
        let panes = [PaneDraw::single(&self.scene, cur)];
        let input = FrameInput {
            panes: &panes,
            clear: [0.0; 4],
        };
        match self.pres.present(&mut self.renderer, &input) {
            PresentOutcome::Presented(_) => {}
            other => panic!("offscreen present failed: {other:?}"),
        }
        self.pres.read_pixels()
    }

    fn reference(&self, cur: Option<&CursorScene>) -> Image {
        reference::render(&self.scene.rows, cur, &self.m, &self.synth.atlas, self.mode)
    }
}

fn assert_close(name: &str, got: &Image, want: &Image, tol: u8, fraction: f64) {
    assert_eq!((got.w, got.h), (want.w, want.h), "{name}: size");
    let d = reference::diff(got, want, tol);
    let allowed = (d.total as f64 * fraction) as usize;
    assert!(
        d.differing <= allowed,
        "{name}: {} of {} pixels differ by more than {tol} (max channel diff {})",
        d.differing,
        d.total,
        d.max_channel
    );
}

#[test]
fn decorations_match_the_reference() {
    let Some(gpu) = gpu() else { return };
    for (mode, tol, frac) in [
        (TextBlending::Native, 2, 0.001),
        (TextBlending::LinearCorrected, 8, 0.01),
        (TextBlending::Linear, 8, 0.01),
    ] {
        let mut h = Harness::new(&gpu, metrics(8, 16), 16, 3, mode);
        h.hl = vec![Highlight {
            row: 2,
            start: 7,
            end: 11,
            bg: Rgb(60, 60, 140),
            fg: Some(Rgb(255, 255, 255)),
        }];
        let got = h.frame(decoration_rows(), vec![0, 1, 2], Damage::Full, None);
        let want = h.reference(None);
        assert_close(&format!("decorations {mode:?}"), &got, &want, tol, frac);
    }
}

#[test]
fn cursor_shapes_match_the_reference() {
    let Some(gpu) = gpu() else { return };
    let m = metrics(8, 16);
    for (shape, focused) in [
        (CursorShape::Block, true),
        (CursorShape::Beam, true),
        (CursorShape::Underline, true),
        (CursorShape::Hollow, false),
    ] {
        let mut h = Harness::new(&gpu, m, 6, 1, TextBlending::Native);
        let rows = vec![text_row("cursor", Style::DEFAULT)];
        let mut cs = CursorScene::default();
        let st = CursorState {
            col: 2,
            row: 0,
            shape,
            visible: true,
            focused,
        };
        // Build the glyph request before the frame so the atlas holds the glyph.
        let ctx = BuildCtx {
            cfg: &h.cfg,
            palette: &h.pal,
            metrics: &h.m,
            highlights: &[],
        };
        cursor::build(&st, &rows[0], &ctx, false, &mut h.synth, &mut cs);
        let got = h.frame(rows, vec![0], Damage::Full, Some(&cs));
        let want = h.reference(Some(&cs));
        assert_close(&format!("cursor {shape:?}"), &got, &want, 2, 0.001);
    }
}

#[test]
fn glyph_grid_alignment_at_every_scale() {
    // Cell sizes for an 8x16 base at scale 1, 1.25, 1.5 and 2.
    let Some(gpu) = gpu() else { return };
    for (w, ht) in [(8, 16), (10, 20), (12, 24), (16, 32)] {
        let mut h = Harness::new(&gpu, metrics(w, ht), 6, 2, TextBlending::Native);
        let rows = vec![
            text_row("abcdef", Style::DEFAULT),
            text_row("ghijkl", Style::DEFAULT),
        ];
        let got = h.frame(rows, vec![0, 1], Damage::Full, None);
        let want = h.reference(None);
        assert_close(&format!("scale cell {w}x{ht}"), &got, &want, 2, 0.001);
    }
}

fn numbered(n: usize, cols: usize) -> Vec<SnapRow> {
    (0..n)
        .map(|i| {
            let ch = char::from(b'a' + (i % 26) as u8);
            text_row(&ch.to_string().repeat(cols), Style::DEFAULT)
        })
        .collect()
}

#[test]
fn ring_and_region_scrolls_equal_a_from_scratch_render() {
    let Some(gpu) = gpu() else { return };
    let (cols, nrows) = (6usize, 7usize);
    let mut h = Harness::new(&gpu, metrics(8, 16), cols, nrows, TextBlending::Native);
    let mut rows = numbered(nrows, cols);
    let first = h.frame(
        rows.clone(),
        (0..nrows as u32).collect(),
        Damage::Full,
        None,
    );
    assert_close("first frame", &first, &h.reference(None), 2, 0.001);

    // Full-grid scroll up by one: the ring offset advances; the new bottom row is drawn.
    rows.remove(0);
    rows.push(text_row("zzzzzz", Style::DEFAULT));
    let img = h.frame(
        rows.clone(),
        vec![nrows as u32 - 1],
        Damage::Scroll {
            top: 0,
            bottom: nrows - 1,
            left: 0,
            right: cols - 1,
            delta: 1,
        },
        None,
    );
    assert_close("full scroll", &img, &h.reference(None), 2, 0.001);

    // Region scroll down by two over rows 1..=5 (DECSTBM), through the scratch texture.
    for _ in 0..2 {
        rows.remove(5);
        rows.insert(1, text_row("......", Style::DEFAULT));
    }
    let img = h.frame(
        rows.clone(),
        vec![1, 2],
        Damage::Scroll {
            top: 1,
            bottom: 5,
            left: 0,
            right: cols - 1,
            delta: -2,
        },
        None,
    );
    assert_close("region scroll", &img, &h.reference(None), 2, 0.001);

    // Edit one row only: untouched rows keep their retained pixels.
    rows[3] = text_row("EDITED", Style::DEFAULT);
    let img = h.frame(rows, vec![3], Damage::Rows, None);
    assert_close("row edit", &img, &h.reference(None), 2, 0.001);
}

#[test]
fn a_skipped_frame_is_repaired_by_a_full_redraw() {
    let Some(gpu) = gpu() else { return };
    let mut h = Harness::new(&gpu, metrics(8, 16), 6, 3, TextBlending::Native);
    let rows = numbered(3, 6);
    let _ = h.frame(rows.clone(), vec![0, 1, 2], Damage::Full, None);
    h.renderer.invalidate_all();
    let img = h.frame(rows, vec![], Damage::Rows, None);
    assert_close("after invalidate", &img, &h.reference(None), 2, 0.001);
}

fn crop(img: &Image, x: usize, w: usize) -> Image {
    let mut out = Image::new(w, img.h);
    for y in 0..img.h {
        let src = (y * img.w + x) * 4;
        out.data[y * w * 4..(y + 1) * w * 4].copy_from_slice(&img.data[src..src + w * 4]);
    }
    out
}

/// Two panes of one window (T-13): each keyed grid draws at its own origin and matches its
/// own reference; the gap between them stays the clear colour.
#[test]
fn two_panes_draw_side_by_side_at_their_origins() {
    let Some(gpu) = gpu() else { return };
    let m = metrics(8, 16);
    let (cols, rows) = (6usize, 3usize);
    let pane_w = cols * m.width as usize;
    let gap = 2 * m.width as usize;
    let mut h = Harness::new(&gpu, m, 2 * cols + 2, rows, TextBlending::Native);
    let mut right = GridScene::new();
    let mut snap = FrameSnapshot::new();
    snap.rows = vec![
        text_row(&format!("{:6}", "right"), Style::default()),
        text_row(&format!("{:6}", "pane"), Style::default()),
        text_row(&format!("{:6}", "two"), Style::default()),
    ];
    snap.changed = vec![0, 1, 2];
    h.snap.rows = vec![
        text_row(&format!("{:6}", "left"), Style::default()),
        text_row(&format!("{:6}", "pane"), Style::default()),
        text_row(&format!("{:6}", "one"), Style::default()),
    ];
    h.snap.changed = vec![0, 1, 2];
    {
        let ctx = BuildCtx {
            cfg: &h.cfg,
            palette: &h.pal,
            metrics: &h.m,
            highlights: &h.hl,
        };
        h.scene
            .update(&h.snap, Damage::Full, &ctx, false, &mut h.synth);
        right.update(&snap, Damage::Full, &ctx, false, &mut h.synth);
    }
    h.renderer
        .upload_mask(0, 0, 0, PAGE, PAGE, h.synth.atlas.mask_page(0));
    let panes = [
        PaneDraw {
            key: 1,
            scene: &h.scene,
            cursor: None,
            origin: Some((0, 0)),
            images_below: &[],
            images_above: &[],
            backdrop: None,
        },
        PaneDraw {
            key: 2,
            scene: &right,
            cursor: None,
            origin: Some(((pane_w + gap) as i32, 0)),
            images_below: &[],
            images_above: &[],
            backdrop: None,
        },
    ];
    let input = FrameInput {
        panes: &panes,
        clear: [0.0; 4],
    };
    match h.pres.present(&mut h.renderer, &input) {
        PresentOutcome::Presented(_) => {}
        other => panic!("offscreen present failed: {other:?}"),
    }
    let got = h.pres.read_pixels();
    let want_l = reference::render(&h.scene.rows, None, &h.m, &h.synth.atlas, h.mode);
    let want_r = reference::render(&right.rows, None, &h.m, &h.synth.atlas, h.mode);
    assert_close("left pane", &crop(&got, 0, pane_w), &want_l, 2, 0.001);
    assert_close(
        "right pane",
        &crop(&got, pane_w + gap, pane_w),
        &want_r,
        2,
        0.001,
    );
    let between = crop(&got, pane_w, gap);
    assert!(
        between.data.iter().all(|b| *b == 0),
        "the gap between the panes is not the clear colour"
    );
}

fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    px.repeat((w * h) as usize)
}

fn at(img: &Image, x: usize, y: usize) -> [u8; 4] {
    let i = (y * img.w + x) * 4;
    img.data[i..i + 4].try_into().unwrap()
}

fn near(got: [u8; 4], want: [u8; 4], tol: u8, what: &str) {
    assert!(
        got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= tol),
        "{what}: got {got:?}, want {want:?}"
    );
}

/// Writes the readback for a human look when `APOTH_IMAGE_DUMP` names a directory.
fn dump(name: &str, img: &Image) {
    let Some(dir) = std::env::var_os("APOTH_IMAGE_DUMP") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(format!("{name}.png"));
    let f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut enc = png::Encoder::new(f, img.w as u32, img.h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&img.to_straight())
        .unwrap();
}

/// T-15 §6 (RENDERING-35..37): a negative-z image draws under the text over the backdrop, a
/// positive one over the text, both on the pixel grid at every scale; 1:1 is pixel exact.
#[test]
fn images_draw_under_and_over_the_text_on_the_pixel_grid() {
    let Some(gpu) = gpu() else { return };
    let red = solid(4, 4, [255, 0, 0, 255]);
    let blue = solid(4, 4, [0, 0, 255, 128]);
    let pair = [255, 0, 0, 255, 0, 255, 0, 255];
    for (cw, ch) in [(8, 16), (10, 20), (12, 24), (16, 32)] {
        let (cwu, chu) = (cw as usize, ch as usize);
        let mut h = Harness::new(&gpu, metrics(cw, ch), 6, 3, TextBlending::Native);
        h.cfg.bg_alpha = 0.0;
        h.snap.rows = vec![
            text_row("ab    ", Style::default()),
            text_row("      ", Style::default()),
            text_row("      ", Style::default()),
        ];
        h.snap.changed = vec![0, 1, 2];
        {
            let ctx = BuildCtx {
                cfg: &h.cfg,
                palette: &h.pal,
                metrics: &h.m,
                highlights: &h.hl,
            };
            h.scene
                .update(&h.snap, Damage::Full, &ctx, false, &mut h.synth);
        }
        h.renderer
            .upload_mask(0, 0, 0, PAGE, PAGE, h.synth.atlas.mask_page(0));
        // Under cells 0-1 of row 0; over cell 4 of row 1; a 2x1 pair 1:1 at row 2, col 1.
        let below = [ImageQuad {
            key: 1,
            width: 4,
            height: 4,
            rgba: &red,
            dst: [0, 0, 2 * cw, ch],
            src: [0, 0, 4, 4],
        }];
        let above = [
            ImageQuad {
                key: 2,
                width: 4,
                height: 4,
                rgba: &blue,
                dst: [4 * cw, ch, cw, ch],
                src: [0, 0, 4, 4],
            },
            ImageQuad {
                key: 3,
                width: 2,
                height: 1,
                rgba: &pair,
                dst: [cw, 2 * ch, 2, 1],
                src: [0, 0, 2, 1],
            },
        ];
        let panes = [PaneDraw {
            backdrop: Some([0.1, 0.1, 0.1, 1.0]),
            images_below: &below,
            images_above: &above,
            ..PaneDraw::single(&h.scene, None)
        }];
        let input = FrameInput {
            panes: &panes,
            clear: [0.0; 4],
        };
        match h.pres.present(&mut h.renderer, &input) {
            PresentOutcome::Presented(_) => {}
            other => panic!("offscreen present failed: {other:?}"),
        }
        let got = h.pres.read_pixels();
        dump(&format!("images-{cw}x{ch}"), &got);
        let what = |s: &str| format!("{s} at cell {cw}x{ch}");
        let bd = [26, 26, 26, 255];
        // The image under the text shows beside the 6-px synthetic glyph box...
        near(
            at(&got, cwu + 7, 0),
            [255, 0, 0, 255],
            1,
            &what("under, no glyph"),
        );
        // ...the text draws over it...
        let fg = h.pal.fg;
        let edge = (0..chu)
            .find(|&y| at(&got, 1, y) != [255, 0, 0, 255])
            .expect("a glyph column over the image");
        near(
            at(&got, 1, edge),
            [fg.0, fg.1, fg.2, 255],
            2,
            &what("text over image"),
        );
        // ...and its edges land on the cell grid.
        near(
            at(&got, 2 * cwu - 1, chu - 1),
            [255, 0, 0, 255],
            1,
            &what("under, last px"),
        );
        near(
            at(&got, 2 * cwu, chu - 1),
            bd,
            1,
            &what("backdrop right of under"),
        );
        near(at(&got, 0, chu), bd, 1, &what("backdrop below under"));
        // Over the text: half blue over the backdrop, exactly on its cell.
        let over = [13, 13, 141, 255];
        near(at(&got, 4 * cwu, chu), over, 2, &what("over, first px"));
        near(
            at(&got, 5 * cwu - 1, 2 * chu - 1),
            over,
            2,
            &what("over, last px"),
        );
        near(at(&got, 4 * cwu - 1, chu), bd, 1, &what("left of over"));
        near(at(&got, 5 * cwu, chu), bd, 1, &what("right of over"));
        // 1:1 samples nearest: two exact pixels.
        assert_eq!(
            at(&got, cwu, 2 * chu),
            [255, 0, 0, 255],
            "{}",
            what("1:1 px 0")
        );
        assert_eq!(
            at(&got, cwu + 1, 2 * chu),
            [0, 255, 0, 255],
            "{}",
            what("1:1 px 1")
        );
        near(at(&got, cwu + 2, 2 * chu), bd, 1, &what("after 1:1"));
    }
}
