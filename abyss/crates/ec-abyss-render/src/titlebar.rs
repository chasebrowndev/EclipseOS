// SPDX-License-Identifier: AGPL-3.0-only
//! Titlebars under `mode "de"` (COMP-05 §7.1, C-24, ADR 0090): the geometry,
//! the hit test and the drawing, with no `AbyssState` in reach.
//!
//! **This is not trusted UI.** It is window furniture the compositor draws,
//! and it carries client text. It uses the overlay face ([`crate::hud_font`],
//! G-02) and none of COMP-10's palette, layout or phrase; titles go through the
//! overlay sanitiser, [`text::sanitize_annotation_line`].
//!
//! Geometry. A toplevel's geometry stays the client's content rect. The *frame*
//! is the content with a bar of `height` logical px above it; the shell's
//! `Floating.rect` is the frame grown by the border ([`outer_of_content`]).
//! Everything that places a window works on frames; only what is told to the
//! client, to `ec-ctl` and to agents is the content ([`content_of_outer`]).
//!
//! The bar is one texture per window and output, rebuilt only when its title,
//! size, scale or button state changes, so a title change damages the bar and
//! nothing else.

use std::{cell::Cell, collections::HashMap};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::GlesRenderer,
        },
    },
    desktop::Window,
    output::Output,
    utils::{Logical, Point, Rectangle, Scale, Size, Transform},
};

use crate::{
    hud::{band, cov, sd_box, Paint},
    hud_font::{self, Face},
    text::{self, Rgba},
};

/// One of the three buttons, left to right as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

/// What a point on a bar is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The bar outside any button.
    Bar,
    Button(Button),
}

/// What the renderer needs to know about one window's bar. Written by the
/// shell on every layout pass and on a hover change; `None` (no data) means
/// the window has no bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BarInfo {
    /// Bar height, logical px.
    pub height: i32,
    pub hover: Option<Button>,
    pub pressed: Option<Button>,
    pub maximized: bool,
    /// Rolled up to its bar (`shell::winops`): the renderer draws the bar and
    /// nothing under it, rounded on all four corners.
    pub shaded: bool,
}

/// The window's [`BarInfo`], in its user data.
pub struct BarData(pub Cell<Option<BarInfo>>);

/// `window`'s bar, if it has one.
pub fn bar_of(window: &Window) -> Option<BarInfo> {
    window.user_data().get::<BarData>().and_then(|b| b.0.get())
}

/// Set (or clear) `window`'s bar. True if that changed what is drawn.
pub fn set_bar(window: &Window, info: Option<BarInfo>) -> bool {
    let data = window.user_data();
    data.insert_if_missing(|| BarData(Cell::new(None)));
    let Some(b) = data.get::<BarData>() else {
        return false;
    };
    let changed = b.0.get() != info;
    b.0.set(info);
    changed
}

// ------------------------------------------------------------- geometry

/// A button's width: a little wider than the bar is tall, so the close
/// button at the corner is easy to hit.
pub fn button_width(height: i32) -> i32 {
    height + 8
}

/// `b`'s hit rectangle, bar-local (origin at the bar's top-left). Close is
/// flush right, then maximize, then minimize.
pub fn button_rect(width: i32, height: i32, b: Button) -> Rectangle<i32, Logical> {
    let bw = button_width(height);
    let slot = match b {
        Button::Close => 1,
        Button::Maximize => 2,
        Button::Minimize => 3,
    };
    Rectangle::new(Point::from((width - slot * bw, 0)), Size::from((bw, height)))
}

/// What the bar-local point `p` is over on a bar `width` x `height`, or
/// `None` outside the bar. No allocation: it runs on pointer motion.
pub fn hit(width: i32, height: i32, p: Point<f64, Logical>) -> Option<Hit> {
    if p.x < 0.0 || p.y < 0.0 || p.x >= f64::from(width) || p.y >= f64::from(height) {
        return None;
    }
    for b in [Button::Close, Button::Maximize, Button::Minimize] {
        let r = button_rect(width, height, b);
        if p.x >= f64::from(r.loc.x) && p.x < f64::from(r.loc.x + r.size.w) {
            return Some(Hit::Button(b));
        }
    }
    Some(Hit::Bar)
}

