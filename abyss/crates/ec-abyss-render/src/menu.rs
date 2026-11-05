// SPDX-License-Identifier: AGPL-3.0-only
//! The window menu under `mode "de"` (COMP-05 §7.1, C-24, ADR 0090): geometry,
//! hit test and drawing, with no `AbyssState` in reach.
//!
//! **This is not trusted UI.** Like the titlebar it is window furniture the
//! compositor draws. It is a backend-prepended overlay pass, like the Radiant
//! drop guides: it is never in a capture pass list, so a capture never sees it
//! (`render/scan_tests.rs` keeps `capture.rs` from naming it). Labels go through
//! the overlay sanitiser ([`crate::titlebar::display_title`]), the same one the
//! bar uses.
//!
//! One texture per output, rebuilt only when its rows, size, scale or hover
//! change, so moving the pointer inside one row repaints nothing.

use std::collections::HashMap;

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
    output::Output,
    utils::{Logical, Point, Rectangle, Scale, Size, Transform},
};

use crate::{
    hud::{band, cov, sd_box, Paint},
    hud_font::{self, Face},
    text::Rgba,
    titlebar::display_title,
    AbyssRenderElement,
};

/// Panel width, logical px.
pub const WIDTH: i32 = 232;
/// One row's height.
pub const ROW_H: i32 = 28;
/// Padding above the first row and below the last.
pub const PAD: i32 = 6;
/// Extra height a row with `separator_before` takes.
pub const SEP_H: i32 = 9;
const RADIUS: f32 = 11.0;
const ROW_INSET: f32 = 4.0;
const ROW_RADIUS: f32 = 7.0;
const TEXT_X: i32 = 30;

const GROUND: [f32; 3] = [0.043, 0.035, 0.024];
const GROUND_ALPHA: f32 = 0.86;
/// The panel's top light: one inside hairline under the rim, so the panel
/// reads as a lifted pane (STYLE.md: top edge highlight on the blur-off fallback).
const TOP_LIGHT: Rgba = [1.0, 1.0, 1.0, 0.07];
const RIM: Rgba = [1.0, 1.0, 1.0, 0.14];
const INK: Rgba = [1.0, 1.0, 1.0, 0.94];
const INK_OFF: Rgba = [1.0, 1.0, 1.0, 0.36];
const HOVER: Rgba = [1.0, 1.0, 1.0, 0.12];
const HOVER_RIM: Rgba = [1.0, 1.0, 1.0, 0.18];
const SEPARATOR: Rgba = [1.0, 1.0, 1.0, 0.10];

/// One row of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuRow {
    pub label: String,
    /// A toggle that is on (drawn with a check).
    pub checked: bool,
    /// A disabled row is drawn dim and does nothing.
    pub enabled: bool,
    /// The row opens another page (drawn with an arrow).
    pub submenu: bool,
    /// A hairline above the row.
    pub separator_before: bool,
}

impl MenuRow {
    pub fn new(label: impl Into<String>) -> MenuRow {
        MenuRow {
            label: label.into(),
            checked: false,
            enabled: true,
            submenu: false,
            separator_before: false,
        }
    }
}

/// What the renderer draws: where the menu is, its rows, which one is lit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MenuView {
    /// Top-left, global logical coordinates.
    pub origin: Point<i32, Logical>,
    pub rows: Vec<MenuRow>,
    pub hover: Option<usize>,
}

impl MenuView {
    pub fn size(&self) -> Size<i32, Logical> {
        let seps = self.rows.iter().filter(|r| r.separator_before).count() as i32;
        Size::from((WIDTH, 2 * PAD + ROW_H * self.rows.len() as i32 + SEP_H * seps))
    }

    pub fn rect(&self) -> Rectangle<i32, Logical> {
        Rectangle::new(self.origin, self.size())
    }

    /// Row `i`'s rectangle, panel-local.
    pub fn row_rect_local(&self, i: usize) -> Rectangle<i32, Logical> {
        let mut y = PAD;
        for (n, r) in self.rows.iter().enumerate() {
            if r.separator_before {
                y += SEP_H;
            }
            if n == i {
                return Rectangle::new(Point::from((0, y)), Size::from((WIDTH, ROW_H)));
            }
            y += ROW_H;
        }
        Rectangle::default()
    }

    /// The row under `pos` (global logical), if any. Does not allocate.
    pub fn row_at(&self, pos: Point<f64, Logical>) -> Option<usize> {
        let local = pos - self.origin.to_f64();
        if local.x < 0.0 || local.x >= f64::from(WIDTH) {
            return None;
        }
        (0..self.rows.len()).find(|&i| {
            let r = self.row_rect_local(i);
            local.y >= f64::from(r.loc.y) && local.y < f64::from(r.loc.y + r.size.h)
        })
    }

    pub fn contains(&self, pos: Point<f64, Logical>) -> bool {
        self.rect().to_f64().contains(pos)
    }
}

