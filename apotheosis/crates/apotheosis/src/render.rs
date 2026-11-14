// SPDX-License-Identifier: AGPL-3.0-only
//! The render thread, one per window (T-00 §4.2, T-07 §1-§3). It sleeps on a channel until
//! poked, so a static screen costs no wakeups and no timers beyond the one the scheduler
//! asks for. Every pane of the shown tab (T-13) is snapshotted under its own short term
//! lock, its scene built without the lock, and the window presented through the wgpu surface
//! (which commits the `wl_surface`; main never commits it). Frame callbacks and presentation
//! feedback come from the `apoth-wl` pacer. The interface chrome (tab strip, pane frames,
//! overlay bar, palette) is laid out by `apoth-ui` from the model main sends and lowered here.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use apoth_font::config::Style;
use apoth_font::system::FontSystem;
use apoth_grid::{Damage, FrameSnapshot};
use apoth_render::colour::{Palette, Rgb, TextBlending};
use apoth_render::cursor::{self, CursorScene, CursorShape, CursorState};
use apoth_render::gpu::{GpuOptions, RenderError};
use apoth_render::image::ImageQuad;
use apoth_render::presenter::{PresentOutcome, Presenter, SurfaceOptions, SurfacePresenter};
use apoth_render::renderer::{FrameInput, PaneDraw, Renderer, RendererConfig};
use apoth_render::scene::{BuildCtx, GridScene, Highlight, SceneConfig};
use apoth_render::sched::{Pacing, Poll, SchedConfig, Scheduler};
use apoth_render::surface::{AlphaRequest, FaultAction, fault_action};
use apoth_term::{CursorInfo, ImageDraw};
use apoth_ui::frame::TabStyle;
use apoth_ui::motion::{Fader, MotionMode};
use apoth_ui::palette::PaletteSpec;
use apoth_ui::tabs::TabSpec;
use apoth_ui::{Rect, UiState};
use apoth_wl::pace::CommitCtl;
use apoth_wl::{PaceEvent, Pacer, WlSurfaceHandle};

use crate::glyphs::{COLOR_PAGES, Glyphs, MASK_PAGES, PAGE};
use crate::hook::{ImageDump, ViewDump};
use crate::pane::{MainMsg, PaneKey, PaneShared, RenderMsg, now_ns};
use crate::ui::{Overlay, UiRaster, build_chrome};
use crate::view::{Source, View, paint};

/// Configured default colours. The term's own values win once a program has changed them
/// (OSC 4/10/11/12): see [`pick`].
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColorCfg {
    pub(crate) fg: (u8, u8, u8),
    pub(crate) bg: (u8, u8, u8),
    pub(crate) cursor: (u8, u8, u8),
    pub(crate) ansi: Option<[(u8, u8, u8); 16]>,
    /// Glyph under a block cursor; `None` uses the cell background.
    pub(crate) cursor_text: Option<(u8, u8, u8)>,
    /// Selection foreground (`None`: the cell's own) and background.
    pub(crate) selection: crate::settings::SelectionColors,
}

/// The colours the `Term` was created with: a value equal to these has not been changed by the
/// running program, so a configured colour replaces it.
#[derive(Clone, Debug)]
pub(crate) struct TermDefaults {
    pub(crate) fg: (u8, u8, u8),
    pub(crate) bg: (u8, u8, u8),
    pub(crate) cursor: (u8, u8, u8),
    pub(crate) ansi: [(u8, u8, u8); 16],
}

/// The colour in force: the configured one while the term still holds its startup value, the
/// term's own once a program has set it.
pub(crate) fn pick(term: (u8, u8, u8), startup: (u8, u8, u8), cfg: (u8, u8, u8)) -> (u8, u8, u8) {
    if term == startup { cfg } else { term }
}

/// A reload for the render thread.
#[derive(Debug)]
pub(crate) struct RenderUpdate {
    pub(crate) settings: RenderSettings,
    pub(crate) font: Option<apoth_font::config::FontConfig>,
}

/// Settings the render thread needs.
#[derive(Clone, Debug)]
pub(crate) struct RenderSettings {
    pub(crate) colors: ColorCfg,
    pub(crate) padding: (u32, u32),
    pub(crate) opacity: f32,
    pub(crate) blending: TextBlending,
    pub(crate) bold_bright: bool,
    pub(crate) allow_software: bool,
    pub(crate) echo_window: Duration,
    pub(crate) sync_timeout: Duration,
    pub(crate) cursor_shape: CursorShape,
    pub(crate) unfocused_cursor_hidden: bool,
    /// `window.alpha-mode`.
    pub(crate) alpha: AlphaRequest,
    /// Interface zoom (`interface.font-size` and `interface.scale`).
    pub(crate) ui_zoom: f32,
    /// `interface.font`.
    pub(crate) interface_font: Option<String>,
    /// The terminal background is light: the interface uses the light palette.
    pub(crate) light: bool,
    /// `interface.motion`: the interface's fades and springs.
    pub(crate) motion: MotionMode,
}

impl RenderSettings {
    /// `blur` is the compositor's blur path for the window: it decides the auto opacity.
    pub(crate) fn from_settings(
        s: &crate::settings::Settings,
        blur: Option<apoth_wl::BlurPath>,
    ) -> RenderSettings {
        let lum = ec_theme::Rgba::rgb(
            (u32::from(s.term.bg.0) << 16) | (u32::from(s.term.bg.1) << 8) | u32::from(s.term.bg.2),
        )
        .luminance();
        RenderSettings {
            alpha: s.glass.alpha,
            ui_zoom: s.glass.interface_zoom,
            interface_font: s.glass.interface_font.clone(),
            motion: s.glass.motion,
            light: lum >= ec_theme::SCHEME_LUMINANCE,
            colors: ColorCfg {
                fg: s.term.fg,
                bg: s.term.bg,
                cursor: s.term.cursor_color,
                ansi: s.ansi,
                cursor_text: s.cursor_text,
                selection: s.selection,
            },
            padding: s.padding,
            opacity: crate::settings::effective_opacity(s.opacity, &s.glass, blur),
            blending: s.blending,
            bold_bright: s.bold_bright,
            allow_software: s.allow_software,
            echo_window: s.echo_window,
            sync_timeout: s.sync_timeout,
            cursor_shape: s.cursor_shape,
            unfocused_cursor_hidden: s.unfocused_cursor_hidden,
        }
    }
}

