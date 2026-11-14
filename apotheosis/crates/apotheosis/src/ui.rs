// SPDX-License-Identifier: AGPL-3.0-only
//! The glass interface bridge (T-08 §2, T-07 §4 pass 7): lowers an `apoth-ui` [`DrawList`]
//! to the renderer's [`ChromeList`], with text shaped and rasterised through `apoth-font`
//! (the T-06 path) and emitted as flat coverage rects. Also the live overlay surface: the
//! error bar and the restart notice (T-10 §4), laid out and drawn on the render thread.
//!
//! Nothing here logs text. Program-supplied text arrives already sanitised by
//! `TextRun::program`.

use std::cell::RefCell;

use apoth_font::config::{Hinting, Style};
use apoth_font::raster::{RasterParams, Rasterizer, SUBPIXEL_BINS};
use apoth_font::system::FontSystem;
use apoth_render::chrome::{ChromeList, ChromeRect, ChromeShadow, ChromeSink};
use apoth_ui::bar::{self, BarSpec, BarState};
use apoth_ui::text::{Measure, TextRun, Weight};
use apoth_ui::tokens::{Palette, Rgba, metrics, space};
use apoth_ui::{Cmd, DrawList, IconId, Point, Rect, WidgetId};

use crate::settings::ErrorPos;

const ELLIPSIS: char = '\u{2026}';
/// Advance of a character no face covers, in ems.
const FALLBACK_ADVANCE: f32 = 0.6;

fn bytes(c: Rgba) -> [u8; 4] {
    c.0.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

fn style_of(w: Weight) -> Style {
    match w {
        Weight::Regular => Style::Regular,
        Weight::Strong => Style::Bold,
    }
}

/// Logical advance of `c` at `size` (independent of the output scale).
fn advance(fonts: &mut FontSystem, style: Style, c: char, size: f32) -> f32 {
    let mut b = [0u8; 4];
    let r = fonts.resolve(style, c.encode_utf8(&mut b));
    let Some(inst) = r.face else {
        return size * FALLBACK_ADVANCE;
    };
    let face = inst.face();
    face.glyph_for(c as u32)
        .and_then(|g| face.advance_units(g))
        .map_or(size * FALLBACK_ADVANCE, |u| {
            u / f32::from(face.units_per_em().max(1)) * size
        })
}

/// The characters of `run` with their logical x offsets, truncated with an ellipsis at
/// `max_width`; and the total width.
fn place(fonts: &mut FontSystem, run: &TextRun) -> (Vec<(char, f32)>, f32) {
    let style = style_of(run.weight);
    let adv: Vec<(char, f32)> = run
        .text
        .chars()
        .map(|c| (c, advance(fonts, style, c, run.size)))
        .collect();
    let total: f32 = adv.iter().map(|a| a.1).sum();
    let mut out = Vec::with_capacity(adv.len() + 1);
    match run.max_width {
        Some(m) if total > m => {
            let ell = advance(fonts, style, ELLIPSIS, run.size);
            let mut x = 0.0;
            for (c, a) in adv {
                if x + a + ell > m {
                    break;
                }
                out.push((c, x));
                x += a;
            }
            if x + ell <= m {
                out.push((ELLIPSIS, x));
                x += ell;
            }
            (out, x)
        }
        _ => {
            let mut x = 0.0;
            for (c, a) in adv {
                out.push((c, x));
                x += a;
            }
            (out, total)
        }
    }
}

/// `apoth-ui` layout measures text through this.
pub(crate) struct FontMeasure<'a>(RefCell<&'a mut FontSystem>);

impl<'a> FontMeasure<'a> {
    pub(crate) fn new(f: &'a mut FontSystem) -> Self {
        FontMeasure(RefCell::new(f))
    }
}

impl Measure for FontMeasure<'_> {
    fn width(&self, run: &TextRun) -> f32 {
        place(&mut self.0.borrow_mut(), run).1
    }
}

/// Rasterises text runs; owns the swash context.
pub(crate) struct UiRaster {
    rast: Rasterizer,
}

impl UiRaster {
    pub(crate) fn new() -> UiRaster {
        UiRaster {
            rast: Rasterizer::new(),
        }
    }

