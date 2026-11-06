// SPDX-License-Identifier: AGPL-3.0-only
//! The window switcher, the overview and hover previews under `mode "de"`
//! (COMP-05 §7.1, C-24, ADR 0090): layout, hit testing and drawing, with no
//! `AbyssState` in reach.
//!
//! **This is not trusted UI.** Like the window menu these are compositor
//! overlays: spliced in by the backends above the windows and below trusted
//! UI, and never in a capture pass list (`render/scan_tests.rs` keeps
//! `capture.rs` from naming them). A card's body is one of two things: the
//! window's thumbnail with opaque covers laid over the rects capture would
//! redact ([`Body::Pixels`]), or a placeholder with a padlock ([`Body::Hidden`]). The
//! shell decides which, from `render::capture::classify`, before any texture
//! is made; this module only draws what it is told, and draws a thumbnail for
//! a `Pixels` body alone, so a `Hidden` card has no path to a texture even if
//! one were still held.
//!
//! Layout is plain arithmetic on rectangles: [`present_grid`] (the overview),
//! [`switcher_layout`], [`strip_layout`] and [`preview_layout`]. Cards never
//! overlap, keep the window's aspect and are never enlarged past its size.

use std::collections::{HashMap, HashSet};

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{GlesRenderer, GlesTexture},
    },
    output::Output,
    utils::{Logical, Point, Rectangle, Scale, Size},
};

use crate::{
    hud::{band, cov, sd_box, Paint},
    hud_font::{self, Face},
    text::Rgba,
    thumb::ThumbStore,
    titlebar::display_title,
    AbyssRenderElement,
};

/// Space between cards.
pub const GAP: i32 = 16;
/// Padding around a card's thumbnail.
pub const CARD_PAD: i32 = 8;
/// Height of the title line under the thumbnail.
pub const TITLE_H: i32 = 24;
/// The largest thumbnail the switcher draws.
pub const SWITCH_BOX: (i32, i32) = (240, 150);
/// Padding between the switcher's panel and its cards.
pub const PANEL_PAD: i32 = 18;
/// The largest thumbnail a hover preview draws.
pub const PREVIEW_BOX: (i32, i32) = (280, 176);
/// Gap between a preview and the rectangle it is anchored to.
pub const PREVIEW_GAP: i32 = 10;
/// A desktop tile of the strip.
pub const TILE: (i32, i32) = (112, 66);
pub const TILE_GAP: i32 = 10;
pub const STRIP_PAD: i32 = 12;
pub const STRIP_TOP: i32 = 20;
/// Margin the overview keeps from the output's edges.
pub const MARGIN: i32 = 32;

const CARD_RADIUS: f32 = 12.0;
/// A thumbnail's corner radius: concentric with the card's, less its padding.
const INSET_RADIUS: f32 = 6.0;
const GROUND: [f32; 3] = [0.043, 0.035, 0.024];
/// Gold marks the label of the selected card or the current desktop, never a
/// fill, a rim or a glow (STYLE.md, bar cells).
const ACCENT: Rgba = [0.961, 0.812, 0.361, 1.0];
/// The panel's and the strip's hairline.
const RIM: Rgba = [1.0, 1.0, 1.0, 0.14];
/// A cell's white fill and inside hairline at rest, under the pointer and
/// selected: the bar cell steps of STYLE.md, one clearer each.
const CELL: (f32, f32) = (0.04, 0.10);
const CELL_HOVER: (f32, f32) = (0.08, 0.16);
const CELL_ON: (f32, f32) = (0.12, 0.24);
/// A hidden card's placeholder inset and the lock on it.
const HIDDEN_FILL: f32 = 0.05;
const HIDDEN_INK: Rgba = [1.0, 1.0, 1.0, 0.34];
const INK: Rgba = [1.0, 1.0, 1.0, 0.94];
const INK_DIM: Rgba = [1.0, 1.0, 1.0, 0.42];
const COVER: [f32; 4] = [0.07, 0.06, 0.05, 1.0];
const DIM: [f32; 4] = [0.02, 0.015, 0.01, 0.58];

type Rect = Rectangle<i32, Logical>;

// -------------------------------------------------------------------- layout

/// Where one card goes: its whole frame and, inside it, the thumbnail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub frame: Rect,
    pub thumb: Rect,
}

/// The size a window's thumbnail has when it must fit `bounds`: aspect kept,
/// never larger than the window itself.
pub fn fit(src: Size<i32, Logical>, bounds: Size<i32, Logical>) -> Size<i32, Logical> {
    let (sw, sh) = (f64::from(src.w.max(1)), f64::from(src.h.max(1)));
    let s = (f64::from(bounds.w.max(1)) / sw)
        .min(f64::from(bounds.h.max(1)) / sh)
        .min(1.0);
    Size::from((((sw * s).round() as i32).max(1), ((sh * s).round() as i32).max(1)))
}

/// A card's frame size for a thumbnail of `thumb`.
pub fn frame_size(thumb: Size<i32, Logical>) -> Size<i32, Logical> {
    Size::from((thumb.w + 2 * CARD_PAD, thumb.h + CARD_PAD + TITLE_H))
}

fn placed_at(frame_loc: Point<i32, Logical>, thumb: Size<i32, Logical>) -> Placed {
    Placed {
        frame: Rectangle::new(frame_loc, frame_size(thumb)),
        thumb: Rectangle::new(frame_loc + Point::from((CARD_PAD, CARD_PAD)), thumb),
    }
}

/// `inner` centred in the cell at `cell` of size `(cw, ch)`.
fn centred(
    cell: Point<i32, Logical>,
    (cw, ch): (i32, i32),
    inner: Size<i32, Logical>,
) -> Point<i32, Logical> {
    Point::from((cell.x + (cw - inner.w) / 2, cell.y + (ch - inner.h) / 2))
}

fn thumb_box(cw: i32, ch: i32) -> Size<i32, Logical> {
    Size::from(((cw - 2 * CARD_PAD).max(1), (ch - CARD_PAD - TITLE_H).max(1)))
}