fn rgb_of(c: ec_theme::Rgba) -> Rgb {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Rgb(b(c.r()), b(c.g()), b(c.b()))
}

/// `tint` (straight alpha) over the opaque pane background.
fn over_bg(tint: apoth_ui::tokens::Rgba, bg: Rgb) -> Rgb {
    let ground = ec_theme::Rgba::new(
        f32::from(bg.0) / 255.0,
        f32::from(bg.1) / 255.0,
        f32::from(bg.2) / 255.0,
        1.0,
    );
    rgb_of(ec_theme::Rgba(tint.0).over_opaque(ground))
}

/// Whichever of the theme's two inks reads better on `bg` (at least 4.5:1 against every
/// background built below, checked by a test).
fn ink_on(bg: Rgb, text: apoth_ui::tokens::Rgba, base: apoth_ui::tokens::Rgba) -> Rgb {
    let ground =
        ec_theme::Rgba::rgb((u32::from(bg.0) << 16) | (u32::from(bg.1) << 8) | u32::from(bg.2));
    let (t, b) = (
        ec_theme::contrast(ec_theme::Rgba(text.0), ground),
        ec_theme::contrast(ec_theme::Rgba(base.0), ground),
    );
    if t >= b {
        rgb_of(ec_theme::Rgba(text.0))
    } else {
        rgb_of(ec_theme::Rgba(base.0))
    }
}

/// Search, hint and copy-mode colours (T-08, T-14), derived from the theme palette over the
/// pane's own background so they read on light and dark alike. Matches are a quiet accent
/// tint; the current match and hint labels are the solid accent with its on-accent ink; the
/// copy-mode cursor is the theme's ink (never the accent, since it sits on the current
/// match). Highlighted text is re-inked from the theme so it keeps 4.5:1 whatever its
/// own colour was.
pub(crate) fn hl_colours(
    light: bool,
    bg: Rgb,
    sel_bg: Rgb,
    sel_fg: Option<Rgb>,
) -> crate::view::HlColours {
    let pal = if light {
        apoth_ui::tokens::Palette::light()
    } else {
        apoth_ui::tokens::Palette::dark()
    };
    let match_bg = over_bg(pal.accent.alpha(MATCH_TINT), bg);
    let hint_bg = over_bg(pal.accent.alpha(HINT_TINT), bg);
    let ink = |g| Some(ink_on(g, pal.text, pal.base));
    crate::view::HlColours {
        sel_bg,
        sel_fg,
        match_bg,
        match_fg: ink(match_bg),
        current_bg: rgb_of(ec_theme::Rgba(pal.accent.0)),
        current_fg: Some(rgb_of(ec_theme::Rgba(pal.on_accent.0))),
        cursor_bg: rgb_of(ec_theme::Rgba(pal.text.0)),
        cursor_fg: Some(rgb_of(ec_theme::Rgba(pal.base.0))),
        hint_bg,
        hint_fg: ink(hint_bg),
        label_bg: rgb_of(ec_theme::Rgba(pal.accent.0)),
        label_fg: Some(rgb_of(ec_theme::Rgba(pal.on_accent.0))),
    }
}

/// Accent strength of an ordinary search match, and of the span a hint label sits on.
const MATCH_TINT: f32 = 0.34;
const HINT_TINT: f32 = 0.18;

/// One pane of the shown tab as main places it.
#[derive(Clone)]
pub(crate) struct PaneView {
    pub(crate) id: PaneKey,
    pub(crate) shared: Arc<PaneShared>,
    pub(crate) defaults: TermDefaults,
    /// Top-left of its grid, physical pixels.
    pub(crate) origin: (i32, i32),
    /// The pane has the window's keyboard focus (its cursor is solid, its sync hold counts).
    pub(crate) focused: bool,
    /// The first pane of the window: it owns the `APOTH_TEST_VIEW` dump.
    pub(crate) first: bool,
    /// A float (MUX-18): the chrome layer before it holds its glass.
    pub(crate) floating: bool,
}

/// The interface chrome around the panes, in interface units (T-08 §4). Main lays the same
/// specs out for hit testing; the render thread lays them out again to draw.
#[derive(Clone, Debug)]
pub(crate) struct ChromeModel {
    pub(crate) strip: Option<(Rect, Vec<TabSpec>)>,
    pub(crate) tab_style: TabStyle,
    pub(crate) frames: Vec<apoth_ui::pane::PaneSpec>,
    pub(crate) palette: Option<PaletteSpec>,
    /// The search sheet (T-14 §6).
    pub(crate) search: Option<apoth_ui::searchbar::SearchSpec>,
    /// The hovered link's URI by the pointer (T-14 §5).
    pub(crate) tooltip: Option<apoth_ui::tooltip::TooltipSpec>,
    /// The pane the visual bell is lighting, as its frame rect (`bell.visual`).
    pub(crate) bell: Option<Rect>,
    /// A media window's focus rim and hover controls.
    pub(crate) media: Option<apoth_ui::mediawin::MediaWinSpec>,
    /// The pane frame the muted notice shows over (T-16 §3 `audio-unavailable`), from main.
    pub(crate) media_muted: Option<Rect>,
    /// The media controls, their fades and the muted notice as drawn. The render thread's
    /// own (main never sees placement geometry): it outlives main's layouts.
    pub(crate) media_fx: MediaFx,
    pub(crate) ui: UiState,
}

/// What the render thread draws for media (T-08 §4), fades applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MediaFx {
    /// Media placements in view with their state, interface units.
    pub(crate) huds: Vec<MediaHud>,
    /// A media window's hover chrome fade level (close button, hint).
    pub(crate) win_alpha: f32,
    /// The muted notice: its pane frame and fade level, while it shows or fades out.
    pub(crate) muted: Option<(Rect, f32)>,
}

/// One media placement as the interface draws its controls (T-08 §4).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MediaHud {
    /// The placement's key (its fade is tracked by it).
    pub(crate) key: u64,
    /// The placement's box, interface units.
    pub(crate) rect: Rect,
    /// The pointer is over it (a click toggles play/pause).
    pub(crate) hovered: bool,
    pub(crate) state: apoth_term::MediaPlay,
    /// Position / duration in 0..=1; `None` without a known duration.
    pub(crate) progress: Option<f32>,
    /// The controls' fade level in `0..=1`.
    pub(crate) alpha: f32,
}