/// The bar's rectangle above a content rect.
pub fn bar_rect(content: Rectangle<i32, Logical>, height: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((content.loc.x, content.loc.y - height)),
        Size::from((content.size.w, height)),
    )
}

/// The decorated rect (bar plus content, no border) of a content rect. What
/// the shadow, border and rounded mask follow.
pub fn frame_of_content(content: Rectangle<i32, Logical>, height: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((content.loc.x, content.loc.y - height)),
        Size::from((content.size.w, content.size.h + height)),
    )
}

/// The content rect inside an outer rect (what `Floating.rect` stores): the
/// border comes off every side, then the bar off the top. `bar = 0` is the
/// old inset exactly.
pub fn content_of_outer(outer: Rectangle<i32, Logical>, border: i32, bar: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((outer.loc.x + border, outer.loc.y + border + bar)),
        Size::from((
            (outer.size.w - 2 * border).max(1),
            (outer.size.h - 2 * border - bar).max(1),
        )),
    )
}

/// The inverse of [`content_of_outer`].
pub fn outer_of_content(content: Rectangle<i32, Logical>, border: i32, bar: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((content.loc.x - border, content.loc.y - border - bar)),
        Size::from((content.size.w + 2 * border, content.size.h + 2 * border + bar)),
    )
}

// ------------------------------------------------------------- painting

const PAD: i32 = 12;

/// The bar's material, the window tint of STYLE.md (warm black, never
/// blue-grey) over the backdrop blur the renderer puts behind the bar. The
/// focused bar is a step clearer than the unfocused one: more tint over the
/// blur plus a lift of white and a crisper seam, and nothing else. No gold, no
/// glow, no gradient (STYLE.md, "glassy, not glowy").
const GROUND: [f32; 3] = [0.043, 0.035, 0.024];
const GROUND_ALPHA: f32 = 0.70;
const GROUND_ALPHA_IDLE: f32 = 0.52;
const LIFT: Rgba = [1.0, 1.0, 1.0, 0.055];
/// The seam where the bar meets the window body.
const SEAM: Rgba = [1.0, 1.0, 1.0, 0.12];
const SEAM_IDLE: Rgba = [1.0, 1.0, 1.0, 0.06];
/// Ink: title and glyphs, STYLE.md text steps.
const INK: Rgba = [1.0, 1.0, 1.0, 0.94];
const INK_IDLE: Rgba = [1.0, 1.0, 1.0, 0.50];
const GLYPH: Rgba = [1.0, 1.0, 1.0, 0.82];
const GLYPH_IDLE: Rgba = [1.0, 1.0, 1.0, 0.46];
/// A hovered or pressed button: the bar-cell step (fill and 1px rim), a
/// step clearer pressed.
const CELL_HOVER: (Rgba, Rgba) = ([1.0, 1.0, 1.0, 0.10], [1.0, 1.0, 1.0, 0.16]);
const CELL_PRESS: (Rgba, Rgba) = ([1.0, 1.0, 1.0, 0.17], [1.0, 1.0, 1.0, 0.24]);
/// Close is the one button that reads the danger colour (#e5484d), as a tinted
/// glass cell and not a solid fill.
const CLOSE_HOVER: (Rgba, Rgba) = ([0.90, 0.28, 0.30, 0.34], [0.95, 0.40, 0.42, 0.60]);
const CLOSE_PRESS: (Rgba, Rgba) = ([0.90, 0.28, 0.30, 0.52], [0.95, 0.40, 0.42, 0.75]);
/// The hovered button's cell: inset from the bar's edge, rounded.
const CELL_INSET: f32 = 4.0;
const CELL_RADIUS: f32 = 7.0;