fn scale_in(src: Size<i32, Logical>, bounds: Size<i32, Logical>) -> f64 {
    (f64::from(bounds.w) / f64::from(src.w.max(1)))
        .min(f64::from(bounds.h) / f64::from(src.h.max(1)))
        .min(1.0)
}

/// Present Windows: `sources` (each window's size) as non-overlapping cards in
/// a grid that fills `area`. The column count is the one that leaves the
/// windows largest (fewer rows on a tie); each card is centred in its cell and
/// a short last row is centred as a whole.
pub fn present_grid(sources: &[Size<i32, Logical>], area: Rect) -> Vec<Placed> {
    let n = sources.len();
    if n == 0 || area.size.w <= 0 || area.size.h <= 0 {
        return Vec::new();
    }
    let cell = |cols: usize, rows: usize| {
        (
            ((area.size.w - (cols as i32 - 1) * GAP) / cols as i32).max(1),
            ((area.size.h - (rows as i32 - 1) * GAP) / rows as i32).max(1),
        )
    };
    let mut best: Option<(f64, usize)> = None;
    for cols in (1..=n).rev() {
        let rows = n.div_ceil(cols);
        let (cw, ch) = cell(cols, rows);
        let bounds = thumb_box(cw, ch);
        let score: f64 = sources.iter().map(|s| scale_in(*s, bounds)).sum();
        if best.is_none_or(|(b, _)| score > b + 1e-9) {
            best = Some((score, cols));
        }
    }
    let cols = best.map_or(1, |b| b.1);
    let rows = n.div_ceil(cols);
    let (cw, ch) = cell(cols, rows);
    let bounds = thumb_box(cw, ch);
    sources
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let (r, c) = (i / cols, i % cols);
            let in_row = cols.min(n - r * cols);
            let shift = (cols - in_row) as i32 * (cw + GAP) / 2;
            let cell_at = Point::from((
                area.loc.x + shift + c as i32 * (cw + GAP),
                area.loc.y + r as i32 * (ch + GAP),
            ));
            let thumb = fit(*src, bounds);
            placed_at(centred(cell_at, (cw, ch), frame_size(thumb)), thumb)
        })
        .collect()
}

/// The window switcher: uniform cells in rows, shrunk in steps until they fit
/// `area`, with the panel behind them centred in it. Returns the panel and the
/// cards in `sources` order (row-major).
pub fn switcher_layout(sources: &[Size<i32, Logical>], area: Rect) -> (Rect, Vec<Placed>) {
    let n = sources.len();
    if n == 0 {
        return (Rectangle::default(), Vec::new());
    }
    const STEPS: [f64; 7] = [1.0, 0.8, 0.64, 0.5, 0.4, 0.32, 0.25];
    let mut chosen = None;
    for (i, k) in STEPS.iter().enumerate() {
        let bounds = Size::<i32, Logical>::from((
            ((f64::from(SWITCH_BOX.0) * k) as i32).max(1),
            ((f64::from(SWITCH_BOX.1) * k) as i32).max(1),
        ));
        let cell = frame_size(bounds);
        let avail = area.size.w - 2 * PANEL_PAD;
        let cols = (((avail + GAP) / (cell.w + GAP)).max(1) as usize).min(n);
        let rows = n.div_ceil(cols);
        let height = rows as i32 * cell.h + (rows as i32 - 1) * GAP + 2 * PANEL_PAD;
        if height <= area.size.h || i == STEPS.len() - 1 {
            chosen = Some((bounds, cell, cols, rows));
            break;
        }
    }
    let Some((bounds, cell, cols, rows)) = chosen else {
        return (Rectangle::default(), Vec::new());
    };
    let full_w = cols as i32 * cell.w + (cols as i32 - 1) * GAP;
    let panel_size = Size::from((
        full_w + 2 * PANEL_PAD,
        rows as i32 * cell.h + (rows as i32 - 1) * GAP + 2 * PANEL_PAD,
    ));
    let panel = Rectangle::new(
        Point::from((
            area.loc.x + (area.size.w - panel_size.w) / 2,
            area.loc.y + (area.size.h - panel_size.h) / 2,
        )),
        panel_size,
    );
    let cards = sources
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let (r, c) = (i / cols, i % cols);
            let in_row = cols.min(n - r * cols);
            let row_w = in_row as i32 * cell.w + (in_row as i32 - 1) * GAP;
            let cell_at = Point::from((
                panel.loc.x + PANEL_PAD + (full_w - row_w) / 2 + c as i32 * (cell.w + GAP),
                panel.loc.y + PANEL_PAD + r as i32 * (cell.h + GAP),
            ));
            let thumb = fit(*src, bounds);
            placed_at(centred(cell_at, (cell.w, cell.h), frame_size(thumb)), thumb)
        })
        .collect();
    (panel, cards)
}

/// The desktop strip: `count` tiles in `rows` rows (the desktop grid), the
/// strip's own rectangle at the top of `area`, and the tiles in desktop order
/// (row-major, 0-based).
pub fn strip_layout(count: usize, rows: usize, area: Rect) -> (Rect, Vec<Rect>) {
    if count == 0 {
        return (Rectangle::default(), Vec::new());
    }
    let rows = rows.clamp(1, count);
    let cols = count.div_ceil(rows);
    let room = area.size.w - 2 * MARGIN - 2 * STRIP_PAD - (cols as i32 - 1) * TILE_GAP;
    let tw = (room / cols as i32).clamp(16, TILE.0);
    let th = (tw * TILE.1 / TILE.0).max(9);
    let size = Size::from((
        cols as i32 * tw + (cols as i32 - 1) * TILE_GAP + 2 * STRIP_PAD,
        rows as i32 * th + (rows as i32 - 1) * TILE_GAP + 2 * STRIP_PAD,
    ));
    let strip = Rectangle::new(
        Point::from((area.loc.x + (area.size.w - size.w) / 2, area.loc.y + STRIP_TOP)),
        size,
    );
    let tiles = (0..count)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            Rectangle::new(
                strip.loc
                    + Point::from((
                        STRIP_PAD + c as i32 * (tw + TILE_GAP),
                        STRIP_PAD + r as i32 * (th + TILE_GAP),
                    )),
                Size::from((tw, th)),
            )
        })
        .collect();
    (strip, tiles)
}