/// The render thread's media fades: one per placement key, the media window's, the muted
/// notice's (with the last pane it showed over, so it can fade out after main drops it).
#[derive(Default)]
struct MediaFades {
    huds: Vec<(u64, Fader)>,
    win: Option<Fader>,
    muted: Option<(Rect, Fader)>,
}

impl MediaFades {
    /// Applies the fades at `now_ms` to `huds` (raw hover state from the snapshot), the media
    /// window's hover and the muted notice. Returns what to draw and whether a fade still runs
    /// (another frame is needed); settled, it requests none.
    fn apply(
        &mut self,
        mut huds: Vec<MediaHud>,
        win_hover: Option<bool>,
        muted: Option<Rect>,
        now_ms: u64,
        mode: MotionMode,
    ) -> (MediaFx, bool) {
        let mut running = false;
        // In a media window the whole window is the placement: its hover shows the controls.
        let win = win_hover.unwrap_or(false);
        for h in &mut huds {
            let i = match self.huds.iter().position(|(k, _)| *k == h.key) {
                Some(i) => i,
                None => {
                    self.huds.push((h.key, Fader::hidden()));
                    self.huds.len() - 1
                }
            };
            let f = &mut self.huds[i].1;
            f.set(h.hovered || win, now_ms, mode);
            h.alpha = f.level(now_ms, mode);
            running |= f.animating(now_ms, mode);
        }
        self.huds.retain(|(k, _)| huds.iter().any(|h| h.key == *k));
        let win_alpha = match win_hover {
            Some(hov) => {
                let f = self.win.get_or_insert(Fader::hidden());
                f.set(hov, now_ms, mode);
                running |= f.animating(now_ms, mode);
                f.level(now_ms, mode)
            }
            None => 0.0,
        };
        let shown = match (muted, self.muted.as_mut()) {
            (Some(r), Some((at, f))) => {
                *at = r;
                f.set(true, now_ms, mode);
                true
            }
            (Some(r), None) => {
                let mut f = Fader::hidden();
                f.set(true, now_ms, mode);
                self.muted = Some((r, f));
                true
            }
            (None, Some((_, f))) => {
                f.set(false, now_ms, mode);
                false
            }
            (None, None) => false,
        };
        let muted = self.muted.and_then(|(r, f)| {
            running |= f.animating(now_ms, mode);
            let a = f.level(now_ms, mode);
            (shown || a > 0.0).then_some((r, a))
        });
        if muted.is_none() {
            self.muted = None;
        }
        (
            MediaFx {
                huds,
                win_alpha,
                muted,
            },
            running,
        )
    }
}

/// The HUD entries of `panes` at `cell` px per cell and `ui` px per interface unit.
fn media_huds(panes: &[PaneR], cell: (u32, u32), ui: f32) -> Vec<MediaHud> {
    let ui = if ui > 0.0 { ui } else { 1.0 };
    let (cw, ch) = (cell.0.max(1) as f32, cell.1.max(1) as f32);
    let mut out = Vec::new();
    for p in panes {
        // With mouse reporting on, a click goes to the program, not to play/pause: no
        // controls promise a toggle that will not happen.
        let reporting = p.v.shared.mirror.load().mouse_mode() != apoth_input::MouseMode::None;
        let hovered = p.view.media_hover().filter(|_| !reporting);
        let rows = p.snap.rows.len();
        for m in &p.media {
            let vrow = m.row as i64 - p.media_top as i64;
            if vrow >= rows as i64 || vrow + i64::from(m.rows) <= 0 {
                continue;
            }
            let x = p.v.origin.0 as f32 + m.col as f32 * cw;
            let y = p.v.origin.1 as f32 + vrow as f32 * ch;
            out.push(MediaHud {
                rect: Rect::new(
                    x / ui,
                    y / ui,
                    m.cols as f32 * cw / ui,
                    m.rows as f32 * ch / ui,
                ),
                key: m.key,
                hovered: hovered == Some(m.key),
                state: m.state,
                progress: m
                    .duration_ms
                    .map(|d| (m.pos_ms as f32 / d.max(1) as f32).clamp(0.0, 1.0)),
                alpha: 0.0,
            });
        }
    }
    out
}

impl ChromeModel {
    pub(crate) fn empty() -> ChromeModel {
        ChromeModel {
            strip: None,
            tab_style: TabStyle::Floating,
            frames: Vec::new(),
            palette: None,
            search: None,
            tooltip: None,
            bell: None,
            media: None,
            media_muted: None,
            media_fx: MediaFx::default(),
            ui: UiState::default(),
        }
    }
}

/// What a window shows: the panes of its shown tab and the chrome.
pub(crate) struct WindowLayout {
    pub(crate) panes: Vec<PaneView>,
    pub(crate) chrome: ChromeModel,
}

/// Consecutive reconfigure or recreate faults before the render thread gives up.
const MAX_FAULT_STREAK: u32 = 30;

pub(crate) struct RenderArgs {
    pub(crate) surface: WlSurfaceHandle,
    pub(crate) pacer: Pacer,
    pub(crate) fonts: FontSystem,
    pub(crate) settings: RenderSettings,
    pub(crate) scale: f64,
    pub(crate) buffer: (u32, u32),
    pub(crate) rx: mpsc::Receiver<RenderMsg>,
    /// A sender into `rx`: the pacer's wake-up for events that arrive on the render queue.
    pub(crate) poke: mpsc::Sender<RenderMsg>,
    pub(crate) to_main: calloop::channel::Sender<MainMsg>,
}

/// Padding in physical pixels at `scale`; main uses the same rule for the grid size.
pub(crate) fn pad_px(padding: u32, scale: f64) -> u32 {
    (f64::from(padding) * scale).round() as u32
}

/// A second font system for `interface.font`, over the same discovery index as the terminal's.
fn build_ui_fonts(family: &str, terminal: &FontSystem) -> Option<FontSystem> {
    let mut cfg = terminal.config().clone();
    for st in [Style::Regular, Style::Bold] {
        cfg.styles[st.index()].family = Some(family.to_owned());
    }
    let dirs = apoth_font::discovery::font_dirs(&cfg.directories);
    let index = match apoth_font::discovery::default_cache_file() {
        Some(f) => apoth_font::discovery::FontIndex::load_or_build(&dirs, &f).0,
        None => apoth_font::discovery::FontIndex::build(&dirs, None).0,
    };
    let mut fs = FontSystem::new(index, Vec::new(), cfg);
    fs.cell_metrics(16.0).map(|_| fs)
}