/// The title as it is drawn: sanitised for the overlay face, one line, cut to
/// `cols` columns with an ellipsis when it does not fit.
pub fn display_title(raw: &str, cols: usize) -> String {
    let clean = text::sanitize_annotation_line(raw);
    let n = clean.chars().count();
    if n <= cols {
        return clean;
    }
    if cols == 0 {
        return String::new();
    }
    let ellipsis = if hud_font::covers('\u{2026}') {
        '\u{2026}'
    } else {
        '.'
    };
    let mut out: String = clean.chars().take(cols - 1).collect();
    out.push(ellipsis);
    out
}

/// Everything that decides the bar's pixels.
#[derive(Debug, Clone, PartialEq)]
struct Look {
    title: String,
    w: i32,
    h: i32,
    dev: usize,
    radius: i32,
    hover: Option<Button>,
    pressed: Option<Button>,
    maximized: bool,
    shaded: bool,
    active: bool,
}

fn paint(look: &Look) -> Vec<u8> {
    let d = look.dev as f32;
    let (pw, ph) = (look.w as usize * look.dev, look.h as usize * look.dev);
    let mut p = Paint::new(pw, ph);
    // Rounded on top only: a box twice as tall, cut off by the canvas.
    let r = look.radius as f32 * d;
    // A shaded window is only its bar, so all four corners round.
    let body = (
        0.0,
        0.0,
        pw as f32,
        if look.shaded {
            ph as f32
        } else {
            ph as f32 + r * 2.0
        },
    );
    let ground = [
        GROUND[0],
        GROUND[1],
        GROUND[2],
        if look.active {
            GROUND_ALPHA
        } else {
            GROUND_ALPHA_IDLE
        },
    ];
    p.fill(body, r, ground);
    if look.active {
        p.fill(body, r, LIFT);
    }
    if !look.shaded {
        p.shade(
            (0.0, ph as f32 - d, pw as f32, d),
            if look.active { SEAM } else { SEAM_IDLE },
            |_, _| 1.0,
        );
    }

    let ink = if look.active { GLYPH } else { GLYPH_IDLE };
    for b in [Button::Minimize, Button::Maximize, Button::Close] {
        let rect = button_rect(look.w, look.h, b);
        let (cx, cy) = (
            (rect.loc.x as f32 + rect.size.w as f32 * 0.5) * d,
            rect.size.h as f32 * 0.5 * d,
        );
        let state = if look.pressed == Some(b) {
            Some(true)
        } else if look.hover == Some(b) && look.pressed.is_none() {
            Some(false)
        } else {
            None
        };
        let mut icon = ink;
        if let Some(down) = state {
            let (fill, rim) = match (b, down) {
                (Button::Close, false) => CLOSE_HOVER,
                (Button::Close, true) => CLOSE_PRESS,
                (_, false) => CELL_HOVER,
                (_, true) => CELL_PRESS,
            };
            let cell = (
                (rect.loc.x as f32 + CELL_INSET) * d,
                CELL_INSET * d,
                (rect.size.w as f32 - 2.0 * CELL_INSET) * d,
                (rect.size.h as f32 - 2.0 * CELL_INSET) * d,
            );
            let (cr, outer) = (
                CELL_RADIUS * d,
                (cell.0 - d, cell.1 - d, cell.2 + 2.0 * d, cell.3 + 2.0 * d),
            );
            p.fill(cell, cr, fill);
            p.shade(outer, rim, |x, y| band(sd_box(x, y, cell, cr), -d, 0.0));
            icon = [1.0, 1.0, 1.0, 0.96];
        }
        let half = 5.0 * d;
        let stroke = 0.8 * d;
        let area = (
            cx - half - 2.0 * d,
            cy - half - 2.0 * d,
            2.0 * (half + 2.0 * d),
            2.0 * (half + 2.0 * d),
        );
        match b {
            Button::Minimize => {
                let seg = (cx - half, cy + 3.0 * d, cx + half, cy + 3.0 * d);
                p.shade(area, icon, |x, y| cov(seg_dist(x, y, seg) - stroke));
            }
            Button::Maximize if look.maximized => {
                // Restore: a square in front of the corner of one behind.
                let front = (
                    cx - half,
                    cy - half + 2.0 * d,
                    2.0 * half - 2.0 * d,
                    2.0 * half - 2.0 * d,
                );
                let back = (
                    cx - half + 2.0 * d,
                    cy - half,
                    2.0 * half - 2.0 * d,
                    2.0 * half - 2.0 * d,
                );
                p.shade(area, icon, |x, y| {
                    let behind =
                        band(sd_box(x, y, back, d), -stroke * 1.4, 0.0) * (1.0 - cov(sd_box(x, y, front, d)));
                    behind.max(band(sd_box(x, y, front, d), -stroke * 1.4, 0.0))
                });
            }
            Button::Maximize => {
                let sq = (cx - half, cy - half, 2.0 * half, 2.0 * half);
                p.shade(area, icon, |x, y| band(sd_box(x, y, sq, d), -stroke * 1.4, 0.0));
            }
            Button::Close => {
                let a = (cx - half, cy - half, cx + half, cy + half);
                let b2 = (cx - half, cy + half, cx + half, cy - half);
                p.shade(area, icon, |x, y| {
                    cov(seg_dist(x, y, a) - stroke).max(cov(seg_dist(x, y, b2) - stroke))
                });
            }
        }
    }

    // Title: left, vertically centred on the face's line box; it stops short
    // of the leftmost button.
    let left = button_rect(look.w, look.h, Button::Minimize).loc.x;
    if !look.title.is_empty() && left > PAD {
        let face = Face::Body;
        let top = ((look.h as usize).saturating_sub(face.line_h())) / 2;
        let ink = if look.active { INK } else { INK_IDLE };
        p.text(face, look.dev, PAD as usize, top, &look.title, ink);
    }
    let raster = p.into_raster();
    // Premultiplied RGBA -> Argb8888 (little-endian B, G, R, A).
    let mut out = raster.px;
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    out
}