/// A hover preview of a window of size `src`, near `anchor` (the taskbar chip)
/// and inside `area`: above the anchor when it sits in the lower half of the
/// area, below it otherwise, centred on it.
pub fn preview_layout(src: Size<i32, Logical>, anchor: Rect, area: Rect) -> Placed {
    let thumb = fit(src, Size::from(PREVIEW_BOX));
    let frame = frame_size(thumb);
    let cx = anchor.loc.x + anchor.size.w / 2;
    let x = cx - frame.w / 2;
    let below = anchor.loc.y + anchor.size.h / 2 < area.loc.y + area.size.h / 2;
    let y = if below {
        anchor.loc.y + anchor.size.h + PREVIEW_GAP
    } else {
        anchor.loc.y - PREVIEW_GAP - frame.h
    };
    let max_x = (area.loc.x + area.size.w - frame.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - frame.h).max(area.loc.y);
    placed_at(
        Point::from((x.clamp(area.loc.x, max_x), y.clamp(area.loc.y, max_y))),
        thumb,
    )
}

/// The cover for a redacted rectangle `cover` of a window whose thumbnail
/// shows `src` (the same coordinate space) in `thumb`. Rounded outwards so the
/// scaled cover is never smaller than what it hides, and cut to the thumbnail.
pub fn cover_in_thumb(src: Rect, cover: Rect, thumb: Rect) -> Option<Rect> {
    if src.size.w <= 0 || src.size.h <= 0 {
        return None;
    }
    let (sx, sy) = (
        f64::from(thumb.size.w) / f64::from(src.size.w),
        f64::from(thumb.size.h) / f64::from(src.size.h),
    );
    let x0 = f64::from(cover.loc.x - src.loc.x) * sx;
    let y0 = f64::from(cover.loc.y - src.loc.y) * sy;
    let x1 = f64::from(cover.loc.x + cover.size.w - src.loc.x) * sx;
    let y1 = f64::from(cover.loc.y + cover.size.h - src.loc.y) * sy;
    let loc = Point::from((thumb.loc.x + x0.floor() as i32, thumb.loc.y + y0.floor() as i32));
    let end = Point::<i32, Logical>::from((thumb.loc.x + x1.ceil() as i32, thumb.loc.y + y1.ceil() as i32));
    Rectangle::new(loc, Size::from((end.x - loc.x, end.y - loc.y)))
        .intersection(thumb)
        .filter(|r| r.size.w > 0 && r.size.h > 0)
}

// --------------------------------------------------------------------- views

/// What a card shows of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// The thumbnail, with an opaque cover over each rect (global logical,
    /// inside the thumbnail's rect).
    Pixels { covers: Vec<Rect> },
    /// A blank placeholder: no pixels and, in its card, no title.
    Hidden,
}

/// One window's card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardView {
    /// The window's handle: the key of its thumbnail.
    pub id: u64,
    pub frame: Rect,
    pub thumb: Rect,
    /// The sanitised-on-draw title; `None` is drawn as no title at all.
    pub title: Option<String>,
    pub body: Body,
    pub hover: bool,
    pub selected: bool,
    pub minimized: bool,
}

impl CardView {
    /// Whether a thumbnail texture may be drawn for this card. Only a
    /// `Pixels` body: a placeholder never has one.
    pub fn wants_thumb(&self) -> bool {
        matches!(self.body, Body::Pixels { .. })
    }
}

/// One desktop of the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileView {
    pub rect: Rect,
    /// 0-based desktop index.
    pub index: usize,
    pub current: bool,
    pub hover: bool,
    /// A dragged card is over it.
    pub target: bool,
    /// How many windows it holds (a count, never a title).
    pub windows: usize,
}

/// A card being dragged: a chip that follows the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragView {
    pub at: Point<i32, Logical>,
    pub label: Option<String>,
}

/// Everything one frame of the switcher, the overview or a preview draws.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverlayView {
    /// The output it is for, global logical.
    pub output_rect: Rect,
    /// Dim the whole output behind it (the overview).
    pub dim: bool,
    /// The switcher's panel.
    pub panel: Option<Rect>,
    /// The strip's backing.
    pub strip: Option<Rect>,
    pub cards: Vec<CardView>,
    pub tiles: Vec<TileView>,
    /// What has been typed, shown as a pill.
    pub filter: Option<String>,
    pub drag: Option<DragView>,
}

impl OverlayView {
    /// The card whose frame holds `pos`.
    pub fn card_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        self.cards.iter().position(|c| c.frame.to_f64().contains(pos))
    }

    /// The strip tile holding `pos`.
    pub fn tile_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        self.tiles.iter().position(|t| t.rect.to_f64().contains(pos))
    }
}

// ------------------------------------------------------------------- painting