struct Stack {
    presenter: SurfacePresenter,
    renderer: Renderer,
}

fn build_stack(
    surface: &Arc<WlSurfaceHandle>,
    size: (u32, u32),
    rs: &RenderSettings,
    cell: (u32, u32),
    scale: f64,
) -> Result<Stack, RenderError> {
    let opts = SurfaceOptions {
        gpu: GpuOptions {
            allow_software: rs.allow_software,
            compositor_hint: None,
        },
        alpha: rs.alpha,
    };
    let presenter = SurfacePresenter::new(Arc::clone(surface), size, &opts)?;
    let cfg = RendererConfig {
        cell,
        target_format: presenter.format(),
        page_size: PAGE,
        mask_pages: MASK_PAGES as u32,
        color_pages: COLOR_PAGES as u32,
        text_blending: rs.blending,
        // Every pane draws at the origin main gives it; this is only the fallback.
        origin: (
            pad_px(rs.padding.0, scale) as i32,
            pad_px(rs.padding.1, scale) as i32,
        ),
    };
    let renderer = Renderer::new(presenter.gpu(), &cfg);
    Ok(Stack {
        presenter,
        renderer,
    })
}

fn shape_of(code: u8, default: CursorShape) -> CursorShape {
    match code {
        1 | 2 => CursorShape::Block,
        3 | 4 => CursorShape::Underline,
        5 | 6 => CursorShape::Beam,
        _ => default,
    }
}

pub(crate) fn spawn(a: RenderArgs) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("apoth-render".into())
        .spawn(move || run(a))
}

/// The render thread's state for one pane.
struct PaneR {
    v: PaneView,
    scene: GridScene,
    snap: FrameSnapshot,
    /// Scrollback offset, selection and preedit (T-02 §5-§6): commands arrive from main.
    view: View,
    hl: Vec<Highlight>,
    cscene: CursorScene,
    pal: Palette,
    source: Source,
    damage: Damage,
    cur: Option<CursorInfo>,
    show_cursor: bool,
    last_cursor: (usize, usize),
    vdump: Option<ViewDump>,
    /// Image placements in view (T-15 §6), taken with the snapshot.
    draws: Vec<ImageDraw>,
    /// Media placements of the shown screen and the absolute row at the view's top, taken
    /// with the snapshot (hover controls and progress, T-08 §4).
    media: Vec<apoth_term::MediaPlacement>,
    media_top: usize,
    /// Some image sits under the text: the scene draws default backgrounds transparent and
    /// the frame puts the background back under the images ([`PaneDraw::backdrop`]).
    below: bool,
    /// `below` flipped: the scene and the retained grid start over.
    restage: bool,
    idump: Option<ImageDump>,
    /// The next animation frame of an image in view (T-15 §1: the frame scheduler wakes
    /// for it; nothing else does).
    due: Option<Instant>,
}

impl PaneR {
    fn new(v: PaneView) -> PaneR {
        let vdump = if v.first { ViewDump::from_env() } else { None };
        let idump = if v.first { ImageDump::from_env() } else { None };
        PaneR {
            v,
            scene: GridScene::new(),
            snap: FrameSnapshot::new(),
            view: View::new(),
            hl: Vec::new(),
            cscene: CursorScene::default(),
            pal: Palette::xterm(),
            source: Source::Live,
            damage: Damage::Full,
            cur: None,
            show_cursor: false,
            last_cursor: (usize::MAX, usize::MAX),
            vdump,
            draws: Vec::new(),
            media: Vec::new(),
            media_top: 0,
            below: false,
            restage: false,
            idump,
            due: None,
        }
    }

    /// Snapshot under the pane's lock: damage, view source, palette, cursor and colours.
    fn snapshot(&mut self, rs: &RenderSettings) {
        let mut t = self.v.shared.lock();
        let d = t.grid_mut().snapshot_dirty(&mut self.snap);
        self.source = self.view.sync(&t, &self.snap, d != Damage::None);
        let def = &self.v.defaults;
        for i in 0..=255u8 {
            let mut c = t.palette(i);
            if let Some(a) = &rs.colors.ansi
                && i < 16
            {
                c = pick(c, def.ansi[usize::from(i)], a[usize::from(i)]);
            }
            self.pal.indexed[usize::from(i)] = Rgb(c.0, c.1, c.2);
        }
        let colors = t.colors();
        self.cur = Some(t.cursor());
        if t.has_images() {
            let top = t.grid().history_rows().saturating_sub(self.view.offset());
            self.due = t.image_draws(top, t.rows(), Instant::now(), &mut self.draws);
        } else {
            self.draws.clear();
            self.due = None;
        }
        if t.has_media() || !self.media.is_empty() {
            self.media = t.media_placements();
            self.media_top = t.grid().history_rows().saturating_sub(self.view.offset());
            self.view.sync_media_hover(&t, &self.media);
        }
        drop(t);
        let below = self.draws.iter().any(|d| d.z < 0);
        if below != self.below {
            self.below = below;
            self.restage = true;
        }
        if let Some(d) = self.idump.as_mut() {
            d.write(&self.draws);
        }
        let fg = pick(colors.0, def.fg, rs.colors.fg);
        let bg = pick(colors.1, def.bg, rs.colors.bg);
        let cc = pick(colors.2, def.cursor, rs.colors.cursor);
        self.pal.fg = Rgb(fg.0, fg.1, fg.2);
        self.pal.bg = Rgb(bg.0, bg.1, bg.2);
        self.pal.cursor = Some(Rgb(cc.0, cc.1, cc.2));
        let rgb = |c: (u8, u8, u8)| Rgb(c.0, c.1, c.2);
        self.pal.cursor_text = rs.colors.cursor_text.map(rgb);
        self.pal.selection_fg = rs.colors.selection.0.map(rgb);
        self.pal.selection_bg = rgb(rs.colors.selection.1);
        // The frame draws the live snapshot, or the composed one while scrolled back or with
        // a preedit. Highlights are baked into rows, so rows whose highlight changed are
        // rebuilt, and a region scroll under a highlight is a full redraw.
        self.damage = match self.source {
            Source::Live => {
                if matches!(d, Damage::Scroll { .. }) && !self.view.highlight_rows().is_empty() {
                    Damage::Full
                } else {
                    d
                }
            }
            Source::LiveFull => Damage::Full,
            Source::Composed { full } => {
                if full {
                    Damage::Full
                } else {
                    Damage::None
                }
            }
        };
        if self.damage != Damage::Full {
            for row in self.view.changed_rows() {
                self.scene.invalidate(row);
            }
        }
        let colours = hl_colours(
            rs.light,
            self.pal.bg,
            self.pal.selection_bg,
            self.pal.selection_fg,
        );
        paint(self.view.highlights(), &colours, &mut self.hl);
    }