/// Distance from `(x, y)` to the segment `(x0, y0)-(x1, y1)`.
fn seg_dist(x: f32, y: f32, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> f32 {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (px, py) = (x0 + t * dx - x, y0 + t * dy - y);
    (px * px + py * py).sqrt()
}

/// The device scale the bar is painted at: the output scale rounded up, so
/// the GPU only ever scales down.
pub fn raster_scale(scale: Scale<f64>) -> usize {
    (scale.x.max(scale.y).ceil() as usize).clamp(1, 4)
}

// ---------------------------------------------------------------- cache

struct Slot {
    look: Look,
    buffer: MemoryRenderBuffer,
}

/// The bars' textures, one per output and window. The capture pass keeps its own
/// cache, so a capture never evicts the physical output's.
#[derive(Default)]
pub struct BarCache {
    slots: HashMap<(Output, Window), Slot>,
}

impl BarCache {
    /// Drop the slots of windows no longer drawn on `output`.
    pub fn retain_live(&mut self, output: &Output, live: &[Window]) {
        self.slots.retain(|(o, w), _| o != output || live.contains(w));
    }

    pub fn remove(&mut self, window: &Window) {
        self.slots.retain(|(_, w), _| w != window);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.len()
    }
}

/// `window`'s title: xdg title or X11 `WM_NAME`. Empty when it has none.
fn title_of(window: &Window) -> String {
    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
    if let Some(toplevel) = window.toplevel() {
        return smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|d| d.lock().ok().and_then(|d| d.title.clone()))
        })
        .unwrap_or_default();
    }
    window.x11_surface().map(|x| x.title()).unwrap_or_default()
}