#[derive(Debug, Clone, PartialEq)]
enum What {
    Card {
        thumb: Size<i32, Logical>,
        title: Option<String>,
        hidden: bool,
        hover: bool,
        selected: bool,
        minimized: bool,
    },
    Tile {
        index: usize,
        windows: usize,
        current: bool,
        hover: bool,
        target: bool,
    },
    /// The corners of a thumbnail and its hairline, laid over the texture
    /// (a texture element has no rounded clip). `lift` is the card's white
    /// fill, so the corners match the card behind them.
    Round {
        lift: f32,
    },
    Panel,
    Strip,
    Pill(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Look {
    w: i32,
    h: i32,
    dev: usize,
    what: What,
}

fn ground(a: f32) -> Rgba {
    [GROUND[0], GROUND[1], GROUND[2], a]
}

/// A cell's white fill and hairline alphas for its state.
fn cell_step(hover: bool, on: bool) -> (f32, f32) {
    if on {
        CELL_ON
    } else if hover {
        CELL_HOVER
    } else {
        CELL
    }
}

/// A padlock centred in `inset` (device px): the mark of a window whose pixels
/// the compositor will not copy. Shrinks with a small card, never past 40%.
fn lock(p: &mut Paint, inset: (f32, f32, f32, f32), d: f32) {
    let k = (inset.3 / d / 64.0).clamp(0.4, 1.0) * d;
    let (cx, cy) = (inset.0 + inset.2 / 2.0, inset.1 + inset.3 / 2.0);
    let (bw, bh) = (22.0 * k, 16.0 * k);
    let body = (cx - bw / 2.0, cy - bh / 2.0 + 5.0 * k, bw, bh);
    let (sw, sh) = (14.0 * k, 18.0 * k);
    let shackle = (cx - sw / 2.0, body.1 - 10.0 * k, sw, sh);
    let top = body.1;
    let w = 2.4 * k;
    p.shade(shackle, HIDDEN_INK, |x, y| {
        if y > top {
            0.0
        } else {
            band(sd_box(x, y, shackle, sw / 2.0), -w, 0.0)
        }
    });
    p.fill(body, 4.0 * k, HIDDEN_INK);
    // The keyhole, cut back to the placeholder's own tone.
    let hole = (cx - 1.8 * k, body.1 + bh / 2.0 - 4.0 * k, 3.6 * k, 7.0 * k);
    p.fill(hole, 1.8 * k, [GROUND[0], GROUND[1], GROUND[2], 0.9]);
}

fn stroke(p: &mut Paint, body: (f32, f32, f32, f32), r: f32, width: f32, c: Rgba) {
    p.shade(body, c, |x, y| band(sd_box(x, y, body, r), -width, 0.0));
}

fn paint(look: &Look) -> crate::text::Raster {
    let d = look.dev as f32;
    let (pw, ph) = (look.w as usize * look.dev, look.h as usize * look.dev);
    let mut p = Paint::new(pw, ph);
    let body = (0.0, 0.0, pw as f32, ph as f32);
    match &look.what {
        What::Card {
            thumb,
            title,
            hidden,
            hover,
            selected,
            minimized,
        } => {
            let r = CARD_RADIUS * d;
            let (fill, rim) = cell_step(*hover, *selected);
            p.fill(body, r, ground(0.84));
            p.fill(body, r, [1.0, 1.0, 1.0, fill]);
            stroke(&mut p, body, r, d, [1.0, 1.0, 1.0, rim]);
            let inset = (
                CARD_PAD as f32 * d,
                CARD_PAD as f32 * d,
                thumb.w as f32 * d,
                thumb.h as f32 * d,
            );
            if *hidden {
                p.fill(inset, INSET_RADIUS * d, [1.0, 1.0, 1.0, HIDDEN_FILL]);
                stroke(&mut p, inset, INSET_RADIUS * d, d, [1.0, 1.0, 1.0, 0.08]);
                lock(&mut p, inset, d);
            } else {
                p.fill(inset, INSET_RADIUS * d, [0.0, 0.0, 0.0, 0.30]);
            }
            if let Some(title) = title {
                let face = Face::Body;
                let cols = ((look.w - 2 * CARD_PAD - 4).max(0) as usize) / hud_font::ADVANCE;
                let text = display_title(title, cols);
                let top =
                    (CARD_PAD + thumb.h) as usize + (TITLE_H as usize).saturating_sub(face.line_h()) / 2;
                p.text(
                    face,
                    look.dev,
                    CARD_PAD as usize + 2,
                    top,
                    &text,
                    if *minimized {
                        INK_DIM
                    } else if *selected {
                        ACCENT
                    } else {
                        INK
                    },
                );
            }
        }
        What::Tile {
            index,
            windows,
            current,
            hover,
            target,
        } => {
            let r = 9.0 * d;
            p.fill(body, r, ground(0.80));
            let (fill, rim) = if *target {
                (CELL_ON.0, 0.36)
            } else if *current {
                CELL_ON
            } else {
                cell_step(*hover, false)
            };
            p.fill(body, r, [1.0, 1.0, 1.0, fill]);
            stroke(&mut p, body, r, d, [1.0, 1.0, 1.0, rim]);
            let num = (index + 1).to_string();
            let face = Face::Title;
            let w = face.width(&num);
            let top = if *windows > 0 {
                (look.h as usize).saturating_sub(face.line_h()) / 2 - 5.min(look.h as usize / 8)
            } else {
                (look.h as usize).saturating_sub(face.line_h()) / 2
            };
            p.text(
                face,
                look.dev,
                (look.w as usize).saturating_sub(w) / 2,
                top,
                &num,
                if *current { ACCENT } else { INK },
            );
            if *windows > 0 {
                let count = windows.to_string();
                let small = Face::Readout;
                let w = small.width(&count);
                p.text(
                    small,
                    look.dev,
                    (look.w as usize).saturating_sub(w) / 2,
                    top + face.line_h() + 1,
                    &count,
                    INK_DIM,
                );
            }
        }
        What::Round { lift } => {
            let r = INSET_RADIUS * d;
            // Opaque, in the colour the card shows around the thumbnail (the
            // dimmed backdrop under it is near the ground).
            let c = |g: f32| g * (1.0 - lift) + lift;
            p.shade(body, [c(GROUND[0]), c(GROUND[1]), c(GROUND[2]), 1.0], |x, y| {
                1.0 - cov(sd_box(x, y, body, r))
            });
            stroke(&mut p, body, r, d, [1.0, 1.0, 1.0, 0.10]);
        }
        What::Panel | What::Strip => {
            let r = if matches!(look.what, What::Panel) {
                18.0
            } else {
                14.0
            } * d;
            p.fill(body, r, ground(0.80));
            stroke(&mut p, body, r, d, RIM);
        }
        What::Pill(text) => {
            let r = (look.h as f32 / 2.0) * d;
            p.fill(body, r, ground(0.88));
            stroke(&mut p, body, r, d, RIM);
            let face = Face::Body;
            let top = (look.h as usize).saturating_sub(face.line_h()) / 2;
            p.text(face, look.dev, 14, top, text, INK);
        }
    }
    p.into_raster()
}

// ------------------------------------------------------------------- elements

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PieceKey {
    Card(u64),
    Round(u64),
    Tile(usize),
    Panel,
    Strip,
    Filter,
    Chip,
}

struct Piece {
    look: Look,
    buffer: TextureBuffer<GlesTexture>,
}

/// The overlay's chrome textures, one per output and piece. Dropped with the
/// view: nothing stays once the overlay closes.
#[derive(Default)]
pub struct OverlayCache {
    pieces: HashMap<(Output, PieceKey), Piece>,
}

impl std::fmt::Debug for OverlayCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayCache")
            .field("pieces", &self.pieces.len())
            .finish()
    }
}