    fn disp(&self) -> &FrameSnapshot {
        match self.source {
            Source::Composed { .. } => self.view.composed(),
            _ => &self.snap,
        }
    }

    /// The scene configuration of this pane: transparent default backgrounds while an
    /// image sits under the text.
    fn scene_cfg(&self, cfg: &SceneConfig) -> SceneConfig {
        SceneConfig {
            bg_alpha: if self.below { 0.0 } else { cfg.bg_alpha },
            ..*cfg
        }
    }

    /// The premultiplied pane background under below-text images.
    fn backdrop(&self, opacity: f32) -> Option<[f32; 4]> {
        let c = |v: u8| f32::from(v) / 255.0 * opacity;
        self.below.then(|| {
            [
                c(self.pal.bg.0),
                c(self.pal.bg.1),
                c(self.pal.bg.2),
                opacity,
            ]
        })
    }

    /// The image quads, split under and over the text (z < 0 under).
    fn quads(&self) -> (Vec<ImageQuad<'_>>, Vec<ImageQuad<'_>>) {
        fn q(d: &ImageDraw) -> ImageQuad<'_> {
            ImageQuad {
                key: d.key,
                width: d.image.width,
                height: d.image.height,
                rgba: &d.image.rgba,
                dst: d.dst,
                src: d.src,
                replaces: d.replaces,
                dirty: d.dirty,
            }
        }
        (
            self.draws.iter().filter(|d| d.z < 0).map(q).collect(),
            self.draws.iter().filter(|d| d.z >= 0).map(q).collect(),
        )
    }

    /// Rebuild the damaged rows (`full`: every row). Rows still waiting for a glyph.
    fn build(&mut self, glyphs: &mut Glyphs, cfg: &SceneConfig, echo: bool, full: bool) -> usize {
        let metrics = glyphs.metrics();
        let cfg = &self.scene_cfg(cfg);
        let ctx = BuildCtx {
            cfg,
            palette: &self.pal,
            metrics: &metrics,
            highlights: &self.hl,
        };
        let damage = if full { Damage::Full } else { self.damage };
        let disp = match self.source {
            Source::Composed { .. } => self.view.composed(),
            _ => &self.snap,
        };
        self.scene.update(disp, damage, &ctx, echo, glyphs).pending
    }

    /// Build the cursor (solid when the pane has focus and the window has it).
    fn cursor(
        &mut self,
        glyphs: &mut Glyphs,
        cfg: &SceneConfig,
        rs: &RenderSettings,
        echo: bool,
        to_main: &calloop::channel::Sender<MainMsg>,
    ) {
        self.show_cursor = false;
        let Some(cur) = self.cur else { return };
        if (cur.y, cur.x) != self.last_cursor {
            self.last_cursor = (cur.y, cur.x);
            let _ = to_main.send(MainMsg::Cursor(self.v.id, self.last_cursor));
        }
        let focused = self.v.focused && self.v.shared.focused.load(Ordering::Acquire);
        let rows = self.disp().rows.len();
        self.show_cursor = cur.visible
            && self.view.offset() == 0
            && cur.y < rows
            && !(rs.unfocused_cursor_hidden && !focused);
        if !self.show_cursor {
            return;
        }
        let metrics = glyphs.metrics();
        let cfg = &self.scene_cfg(cfg);
        let ctx = BuildCtx {
            cfg,
            palette: &self.pal,
            metrics: &metrics,
            highlights: &self.hl,
        };
        let st = CursorState {
            col: cur.x,
            row: cur.y,
            shape: shape_of(cur.style.shape, rs.cursor_shape),
            visible: true,
            focused,
        };
        let disp = match self.source {
            Source::Composed { .. } => self.view.composed(),
            _ => &self.snap,
        };
        cursor::build(&st, &disp.rows[cur.y], &ctx, echo, glyphs, &mut self.cscene);
    }

    fn reset_scene(&mut self) {
        self.scene = GridScene::new();
    }
}

/// The interface fonts: `interface.font` when set, else the terminal's.
fn ui_fonts<'a>(
    rs: &RenderSettings,
    cache: &'a mut Option<(String, FontSystem)>,
    terminal: &'a mut FontSystem,
) -> &'a mut FontSystem {
    if let Some(fam) = &rs.interface_font {
        if cache.as_ref().is_none_or(|(f, _)| f != fam) {
            *cache = build_ui_fonts(fam, terminal).map(|f| (fam.clone(), f));
        }
        if let Some((_, f)) = cache.as_mut() {
            return f;
        }
    }
    terminal
}