/// One window's titlebar as a render element, or `None` when it cannot be
/// built (no room, or the upload failed).
///
/// `bar` is the bar's rectangle: `loc` output-local physical, `size.w` its
/// logical width and `size.h` the bar height (taken from `info.height`).
/// `radius` is the logical corner radius of the top corners, 0 for a square
/// (maximized) bar. `alpha` is the window's. A redacted window never gets here:
/// the capture pass covers its bar with the placeholder (COMP-02 §8).
///
/// The texture is rebuilt only when something that decides its pixels changed.
#[allow(clippy::too_many_arguments)]
pub fn bar_element(
    renderer: &mut GlesRenderer,
    cache: &mut BarCache,
    output: &Output,
    window: &Window,
    info: BarInfo,
    loc: Point<i32, smithay::utils::Physical>,
    width: i32,
    scale: Scale<f64>,
    active: bool,
    radius: i32,
    alpha: f32,
) -> Option<MemoryRenderBufferRenderElement<GlesRenderer>> {
    if width <= 0 || info.height <= 0 {
        return None;
    }
    let dev = raster_scale(scale);
    let left = button_rect(width, info.height, Button::Minimize).loc.x;
    let cols = ((left - 2 * PAD).max(0) as usize) / hud_font::ADVANCE;
    let raw = title_of(window);
    let look = Look {
        title: display_title(&raw, cols),
        w: width,
        h: info.height,
        dev,
        radius,
        hover: info.hover,
        pressed: info.pressed,
        maximized: info.maximized,
        shaded: info.shaded,
        active,
    };
    let key = (output.clone(), window.clone());
    let size = (width * dev as i32, info.height * dev as i32);
    if cache.slots.get(&key).is_some_and(|s| s.look.dev != dev) {
        // The scale changed: the buffer carries its scale, so start over.
        cache.slots.remove(&key);
    }
    let slot = match cache.slots.get_mut(&key) {
        Some(slot) => {
            if slot.look != look {
                let px = paint(&look);
                let mut ctx = slot.buffer.render();
                // A new size resets the buffer; a new scale was dropped above.
                ctx.resize(size);
                let _ = ctx.draw(|data| {
                    data.copy_from_slice(&px);
                    Ok::<_, std::convert::Infallible>(vec![Rectangle::from_size(Size::from(size))])
                });
                drop(ctx);
                slot.look = look;
            }
            slot
        }
        None => {
            let buffer = MemoryRenderBuffer::from_slice(
                &paint(&look),
                Fourcc::Argb8888,
                size,
                dev as i32,
                Transform::Normal,
                None,
            );
            cache.slots.entry(key).or_insert(Slot { look, buffer })
        }
    };
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        loc.to_f64(),
        &slot.buffer,
        Some(alpha),
        None,
        Some(Size::from((width, info.height))),
        Kind::Unspecified,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn content_and_outer_round_trip() {
        for bar in [0, 24, 32] {
            for border in [0, 2, 5] {
                let content = r(100, 200, 640, 480);
                let outer = outer_of_content(content, border, bar);
                assert_eq!(content_of_outer(outer, border, bar), content);
            }
        }
        // No bar: exactly the old border inset.
        assert_eq!(content_of_outer(r(0, 0, 100, 80), 2, 0), r(2, 2, 96, 76));
    }

    #[test]
    fn the_bar_sits_above_the_content_and_the_frame_covers_both() {
        let content = r(10, 100, 300, 200);
        assert_eq!(bar_rect(content, 32), r(10, 68, 300, 32));
        assert_eq!(frame_of_content(content, 32), r(10, 68, 300, 232));
    }

    #[test]
    fn hit_finds_the_buttons_from_the_right_edge() {
        let (w, h) = (400, 32);
        let bw = button_width(h);
        let at = |x: i32, y: i32| hit(w, h, Point::from((f64::from(x) + 0.5, f64::from(y) + 0.5)));
        assert_eq!(at(w - 1, 0), Some(Hit::Button(Button::Close)));
        assert_eq!(at(w - bw, 31), Some(Hit::Button(Button::Close)));
        assert_eq!(at(w - bw - 1, 5), Some(Hit::Button(Button::Maximize)));
        assert_eq!(at(w - 2 * bw - 1, 5), Some(Hit::Button(Button::Minimize)));
        assert_eq!(at(w - 3 * bw - 1, 5), Some(Hit::Bar));
        assert_eq!(at(0, 0), Some(Hit::Bar));
    }

    #[test]
    fn hit_is_none_outside_the_bar() {
        let p = |x: f64, y: f64| hit(400, 32, Point::from((x, y)));
        assert_eq!(p(-0.1, 5.0), None);
        assert_eq!(p(5.0, -0.1), None);
        assert_eq!(p(400.0, 5.0), None);
        assert_eq!(p(5.0, 32.0), None);
    }

    #[test]
    fn the_buttons_tile_the_bar_end_without_gaps() {
        let (w, h) = (300, 28);
        let rects = [Button::Minimize, Button::Maximize, Button::Close].map(|b| button_rect(w, h, b));
        assert_eq!(rects[0].loc.x + rects[0].size.w, rects[1].loc.x);
        assert_eq!(rects[1].loc.x + rects[1].size.w, rects[2].loc.x);
        assert_eq!(rects[2].loc.x + rects[2].size.w, w);
    }

    #[test]
    fn titles_are_sanitised_and_truncated_with_an_ellipsis() {
        assert_eq!(display_title("hello", 10), "hello");
        assert_eq!(display_title("a\u{1b}[31mb\r\nc\u{202e}d", 20), "a[31mb cd");
        let cut = display_title("0123456789", 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.starts_with("0123"));
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(display_title("anything", 0), "");
    }

    #[test]
    fn what_the_overlay_face_cannot_draw_becomes_a_question_mark() {
        // Latin-1 and Greek draw; CJK, Cyrillic and emoji do not.
        assert_eq!(display_title("café Σ", 20), "café Σ");
        assert_eq!(display_title("日本語 Привет \u{1f600}", 40), "??? ?????? ?");
    }

    #[test]
    fn raster_scale_rounds_up() {
        assert_eq!(raster_scale(Scale::from(1.0)), 1);
        assert_eq!(raster_scale(Scale::from(1.25)), 2);
        assert_eq!(raster_scale(Scale::from(2.0)), 2);
    }

    fn look() -> Look {
        Look {
            title: "term".into(),
            w: 300,
            h: 32,
            dev: 1,
            radius: 9,
            hover: None,
            pressed: None,
            maximized: false,
            shaded: false,
            active: true,
        }
    }

    fn alpha_at(px: &[u8], w: i32, x: i32, y: i32) -> u8 {
        px[((y * w + x) * 4 + 3) as usize]
    }

    #[test]
    fn the_top_corners_round_and_the_bottom_stays_square() {
        let l = look();
        let px = paint(&l);
        assert_eq!(px.len(), 300 * 32 * 4);
        assert_eq!(alpha_at(&px, 300, 0, 0), 0, "top-left corner is cut");
        assert_eq!(alpha_at(&px, 300, 299, 0), 0, "top-right corner is cut");
        assert!(alpha_at(&px, 300, 0, 31) > 0, "bottom-left meets the body square");
        assert!(alpha_at(&px, 300, 299, 31) > 0);
        let square = paint(&Look { radius: 0, ..look() });
        assert!(alpha_at(&square, 300, 0, 0) > 0, "a maximized bar is square");
    }

    #[test]
    fn a_shaded_bar_rounds_all_four_corners() {
        let px = paint(&Look {
            shaded: true,
            ..look()
        });
        assert_eq!(alpha_at(&px, 300, 0, 0), 0);
        assert_eq!(alpha_at(&px, 300, 0, 31), 0, "bottom-left is cut too");
        assert_eq!(alpha_at(&px, 300, 299, 31), 0, "bottom-right is cut too");
        assert!(
            alpha_at(&px, 300, 150, 31) > 0,
            "the middle of the bar is still there"
        );
    }

    #[test]
    fn hover_and_a_title_change_the_pixels_and_an_identical_look_does_not() {
        let base = paint(&look());
        assert_eq!(base, paint(&look()));
        let hovered = paint(&Look {
            hover: Some(Button::Close),
            ..look()
        });
        assert_ne!(base, hovered);
        let retitled = paint(&Look {
            title: "other".into(),
            ..look()
        });
        assert_ne!(base, retitled);
        let blank = paint(&Look {
            title: String::new(),
            ..look()
        });
        assert_ne!(base, blank);
    }

    #[test]
    fn the_cache_starts_empty() {
        assert_eq!(BarCache::default().len(), 0);
    }
}