    /// Draws `run` with its line box's top-left at `pos` (logical) in `colour`, as flat
    /// coverage rects (one per horizontal run of equal coverage).
    fn text<S: ChromeSink>(
        &mut self,
        fonts: &mut FontSystem,
        run: &TextRun,
        pos: Point,
        colour: Rgba,
        scale: f32,
        sink: &mut S,
    ) {
        let px = run.size * scale;
        let Some(m) = fonts.cell_metrics(px) else {
            return;
        };
        let box_h = run.size * scale;
        let baseline =
            (pos.y * scale + (box_h - m.height as f32) / 2.0 + m.ascent_from_top() as f32).round()
                as i32;
        let style = style_of(run.weight);
        let base = bytes(colour);
        let (glyphs, _) = place(fonts, run);
        for (c, x) in glyphs {
            if c.is_whitespace() {
                continue;
            }
            let mut b = [0u8; 4];
            let r = fonts.resolve(style, c.encode_utf8(&mut b));
            let Some(inst) = r.face else { continue };
            let Some(gid) = inst.face().glyph_for(c as u32) else {
                continue;
            };
            let pen = (pos.x + x) * scale;
            let ix = pen.floor();
            let bin = (((pen - ix) * f32::from(SUBPIXEL_BINS)) as u8).min(SUBPIXEL_BINS - 1);
            let params = RasterParams {
                px_size: px,
                hinting: Hinting::None,
                subpixel_bin: bin,
                thicken_pct: 0.0,
            };
            let Some(bm) = self.rast.gray(&inst, gid, &params) else {
                continue;
            };
            let (ox, oy) = (ix as i32 + bm.left, baseline - bm.top);
            for row in 0..bm.height as usize {
                let line = &bm.data[row * bm.width as usize..(row + 1) * bm.width as usize];
                let mut col = 0;
                while col < line.len() {
                    let cov = line[col];
                    let start = col;
                    while col < line.len() && line[col] == cov {
                        col += 1;
                    }
                    if cov == 0 {
                        continue;
                    }
                    let a = (u32::from(base[3]) * u32::from(cov) + 127) / 255;
                    sink.flat(
                        [
                            (ox + start as i32) as f32,
                            (oy + row as i32) as f32,
                            (col - start) as f32,
                            1.0,
                        ],
                        [base[0], base[1], base[2], a as u8],
                    );
                }
            }
        }
    }