impl OverlayCache {
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    pub fn clear(&mut self) {
        self.pieces.clear();
    }
}

struct Pass<'a> {
    renderer: &'a mut GlesRenderer,
    cache: &'a mut OverlayCache,
    output: &'a Output,
    output_loc: Point<i32, Logical>,
    scale: Scale<f64>,
    dev: usize,
    seen: HashSet<PieceKey>,
    out: Vec<AbyssRenderElement>,
}

impl Pass<'_> {
    fn local(&self, p: Point<i32, Logical>) -> Point<f64, smithay::utils::Physical> {
        (p - self.output_loc).to_f64().to_physical(self.scale)
    }

    fn texture(&mut self, buffer: &TextureBuffer<GlesTexture>, rect: Rect) {
        self.out.push(AbyssRenderElement::Texture(
            TextureRenderElement::from_texture_buffer(
                self.local(rect.loc).to_i32_round::<i32>().to_f64(),
                buffer,
                None,
                None,
                Some(rect.size),
                Kind::Unspecified,
            ),
        ));
    }

    fn solid(&mut self, rect: Rect, color: [f32; 4]) {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            return;
        }
        let buffer = SolidColorBuffer::new(rect.size, color);
        self.out
            .push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
                &buffer,
                self.local(rect.loc).to_i32_round(),
                self.scale,
                1.0,
                Kind::Unspecified,
            )));
    }

    /// Draw the piece `key` painted for `what` into `rect`, repainting its
    /// texture only when `what` or the size changed.
    fn piece(&mut self, key: PieceKey, rect: Rect, what: What) {
        if rect.size.w <= 0 || rect.size.h <= 0 {
            return;
        }
        let look = Look {
            w: rect.size.w,
            h: rect.size.h,
            dev: self.dev,
            what,
        };
        self.seen.insert(key.clone());
        let slot = (self.output.clone(), key);
        let fresh = self.cache.pieces.get(&slot).is_none_or(|p| p.look != look);
        if fresh {
            let raster = paint(&look);
            let Some(buffer) = crate::annotation::upload(self.renderer, &raster, self.dev) else {
                self.cache.pieces.remove(&slot);
                return;
            };
            self.cache.pieces.insert(slot.clone(), Piece { look, buffer });
        }
        if let Some(piece) = self.cache.pieces.get(&slot) {
            let buffer = piece.buffer.clone();
            self.texture(&buffer, rect);
        }
    }
}

fn pill_size(text: &str) -> Size<i32, Logical> {
    Size::from((hud_font::ADVANCE as i32 * text.chars().count() as i32 + 28, 30))
}