#[allow(clippy::too_many_lines)]
fn run(a: RenderArgs) {
    let RenderArgs {
        surface,
        mut pacer,
        mut fonts,
        settings: mut rs,
        mut scale,
        mut buffer,
        rx,
        poke,
        to_main,
    } = a;
    pacer.set_wake(move || {
        let _ = poke.send(RenderMsg::Poke);
    });
    let ctl = Arc::new(CommitCtl::default());
    let surface = Arc::new(surface);
    let px = fonts.config().pixel_size(scale as f32);
    let Some(m) = fonts.cell_metrics(px) else {
        let _ = to_main.send(MainMsg::RenderFailed("no usable font".into()));
        return;
    };
    let mut glyphs = Glyphs::new(fonts, m, px);
    let cell = |g: &Glyphs| (g.metrics().width as u32, g.metrics().height as u32);
    let mut stack = match build_stack(&surface, buffer, &rs, cell(&glyphs), scale) {
        Ok(s) => s,
        Err(e) => {
            let _ = to_main.send(MainMsg::RenderFailed(e.to_string()));
            return;
        }
    };
    stack
        .presenter
        .set_pre_present(pacer.commit_hook(Arc::clone(&ctl)));
    let info = stack.presenter.info();
    tracing::info!(
        present_mode = ?info.present_mode,
        alpha = ?info.alpha,
        opaque_fallback = info.opaque_fallback,
        "presenting"
    );
    let mut sched = Scheduler::new(
        SchedConfig {
            echo_window: rs.echo_window,
            sync_timeout: rs.sync_timeout,
            ..SchedConfig::default()
        },
        Pacing::choose(true, info.present_mode),
    );
    let mut panes: Vec<PaneR> = Vec::new();
    let mut chrome = ChromeModel::empty();
    let mut scene_cfg = SceneConfig {
        bold_bright: rs.bold_bright,
        bg_alpha: rs.opacity,
        ..SceneConfig::default()
    };
    let (mut seen_key, mut was_hold, mut seen_focus) = (0u64, false, false);
    let mut fault_streak = 0u32;
    let mut overlay: Option<Overlay> = None;
    let mut ui_cache: Option<(String, FontSystem)> = None;
    let mut raster = UiRaster::new();
    let mut chrome_dirty = false;
    // Media control fades (T-08 §4) on this thread's clock.
    let mut fades = MediaFades::default();
    let t0 = Instant::now();
    sched.damage(now_ns());

    'outer: loop {
        let now = now_ns();
        let mut surface_event = false;
        while let Some(ev) = pacer.try_recv() {
            match ev {
                PaceEvent::Frame { .. } => sched.frame_callback(),
                PaceEvent::Presented(p) => {
                    sched.presentation_feedback(Duration::from_nanos(u64::from(p.refresh_ns)));
                }
                PaceEvent::Discarded { .. } => {}
                PaceEvent::Surface {
                    buffer: b,
                    scale: s,
                    suspended,
                    ..
                } => {
                    surface_event = true;
                    sched.set_suspended(suspended, now);
                    let s = s.as_f64();
                    if b != buffer {
                        buffer = b;
                        chrome_dirty = true;
                        stack.presenter.resize(b);
                        sched.damage(now);
                    }
                    if (s - scale).abs() > f64::EPSILON {
                        scale = s;
                        let px = glyphs.fonts.config().pixel_size(scale as f32);
                        if glyphs.set_px(px) {
                            let c = cell(&glyphs);
                            stack.renderer.set_cell(c);
                            chrome_dirty = true;
                            stack.renderer.invalidate_all();
                            panes.iter_mut().for_each(PaneR::reset_scene);
                            let _ = to_main.send(MainMsg::CellSize(c));
                            sched.damage(now);
                        }
                    }
                }
            }
        }
        if surface_event && pacer.ack_pending() {
            // A configure waits for its ack: draw a frame at the new size to carry it.
            sched.damage(now);
        }
        // Typing latency and the mode 2026 hold follow the focused pane; output in any pane
        // damages the frame.
        if let Some(f) = panes.iter().find(|p| p.v.focused) {
            let k = f.v.shared.last_key_ns.load(Ordering::Acquire);
            if k != seen_key {
                seen_key = k;
                sched.note_key(k);
            }
            let h = f.v.shared.hold.load(Ordering::Acquire);
            if h != was_hold {
                was_hold = h;
                if h {
                    sched.sync_begin(now);
                } else {
                    sched.sync_end(now);
                }
            }
            let fo = f.v.shared.focused.load(Ordering::Acquire);
            if fo != seen_focus {
                seen_focus = fo;
                sched.damage(now);
            }
        }
        for p in &panes {
            if p.v.shared.dirty.swap(false, Ordering::AcqRel) {
                sched.damage(now);
            }
        }

        let msg = match sched.poll(now_ns()) {
            Poll::Render(r) => {
                glyphs.begin_frame();
                let mut pending = 0usize;
                for p in &mut panes {
                    p.snapshot(&rs);
                    if std::mem::take(&mut p.restage) {
                        stack.renderer.invalidate(p.v.id);
                        p.reset_scene();
                    }
                    pending += p.build(&mut glyphs, &scene_cfg, r.echo, false);
                    for m in p.view.take_out() {
                        let _ = to_main.send(MainMsg::Nav(p.v.id, m));
                    }
                    if p.view.busy() {
                        // The search still has history to scan: another frame continues it.
                        pending += 1;
                    }
                }
                let huds = media_huds(
                    &panes,
                    cell(&glyphs),
                    (scale * f64::from(rs.ui_zoom)) as f32,
                );
                let (fx, fading) = fades.apply(
                    huds,
                    chrome.media.as_ref().map(|m| m.hovered),
                    chrome.media_muted,
                    t0.elapsed().as_millis() as u64,
                    rs.motion,
                );
                if fx != chrome.media_fx {
                    chrome.media_fx = fx;
                    chrome_dirty = true;
                }
                let mut retries = 0;
                while std::mem::take(&mut glyphs.flushed) {
                    // An atlas was cleared mid-update: every row of every pane must be
                    // rebuilt. If the rebuild overflows too, the last pass draws what does
                    // not fit as blank (it cannot clear again), so no row keeps slots another
                    // glyph reuses.
                    retries += 1;
                    glyphs.degraded = retries >= 2;
                    stack.renderer.invalidate_all();
                    pending = 0;
                    for p in &mut panes {
                        pending += p.build(&mut glyphs, &scene_cfg, r.echo, true);
                    }
                }
                // The cursor glyphs must not clear an atlas the rows were just built on.
                glyphs.degraded = true;
                for p in &mut panes {
                    p.cursor(&mut glyphs, &scene_cfg, &rs, r.echo, &to_main);
                    if let Some(d) = p.vdump.as_mut() {
                        let rows = match p.source {
                            Source::Composed { .. } => &p.view.composed().rows,
                            _ => &p.snap.rows,
                        };
                        d.write(rows, p.view.offset());
                    }
                }
                glyphs.degraded = false;
                glyphs.flush(&stack.renderer);
                if chrome_dirty {
                    chrome_dirty = false;
                    let age = overlay
                        .as_ref()
                        .map_or(0, |o| o.shown_at.elapsed().as_millis() as u64);
                    let ui_scale = (scale * f64::from(rs.ui_zoom)) as f32;
                    let fonts = ui_fonts(&rs, &mut ui_cache, &mut glyphs.fonts);
                    build_chrome(
                        stack.renderer.chrome_mut(),
                        overlay.as_ref(),
                        &chrome,
                        buffer,
                        ui_scale,
                        rs.light,
                        age,
                        fonts,
                        &mut raster,
                    );
                }
                let a = rs.opacity;
                // The clear is the focused pane's background (dividers and padding show it).
                let bg = panes
                    .iter()
                    .find(|p| p.v.focused)
                    .or(panes.first())
                    .map_or(Rgb(0, 0, 0), |p| p.pal.bg);
                let clear = [
                    f32::from(bg.0) / 255.0 * a,
                    f32::from(bg.1) / 255.0 * a,
                    f32::from(bg.2) / 255.0 * a,
                    a,
                ];
                // The pre-present hook sends ack, frame request and feedback, and only for a
                // frame that is actually committed.
                ctl.set_frame(r.request_frame);
                ctl.set_size(stack.presenter.size());
                let quads: Vec<_> = panes.iter().map(PaneR::quads).collect();
                let list: Vec<PaneDraw<'_>> = panes
                    .iter()
                    .zip(&quads)
                    .map(|(p, (below, above))| PaneDraw {
                        key: p.v.id,
                        scene: &p.scene,
                        cursor: p.show_cursor.then_some(&p.cscene),
                        origin: Some(p.v.origin),
                        images_below: below,
                        images_above: above,
                        backdrop: p.backdrop(a),
                    })
                    .collect();
                let floats: Vec<usize> = panes
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.v.floating)
                    .map(|(i, _)| i)
                    .collect();
                let input = FrameInput {
                    panes: &list,
                    chrome_before: &floats,
                    clear,
                };
                let outcome = stack.presenter.present(&mut stack.renderer, &input);
                drop(list);
                drop(quads);
                match outcome {
                    PresentOutcome::Presented(st) => {
                        fault_streak = 0;
                        sched.rendered();
                        let due = panes.iter().filter_map(|p| p.due).min();
                        sched.animate(due.map(|d| {
                            now_ns()
                                + u64::try_from(
                                    d.saturating_duration_since(Instant::now()).as_nanos(),
                                )
                                .unwrap_or(u64::MAX / 2)
                        }));
                        tracing::trace!(
                            panes = panes.len(),
                            rows = st.draw.rows_drawn,
                            glyphs = st.draw.glyphs,
                            full = st.draw.full,
                            "frame"
                        );
                    }
                    PresentOutcome::Skipped(fault) => {
                        tracing::debug!(?fault, "frame skipped");
                        match fault_action(fault) {
                            FaultAction::Abort => {
                                let _ = to_main.send(MainMsg::RenderFailed(format!(
                                    "surface fault: {fault:?}"
                                )));
                                break 'outer;
                            }
                            FaultAction::Reconfigure | FaultAction::RecreateSurface => {
                                // The presenter already reconfigured or recreated the
                                // swapchain; redraw at the known size. A fault that keeps
                                // coming back is fatal rather than a silent spin.
                                fault_streak += 1;
                                if fault_streak > MAX_FAULT_STREAK {
                                    let _ = to_main.send(MainMsg::RenderFailed(format!(
                                        "surface fault persists: {fault:?}"
                                    )));
                                    break 'outer;
                                }
                                stack.presenter.resize(buffer);
                                sched.skipped(now_ns());
                            }
                            FaultAction::RebuildDevice | FaultAction::SkipFrame => {
                                sched.skipped(now_ns());
                            }
                        }
                    }
                    PresentOutcome::DeviceLost => {
                        tracing::warn!("device lost; rebuilding the renderer");
                        glyphs.reset();
                        glyphs.flushed = false;
                        panes.iter_mut().for_each(PaneR::reset_scene);
                        match build_stack(&surface, buffer, &rs, cell(&glyphs), scale) {
                            Ok(s) => {
                                stack = s;
                                stack
                                    .presenter
                                    .set_pre_present(pacer.commit_hook(Arc::clone(&ctl)));
                                chrome_dirty = true;
                                sched.damage(now_ns());
                            }
                            Err(e) => {
                                let _ = to_main.send(MainMsg::RenderFailed(e.to_string()));
                                break 'outer;
                            }
                        }
                    }
                }
                if pending > 0 || fading {
                    sched.damage(now_ns());
                }
                continue;
            }
            Poll::Idle | Poll::Wait(None) => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Poll::Wait(Some(t)) => {
                rx.recv_timeout(Duration::from_nanos(t.saturating_sub(now_ns())))
            }
        };
        match msg {
            Ok(RenderMsg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(RenderMsg::Poke) | Err(RecvTimeoutError::Timeout) => {}
            Ok(RenderMsg::View(id, cmd)) => {
                if let Some(p) = panes.iter_mut().find(|p| p.v.id == id) {
                    let copied = {
                        let mut t = p.v.shared.lock();
                        p.view.apply(&mut t, cmd)
                    };
                    if let Some(c) = copied {
                        let _ = to_main.send(MainMsg::Copied(c));
                    }
                    for m in p.view.take_out() {
                        let _ = to_main.send(MainMsg::Nav(p.v.id, m));
                    }
                    sched.damage(now_ns());
                }
            }
            Ok(RenderMsg::Layout(l)) => {
                let WindowLayout {
                    panes: views,
                    chrome: c,
                } = *l;
                let mut next = Vec::with_capacity(views.len());
                for v in views {
                    match panes.iter().position(|p| p.v.id == v.id) {
                        Some(i) => {
                            let mut p = panes.swap_remove(i);
                            if p.v.origin != v.origin {
                                stack.renderer.invalidate(v.id);
                            }
                            p.v = v;
                            next.push(p);
                        }
                        None => next.push(PaneR::new(v)),
                    }
                }
                panes = next;
                let keys: Vec<u64> = panes.iter().map(|p| p.v.id).collect();
                stack.renderer.retain(&keys);
                // The media controls are this thread's: they outlive main's layout.
                let fx = std::mem::take(&mut chrome.media_fx);
                chrome = c;
                chrome.media_fx = fx;
                chrome_dirty = true;
                sched.damage(now_ns());
            }
            Ok(RenderMsg::Overlay(o)) => {
                overlay = o.map(|b| *b);
                chrome_dirty = true;
                sched.damage(now_ns());
            }
            Ok(RenderMsg::Update(u)) => {
                let RenderUpdate {
                    settings: new,
                    font,
                } = *u;
                let now = now_ns();
                if let Some(f) = font {
                    glyphs.fonts.set_config(f);
                    let px = glyphs.fonts.config().pixel_size(scale as f32);
                    if glyphs.set_px(px) {
                        let c = cell(&glyphs);
                        stack.renderer.set_cell(c);
                        let _ = to_main.send(MainMsg::CellSize(c));
                    }
                }
                stack.renderer.set_text_blending(new.blending);
                if new.echo_window != rs.echo_window || new.sync_timeout != rs.sync_timeout {
                    let pacing = sched.pacing();
                    sched = Scheduler::new(
                        SchedConfig {
                            echo_window: new.echo_window,
                            sync_timeout: new.sync_timeout,
                            ..SchedConfig::default()
                        },
                        pacing,
                    );
                }
                scene_cfg.bold_bright = new.bold_bright;
                scene_cfg.bg_alpha = new.opacity;
                rs = new;
                chrome_dirty = true;
                // Colours, opacity and glyph tables all feed the cached rows: rebuild them.
                stack.renderer.invalidate_all();
                panes.iter_mut().for_each(PaneR::reset_scene);
                sched.damage(now);
            }
        }
    }
    // Swapchain before the window roles (INTEGRATION-61): dropped here, before main destroys
    // the window.
    drop(stack);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hud(key: u64, hovered: bool) -> MediaHud {
        MediaHud {
            key,
            rect: Rect::new(0.0, 0.0, 160.0, 90.0),
            hovered,
            state: apoth_term::MediaPlay::Playing,
            progress: Some(0.25),
            alpha: 0.0,
        }
    }

    #[test]
    fn media_fades_run_then_settle_and_request_no_frames() {
        let full = MotionMode::Full;
        let fade = apoth_ui::tokens::motion::FADE_MS;
        let mut f = MediaFades::default();
        // At rest: nothing shows, nothing runs.
        let (fx, run) = f.apply(vec![hud(1, false)], None, None, 0, full);
        assert_eq!(fx.huds[0].alpha, 0.0);
        assert!(!run && fx.muted.is_none());
        // Hover: the controls fade in over FADE_MS, then settle.
        let (fx, run) = f.apply(vec![hud(1, true)], None, None, 10, full);
        assert!(run && fx.huds[0].alpha < 0.5);
        let (fx, run) = f.apply(vec![hud(1, true)], None, None, 10 + fade, full);
        assert!(!run && (fx.huds[0].alpha - 1.0).abs() < 1e-6);
        // The muted notice fades in, and out after main drops it, then is gone.
        let pane = Rect::new(0.0, 0.0, 400.0, 300.0);
        let (fx, _) = f.apply(vec![], None, Some(pane), 1000, full);
        assert_eq!(fx.muted.map(|m| m.0), Some(pane));
        let (fx, run) = f.apply(vec![], None, None, 1000 + fade, full);
        assert!(run && fx.muted.is_some(), "fading out from where it was");
        let (fx, run) = f.apply(vec![], None, None, 1000 + 3 * fade, full);
        assert!(!run && fx.muted.is_none());
        // A media window's hover shows every placement's controls and its own chrome.
        let (fx, _) = f.apply(
            vec![hud(2, false)],
            Some(true),
            None,
            5000,
            MotionMode::None,
        );
        assert_eq!(fx.huds[0].alpha, 1.0);
        assert_eq!(fx.win_alpha, 1.0);
    }

    fn lum(c: Rgb) -> f32 {
        let l = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * l(c.0) + 0.7152 * l(c.1) + 0.0722 * l(c.2)
    }
    fn ratio(a: Rgb, b: Rgb) -> f32 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn highlight_colours_keep_contrast_on_both_schemes() {
        for (light, bg) in [(false, Rgb(12, 11, 8)), (true, Rgb(244, 242, 235))] {
            let c = hl_colours(light, bg, Rgb(80, 80, 80), None);
            for (b, f) in [
                (c.match_bg, c.match_fg),
                (c.current_bg, c.current_fg),
                (c.hint_bg, c.hint_fg),
                (c.label_bg, c.label_fg),
                (c.cursor_bg, c.cursor_fg),
            ] {
                assert!(ratio(b, f.unwrap()) >= 4.5, "light={light} {b:?} {f:?}");
            }
        }
    }

    #[test]
    fn configured_colour_yields_to_a_program_set_one() {
        let startup = (1, 2, 3);
        let cfg = (9, 9, 9);
        assert_eq!(pick(startup, startup, cfg), cfg);
        assert_eq!(pick((7, 7, 7), startup, cfg), (7, 7, 7));
    }

    #[test]
    fn padding_scales_and_rounds() {
        assert_eq!(pad_px(6, 1.0), 6);
        assert_eq!(pad_px(6, 1.25), 8);
        assert_eq!(pad_px(0, 2.0), 0);
    }

    /// T-23 snapshot-lock bound: a full 300x80 snapshot under the term lock takes <= 100 us.
    /// Timing is meaningless unoptimised, so this runs only with
    /// `cargo test --release -p apotheosis -- --ignored snapshot_lock`. Measured at M11 fix
    /// round 1: p99 about 385 us, so the bound is NOT met yet (apoth-grid snapshot cost).
    #[test]
    #[ignore = "timing bound; run in release"]
    fn snapshot_lock_300x80_within_100us() {
        let mut t = apoth_term::Term::new(apoth_term::TermConfig {
            rows: 80,
            cols: 300,
            ..apoth_term::TermConfig::default()
        });
        let line = "x".repeat(300);
        for _ in 0..80 {
            t.feed(line.as_bytes(), std::time::Instant::now());
        }
        let mut snap = FrameSnapshot::new();
        let mut times = Vec::new();
        for _ in 0..200 {
            t.grid_mut().mark_full();
            let t0 = std::time::Instant::now();
            let _ = t.grid_mut().snapshot_dirty(&mut snap);
            times.push(t0.elapsed());
        }
        times.sort();
        let p99 = times[times.len() * 99 / 100];
        assert!(p99 <= Duration::from_micros(100), "p99 {p99:?}");
    }

    #[test]
    fn decscusr_codes_map_to_shapes() {
        assert_eq!(shape_of(2, CursorShape::Beam), CursorShape::Block);
        assert_eq!(shape_of(4, CursorShape::Block), CursorShape::Underline);
        assert_eq!(shape_of(6, CursorShape::Block), CursorShape::Beam);
        assert_eq!(shape_of(0, CursorShape::Beam), CursorShape::Beam);
    }
}