/// Where a menu of `size` opened at `anchor` goes: at the anchor when it fits
/// in `area`, otherwise flipped to the other side of it, then held inside.
pub fn place(
    anchor: Point<i32, Logical>,
    size: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let mut x = anchor.x;
    let mut y = anchor.y;
    if x + size.w > area.loc.x + area.size.w {
        x -= size.w;
    }
    if y + size.h > area.loc.y + area.size.h {
        y -= size.h;
    }
    let max_x = (area.loc.x + area.size.w - size.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - size.h).max(area.loc.y);
    Point::from((x.clamp(area.loc.x, max_x), y.clamp(area.loc.y, max_y)))
}

#[derive(Debug, Clone, PartialEq)]
struct Look {
    rows: Vec<MenuRow>,
    hover: Option<usize>,
    w: i32,
    h: i32,
    dev: usize,
}

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

fn paint(look: &Look, view: &MenuView) -> Vec<u8> {
    let d = look.dev as f32;
    let (pw, ph) = (look.w as usize * look.dev, look.h as usize * look.dev);
    let mut p = Paint::new(pw, ph);
    let body = (0.0, 0.0, pw as f32, ph as f32);
    let r = RADIUS * d;
    p.fill(body, r, [GROUND[0], GROUND[1], GROUND[2], GROUND_ALPHA]);
    p.shade(body, RIM, |x, y| band(sd_box(x, y, body, r), -d, 0.0));
    p.shade((r, d, pw as f32 - 2.0 * r, d), TOP_LIGHT, |_, _| 1.0);
    let face = Face::Body;
    let cols = ((WIDTH - TEXT_X - 24).max(0) as usize) / hud_font::ADVANCE;
    for (i, row) in look.rows.iter().enumerate() {
        let rr = view.row_rect_local(i);
        if row.separator_before {
            let y = (rr.loc.y - SEP_H / 2) as f32 * d;
            p.shade(
                (ROW_INSET * 2.0 * d, y, pw as f32 - ROW_INSET * 4.0 * d, d),
                SEPARATOR,
                |_, _| 1.0,
            );
        }
        let cell = (
            ROW_INSET * d,
            rr.loc.y as f32 * d,
            (WIDTH as f32 - 2.0 * ROW_INSET) * d,
            ROW_H as f32 * d,
        );
        if look.hover == Some(i) && row.enabled {
            p.fill(cell, ROW_RADIUS * d, HOVER);
            let outer = (cell.0 - d, cell.1 - d, cell.2 + 2.0 * d, cell.3 + 2.0 * d);
            p.shade(outer, HOVER_RIM, |x, y| {
                band(sd_box(x, y, cell, ROW_RADIUS * d), -d, 0.0)
            });
        }
        let ink = if row.enabled { INK } else { INK_OFF };
        let mid_y = (rr.loc.y as f32 + ROW_H as f32 * 0.5) * d;
        if row.checked {
            let cx = 15.0 * d;
            let a = (cx - 4.0 * d, mid_y, cx - 1.0 * d, mid_y + 3.5 * d);
            let b = (cx - 1.0 * d, mid_y + 3.5 * d, cx + 5.0 * d, mid_y - 4.0 * d);
            let area = (cx - 8.0 * d, mid_y - 8.0 * d, 16.0 * d, 16.0 * d);
            p.shade(area, ink, |x, y| {
                cov(seg_dist(x, y, a) - 0.9 * d).max(cov(seg_dist(x, y, b) - 0.9 * d))
            });
        }
        let top = (rr.loc.y as usize) + (ROW_H as usize).saturating_sub(face.line_h()) / 2;
        let label = display_title(&row.label, cols);
        p.text(face, look.dev, TEXT_X as usize, top, &label, ink);
        if row.submenu {
            let cx = (WIDTH - 16) as f32 * d;
            let a = (cx - 2.5 * d, mid_y - 4.0 * d, cx + 2.0 * d, mid_y);
            let b = (cx + 2.0 * d, mid_y, cx - 2.5 * d, mid_y + 4.0 * d);
            let area = (cx - 8.0 * d, mid_y - 8.0 * d, 16.0 * d, 16.0 * d);
            p.shade(area, ink, |x, y| {
                cov(seg_dist(x, y, a) - 0.9 * d).max(cov(seg_dist(x, y, b) - 0.9 * d))
            });
        }
    }
    let raster = p.into_raster();
    let mut out = raster.px;
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    out
}

struct Slot {
    look: Look,
    buffer: MemoryRenderBuffer,
}

/// The menu's textures, one per output.
#[derive(Default)]
pub struct MenuCache {
    slots: HashMap<Output, Slot>,
}

impl std::fmt::Debug for MenuCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuCache").finish_non_exhaustive()
    }
}