/// The overlay's elements for `output`, front to back: the drag chip, the
/// filter, covers, thumbnails, cards, strip tiles, strip, panel, dim. Empty
/// when no view is open or it is for another output; the chrome textures are
/// dropped then. `thumbs` is only read, and only for a `Pixels` card.
pub fn overlay_elements(
    renderer: &mut GlesRenderer,
    cache: &mut OverlayCache,
    thumbs: &ThumbStore,
    view: Option<&OverlayView>,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    let Some(view) = view.filter(|v| v.output_rect.loc == output_loc) else {
        cache.pieces.retain(|(o, _), _| o != output);
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mut pass = Pass {
        renderer,
        cache,
        output,
        output_loc,
        scale,
        dev: crate::titlebar::raster_scale(scale),
        seen: HashSet::new(),
        out: Vec::new(),
    };
    if let Some(drag) = &view.drag {
        let text = drag
            .label
            .as_deref()
            .map(|t| display_title(t, 28))
            .unwrap_or_default();
        let size = pill_size(&text);
        let rect = Rectangle::new(drag.at - Point::from((size.w / 2, size.h / 2)), size);
        pass.piece(PieceKey::Chip, rect, What::Pill(text));
    }
    if let Some(filter) = view.filter.as_ref().filter(|f| !f.is_empty()) {
        let text = display_title(filter, 40);
        let size = pill_size(&text);
        let rect = Rectangle::new(
            Point::from((
                view.output_rect.loc.x + (view.output_rect.size.w - size.w) / 2,
                view.output_rect.loc.y + view.output_rect.size.h - size.h - MARGIN,
            )),
            size,
        );
        pass.piece(PieceKey::Filter, rect, What::Pill(text));
    }
    for card in &view.cards {
        let thumb = card.wants_thumb().then(|| thumbs.buffer(card.id)).flatten();
        if thumb.is_some() {
            pass.piece(
                PieceKey::Round(card.id),
                card.thumb,
                What::Round {
                    lift: cell_step(card.hover, card.selected).0,
                },
            );
        }
        if let Body::Pixels { covers } = &card.body {
            for cover in covers {
                pass.solid(*cover, COVER);
            }
        }
        if let Some(buffer) = thumb {
            pass.texture(buffer, card.thumb);
        }
        pass.piece(
            PieceKey::Card(card.id),
            card.frame,
            What::Card {
                thumb: card.thumb.size,
                title: card.title.clone(),
                hidden: !card.wants_thumb(),
                hover: card.hover,
                selected: card.selected,
                minimized: card.minimized,
            },
        );
    }
    for tile in &view.tiles {
        pass.piece(
            PieceKey::Tile(tile.index),
            tile.rect,
            What::Tile {
                index: tile.index,
                windows: tile.windows,
                current: tile.current,
                hover: tile.hover,
                target: tile.target,
            },
        );
    }
    if let Some(strip) = view.strip {
        pass.piece(PieceKey::Strip, strip, What::Strip);
    }
    if let Some(panel) = view.panel {
        pass.piece(PieceKey::Panel, panel, What::Panel);
    }
    if view.dim {
        pass.solid(view.output_rect, DIM);
    }
    let Pass { out, seen, .. } = pass;
    cache.pieces.retain(|(o, k), _| o != output || seen.contains(k));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    fn s(w: i32, h: i32) -> Size<i32, Logical> {
        Size::from((w, h))
    }

    fn no_overlap(cards: &[Placed]) {
        for (i, a) in cards.iter().enumerate() {
            for b in &cards[i + 1..] {
                assert!(!a.frame.overlaps(b.frame), "{:?} overlaps {:?}", a.frame, b.frame);
            }
        }
    }

    #[test]
    fn the_grid_never_overlaps_stays_inside_and_keeps_aspect() {
        let area = r(0, 150, 1280, 650);
        for n in 1..=17 {
            let sources: Vec<_> = (0..n)
                .map(|i| match i % 4 {
                    0 => s(800, 500),
                    1 => s(300, 700),
                    2 => s(1000, 300),
                    _ => s(640, 640),
                })
                .collect();
            let cards = present_grid(&sources, area);
            assert_eq!(cards.len(), n);
            no_overlap(&cards);
            for (c, src) in cards.iter().zip(&sources) {
                assert!(
                    area.contains_rect(c.frame),
                    "n={n}: {:?} leaves {area:?}",
                    c.frame
                );
                assert!(c.frame.contains_rect(c.thumb));
                let want = f64::from(src.w) / f64::from(src.h);
                let got = f64::from(c.thumb.size.w) / f64::from(c.thumb.size.h);
                // One pixel of rounding on the short side.
                let slack = 1.0 / f64::from(c.thumb.size.w.min(c.thumb.size.h)) + 1e-9;
                assert!(
                    (got / want - 1.0).abs() <= slack + 0.01,
                    "n={n}: aspect {got} vs {want}"
                );
                assert!(
                    c.thumb.size.w <= src.w && c.thumb.size.h <= src.h,
                    "never enlarged"
                );
            }
        }
    }

    #[test]
    fn a_lone_small_window_is_centred_at_its_own_size() {
        let area = r(0, 0, 1000, 600);
        let cards = present_grid(&[s(300, 200)], area);
        assert_eq!(cards[0].thumb.size, s(300, 200));
        let c = cards[0].frame;
        assert_eq!(c.loc.x + c.size.w / 2, 500);
        assert!((c.loc.y + c.size.h / 2 - 300).abs() <= 1);
    }

    #[test]
    fn the_last_row_is_centred() {
        let area = r(0, 0, 1200, 800);
        let cards = present_grid(&[s(400, 300); 5], area);
        let cols = cards
            .iter()
            .filter(|c| c.frame.loc.y == cards[0].frame.loc.y)
            .count();
        assert!(cols < 5);
        let last: Vec<_> = cards
            .iter()
            .filter(|c| c.frame.loc.y == cards[4].frame.loc.y)
            .collect();
        if last.len() < cols {
            let left = last.iter().map(|c| c.frame.loc.x).min().unwrap();
            let right = last.iter().map(|c| c.frame.loc.x + c.frame.size.w).max().unwrap();
            assert!((left - (1200 - right)).abs() <= 2, "{left} vs {}", 1200 - right);
        }
    }

    #[test]
    fn the_switcher_fits_its_area_in_one_row_when_it_can() {
        let area = r(0, 0, 1280, 800);
        let (panel, cards) = switcher_layout(&[s(900, 600); 3], area);
        assert_eq!(cards.len(), 3);
        assert!(area.contains_rect(panel));
        no_overlap(&cards);
        let y = cards[0].frame.loc.y;
        assert!(cards.iter().all(|c| c.frame.loc.y == y), "one row");
        for c in &cards {
            assert!(panel.contains_rect(c.frame));
        }
        // Centred.
        assert_eq!(panel.loc.x * 2 + panel.size.w, 1280);
    }

    #[test]
    fn many_windows_shrink_the_switcher_to_fit_without_overlap() {
        let area = r(0, 0, 1280, 800);
        let (panel, cards) = switcher_layout(&vec![s(900, 600); 40], area);
        assert_eq!(cards.len(), 40);
        assert!(area.contains_rect(panel), "{panel:?}");
        no_overlap(&cards);
    }

    #[test]
    fn the_strip_lays_the_desktop_grid_row_major() {
        let area = r(0, 0, 1280, 800);
        let (strip, tiles) = strip_layout(10, 2, area);
        assert_eq!(tiles.len(), 10);
        assert!(area.contains_rect(strip));
        for t in &tiles {
            assert!(strip.contains_rect(*t));
        }
        // 2 rows by 5 columns: desktop 6 is under desktop 1.
        assert_eq!(tiles[0].loc.x, tiles[5].loc.x);
        assert!(tiles[5].loc.y > tiles[0].loc.y);
        assert!(tiles[1].loc.x > tiles[0].loc.x);
        for (i, a) in tiles.iter().enumerate() {
            for b in &tiles[i + 1..] {
                assert!(!a.overlaps(*b));
            }
        }
        let (_, one_row) = strip_layout(10, 1, area);
        assert!(one_row.iter().all(|t| t.loc.y == one_row[0].loc.y));
    }

    #[test]
    fn a_preview_sits_by_its_anchor_and_stays_on_the_output() {
        let area = r(0, 0, 1280, 800);
        let src = s(1000, 700);
        // A chip on a bottom bar: the preview opens above it.
        let chip = r(600, 770, 120, 30);
        let p = preview_layout(src, chip, area);
        assert!(p.frame.loc.y + p.frame.size.h <= chip.loc.y);
        assert!(area.contains_rect(p.frame));
        // A chip on a top bar: below it.
        let top = r(600, 0, 120, 30);
        let p = preview_layout(src, top, area);
        assert!(p.frame.loc.y >= 30);
        // A chip at the far corner: clamped.
        let corner = r(1200, 770, 80, 30);
        assert!(area.contains_rect(preview_layout(src, corner, area).frame));
        let left = r(0, 770, 80, 30);
        assert!(area.contains_rect(preview_layout(src, left, area).frame));
    }

    #[test]
    fn a_cover_scales_into_the_thumbnail_and_never_shrinks_below_what_it_hides() {
        let src = r(10, 20, 400, 300);
        let thumb = r(100, 100, 100, 75);
        let cover = r(110, 70, 100, 60);
        let c = cover_in_thumb(src, cover, thumb).unwrap();
        // 1/4 scale: the cover is x 25..50, y 12.5..27.5 of the thumbnail.
        assert_eq!(c, r(125, 112, 25, 16));
        assert!(thumb.contains_rect(c));
        let hidden_x0 = f64::from(thumb.loc.x) + 25.0;
        assert!(f64::from(c.loc.x) <= hidden_x0);
        // Out of the thumbnail altogether: nothing.
        assert_eq!(cover_in_thumb(src, r(900, 900, 10, 10), thumb), None);
        // The whole source is covered by the whole thumbnail.
        assert_eq!(cover_in_thumb(src, src, thumb), Some(thumb));
    }

    #[test]
    fn only_a_pixels_card_may_draw_a_thumbnail() {
        let mut c = CardView {
            id: 1,
            frame: r(0, 0, 10, 10),
            thumb: r(0, 0, 10, 10),
            title: None,
            body: Body::Hidden,
            hover: false,
            selected: false,
            minimized: false,
        };
        assert!(!c.wants_thumb());
        c.body = Body::Pixels { covers: vec![] };
        assert!(c.wants_thumb());
    }

    #[test]
    fn hit_tests_find_the_card_and_tile_under_a_point() {
        let view = OverlayView {
            cards: vec![CardView {
                id: 9,
                frame: r(10, 10, 100, 80),
                thumb: r(18, 18, 84, 40),
                title: None,
                body: Body::Hidden,
                hover: false,
                selected: false,
                minimized: false,
            }],
            tiles: vec![TileView {
                rect: r(200, 10, 50, 30),
                index: 3,
                current: false,
                hover: false,
                target: false,
                windows: 0,
            }],
            ..Default::default()
        };
        assert_eq!(view.card_at(Point::from((50.0, 50.0))), Some(0));
        assert_eq!(view.card_at(Point::from((150.0, 50.0))), None);
        assert_eq!(view.tile_at(Point::from((210.0, 20.0))), Some(0));
    }

    #[test]
    fn painting_differs_by_state_and_fills_the_card() {
        let look = |selected, title: Option<&str>| Look {
            w: 120,
            h: 100,
            dev: 1,
            what: What::Card {
                thumb: s(104, 60),
                title: title.map(Into::into),
                hidden: false,
                hover: false,
                selected,
                minimized: false,
            },
        };
        let plain = paint(&look(false, Some("a title")));
        assert_eq!(plain.px.len(), 120 * 100 * 4);
        assert_ne!(
            plain.px,
            paint(&look(true, Some("a title"))).px,
            "selection shows"
        );
        assert_ne!(
            plain.px,
            paint(&look(false, None)).px,
            "a title is drawn only when given"
        );
    }

    // ---- a picture for a human (the overlays are absent from grim by design)

    struct Canvas {
        w: usize,
        h: usize,
        px: Vec<[f32; 3]>,
    }

    impl Canvas {
        fn new(w: usize, h: usize) -> Canvas {
            // A stand-in for the desktop behind: a warm dark wash.
            let px = (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
                    [0.10 + 0.10 * x, 0.12 + 0.08 * y, 0.18 + 0.10 * (1.0 - y)]
                })
                .collect();
            Canvas { w, h, px }
        }

        fn fill(&mut self, r: Rect, c: [f32; 4]) {
            for y in r.loc.y.max(0)..(r.loc.y + r.size.h).min(self.h as i32) {
                for x in r.loc.x.max(0)..(r.loc.x + r.size.w).min(self.w as i32) {
                    let d = &mut self.px[y as usize * self.w + x as usize];
                    for (k, v) in d.iter_mut().enumerate() {
                        *v = c[k] * c[3] + *v * (1.0 - c[3]);
                    }
                }
            }
        }

        /// A stand-in thumbnail: a window with a bar and some text lines.
        fn thumb(&mut self, r: Rect, hue: f32) {
            for y in 0..r.size.h {
                for x in 0..r.size.w {
                    let (u, v) = (x as f32 / r.size.w as f32, y as f32 / r.size.h as f32);
                    let c = if v < 0.09 {
                        [0.20, 0.20, 0.22]
                    } else if (v * 40.0) as i32 % 3 == 0 && u > 0.06 && u < 0.78 {
                        [0.55, 0.57, 0.62]
                    } else {
                        [0.09 + 0.05 * hue, 0.10, 0.13 + 0.04 * (1.0 - hue)]
                    };
                    let (px, py) = (r.loc.x + x, r.loc.y + y);
                    if px >= 0 && py >= 0 && (px as usize) < self.w && (py as usize) < self.h {
                        self.px[py as usize * self.w + px as usize] = c;
                    }
                }
            }
        }

        fn over(&mut self, at: Point<i32, Logical>, look: &Look) {
            let r = paint(look);
            for y in 0..r.h {
                for x in 0..r.w {
                    let (px, py) = (at.x + x, at.y + y);
                    if px < 0 || py < 0 || px as usize >= self.w || py as usize >= self.h {
                        continue;
                    }
                    let i = (y * r.w + x) as usize * 4;
                    let a = f32::from(r.px[i + 3]) / 255.0;
                    let d = &mut self.px[py as usize * self.w + px as usize];
                    for (k, v) in d.iter_mut().enumerate() {
                        *v = f32::from(r.px[i + k]) / 255.0 + *v * (1.0 - a);
                    }
                }
            }
        }

        fn ppm(&self) -> Vec<u8> {
            let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
            for p in &self.px {
                out.extend(p.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
            }
            out
        }
    }

    fn card_look(c: &CardView) -> Look {
        Look {
            w: c.frame.size.w,
            h: c.frame.size.h,
            dev: 1,
            what: What::Card {
                thumb: c.thumb.size,
                title: c.title.clone(),
                hidden: !c.wants_thumb(),
                hover: c.hover,
                selected: c.selected,
                minimized: c.minimized,
            },
        }
    }

    fn put_card(cv: &mut Canvas, c: &CardView, hue: f32) {
        cv.over(c.frame.loc, &card_look(c));
        if let Body::Pixels { covers } = &c.body {
            cv.thumb(c.thumb, hue);
            for r in covers {
                cv.fill(*r, [0.07, 0.06, 0.05, 1.0]);
            }
        }
    }

    fn card(id: u64, p: Placed, title: Option<&str>, body: Body, selected: bool, hover: bool) -> CardView {
        CardView {
            id,
            frame: p.frame,
            thumb: p.thumb,
            title: title.map(Into::into),
            body,
            hover,
            selected,
            minimized: false,
        }
    }

    /// Writes `overview.ppm`, `switcher.ppm` and `preview.ppm` into
    /// `$OVERVIEW_SHEET_DIR` for a person to look at:
    /// `OVERVIEW_SHEET_DIR=/tmp/x cargo test -p ec-abyss-render contact_sheet -- --ignored`.
    #[test]
    #[ignore = "writes pictures for a human to look at"]
    fn contact_sheet() {
        let Some(dir) = std::env::var_os("OVERVIEW_SHEET_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let (w, h) = (1280usize, 800usize);
        let out = r(0, 0, w as i32, h as i32);
        let sizes = [s(900, 600), s(500, 700), s(800, 500), s(640, 640), s(1000, 420)];
        let titles = [
            "Terminal",
            "Notes - draft",
            "Browser: a rather long window title that must be cut",
            "",
            "Editor",
        ];

        // The overview.
        let mut cv = Canvas::new(w, h);
        cv.fill(out, [0.02, 0.015, 0.01, 0.58]);
        let (strip, tiles) = strip_layout(10, 2, out);
        cv.over(
            strip.loc,
            &Look {
                w: strip.size.w,
                h: strip.size.h,
                dev: 1,
                what: What::Strip,
            },
        );
        for (i, t) in tiles.iter().enumerate() {
            cv.over(
                t.loc,
                &Look {
                    w: t.size.w,
                    h: t.size.h,
                    dev: 1,
                    what: What::Tile {
                        index: i,
                        windows: [3, 0, 1, 0, 0, 2, 0, 0, 0, 0][i],
                        current: i == 0,
                        hover: i == 2,
                        target: i == 5,
                    },
                },
            );
        }
        let top = strip.loc.y + strip.size.h + GAP;
        let area = r(MARGIN, top, w as i32 - 2 * MARGIN, h as i32 - MARGIN - 44 - top);
        let placed = present_grid(&sizes, area);
        for (i, p) in placed.iter().enumerate() {
            let (title, body) = match i {
                3 => (None, Body::Hidden),
                1 => (
                    Some(titles[i]),
                    Body::Pixels {
                        covers: vec![cover_in_thumb(r(0, 0, 500, 700), r(60, 260, 380, 60), p.thumb).unwrap()],
                    },
                ),
                _ => (Some(titles[i]), Body::Pixels { covers: vec![] }),
            };
            put_card(
                &mut cv,
                &card(i as u64, *p, title, body, false, i == 2),
                i as f32 / 5.0,
            );
        }
        let text = "notes";
        let size = pill_size(text);
        cv.over(
            Point::from(((w as i32 - size.w) / 2, h as i32 - size.h - MARGIN)),
            &Look {
                w: size.w,
                h: size.h,
                dev: 1,
                what: What::Pill(text.into()),
            },
        );
        std::fs::write(dir.join("overview.ppm"), cv.ppm()).unwrap();

        // The switcher.
        let mut cv = Canvas::new(w, h);
        let (panel, placed) = switcher_layout(&sizes, out.to_f64().to_i32_round().intersection(out).unwrap());
        cv.over(
            panel.loc,
            &Look {
                w: panel.size.w,
                h: panel.size.h,
                dev: 1,
                what: What::Panel,
            },
        );
        for (i, p) in placed.iter().enumerate() {
            let body = if i == 3 {
                Body::Hidden
            } else {
                Body::Pixels { covers: vec![] }
            };
            let title = if i == 3 { None } else { Some(titles[i]) };
            put_card(
                &mut cv,
                &card(i as u64, *p, title, body, i == 1, false),
                i as f32 / 5.0,
            );
        }
        std::fs::write(dir.join("switcher.ppm"), cv.ppm()).unwrap();

        // A hover preview over a bar at the bottom.
        let mut cv = Canvas::new(w, h);
        cv.fill(r(0, 764, w as i32, 36), [0.04, 0.035, 0.03, 0.9]);
        let chip = r(300, 770, 150, 24);
        cv.fill(chip, [1.0, 1.0, 1.0, 0.14]);
        let p = preview_layout(s(900, 600), chip, out);
        put_card(
            &mut cv,
            &card(
                1,
                p,
                Some("Terminal"),
                Body::Pixels { covers: vec![] },
                false,
                false,
            ),
            0.2,
        );
        std::fs::write(dir.join("preview.ppm"), cv.ppm()).unwrap();
    }
}