    /// Lowers the whole list at output `scale` (physical px per logical px).
    pub(crate) fn lower<S: ChromeSink>(
        &mut self,
        list: &DrawList,
        scale: f32,
        fonts: &mut FontSystem,
        sink: &mut S,
    ) {
        let sc = |r: Rect| [r.x * scale, r.y * scale, r.w * scale, r.h * scale];
        for cmd in &list.cmds {
            match cmd {
                Cmd::RoundedRect {
                    rect,
                    radius,
                    fill,
                    rim,
                } => sink.rounded_rect(&ChromeRect {
                    rect: sc(*rect),
                    radii: [radius * scale; 4],
                    fill: bytes(*fill),
                    rim: rim.map_or([0; 4], |r| bytes(r.colour)),
                    rim_width: rim.map_or(0.0, |r| r.width * scale),
                }),
                Cmd::Shadow {
                    rect,
                    radius,
                    spread,
                    alpha,
                } => sink.shadow(&ChromeShadow {
                    rect: sc(*rect),
                    radii: [radius * scale; 4],
                    spread: spread * scale,
                    alpha: *alpha,
                }),
                Cmd::PushClip { rect, .. } => sink.push_clip(sc(*rect)),
                Cmd::PopClip => sink.pop_clip(),
                Cmd::Text { run, pos, colour } => {
                    self.text(fonts, run, *pos, *colour, scale, sink);
                }
                Cmd::Icon { icon, rect, colour } => {
                    let (glyph, weight) = match icon {
                        IconId::Close => ("\u{00d7}", Weight::Regular),
                        IconId::Plus => ("+", Weight::Regular),
                        IconId::Warning => ("!", Weight::Strong),
                        IconId::Shield => ("i", Weight::Strong),
                        IconId::ArrowUp => ("\u{2191}", Weight::Regular),
                        IconId::ArrowDown => ("\u{2193}", Weight::Regular),
                    };
                    let mut run = TextRun::interface(glyph, rect.h * 0.9);
                    run.weight = weight;
                    let w = place(fonts, &run).1;
                    let pos = Point::new(rect.x + (rect.w - w) / 2.0, rect.y + rect.h * 0.05);
                    self.text(fonts, &run, pos, *colour, scale, sink);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The overlay surface: error bar and restart notice.

/// What the render thread draws over the top of the window.
#[derive(Clone, Debug)]
pub(crate) struct Overlay {
    pub(crate) spec: BarSpec,
    /// When the bar appeared: its first 500 ms draw the buttons as inert (the input guard).
    pub(crate) shown_at: std::time::Instant,
}

impl Overlay {
    /// A bar takes room at the top of the window; a sheet (a spec with a body) floats over it.
    pub(crate) fn takes_room(&self) -> bool {
        self.spec.body.is_none()
    }
}

/// Where the search sheet may sit, in interface units: below the overlay bar and the strip.
pub(crate) fn search_area(window: Rect, strip: Option<Rect>, bar_room: bool) -> Rect {
    let inset = if bar_room {
        metrics::BAR_H + space::SM
    } else {
        0.0
    };
    apoth_ui::searchbar::free_area(window, strip, inset)
}

/// Physical pixels the bar takes from the top of the pane (it never overlaps the grid, so a
/// program cannot draw under or imitate it, T-08 §5).
pub(crate) fn bar_inset_px(scale: f64, zoom: f32) -> u32 {
    let s = scale * f64::from(zoom);
    (f64::from(metrics::BAR_H + space::SM) * s).round() as u32
}

/// Widget id of the tab strip (its new-tab button is `STRIP_ID.child(u64::MAX)`).
pub(crate) const STRIP_ID: WidgetId = WidgetId(0x5354_5249_5000);
/// Widget id of the command palette sheet (rows are `palette::row_id`).
pub(crate) const PALETTE_ID: WidgetId = WidgetId(0x5041_4c45_5400);

/// Widget id of tab `tab` in the strip (its close button is `.child(0)`).
pub(crate) fn tab_widget(tab: u64) -> WidgetId {
    WidgetId(0x7441_0000_0000_0000 ^ tab)
}

/// Widget id of pane `pane`'s frame.
pub(crate) fn pane_widget(pane: u64) -> WidgetId {
    WidgetId(0x7041_0000_0000_0000 ^ pane)
}

/// Lowers the chrome into `chrome` for a `buffer` of physical pixels at interface scale
/// `ui` (physical pixels per interface unit). Back to front: pane frames (focus rim, inactive
/// dim), tab strip, overlay bar, palette sheet (T-08 §5 order).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_chrome(
    chrome: &mut ChromeList,
    overlay: Option<&Overlay>,
    model: &crate::render::ChromeModel,
    buffer: (u32, u32),
    ui: f32,
    light: bool,
    age_ms: u64,
    fonts: &mut FontSystem,
    raster: &mut UiRaster,
) {
    chrome.clear();
    let s = if ui > 0.0 { ui } else { 1.0 };
    let area = Rect::new(0.0, 0.0, buffer.0 as f32 / s, buffer.1 as f32 / s);
    let pal = if light {
        Palette::light()
    } else {
        Palette::dark()
    };
    let mut dl = DrawList::new();
    {
        let measure = FontMeasure::new(fonts);
        for f in &model.frames {
            let l = apoth_ui::pane::layout(f);
            apoth_ui::pane::draw(f, &l, &pal, &mut dl);
        }
        if let Some(r) = model.bell {
            apoth_ui::pane::draw_bell(r, &pal, &mut dl);
        }
        if let Some((strip, tabs)) = &model.strip {
            let l = apoth_ui::tabs::layout(*strip, STRIP_ID, tabs);
            apoth_ui::tabs::draw(&l, tabs, model.tab_style, &model.ui, &pal, &mut dl);
        }
        if let Some(o) = overlay.filter(|o| o.takes_room()) {
            let l = bar::layout(area, &o.spec, &apoth_ui::text::geometry());
            bar::draw(
                &l,
                &o.spec,
                &BarState { shown_at_ms: 0 },
                age_ms,
                &model.ui,
                &pal,
                &mut dl,
            );
        }
        if let Some(sp) = &model.search {
            let free = search_area(
                area,
                model.strip.as_ref().map(|s| s.0),
                overlay.is_some_and(Overlay::takes_room),
            );
            let l = apoth_ui::searchbar::layout(free, sp, &apoth_ui::text::geometry());
            apoth_ui::searchbar::draw(&l, sp, &model.ui, &pal, &measure, &mut dl);
        }
        if let Some(t) = &model.tooltip {
            let r = apoth_ui::tooltip::layout(area, t, &measure);
            apoth_ui::tooltip::draw(r, t, &pal, &mut dl);
        }
        if let Some(p) = &model.palette {
            let l = apoth_ui::palette::layout(area, p);
            apoth_ui::palette::draw(&l, p, &model.ui, &pal, &measure, &mut dl);
        }
        if let Some(o) = overlay.filter(|o| !o.takes_room()) {
            let l = bar::layout(area, &o.spec, &apoth_ui::text::geometry());
            bar::draw(
                &l,
                &o.spec,
                &BarState { shown_at_ms: 0 },
                age_ms,
                &model.ui,
                &pal,
                &mut dl,
            );
        }
    }
    if dl.cmds.is_empty() {
        return;
    }
    raster.lower(&dl, s, fonts, chrome);
}

/// The bar a window shows for the current configuration errors, or `None` when the file is
/// good (T-10 §4: the error bar is in every window).
pub(crate) fn error_bar(id: WidgetId, errors: &[ErrorPos]) -> Option<BarSpec> {
    let first = errors.first()?;
    let file = first
        .file
        .as_ref()
        .and_then(|p| p.file_name())
        .map_or_else(|| "config".to_owned(), |n| n.to_string_lossy().into_owned());
    let mut msg = if first.message.is_empty() {
        "the file could not be read".to_owned()
    } else {
        first.message.clone()
    };
    if errors.len() > 1 {
        msg.push_str(&format!(" (+{} more)", errors.len() - 1));
    }
    let mut spec = BarSpec::config_error(id, &file, u32::try_from(first.line).unwrap_or(0), &msg);
    // Keyboard answers are limited to Escape: a letter key must never be taken from the
    // shell. "Open config" needs a pointer or chord path (not wired yet), so it is not drawn.
    spec.buttons.retain(|b| b.key == apoth_ui::bar::Key::Escape);
    spec.escape = Some(0);
    Some(spec)
}

/// The bar for each of `windows`: the error bar wins over the restart notice.
pub(crate) fn bars_for(
    windows: &[WidgetId],
    errors: &[ErrorPos],
    restart: &[&'static str],
) -> Vec<(WidgetId, Option<BarSpec>)> {
    windows
        .iter()
        .map(|&w| {
            let spec = error_bar(w, errors).or_else(|| BarSpec::restart_required(w, restart));
            (w, spec)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use apoth_font::config::FontConfig;
    use apoth_font::discovery::FontIndex;
    use apoth_font::system::EmbeddedFont;
    use apoth_render::reference::Image;
    use apoth_ui::UiState;
    use std::path::PathBuf;
    use std::sync::Arc;

    const FONT: &[u8] = include_bytes!("../../apoth-font/tests/fixtures/FiraMono-Medium.ttf");

    fn fonts() -> FontSystem {
        let e = EmbeddedFont {
            name: "FiraMono-Medium.ttf".into(),
            data: Arc::from(FONT),
        };
        FontSystem::new(FontIndex::default(), vec![e], FontConfig::default())
    }

    fn err(line: usize, msg: &str) -> ErrorPos {
        ErrorPos {
            file: Some(PathBuf::from("/home/u/.config/apoth/apotheosis.kdl")),
            line,
            column: 3,
            key: Some("font.size".into()),
            message: msg.into(),
        }
    }

    #[test]
    fn a_broken_file_yields_a_bar_per_window() {
        let wins = [WidgetId(1), WidgetId(2), WidgetId(3)];
        let bars = bars_for(&wins, &[err(7, "expected a number"), err(9, "x")], &[]);
        assert_eq!(bars.len(), 3);
        for ((id, b), w) in bars.iter().zip(wins) {
            assert_eq!(*id, w);
            let b = b.as_ref().expect("every window shows the bar");
            assert_eq!(b.id, w);
            assert!(b.headline.text.contains("apotheosis.kdl:7"));
            assert!(b.detail.as_ref().unwrap().text.contains("(+1 more)"));
            assert_eq!(b.buttons.len(), 1, "only the Escape answer is live");
        }
        // A good file shows nothing; pending restarts show the notice instead.
        assert!(bars_for(&wins, &[], &[]).iter().all(|(_, b)| b.is_none()));
        let n = bars_for(&wins, &[], &["window.decorations"]);
        assert!(n.iter().all(|(_, b)| b.is_some()));
        // An error outranks the restart notice.
        let e = bars_for(&wins[..1], &[err(1, "bad")], &["window.decorations"]);
        assert!(
            e[0].1
                .as_ref()
                .unwrap()
                .headline
                .text
                .contains("Config error")
        );
    }

    #[test]
    fn program_text_in_the_bar_is_sanitised() {
        let b = error_bar(WidgetId(1), &[err(1, "bad\u{1b}[31m\u{202e}value")]).unwrap();
        let d = &b.detail.unwrap().text;
        assert!(!d.contains('\u{1b}') && !d.contains('\u{202e}'));
    }

    fn lowered(scale: f32, light: bool) -> ChromeList {
        let mut f = fonts();
        let mut r = UiRaster::new();
        let mut l = ChromeList::new();
        let o = Overlay {
            spec: error_bar(WidgetId(1), &[err(12, "unknown key font.sizee")]).unwrap(),
            shown_at: std::time::Instant::now(),
        };
        let buf = (480.0 * scale, 90.0 * scale);
        build_chrome(
            &mut l,
            Some(&o),
            &crate::render::ChromeModel::empty(),
            (buf.0 as u32, buf.1 as u32),
            scale,
            light,
            900,
            &mut f,
            &mut r,
        );
        l
    }

    /// Two tabs, two framed panes (the right one unfocused, so dimmed) and the palette open
    /// over them with three rows, the second selected.
    fn chrome_model(scale: f32) -> (crate::render::ChromeModel, (u32, u32)) {
        use apoth_ui::palette::{Entry, PaletteSpec};
        use apoth_ui::pane::{FrameMode, PaneSpec};
        use apoth_ui::tabs::TabSpec;
        let (w, h) = (640.0, 400.0);
        let strip = Rect::new(0.0, 0.0, w, crate::layout::strip_px(1.0) as f32);
        let tabs = vec![
            TabSpec {
                id: tab_widget(1),
                title: TextRun::program("zsh", apoth_ui::tokens::size::BODY),
                active: true,
                badges: Vec::new(),
                profile: None,
            },
            TabSpec {
                id: tab_widget(2),
                title: TextRun::program("htop", apoth_ui::tokens::size::BODY),
                active: false,
                badges: Vec::new(),
                profile: None,
            },
        ];
        let top = strip.bottom() + 4.0;
        let frame = |id, x: f32, focused| PaneSpec {
            id: pane_widget(id),
            rect: Rect::new(x, top, w / 2.0 - 8.0, h - top - 4.0),
            focused,
            title: None,
            mode: FrameMode::Full,
            inactive_dim: 0.12,
        };
        let row = |t: &str, k: Option<&str>| Entry {
            title: TextRun::interface(t, apoth_ui::tokens::size::BODY),
            hint: k.map(|k| TextRun::interface(k, apoth_ui::tokens::size::BODY_SMALL)),
        };
        let model = crate::render::ChromeModel {
            strip: Some((strip, tabs)),
            tab_style: apoth_ui::frame::TabStyle::Floating,
            frames: vec![frame(10, 4.0, true), frame(11, w / 2.0 + 4.0, false)],
            palette: Some(PaletteSpec {
                id: PALETTE_ID,
                query: "spl".into(),
                placeholder: TextRun::interface("Run an action", apoth_ui::tokens::size::BODY),
                entries: vec![
                    row("Split pane", Some("ctrl+shift+d")),
                    row("Split pane down", Some("ctrl+shift+e")),
                    row("Select all", Some("ctrl+shift+a")),
                ],
                selected: Some(1),
                total: 3,
            }),
            search: None,
            tooltip: None,
            bell: None,
            ui: UiState::default(),
        };
        (model, ((w * scale) as u32, (h * scale) as u32))
    }

    fn lowered_chrome(scale: f32, light: bool) -> ChromeList {
        let mut f = fonts();
        let mut r = UiRaster::new();
        let mut l = ChromeList::new();
        let (m, buf) = chrome_model(scale);
        build_chrome(&mut l, None, &m, buf, scale, light, 0, &mut f, &mut r);
        l
    }

    #[test]
    fn chrome_draws_frames_strip_and_palette_in_order() {
        let l = lowered_chrome(1.0, false);
        let rects: Vec<_> = l.instances.iter().filter(|i| i.params[2] < 0.5).collect();
        // Two frame rims, one dim, two tab backgrounds, new-tab, sheet, field, caret, row.
        assert!(rects.len() >= 9, "{}", rects.len());
        // The palette sheet comes after (in front of) the frames.
        let sheet_w = (apoth_ui::tokens::metrics::PALETTE_W).min(640.0 - 2.0 * 24.0);
        let sheet = rects
            .iter()
            .position(|i| (i.rect[2] - sheet_w).abs() < 1.0)
            .expect("palette sheet");
        let frame = rects
            .iter()
            .position(|i| (i.rect[2] - (320.0 - 8.0)).abs() < 1.0)
            .expect("pane frame");
        assert!(frame < sheet);
        // Nothing is drawn for an empty model.
        let mut f = fonts();
        let mut out = ChromeList::new();
        build_chrome(
            &mut out,
            None,
            &crate::render::ChromeModel::empty(),
            (100, 100),
            1.0,
            false,
            0,
            &mut f,
            &mut UiRaster::new(),
        );
        assert!(out.instances.is_empty());
    }

    #[test]
    fn lowering_scales_geometry_and_draws_text() {
        let (a, b) = (lowered(1.0, false), lowered(2.0, false));
        assert!(!a.instances.is_empty());
        // Same rounded-rect count at both scales; geometry doubles.
        let rects = |l: &ChromeList| l.instances.iter().filter(|i| i.params[2] < 0.5).count();
        assert_eq!(rects(&a), rects(&b));
        assert!(rects(&a) >= 3, "bar, button, keycap");
        let (r1, r2) = (&a.instances[0], &b.instances[0]);
        assert!((r2.rect[2] - 2.0 * r1.rect[2]).abs() < 0.01);
        assert!(
            (r2.params[0] - 2.0 * r1.params[0]).abs() < 0.01,
            "rim width scales"
        );
        let flats = |l: &ChromeList| l.instances.iter().filter(|i| i.params[2] > 1.5).count();
        assert!(
            flats(&a) > 100,
            "text reached the glyph path: {}",
            flats(&a)
        );
        assert!(flats(&b) > flats(&a));
    }

    #[test]
    fn long_program_text_is_truncated_with_an_ellipsis() {
        let mut f = fonts();
        let run = TextRun::program(&"w".repeat(80), 12.0).clipped(100.0);
        let (g, w) = place(&mut f, &run);
        assert!(w <= 100.0 + 0.01);
        assert_eq!(g.last().unwrap().0, ELLIPSIS);
    }

    #[test]
    fn inset_matches_the_layout_split() {
        assert_eq!(bar_inset_px(1.0, 1.0), 48);
        assert_eq!(bar_inset_px(2.0, 1.0), 96);
        assert_eq!(bar_inset_px(1.0, 2.0), 96);
    }

    // ---- goldens: surface x theme x scale ---------------------------------------------------
    //
    // The golden is a text digest of the lowered list. It is produced by this crate's own
    // lowering and by the CPU reference of `apoth-render`; neither is an independent visual
    // oracle, so the goldens catch drift, not STYLE.md conformance. The PNGs written with
    // `APOTH_SHOT=<dir>` are what a human compares with STYLE.md.

    fn digest(l: &ChromeList) -> String {
        let mut s = String::new();
        let (mut n, mut h) = (0usize, 0xcbf2_9ce4_8422_2325_u64);
        for i in &l.instances {
            if i.params[2] > 1.5 {
                n += 1;
                let mut buf = Vec::new();
                i.pack(&mut buf);
                buf.extend_from_slice(&i.fill);
                for b in buf {
                    h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
                }
            } else {
                s += &format!(
                    "rect {:?} radii {:?} params {:?} fill {:?} rim {:?}\n",
                    i.rect, i.radii, i.params, i.fill, i.rim
                );
            }
        }
        s + &format!("glyph-rects {n} fnv {h:016x}\n")
    }

    #[test]
    fn overlay_goldens_dark_light_scale_1_2() {
        let mut out = String::new();
        for light in [false, true] {
            for scale in [1.0_f32, 2.0] {
                out += &format!("== error-bar light={light} scale={scale}\n");
                out += &digest(&lowered(scale, light));
            }
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("golden/overlay.txt");
        if std::env::var_os("APOTH_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &out).unwrap();
            return;
        }
        let want =
            std::fs::read_to_string(&path).expect("golden/overlay.txt: bless with APOTH_BLESS=1");
        assert_eq!(want, out, "overlay golden drifted");
    }

    #[test]
    fn chrome_goldens_dark_light_scale_1_2() {
        let mut out = String::new();
        for light in [false, true] {
            for scale in [1.0_f32, 2.0] {
                out += &format!("== tabs+frames+palette light={light} scale={scale}\n");
                out += &digest(&lowered_chrome(scale, light));
            }
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("golden/chrome.txt");
        if std::env::var_os("APOTH_BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &out).unwrap();
            return;
        }
        let want =
            std::fs::read_to_string(&path).expect("golden/chrome.txt: bless with APOTH_BLESS=1");
        assert_eq!(want, out, "chrome golden drifted");
    }

    /// Writes PNGs of the overlay over a flat stand-in ground when `APOTH_SHOT=<dir>` is set.
    /// The ground is NOT a compositor blur; it only gives the translucent fills something to
    /// sit on.
    #[test]
    fn write_screenshots_when_asked() {
        let Some(dir) = std::env::var_os("APOTH_SHOT").map(PathBuf::from) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        for light in [false, true] {
            for scale in [1.0_f32, 2.0] {
                let l = lowered(scale, light);
                let (w, h) = ((480.0 * scale) as usize, (90.0 * scale) as usize);
                let ground: [u8; 3] = if light {
                    [0xcf, 0xd4, 0xdb]
                } else {
                    [0x24, 0x2a, 0x33]
                };
                let mut img = Image::new(w, h);
                apoth_render::chrome::reference::render(&l, &mut img);
                let mut rgb = Vec::with_capacity(w * h * 3);
                for p in img.data.chunks_exact(4) {
                    for c in 0..3 {
                        let g = u32::from(ground[c]);
                        rgb.push(
                            (u32::from(p[c]) + g * (255 - u32::from(p[3])) / 255).min(255) as u8,
                        );
                    }
                }
                let name = format!(
                    "error-bar-{}-{}x.png",
                    if light { "light" } else { "dark" },
                    scale as u32
                );
                std::fs::write(dir.join(name), png_rgb(w, h, &rgb)).unwrap();
                let l = lowered_chrome(scale, light);
                let (w, h) = ((640.0 * scale) as usize, (400.0 * scale) as usize);
                let mut img = Image::new(w, h);
                apoth_render::chrome::reference::render(&l, &mut img);
                let mut rgb = Vec::with_capacity(w * h * 3);
                for p in img.data.chunks_exact(4) {
                    for c in 0..3 {
                        let g = u32::from(ground[c]);
                        rgb.push(
                            (u32::from(p[c]) + g * (255 - u32::from(p[3])) / 255).min(255) as u8,
                        );
                    }
                }
                let name = format!(
                    "chrome-{}-{}x.png",
                    if light { "light" } else { "dark" },
                    scale as u32
                );
                std::fs::write(dir.join(name), png_rgb(w, h, &rgb)).unwrap();
            }
        }
    }

    fn png_rgb(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = 0xffff_ffff_u32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend_from_slice(data);
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc(&body).to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in rgb.chunks_exact(w * 3) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let mut z = vec![0x78, 0x01];
        let mut chunks = raw.chunks(65535).peekable();
        while let Some(c) = chunks.next() {
            z.push(u8::from(chunks.peek().is_none()));
            z.extend_from_slice(&(c.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
            z.extend_from_slice(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}