/// The menu's elements for `output`, front-to-back. Empty when no menu is open
/// or it does not reach this output.
pub fn menu_elements(
    renderer: &mut GlesRenderer,
    cache: &mut MenuCache,
    view: Option<&MenuView>,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    let Some(view) = view else {
        cache.slots.clear();
        return Vec::new();
    };
    let rect = view.rect();
    let bounds = Rectangle::new(
        output_loc,
        output.current_mode().map_or(rect.size, |m| {
            let s = output.current_scale().fractional_scale();
            Size::from((
                (f64::from(m.size.w) / s).round() as i32,
                (f64::from(m.size.h) / s).round() as i32,
            ))
        }),
    );
    if !bounds.overlaps(rect) {
        return Vec::new();
    }
    let scale = Scale::from(output.current_scale().fractional_scale());
    let dev = crate::titlebar::raster_scale(scale);
    let look = Look {
        rows: view.rows.clone(),
        hover: view.hover,
        w: rect.size.w,
        h: rect.size.h,
        dev,
    };
    let size = (rect.size.w * dev as i32, rect.size.h * dev as i32);
    if cache.slots.get(output).is_some_and(|s| s.look.dev != dev) {
        cache.slots.remove(output);
    }
    let slot = match cache.slots.get_mut(output) {
        Some(slot) => {
            if slot.look != look {
                let px = paint(&look, view);
                let mut ctx = slot.buffer.render();
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
                &paint(&look, view),
                Fourcc::Argb8888,
                size,
                dev as i32,
                Transform::Normal,
                None,
            );
            cache.slots.entry(output.clone()).or_insert(Slot { look, buffer })
        }
    };
    let loc = (rect.loc - output_loc)
        .to_f64()
        .to_physical(scale)
        .to_i32_round::<i32>();
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        loc.to_f64(),
        &slot.buffer,
        None,
        None,
        Some(rect.size),
        Kind::Unspecified,
    )
    .map(|e| vec![AbyssRenderElement::Memory(e)])
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(n: usize) -> MenuView {
        MenuView {
            origin: Point::from((100, 50)),
            rows: (0..n).map(|i| MenuRow::new(format!("row {i}"))).collect(),
            hover: None,
        }
    }

    fn area() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((0, 0)), Size::from((800, 600)))
    }

    #[test]
    fn the_size_counts_rows_padding_and_separators() {
        let mut v = view(3);
        assert_eq!(v.size(), Size::from((WIDTH, 2 * PAD + 3 * ROW_H)));
        v.rows[1].separator_before = true;
        assert_eq!(v.size().h, 2 * PAD + 3 * ROW_H + SEP_H);
    }

    #[test]
    fn row_at_finds_the_row_and_nothing_outside() {
        let v = view(3);
        let at = |x: f64, y: f64| v.row_at(Point::from((x, y)));
        assert_eq!(at(110.0, 50.0 + f64::from(PAD) + 1.0), Some(0));
        assert_eq!(at(110.0, 50.0 + f64::from(PAD + ROW_H) + 1.0), Some(1));
        assert_eq!(at(110.0, 50.0 + f64::from(PAD + 2 * ROW_H) + 1.0), Some(2));
        assert_eq!(at(110.0, 50.0 + 2.0), None, "the top padding is no row");
        assert_eq!(at(99.0, 80.0), None, "left of the panel");
        assert_eq!(at(100.0 + f64::from(WIDTH), 80.0), None, "right of the panel");
    }

    #[test]
    fn a_separator_pushes_the_rows_below_it() {
        let mut v = view(2);
        v.rows[1].separator_before = true;
        assert_eq!(v.row_rect_local(1).loc.y, PAD + ROW_H + SEP_H);
        let y = 50.0 + f64::from(PAD + ROW_H) + 2.0;
        assert_eq!(v.row_at(Point::from((110.0, y))), None, "the separator is no row");
    }

    #[test]
    fn placement_flips_and_clamps_inside_the_area() {
        let size = Size::from((WIDTH, 200));
        assert_eq!(place(Point::from((10, 10)), size, area()), Point::from((10, 10)));
        // Past the right edge: opens leftwards of the anchor.
        assert_eq!(place(Point::from((700, 10)), size, area()).x, 700 - WIDTH);
        // Past the bottom: opens upwards.
        assert_eq!(place(Point::from((10, 500)), size, area()).y, 300);
        // Bigger than the area: held at its origin.
        let big = Size::from((WIDTH, 900));
        assert_eq!(place(Point::from((10, 10)), big, area()).y, 0);
    }

    #[test]
    fn painting_fills_the_panel_and_lights_only_the_hovered_enabled_row() {
        let mut v = view(2);
        v.rows[1].enabled = false;
        let look = |hover| Look {
            rows: v.rows.clone(),
            hover,
            w: v.size().w,
            h: v.size().h,
            dev: 1,
        };
        let plain = paint(&look(None), &v);
        let lit = paint(&look(Some(0)), &v);
        let off = paint(&look(Some(1)), &v);
        assert_eq!(plain.len(), (v.size().w * v.size().h * 4) as usize);
        assert_ne!(plain, lit, "hover repaints");
        assert_eq!(plain, off, "a disabled row is not lit");
    }
}
