// SPDX-License-Identifier: AGPL-3.0-only
//! The login layout editor's canvas: the login screen at 16:9 over its
//! wallpaper, every tile of the pending layout as a glass pane with a small
//! faithful mock of what the greeter draws in it, and under it the tray of
//! optional tiles that are not placed.
//!
//! A widget and not a stack of containers because it is a gesture and a
//! coordinate system at once: one pointer drags a tile across the preview, a
//! tray chip onto it, or a grip to resize, and every one of those has to hit
//! test against the same geometry and animate on the same clock. The widget
//! decides nothing about geometry: every landing comes from
//! [`super::layout_edit`], which asks `ec_layout`. It publishes one message per
//! finished gesture, the whole new [`Layout`].
//!
//! Motion: a held tile trails the pointer on a fast spring, a ghost hops
//! between the cells it would land on, and a dropped, resized, added or reset
//! tile settles on a bouncy spring (`ec_ui::motion`).

use std::time::{Duration, Instant};

use ec_layout::{Kind, Layout, Rect, COLS, ROWS};
use iced::advanced::graphics::geometry;
use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout as NodeLayout};
use iced::advanced::renderer;
use iced::advanced::text::{self, LineHeight, Paragraph as _, Renderer as _, Shaping, Wrapping};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{Clipboard, Renderer as _, Shell};
use iced::{
    alignment, border, mouse, touch, window, Color, Element, Event, Length, Pixels, Point, Rectangle, Shadow,
    Size, Theme, Vector,
};

use ec_ui::motion::{Animated, Curve, Motion};
use ec_ui::tokens::{color, font, layout_edit as t, login_preview as lp, motion, space};

use super::layout_edit::{self, Handle};

/// What the mocks draw with: the previewed theme's colours and the texts the
/// page is about to send.
#[derive(Clone)]
pub struct Skin {
    pub base: Color,
    pub ink: Color,
    /// A chosen picture is behind the preview: the tiles are denser.
    pub dense: bool,
    pub clock: String,
    pub greeting: String,
    pub backdrop: Option<iced::widget::image::Handle>,
}

/// The apply bar under the tray: what differs from the machine, and the two
/// ways out of it. `summary` is `None` while nothing differs, and the bar
/// slides away.
#[derive(Clone)]
pub struct Bar<Message> {
    pub summary: Option<String>,
    /// What the slot says while nothing differs, if not its default: the last
    /// run's result, and whether it went well.
    pub note: Option<(String, bool)>,
    pub can_apply: bool,
    pub applying: bool,
    pub on_apply: Message,
    pub on_revert: Message,
}

pub struct Editor<'a, Message> {
    layout: &'a Layout,
    skin: Skin,
    on_change: Box<dyn Fn(Layout) -> Message + 'a>,
    bar: Option<Bar<Message>>,
}

pub fn editor<'a, Message>(
    layout: &'a Layout,
    skin: Skin,
    on_change: impl Fn(Layout) -> Message + 'a,
) -> Editor<'a, Message> {
    Editor {
        layout,
        skin,
        on_change: Box::new(on_change),
        bar: None,
    }
}

impl<'a, Message> Editor<'a, Message> {
    /// Give the editor its apply bar.
    pub fn bar(mut self, bar: Bar<Message>) -> Self {
        self.bar = Some(bar);
        self
    }

    fn bar_label(&self) -> Option<&'static str> {
        self.bar.as_ref().map(|b| {
            if b.applying {
                "Waiting for authorisation\u{2026}"
            } else {
                "Apply"
            }
        })
    }
}

// ---------------------------------------------------------------- geometry

/// Where everything is, relative to the widget's top-left, for one width.
#[derive(Debug, Clone, PartialEq)]
struct Geo {
    w: f32,
    h: f32,
    /// The preview (the 16:9 screen).
    pv: Rectangle,
    cw: f32,
    ch: f32,
    chips: Vec<(Kind, Rectangle)>,
    /// Where the tray starts.
    tray_y: f32,
    /// The apply bar and its two buttons (revert, apply), when there is one.
    bar: Option<BarGeo>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BarGeo {
    rect: Rectangle,
    revert: Rectangle,
    apply: Rectangle,
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Clock => "Clock",
        Kind::Greeting => "Greeting",
        Kind::Users => "Accounts",
        Kind::Password => "Password",
        Kind::Power => "Power",
        Kind::Battery => "Battery",
        Kind::Wifi => "Wi-Fi",
        Kind::NowPlaying => "Now playing",
    }
}

type Measure<'a> = &'a dyn Fn(&str, f32, iced::Font) -> f32;

/// The width of `s` as the renderer will set it. Cached: the same few strings
/// are asked for on every event and frame.
fn measure(s: &str, size: f32, f: iced::Font) -> f32 {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<(String, u32, iced::Font), f32>> = RefCell::new(HashMap::new());
    }
    let key = (s.to_owned(), size.to_bits(), f);
    if let Some(w) = CACHE.with(|c| c.borrow().get(&key).copied()) {
        return w;
    }
    let w = <iced::Renderer as text::Renderer>::Paragraph::with_text(text::Text {
        content: s,
        bounds: Size::new(f32::INFINITY, size * 2.0),
        size: Pixels(size),
        line_height: LineHeight::Relative(1.0),
        font: f,
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Basic,
        wrapping: Wrapping::None,
    })
    .min_width();
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 1024 {
            c.clear();
        }
        c.insert(key, w);
    });
    w
}

fn chip_width(kind: Kind, m: Measure<'_>) -> f32 {
    // The plus mark, then the label.
    2.0 * t::CHIP_PAD + t::CHIP_SIZE + m(kind_label(kind), t::CHIP_SIZE, font::UI_MEDIUM).ceil()
}

fn geo(width: f32, offered: &[Kind], bar_label: Option<&str>) -> Geo {
    geo_with(width, offered, bar_label, &measure)
}

fn geo_with(width: f32, offered: &[Kind], bar_label: Option<&str>, m: Measure<'_>) -> Geo {
    let w = width.clamp(0.0, t::MAX_W);
    let pw = (w - 2.0 * t::PAD).max(1.0);
    let pv = Rectangle::new(Point::new(t::PAD, t::PAD), Size::new(pw, pw * t::ASPECT));
    let tray_y = pv.y + pv.height + t::TRAY_GAP;
    let (mut x, mut y) = (t::PAD, tray_y);
    let mut chips = Vec::with_capacity(offered.len());
    for &k in offered {
        let cw = chip_width(k, m);
        if x > t::PAD && x + cw > w - t::PAD {
            x = t::PAD;
            y += t::CHIP_H + t::CHIP_GAP;
        }
        chips.push((k, Rectangle::new(Point::new(x, y), Size::new(cw, t::CHIP_H))));
        x += cw + t::CHIP_GAP;
    }
    let mut h = y + t::CHIP_H + t::PAD;
    let bar = bar_label.map(|label| {
        // Beside the last row of chips when there is room for it (the tray is
        // short, so most of the time), else a row of its own.
        let free = w - t::PAD - if chips.is_empty() { t::PAD } else { x };
        let beside = free >= t::BAR_MIN_W;
        let rect = if beside {
            let lift = (t::BAR_H - t::CHIP_H) / 2.0;
            for (_, r) in chips.iter_mut().filter(|(_, r)| r.y == y) {
                r.y += lift;
            }
            h = y + t::BAR_H + t::PAD;
            let x0 = if chips.is_empty() { t::PAD } else { x };
            Rectangle::new(Point::new(x0, y), Size::new(w - t::PAD - x0, t::BAR_H))
        } else {
            let r = Rectangle::new(
                Point::new(t::PAD, y + t::CHIP_H + t::BAR_GAP),
                Size::new(pw, t::BAR_H),
            );
            h = r.y + r.height + t::PAD;
            r
        };
        let btn = |s: &str| m(s, t::BAR_TEXT, font::UI_MEDIUM).ceil() + 2.0 * t::BTN_PAD;
        let by = rect.y + (rect.height - t::BTN_H) / 2.0;
        let apply_w = btn(label);
        let apply = Rectangle::new(
            Point::new(rect.x + rect.width - t::BAR_PAD - apply_w, by),
            Size::new(apply_w, t::BTN_H),
        );
        let revert_w = btn("Revert");
        let revert = Rectangle::new(
            Point::new(apply.x - t::CHIP_GAP - revert_w, by),
            Size::new(revert_w, t::BTN_H),
        );
        BarGeo { rect, revert, apply }
    });
    Geo {
        w,
        h,
        cw: pv.width / COLS as f32,
        ch: pv.height / ROWS as f32,
        pv,
        chips,
        tray_y,
        bar,
    }
}

impl Geo {
    /// A tile (in fractional cells) as pixels relative to the widget, with the
    /// gap taken off every side.
    fn px(&self, x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        let g = t::TILE_GAP.min(w * self.cw / 4.0).min(h * self.ch / 4.0);
        Rectangle::new(
            Point::new(self.pv.x + x * self.cw + g, self.pv.y + y * self.ch + g),
            Size::new((w * self.cw - 2.0 * g).max(1.0), (h * self.ch - 2.0 * g).max(1.0)),
        )
    }

    fn rect_px(&self, r: Rect) -> Rectangle {
        self.px(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
    }

    /// A widget-relative point as fractional cells.
    fn cell(&self, p: Point) -> (f32, f32) {
        ((p.x - self.pv.x) / self.cw, (p.y - self.pv.y) / self.ch)
    }
}

/// The remove badge of a tile: a circle that overhangs the top-right corner.
fn badge_rect(tile: Rectangle) -> Rectangle {
    let d = t::BADGE.min(tile.height * 0.9).max(t::BADGE * 0.7);
    Rectangle::new(
        Point::new(tile.x + tile.width - d + t::BADGE_OUT, tile.y - t::BADGE_OUT),
        Size::new(d, d),
    )
}

/// Which grip, if any, `p` is over on a tile: the corners win over the edges,
/// and the zones shrink with the tile so a small one keeps a body to drag by.
fn grip_at(tile: Rectangle, p: Point) -> Option<Handle> {
    if !tile.contains(p) {
        return None;
    }
    let (l, r) = (p.x - tile.x, tile.x + tile.width - p.x);
    let (u, d) = (p.y - tile.y, tile.y + tile.height - p.y);
    let c = t::HIT_CORNER.min(tile.width / 3.0).min(tile.height / 3.0);
    let ex = t::HIT_EDGE.min(tile.width / 4.0);
    let ey = t::HIT_EDGE.min(tile.height / 4.0);
    Some(match (l <= c, r <= c, u <= c, d <= c) {
        (_, true, _, true) => Handle::SouthEast,
        (true, _, _, true) => Handle::SouthWest,
        (true, _, true, _) => Handle::NorthWest,
        (_, true, true, _) => Handle::NorthEast,
        _ if r <= ex => Handle::East,
        _ if l <= ex => Handle::West,
        _ if d <= ey => Handle::South,
        _ if u <= ey => Handle::North,
        _ => return None,
    })
}

// ------------------------------------------------------------------- state

/// Aim an animation, but only when the aim has changed. The editor re-states
/// every target on every event; a bounce is a tween, and a tween that is
/// re-aimed at the value it is already heading for restarts from where it is,
/// so a landing that is re-stated each frame never lands.
trait Go {
    fn go(&mut self, target: f32, m: Motion, now: Instant);
}

impl Go for Animated {
    fn go(&mut self, target: f32, m: Motion, now: Instant) {
        if self.target() != target || self.motion() != m {
            self.set_target_under(target, m, now);
        }
    }
}

const FOLLOW: Motion = Motion {
    enabled: true,
    curve: Curve::Spring,
    duration: Duration::from_millis(t::FOLLOW_MS),
};
const LAND: Motion = Motion {
    enabled: true,
    curve: Curve::Bounce,
    duration: Duration::from_millis(t::LAND_MS),
};
const HOVER: Motion = Motion {
    enabled: true,
    curve: Curve::Spring,
    duration: Duration::from_millis(t::HOVER_MS),
};
const HOP: Motion = Motion {
    enabled: true,
    curve: Curve::Spring,
    duration: Duration::from_millis(t::GHOST_MS),
};
const BAR: Motion = Motion {
    enabled: true,
    curve: Curve::Bounce,
    duration: Duration::from_millis(t::BAR_MS),
};
const DOTS: Motion = Motion {
    enabled: true,
    curve: Curve::Spring,
    duration: Duration::from_millis(t::DOTS_MS),
};

struct Slot {
    kind: Kind,
    x: Animated,
    y: Animated,
    w: Animated,
    h: Animated,
    /// 0 is gone, 1 is there; the bounce overshoots it on arrival.
    pop: Animated,
}

impl Slot {
    fn new(kind: Kind, r: Rect, pop: f32) -> Slot {
        let a = |v: i32| Animated::new(v as f32, LAND);
        let mut s = Slot {
            kind,
            x: a(r.x),
            y: a(r.y),
            w: a(r.w),
            h: a(r.h),
            pop: Animated::new(pop, LAND),
        };
        s.pop.set_target(1.0, Instant::now());
        s
    }

    fn aim(&mut self, r: Rect, m: Motion, now: Instant) {
        self.x.go(r.x as f32, m, now);
        self.y.go(r.y as f32, m, now);
        self.w.go(r.w as f32, m, now);
        self.h.go(r.h as f32, m, now);
    }

    fn aim_f(&mut self, x: f32, y: f32, m: Motion, now: Instant) {
        self.x.go(x, m, now);
        self.y.go(y, m, now);
    }

    fn tick(&mut self, now: Instant) {
        for a in [&mut self.x, &mut self.y, &mut self.w, &mut self.h, &mut self.pop] {
            a.tick(now);
        }
    }

    fn busy(&self) -> bool {
        [&self.x, &self.y, &self.w, &self.h, &self.pop]
            .iter()
            .any(|a| a.animating())
    }
}

struct Ghost {
    rect: Rect,
    x: Animated,
    y: Animated,
    w: Animated,
    h: Animated,
    shown: Animated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Nothing,
    Tile(Kind),
    Grip(Kind, Handle),
    Badge(Kind),
    Chip(Kind),
    Revert,
    Apply,
}

impl Hit {
    fn kind(self) -> Option<Kind> {
        match self {
            Hit::Nothing | Hit::Revert | Hit::Apply => None,
            Hit::Tile(k) | Hit::Grip(k, _) | Hit::Badge(k) | Hit::Chip(k) => Some(k),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Grab {
    hit: Hit,
    origin: Point,
    /// Fractional cells from the tile's top-left (a move) or bottom-right (a
    /// resize) to where it was taken, so it never jumps under the pointer.
    off: (f32, f32),
    dragging: bool,
    finger: Option<touch::Finger>,
    /// Where it lands if let go now.
    target: Option<Rect>,
}

#[derive(Default)]
struct State {
    slots: Vec<Slot>,
    hov: Vec<(Kind, Animated)>,
    mounted: bool,
    hover: Option<Hit>,
    grab: Option<Grab>,
    ghost: Option<Ghost>,
    dots: Option<Animated>,
    /// The apply bar's arrival (0 away, 1 there; the bounce overshoots), the
    /// last summary it showed (so it can leave still saying it), and the
    /// hover light of its two buttons (revert, apply).
    bar: Option<Animated>,
    bar_text: String,
    bar_hot: [Option<Animated>; 2],
    /// A chip dragged over the preview: the tile it would become, centred on
    /// the pointer (widget-relative).
    float: Option<(Kind, Point)>,
    /// The last pointer position a motion reported (window coordinates), and
    /// how far a scroll has moved this widget from them.
    raw: Option<Point>,
    shift: Vector,
}

impl State {
    fn slot(&mut self, kind: Kind) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.kind == kind)
    }

    fn hov(&mut self, kind: Kind) -> &mut Animated {
        if let Some(i) = self.hov.iter().position(|(k, _)| *k == kind) {
            return &mut self.hov[i].1;
        }
        self.hov.push((kind, Animated::new(0.0, HOVER)));
        &mut self.hov.last_mut().expect("just pushed").1
    }

    fn hot(&self, kind: Kind) -> f32 {
        self.hov
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(0.0, |(_, a)| a.value().clamp(0.0, 1.0))
    }

    /// Follow the page's layout: new tiles pop in, gone ones pop out, moved
    /// ones settle. The tile in the hand is the pointer's, not the layout's.
    fn sync(&mut self, layout: &Layout, now: Instant) {
        let held = self
            .grab
            .filter(|g| g.dragging)
            .and_then(|g| g.hit.kind())
            .filter(|_| matches!(self.grab.map(|g| g.hit), Some(Hit::Tile(_) | Hit::Grip(..))));
        for tile in &layout.tiles {
            match self.slot(tile.kind) {
                Some(s) => {
                    if held != Some(tile.kind) {
                        s.aim(tile.rect, LAND, now);
                    }
                    s.pop.go(1.0, LAND, now);
                }
                None => {
                    let pop = if self.mounted { 0.0 } else { 1.0 };
                    self.slots.push(Slot::new(tile.kind, tile.rect, pop));
                }
            }
        }
        for s in &mut self.slots {
            if layout.get(s.kind).is_none() {
                s.pop.go(0.0, LAND, now);
            }
        }
        self.slots
            .retain(|s| layout.get(s.kind).is_some() || s.pop.animating() || s.pop.value() > 0.02);
        self.mounted = true;
    }

    fn tick(&mut self, now: Instant) {
        for s in &mut self.slots {
            s.tick(now);
        }
        for (_, a) in &mut self.hov {
            a.tick(now);
        }
        if let Some(g) = &mut self.ghost {
            for a in [&mut g.x, &mut g.y, &mut g.w, &mut g.h, &mut g.shown] {
                a.tick(now);
            }
            if g.shown.value() <= 0.01 && !g.shown.animating() && g.shown.target() == 0.0 {
                self.ghost = None;
            }
        }
        for a in self.bar.iter_mut().chain(self.bar_hot.iter_mut().flatten()) {
            a.tick(now);
        }
        if let Some(d) = &mut self.dots {
            d.tick(now);
            if d.value() <= 0.01 && !d.animating() && d.target() == 0.0 {
                self.dots = None;
            }
        }
    }

    fn bar_value(&self) -> f32 {
        self.bar.as_ref().map_or(0.0, Animated::value)
    }

    fn bar_hot(&self, i: usize) -> f32 {
        self.bar_hot[i]
            .as_ref()
            .map_or(0.0, |a| a.value().clamp(0.0, 1.0))
    }

    /// Slide the bar in or out, remembering what it says.
    fn show_bar(&mut self, summary: Option<&str>, now: Instant) {
        if let Some(s) = summary {
            if self.bar_text != s {
                self.bar_text = s.to_owned();
            }
        }
        let a = self.bar.get_or_insert_with(|| Animated::new(0.0, BAR));
        a.go(if summary.is_some() { 1.0 } else { 0.0 }, BAR, now);
    }

    fn light_bar(&mut self, hit: Option<Hit>, now: Instant) {
        for (i, h) in [Hit::Revert, Hit::Apply].into_iter().enumerate() {
            let a = self.bar_hot[i].get_or_insert_with(|| Animated::new(0.0, HOVER));
            let on = hit == Some(h);
            a.go(if on { 1.0 } else { 0.0 }, HOVER, now);
        }
    }

    fn busy(&self) -> bool {
        self.bar.as_ref().is_some_and(Animated::animating)
            || self.bar_hot.iter().flatten().any(Animated::animating)
            || self.slots.iter().any(Slot::busy)
            || self.hov.iter().any(|(_, a)| a.animating())
            || self
                .ghost
                .as_ref()
                .is_some_and(|g| [&g.x, &g.y, &g.w, &g.h, &g.shown].iter().any(|a| a.animating()))
            || self.dots.as_ref().is_some_and(|d| d.animating())
            || self.grab.is_some_and(|g| g.dragging)
    }

    fn set_ghost(&mut self, want: Option<Rect>, now: Instant) {
        match (want, &mut self.ghost) {
            (Some(r), Some(g)) => {
                if g.rect != r {
                    g.rect = r;
                    g.x.go(r.x as f32, HOP, now);
                    g.y.go(r.y as f32, HOP, now);
                    g.w.go(r.w as f32, HOP, now);
                    g.h.go(r.h as f32, HOP, now);
                }
                g.shown.go(1.0, HOP, now);
            }
            (Some(r), None) => {
                let a = |v: i32| Animated::new(v as f32, HOP);
                let mut shown = Animated::new(0.0, HOP);
                shown.go(1.0, HOP, now);
                self.ghost = Some(Ghost {
                    rect: r,
                    x: a(r.x),
                    y: a(r.y),
                    w: a(r.w),
                    h: a(r.h),
                    shown,
                });
            }
            (None, Some(g)) => g.shown.go(0.0, HOP, now),
            (None, None) => {}
        }
    }

    fn set_dots(&mut self, on: bool, now: Instant) {
        let d = self.dots.get_or_insert_with(|| Animated::new(0.0, DOTS));
        d.go(if on { 1.0 } else { 0.0 }, DOTS, now);
    }

    /// Light the tile (or chip) under the pointer, dim the rest.
    fn light(&mut self, hit: Option<Hit>, now: Instant) {
        let lit = hit.and_then(Hit::kind);
        let held = self.grab.and_then(|g| g.hit.kind());
        let kinds: Vec<Kind> = self
            .slots
            .iter()
            .map(|s| s.kind)
            .chain(self.hov.iter().map(|(k, _)| *k))
            .collect();
        for k in kinds {
            let on = held.map_or(lit == Some(k), |h| h == k);
            self.hov(k).go(if on { 1.0 } else { 0.0 }, HOVER, now);
        }
        if let Some(k) = lit {
            self.hov(k)
                .go(if held.is_none_or(|h| h == k) { 1.0 } else { 0.0 }, HOVER, now);
        }
    }
}

// -------------------------------------------------------------- the widget

impl<M> Editor<'_, M> {
    fn hit(&self, g: &Geo, p: Point) -> Hit {
        if let (Some(bg), Some(bar)) = (&g.bar, &self.bar) {
            if bar.summary.is_some() {
                if bg.revert.contains(p) {
                    return Hit::Revert;
                }
                if bg.apply.contains(p) && bar.can_apply {
                    return Hit::Apply;
                }
            }
        }
        for tile in &self.layout.tiles {
            let r = g.rect_px(tile.rect);
            if layout_edit::removable(tile.kind) && badge_rect(r).contains(p) {
                return Hit::Badge(tile.kind);
            }
        }
        for tile in &self.layout.tiles {
            let r = g.rect_px(tile.rect);
            if r.contains(p) {
                // A content-sized tile has no size to grip: all of it drags.
                return match grip_at(r, p).filter(|_| !tile.kind.is_intrinsic()) {
                    Some(h) => Hit::Grip(tile.kind, h),
                    None => Hit::Tile(tile.kind),
                };
            }
        }
        g.chips
            .iter()
            .find(|(_, r)| r.contains(p))
            .map_or(Hit::Nothing, |(k, _)| Hit::Chip(*k))
    }

    /// Where the grab would land the tile (or the chip's new tile) right now.
    fn landing(&self, g: &Geo, grab: &Grab, p: Point) -> Option<Rect> {
        let (cx, cy) = g.cell(p);
        match grab.hit {
            Hit::Tile(k) => {
                let (x, y) = ((cx - grab.off.0).round() as i32, (cy - grab.off.1).round() as i32);
                layout_edit::drag_to(self.layout, k, x, y)
            }
            Hit::Grip(k, h) => {
                let (fx, fy) = ((cx + grab.off.0).round() as i32, (cy + grab.off.1).round() as i32);
                layout_edit::resize_to(self.layout, k, h, fx, fy)
            }
            Hit::Chip(k) => {
                if !g.pv.contains(p) {
                    return None;
                }
                let (w, h) = layout_edit::default_size(k);
                let at = (
                    (cx - w as f32 / 2.0).round() as i32,
                    (cy - h as f32 / 2.0).round() as i32,
                );
                layout_edit::add(self.layout, k, Some(at))?.get(k)
            }
            _ => None,
        }
    }
}

fn travel(this: &Editor<'_, impl Clone>, st: &mut State, g: &Geo, b: Rectangle, p: Point, now: Instant) {
    let rel = Point::new(p.x - b.x, p.y - b.y);
    let Some(mut grab) = st.grab else { return };
    if !grab.dragging && grab.origin.distance(p) > motion::TAP_SLOP {
        if matches!(grab.hit, Hit::Badge(_) | Hit::Revert | Hit::Apply) {
            // A press that wandered off the badge is not a remove.
            st.grab = Some(Grab {
                dragging: true,
                ..grab
            });
            return;
        }
        grab.dragging = true;
        st.set_dots(true, now);
    }
    if !grab.dragging {
        st.grab = Some(grab);
        return;
    }
    grab.target = this.landing(g, &grab, rel);
    match grab.hit {
        Hit::Tile(k) => {
            if let Some(cur) = this.layout.get(k) {
                let (cx, cy) = g.cell(rel);
                let x = (cx - grab.off.0).clamp(0.0, (COLS - cur.w) as f32);
                let y = (cy - grab.off.1).clamp(0.0, (ROWS - cur.h) as f32);
                if let Some(s) = st.slot(k) {
                    s.aim_f(x, y, FOLLOW, now);
                }
            }
            st.set_ghost(grab.target, now);
        }
        Hit::Grip(k, _) => {
            let r = grab.target.or_else(|| this.layout.get(k));
            if let (Some(r), Some(s)) = (r, st.slot(k)) {
                s.aim(r, FOLLOW, now);
            }
        }
        Hit::Chip(k) => {
            st.float = g.pv.contains(rel).then_some((k, rel));
            st.set_ghost(grab.target, now);
        }
        _ => {}
    }
    st.grab = Some(grab);
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: lerp(a.r, b.r, t),
        g: lerp(a.g, b.g, t),
        b: lerp(a.b, b.b, t),
        a: lerp(a.a, b.a, t),
    }
}

impl<M: Clone> Widget<M, Theme, iced::Renderer> for Editor<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let width = limits.max().width.min(t::MAX_W);
        let g = geo(width, &layout_edit::offered(self.layout), self.bar_label());
        layout::Node::new(
            limits
                .resolve(Length::Fill, Length::Shrink, Size::new(g.w, g.h))
                .min(Size::new(g.w, g.h)),
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: NodeLayout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        _viewport: &Rectangle,
    ) {
        let b = layout.bounds();
        let g = geo(b.width, &layout_edit::offered(self.layout), self.bar_label());
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        st.sync(self.layout, now);
        st.show_bar(self.bar.as_ref().and_then(|b| b.summary.as_deref()), now);
        // The pointer as the last motion said it, not as the cursor is now: a
        // press that arrives in the same batch as the moves after it still
        // lands where it was made. Events carry window coordinates and this
        // tree lives in a scroll, so the scroll's shift (measured against the
        // cursor at each redraw, when the two agree) is added back.
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => st.raw = Some(*position),
            Event::Mouse(mouse::Event::CursorLeft) => st.raw = None,
            Event::Window(window::Event::RedrawRequested(_)) => {
                if let (Some(c), Some(r)) = (cursor.land().position(), st.raw) {
                    st.shift = Vector::new(c.x - r.x, c.y - r.y);
                }
            }
            _ => {}
        }
        let st_shift = st.shift;
        let shifted = |p: Point| Point::new(p.x + st_shift.x, p.y + st_shift.y);
        let pointer = st.raw.map(shifted);
        let here = |fallback: Point| pointer.unwrap_or_else(|| shifted(fallback));
        let rel = |p: Point| Point::new(p.x - b.x, p.y - b.y);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if st.grab.is_none() => {
                if let Some(p) = pointer.or_else(|| cursor.position()).filter(|p| b.contains(*p)) {
                    self.press(st, &g, b, p, None, shell, now);
                }
            }
            Event::Touch(touch::Event::FingerPressed { id, position }) if st.grab.is_none() => {
                let p = shifted(*position);
                if b.contains(p) {
                    self.press(st, &g, b, p, Some(*id), shell, now);
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let p = here(*position);
                if st.grab.is_some_and(|g| g.finger.is_none()) {
                    travel(self, st, &g, b, p, now);
                    shell.request_redraw();
                    shell.capture_event();
                } else if st.grab.is_none() {
                    let hit = b.contains(p).then(|| self.hit(&g, rel(p)));
                    let hit = hit.filter(|h| *h != Hit::Nothing);
                    if hit != st.hover {
                        st.hover = hit;
                        st.light(hit, now);
                        st.light_bar(hit, now);
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if st.grab.is_none() && st.hover.is_some() => {
                st.hover = None;
                st.light(None, now);
                st.light_bar(None, now);
                shell.request_redraw();
            }
            Event::Touch(touch::Event::FingerMoved { id, position })
                if st.grab.is_some_and(|g| g.finger == Some(*id)) =>
            {
                travel(self, st, &g, b, shifted(*position), now);
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if st.grab.is_some_and(|g| g.finger.is_none()) =>
            {
                let p = here(Point::ORIGIN);
                self.release(st, &g, rel(p), shell, now);
            }
            Event::Touch(touch::Event::FingerLifted { id, position })
                if st.grab.is_some_and(|g| g.finger == Some(*id)) =>
            {
                self.release(st, &g, rel(shifted(*position)), shell, now);
            }
            Event::Touch(touch::Event::FingerLost { id, .. })
                if st.grab.is_some_and(|g| g.finger == Some(*id)) =>
            {
                self.cancel(st, now);
                shell.request_redraw();
            }
            Event::Window(window::Event::RedrawRequested(at)) => {
                st.tick(*at);
            }
            _ => {}
        }
        if st.busy() {
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        _layout: NodeLayout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<State>();
        let hit = st.grab.map(|g| g.hit).or(st.hover);
        let held = st.grab.is_some_and(|g| g.dragging);
        match hit {
            Some(Hit::Grip(_, Handle::East | Handle::West)) => mouse::Interaction::ResizingHorizontally,
            Some(Hit::Grip(_, Handle::South | Handle::North)) => mouse::Interaction::ResizingVertically,
            Some(Hit::Grip(_, Handle::SouthEast | Handle::NorthWest)) => {
                mouse::Interaction::ResizingDiagonallyDown
            }
            Some(Hit::Grip(_, Handle::NorthEast | Handle::SouthWest)) => {
                mouse::Interaction::ResizingDiagonallyUp
            }
            Some(Hit::Badge(_) | Hit::Revert | Hit::Apply) => mouse::Interaction::Pointer,
            Some(Hit::Chip(_)) if held => mouse::Interaction::Grabbing,
            Some(Hit::Chip(_)) => mouse::Interaction::Pointer,
            Some(Hit::Tile(_)) if held => mouse::Interaction::Grabbing,
            Some(Hit::Tile(_)) => mouse::Interaction::Grab,
            _ => mouse::Interaction::None,
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: NodeLayout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let b = layout.bounds();
        // Layers do not inherit the scroll's clip, so every one is cut to the
        // visible part of the widget by hand.
        let Some(clip) = b.intersection(viewport) else {
            return;
        };
        let g = geo(b.width, &layout_edit::offered(self.layout), self.bar_label());
        let st = tree.state.downcast_ref::<State>();
        let at = |r: Rectangle| Rectangle::new(Point::new(b.x + r.x, b.y + r.y), r.size());
        let sk = &self.skin;
        let ink = sk.ink;
        let pv = at(g.pv);

        // The screen: the picture (or the base), a scrim, a hairline.
        renderer.with_layer(clip, |r| {
            match &sk.backdrop {
                Some(h) => {
                    let mut img = iced::advanced::image::Image::new(h.clone());
                    img.border_radius = lp::RADIUS.into();
                    r.draw_image(img, pv, clip);
                    fill(r, pv, lp::RADIUS, sk.base.scale_alpha(t::SCRIM), None);
                }
                None => fill(r, pv, lp::RADIUS, sk.base, None),
            }
            fill(
                r,
                pv,
                lp::RADIUS,
                Color::TRANSPARENT,
                Some((color::border(), space::HAIRLINE)),
            );
        });

        // The grid dots, while something is held.
        if let Some(d) = st
            .dots
            .as_ref()
            .map(|d| d.value().clamp(0.0, 1.0))
            .filter(|d| *d > 0.01)
        {
            renderer.with_layer(pv.intersection(&clip).unwrap_or(clip), |r| {
                let c = ink.scale_alpha(t::DOT_ALPHA * d);
                for i in 0..=COLS {
                    for j in 0..=ROWS {
                        let p = Point::new(pv.x + i as f32 * g.cw, pv.y + j as f32 * g.ch);
                        let dot = Rectangle::new(
                            Point::new(p.x - t::DOT / 2.0, p.y - t::DOT / 2.0),
                            Size::new(t::DOT, t::DOT),
                        );
                        fill(r, dot, t::DOT / 2.0, c, None);
                    }
                }
            });
        }

        // The ghost: where the held tile will land.
        if let Some(gh) = &st.ghost {
            let a = gh.shown.value().clamp(0.0, 1.0);
            let rect = at(g.px(gh.x.value(), gh.y.value(), gh.w.value(), gh.h.value()));
            renderer.with_layer(clip, |r| {
                fill(
                    r,
                    rect,
                    t::TILE_RADIUS,
                    ink.scale_alpha(t::GHOST_FILL * a),
                    Some((ink.scale_alpha(t::GHOST_RIM * a), t::GHOST_RIM_W)),
                );
            });
        }

        // Tiles; the one in the hand last, so it rides above the rest.
        let held = st.grab.filter(|g| g.dragging).and_then(|g| match g.hit {
            Hit::Tile(k) | Hit::Grip(k, _) => Some(k),
            _ => None,
        });
        let mut order: Vec<&Slot> = st.slots.iter().collect();
        order.sort_by_key(|s| Some(s.kind) == held);
        for s in order {
            let pop = s.pop.value();
            let alpha = pop.clamp(0.0, 1.0);
            if alpha <= 0.01 {
                continue;
            }
            let hot = st.hot(s.kind);
            let lifted = held == Some(s.kind);
            let rect = at(g.px(
                s.x.value(),
                s.y.value(),
                s.w.value().max(0.5),
                s.h.value().max(0.5),
            ));
            let scale = lerp(t::POP_FROM, 1.0, pop.max(0.0)) * if lifted { t::LIFT } else { 1.0 };
            let base = rect;
            let rect = scaled(rect, scale);
            let removable = layout_edit::removable(s.kind);
            renderer.with_layer(clip, |r| {
                if lifted {
                    r.fill_quad(
                        renderer::Quad {
                            bounds: rect,
                            border: border::rounded(t::TILE_RADIUS),
                            shadow: Shadow {
                                color: Color::BLACK.scale_alpha(t::SHADOW * alpha),
                                offset: Vector::new(0.0, t::SHADOW_DROP),
                                blur_radius: t::SHADOW_SPREAD * 2.0,
                            },
                            ..renderer::Quad::default()
                        },
                        Color::TRANSPARENT,
                    );
                }
                self.tile(r, s.kind, rect, base, hot, alpha, lifted);
                if !s.kind.is_intrinsic() {
                    grip_marks(r, rect, ink, alpha, hot, !removable);
                }
                if removable {
                    badge(
                        r,
                        badge_rect(rect),
                        sk,
                        hot,
                        alpha,
                        st.hover == Some(Hit::Badge(s.kind)),
                    );
                }
            });
        }

        // A chip in the air: the tile it would become.
        if let Some((k, p)) = st.float {
            let (w, h) = layout_edit::default_size(k);
            let (cw, ch) = (w as f32 * g.cw, h as f32 * g.ch);
            let rect = at(Rectangle::new(
                Point::new(p.x - cw / 2.0, p.y - ch / 2.0),
                Size::new(cw, ch),
            ));
            let base = inset(rect, t::TILE_GAP);
            let rect = scaled(base, t::LIFT);
            renderer.with_layer(clip, |r| {
                r.fill_quad(
                    renderer::Quad {
                        bounds: rect,
                        border: border::rounded(t::TILE_RADIUS),
                        shadow: Shadow {
                            color: Color::BLACK.scale_alpha(t::SHADOW),
                            offset: Vector::new(0.0, t::SHADOW_DROP),
                            blur_radius: t::SHADOW_SPREAD * 2.0,
                        },
                        ..renderer::Quad::default()
                    },
                    Color::TRANSPARENT,
                );
                self.tile(r, k, rect, base, 1.0, 1.0, true);
            });
        }

        // The tray.
        let offered = layout_edit::offered(self.layout);
        renderer.with_layer(clip, |r| {
            if offered.is_empty() {
                // The bar's slot beside it says everything that needs saying.
                if self.bar.is_some() {
                    return;
                }
                let row = at(Rectangle::new(
                    Point::new(t::PAD, g.tray_y),
                    Size::new(g.w - 2.0 * t::PAD, t::CHIP_H),
                ));
                put(
                    r,
                    "Everything is on the screen. Remove a tile to put it back here.",
                    row,
                    t::CHIP_SIZE,
                    font::UI,
                    color::text_tertiary(),
                    text::Alignment::Left,
                    alignment::Vertical::Center,
                    row,
                );
                return;
            }
            for (k, rr) in &g.chips {
                let rect = at(*rr);
                let hot = st.hot(*k);
                let dragging = st.grab.is_some_and(|g| g.dragging && g.hit == Hit::Chip(*k));
                let rest = if dragging { 0.4 } else { 1.0 };
                let fillc = mix(color::cell(), color::cell_hover(), hot);
                let rim = mix(color::cell_rim(), color::cell_rim_hover(), hot);
                fill(
                    r,
                    rect,
                    t::CHIP_H / 2.0,
                    fillc.scale_alpha(rest),
                    Some((rim.scale_alpha(rest), space::HAIRLINE)),
                );
                let ink_c = mix(color::text_secondary(), color::text(), hot).scale_alpha(rest);
                let plus = Rectangle::new(
                    Point::new(rect.x + t::CHIP_PAD, rect.y),
                    Size::new(t::CHIP_SIZE, rect.height),
                );
                put(
                    r,
                    "+",
                    plus,
                    t::CHIP_SIZE + 2.0,
                    font::UI_MEDIUM,
                    ink_c,
                    text::Alignment::Center,
                    alignment::Vertical::Center,
                    rect,
                );
                let label = Rectangle::new(
                    Point::new(plus.x + t::CHIP_SIZE, rect.y),
                    Size::new(rect.width - t::CHIP_SIZE - t::CHIP_PAD, rect.height),
                );
                put(
                    r,
                    kind_label(*k),
                    label,
                    t::CHIP_SIZE,
                    font::UI_MEDIUM,
                    ink_c,
                    text::Alignment::Left,
                    alignment::Vertical::Center,
                    rect,
                );
            }
        });

        self.draw_bar(renderer, st, &g, b, clip);
    }
}

impl<M> Editor<'_, M> {
    /// The apply bar: a glass pill that rises and fades in while anything
    /// differs from the machine, and leaves the same way; while nothing does
    /// its slot just says so.
    fn draw_bar(&self, renderer: &mut iced::Renderer, st: &State, g: &Geo, b: Rectangle, clip: Rectangle) {
        let (Some(bg), Some(bar)) = (&g.bar, &self.bar) else {
            return;
        };
        let at = |r: Rectangle, dy: f32| Rectangle::new(Point::new(b.x + r.x, b.y + r.y + dy), r.size());
        let reveal = st.bar_value();
        let a = reveal.clamp(0.0, 1.0);
        // The slot's words and the bar's own never share the stage: the old
        // ones are gone by the time the bar is a third of the way up, the new
        // ones come in over the rest.
        let idle = (1.0 - a * 3.0).clamp(0.0, 1.0);
        let words = ((a - 0.34) / 0.66).clamp(0.0, 1.0);
        renderer.with_layer(clip, |r| {
            if idle > 0.02 {
                let row = at(bg.rect, 0.0);
                let (words, tone) = match &bar.note {
                    Some((m, true)) => (m.as_str(), color::text_secondary()),
                    Some((m, false)) => (m.as_str(), color::danger()),
                    None => (t::BAR_IDLE, color::text_tertiary()),
                };
                put(
                    r,
                    words,
                    Rectangle::new(
                        Point::new(row.x + t::BAR_RADIUS * 0.75, row.y),
                        Size::new(row.width, row.height),
                    ),
                    t::BAR_TEXT,
                    font::UI,
                    tone.scale_alpha(idle),
                    text::Alignment::Left,
                    alignment::Vertical::Center,
                    row,
                );
            }
            if a <= 0.01 {
                return;
            }
            // Rises into place; the spring's overshoot carries it a touch past.
            let dy = (1.0 - reveal) * t::BAR_SLIDE;
            let rect = at(bg.rect, dy);
            r.fill_quad(
                renderer::Quad {
                    bounds: rect,
                    border: border::rounded(t::BAR_RADIUS),
                    shadow: Shadow {
                        color: Color::BLACK.scale_alpha(t::SHADOW * a),
                        offset: Vector::new(0.0, t::SHADOW_DROP),
                        blur_radius: t::SHADOW_SPREAD * 2.0,
                    },
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
            );
            fill(
                r,
                rect,
                t::BAR_RADIUS,
                mix(color::surface_2(), color::lift_soft(), 0.5).scale_alpha(a),
                Some((color::cell_rim_hover().scale_alpha(a), space::HAIRLINE)),
            );
            put(
                r,
                &st.bar_text,
                Rectangle::new(
                    Point::new(rect.x + t::BAR_RADIUS * 0.75, rect.y),
                    Size::new(bg.revert.x - rect.x - t::BAR_RADIUS, rect.height),
                ),
                t::BAR_TEXT,
                font::UI_MEDIUM,
                color::text().scale_alpha(words),
                text::Alignment::Left,
                alignment::Vertical::Center,
                rect,
            );
            let button =
                |r: &mut iced::Renderer, rr: Rectangle, label: &str, i: usize, on: bool, primary: bool| {
                    let hot = st.bar_hot(i);
                    let rr = at(rr, dy);
                    let (fillc, rim, ink) = if primary && on {
                        (
                            mix(color::highlight(), color::highlight_strong(), hot),
                            color::highlight_rim(),
                            color::text(),
                        )
                    } else if primary {
                        (color::lift_soft(), color::hairline(), color::text_tertiary())
                    } else {
                        (
                            mix(color::lift_soft(), color::lift(), hot),
                            color::border(),
                            mix(color::text_secondary(), color::text(), hot),
                        )
                    };
                    // Pressed-in under the pointer, a hair bigger when lit.
                    let rr = scaled(rr, lerp(1.0, t::LIFT, hot));
                    fill(
                        r,
                        rr,
                        rr.height / 2.0,
                        fillc.scale_alpha(a),
                        Some((rim.scale_alpha(a), space::HAIRLINE)),
                    );
                    put(
                        r,
                        label,
                        rr,
                        t::BAR_TEXT,
                        font::UI_MEDIUM,
                        ink.scale_alpha(a),
                        text::Alignment::Center,
                        alignment::Vertical::Center,
                        rr,
                    );
                };
            button(r, bg.revert, "Revert", 0, true, false);
            button(
                r,
                bg.apply,
                if bar.applying {
                    "Waiting for authorisation\u{2026}"
                } else {
                    "Apply"
                },
                1,
                bar.can_apply && !bar.applying,
                true,
            );
        });
    }
}

fn scaled(r: Rectangle, s: f32) -> Rectangle {
    let (w, h) = (r.width * s, r.height * s);
    Rectangle::new(
        Point::new(r.x + (r.width - w) / 2.0, r.y + (r.height - h) / 2.0),
        Size::new(w, h),
    )
}

fn inset(r: Rectangle, by: f32) -> Rectangle {
    Rectangle::new(
        Point::new(r.x + by, r.y + by),
        Size::new((r.width - 2.0 * by).max(1.0), (r.height - 2.0 * by).max(1.0)),
    )
}

fn fill(r: &mut iced::Renderer, b: Rectangle, radius: f32, c: Color, rim: Option<(Color, f32)>) {
    let mut border = border::rounded(radius);
    if let Some((col, w)) = rim {
        border = border.color(col).width(w);
    }
    r.fill_quad(
        renderer::Quad {
            bounds: b,
            border,
            ..renderer::Quad::default()
        },
        c,
    );
}

#[allow(clippy::too_many_arguments)]
fn put(
    r: &mut iced::Renderer,
    s: &str,
    bounds: Rectangle,
    size: f32,
    f: iced::Font,
    c: Color,
    ax: text::Alignment,
    ay: alignment::Vertical,
    clip: Rectangle,
) {
    // Vertical placement is done here: the line box is `size` tall, so the
    // text sits where the caller's box says, whatever the renderer does with
    // `align_y`.
    let dy = match ay {
        alignment::Vertical::Top => 0.0,
        alignment::Vertical::Center => (bounds.height - size) / 2.0,
        alignment::Vertical::Bottom => bounds.height - size,
    };
    r.fill_text(
        text::Text {
            content: s.to_owned(),
            bounds: Size::new(bounds.width, size),
            size: Pixels(size),
            line_height: LineHeight::Relative(1.0),
            font: f,
            align_x: ax,
            align_y: alignment::Vertical::Top,
            shaping: Shaping::Basic,
            wrapping: Wrapping::None,
        },
        // The renderer anchors centred and right-aligned text at the middle
        // and the right edge of its bounds, not the left.
        Point::new(
            bounds.x
                + match ax {
                    text::Alignment::Center => bounds.width / 2.0,
                    text::Alignment::Right => bounds.width,
                    _ => 0.0,
                },
            bounds.y + dy,
        ),
        c,
        clip,
    );
}

/// What says "pull me": the south-east bracket always, and while the tile is
/// lit the other brackets (not under the remove badge) and an edge bar on each
/// side, so every edge and corner of a lit tile reads as a handle.
fn grip_marks(r: &mut iced::Renderer, tile: Rectangle, ink: Color, alpha: f32, hot: f32, ne: bool) {
    let w = t::GRIP_W;
    let inset_c = t::TILE_RADIUS * 0.55;
    let len = t::GRIP_LEN.min(tile.width / 3.0).min(tile.height / 2.5);
    let (l, rt) = (tile.x + inset_c, tile.x + tile.width - inset_c);
    let (tp, bt) = (tile.y + inset_c, tile.y + tile.height - inset_c);
    let bracket = |r: &mut iced::Renderer, cx: f32, cy: f32, sx: f32, sy: f32, c: Color| {
        let hx = if sx > 0.0 { cx - len } else { cx };
        let vy = if sy > 0.0 { cy - len } else { cy };
        let hy = if sy > 0.0 { cy - w } else { cy };
        let vx = if sx > 0.0 { cx - w } else { cx };
        fill(
            r,
            Rectangle::new(Point::new(hx, hy), Size::new(len, w)),
            w / 2.0,
            c,
            None,
        );
        fill(
            r,
            Rectangle::new(Point::new(vx, vy), Size::new(w, len)),
            w / 2.0,
            c,
            None,
        );
    };
    bracket(
        r,
        rt,
        bt,
        1.0,
        1.0,
        ink.scale_alpha(lerp(t::GRIP_REST, 1.0, hot) * alpha),
    );
    if hot <= 0.02 {
        return;
    }
    let c = ink.scale_alpha(t::GRIP_HOT * hot * alpha);
    bracket(r, l, bt, -1.0, 1.0, c);
    bracket(r, l, tp, -1.0, -1.0, c);
    if ne {
        bracket(r, rt, tp, 1.0, -1.0, c);
    }
    // Edge bars sit on the tile's own edge, only when there is room for them.
    let edge = t::GRIP_EDGE_LEN;
    let c = c.scale_alpha(0.8);
    if tile.height > 2.0 * edge + 2.0 * len {
        let y = tile.y + (tile.height - edge) / 2.0;
        fill(
            r,
            Rectangle::new(Point::new(tile.x + w + 1.0, y), Size::new(w, edge)),
            w / 2.0,
            c,
            None,
        );
        fill(
            r,
            Rectangle::new(
                Point::new(tile.x + tile.width - 2.0 * w - 1.0, y),
                Size::new(w, edge),
            ),
            w / 2.0,
            c,
            None,
        );
    }
    if tile.width > 2.0 * edge + 2.0 * len {
        let x = tile.x + (tile.width - edge) / 2.0;
        fill(
            r,
            Rectangle::new(Point::new(x, tile.y + w + 1.0), Size::new(edge, w)),
            w / 2.0,
            c,
            None,
        );
        fill(
            r,
            Rectangle::new(
                Point::new(x, tile.y + tile.height - 2.0 * w - 1.0),
                Size::new(edge, w),
            ),
            w / 2.0,
            c,
            None,
        );
    }
}

fn badge(r: &mut iced::Renderer, b: Rectangle, sk: &Skin, hot: f32, alpha: f32, over: bool) {
    let a = alpha * if over { 1.0 } else { lerp(t::BADGE_ALPHA, 1.0, hot) };
    let rest = sk.base.scale_alpha(t::BADGE_FILL);
    let fillc = if over {
        mix(rest, sk.ink.scale_alpha(0.30), 0.6)
    } else {
        rest
    };
    fill(
        r,
        b,
        b.width / 2.0,
        fillc.scale_alpha(a),
        Some((
            sk.ink
                .scale_alpha(if over { t::BADGE_RIM_HOT } else { t::BADGE_RIM } * a),
            space::HAIRLINE,
        )),
    );
    put(
        r,
        "\u{d7}",
        b,
        b.width * 0.82,
        font::UI_MEDIUM,
        sk.ink.scale_alpha(a),
        text::Alignment::Center,
        alignment::Vertical::Center,
        b,
    );
}

// ---------------------------------------------------------- gestures, tiles

impl<M: Clone> Editor<'_, M> {
    #[allow(clippy::too_many_arguments)]
    fn press(
        &self,
        st: &mut State,
        g: &Geo,
        b: Rectangle,
        p: Point,
        finger: Option<touch::Finger>,
        shell: &mut Shell<'_, M>,
        now: Instant,
    ) {
        let rel = Point::new(p.x - b.x, p.y - b.y);
        let hit = self.hit(g, rel);
        if hit == Hit::Nothing {
            return;
        }
        let (cx, cy) = g.cell(rel);
        let off = match hit {
            Hit::Tile(k) => self
                .layout
                .get(k)
                .map_or((0.0, 0.0), |r| (cx - r.x as f32, cy - r.y as f32)),
            // From the held edge (or corner) to the pointer, so the edge does
            // not jump to it.
            Hit::Grip(k, h) => self.layout.get(k).map_or((0.0, 0.0), |r| {
                let ex = if h.west() {
                    r.x as f32 - cx
                } else if h.east() {
                    (r.x + r.w) as f32 - cx
                } else {
                    0.0
                };
                let ey = if h.north() {
                    r.y as f32 - cy
                } else if h.south() {
                    (r.y + r.h) as f32 - cy
                } else {
                    0.0
                };
                (ex, ey)
            }),
            _ => (0.0, 0.0),
        };
        st.grab = Some(Grab {
            hit,
            origin: p,
            off,
            dragging: false,
            finger,
            target: None,
        });
        st.light(Some(hit), now);
        st.light_bar(Some(hit), now);
        shell.request_redraw();
        shell.capture_event();
    }

    fn cancel(&self, st: &mut State, now: Instant) {
        st.grab = None;
        st.float = None;
        st.set_ghost(None, now);
        st.set_dots(false, now);
        st.light(None, now);
        st.light_bar(None, now);
        st.sync(self.layout, now);
    }

    fn release(&self, st: &mut State, g: &Geo, rel: Point, shell: &mut Shell<'_, M>, now: Instant) {
        let Some(grab) = st.grab.take() else { return };
        st.float = None;
        st.set_ghost(None, now);
        st.set_dots(false, now);
        let mut next: Option<Layout> = None;
        match grab.hit {
            Hit::Badge(k) => {
                if !grab.dragging
                    && g.rect_px_of(self.layout, k)
                        .is_some_and(|r| badge_rect(r).contains(rel))
                {
                    next = layout_edit::remove(self.layout, k);
                }
            }
            Hit::Apply | Hit::Revert => {
                if !grab.dragging && self.hit(g, rel) == grab.hit {
                    if let Some(bar) = &self.bar {
                        shell.publish(if grab.hit == Hit::Apply {
                            bar.on_apply.clone()
                        } else {
                            bar.on_revert.clone()
                        });
                    }
                }
            }
            Hit::Chip(k) => {
                let at = if grab.dragging {
                    grab.target.map(|_| {
                        let (cx, cy) = g.cell(rel);
                        let (w, h) = layout_edit::default_size(k);
                        (
                            (cx - w as f32 / 2.0).round() as i32,
                            (cy - h as f32 / 2.0).round() as i32,
                        )
                    })
                } else {
                    None
                };
                if !grab.dragging || at.is_some() {
                    next = layout_edit::add(self.layout, k, at);
                }
            }
            Hit::Tile(k) | Hit::Grip(k, _) if grab.dragging => {
                next = grab.target.and_then(|r| layout_edit::commit(self.layout, k, r));
            }
            _ => {}
        }
        match next {
            Some(l) if l != *self.layout => shell.publish((self.on_change)(l)),
            _ => {}
        }
        // Whatever happened, the tile lands (or springs back) on the bounce.
        let after = self.hover_after(g, rel);
        st.light(after, now);
        st.light_bar(after, now);
        st.sync(self.layout, now);
        if let Hit::Tile(k) | Hit::Grip(k, _) = grab.hit {
            if let (Some(rect), Some(s)) = (
                grab.target
                    .filter(|_| grab.dragging)
                    .or_else(|| self.layout.get(k)),
                st.slot(k),
            ) {
                s.aim(rect, LAND, now);
            }
        }
        shell.request_redraw();
        shell.capture_event();
    }

    fn hover_after(&self, g: &Geo, rel: Point) -> Option<Hit> {
        Some(self.hit(g, rel)).filter(|h| *h != Hit::Nothing)
    }
}

impl Geo {
    fn rect_px_of(&self, l: &Layout, k: Kind) -> Option<Rectangle> {
        l.get(k).map(|r| self.rect_px(r))
    }
}

impl<M: Clone> Editor<'_, M> {
    /// One glass tile: the pane, then the mock of what the greeter draws in it.
    ///
    /// `full` is the tile at the size it will be once it has landed: the mock
    /// is laid out for that, centred in `b`, so it does not reflow (and change
    /// how much it shows) while the tile pops in or out. Only a real resize
    /// does that.
    #[allow(clippy::too_many_arguments)]
    fn tile(
        &self,
        r: &mut iced::Renderer,
        kind: Kind,
        b: Rectangle,
        full: Rectangle,
        hot: f32,
        alpha: f32,
        lifted: bool,
    ) {
        let sk = &self.skin;
        let ink = sk.ink;
        let hot = if lifted { 1.0 } else { hot };
        // Frosted ground, then the light tint a step brighter when held.
        fill(
            r,
            b,
            t::TILE_RADIUS,
            sk.base
                .scale_alpha(if sk.dense { lp::CARD_DENSE } else { t::GROUND } * alpha),
            None,
        );
        fill(
            r,
            b,
            t::TILE_RADIUS,
            ink.scale_alpha(lerp(t::TILE_FILL, t::TILE_FILL_HOT, hot) * alpha),
            Some((
                ink.scale_alpha(lerp(t::TILE_RIM, t::TILE_RIM_HOT, hot) * alpha),
                space::HAIRLINE,
            )),
        );
        let c = b.center();
        let full = Rectangle::new(
            Point::new(c.x - full.width / 2.0, c.y - full.height / 2.0),
            full.size(),
        );
        let inner = inset(full, (full.height.min(full.width) * 0.09).clamp(3.0, 12.0));
        mock(r, kind, full, inner, b, sk, alpha);
    }
}

/// `size` if it is readable, else nothing: text below the floor is hidden, not
/// drawn small.
fn ts(want: f32, max: f32) -> Option<f32> {
    let s = want.min(max);
    (s >= t::MIN_TEXT).then_some(s)
}

/// `want` shrunk until `s` fits `w`, if it can without going below the floor.
fn fit_w(s: &str, want: f32, w: f32, f: iced::Font) -> Option<f32> {
    let want = ts(want, want)?;
    let wide = measure(s, want, f);
    let size = if wide > w && wide > 0.0 {
        want * w / wide
    } else {
        want
    };
    (size >= t::MIN_TEXT).then_some(size)
}

/// `s` cut with an ellipsis until it fits `w` at `size`.
fn ellipsize(s: &str, size: f32, w: f32, f: iced::Font) -> String {
    if measure(s, size, f) <= w {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cut: String = chars[..mid].iter().collect::<String>().trim_end().to_owned() + "\u{2026}";
        if measure(&cut, size, f) <= w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>().trim_end().to_owned() + "\u{2026}"
}

/// A stroke-only circle.
fn ring(r: &mut iced::Renderer, b: Rectangle, w: f32, c: Color) {
    fill(r, b, b.width / 2.0, Color::TRANSPARENT, Some((c, w)));
}

/// A small icon for `kind` in the square `b`, drawn from quads.
fn glyph(r: &mut iced::Renderer, kind: Kind, b: Rectangle, c: Color) {
    let d = b.width.min(b.height);
    let w = (d * 0.11).max(1.5);
    let (cx, cy) = (b.x + b.width / 2.0, b.y + b.height / 2.0);
    let sq = |x: f32, y: f32, s: f32| Rectangle::new(Point::new(x, y), Size::new(s, s));
    let rc = |x: f32, y: f32, w: f32, h: f32| Rectangle::new(Point::new(x, y), Size::new(w, h));
    match kind {
        Kind::Power => {
            ring(r, sq(cx - d * 0.42, cy - d * 0.36, d * 0.84), w, c);
            fill(r, rc(cx - w / 2.0, cy - d * 0.5, w, d * 0.5), w / 2.0, c, None);
        }
        Kind::Clock => {
            ring(r, sq(cx - d * 0.5, cy - d * 0.5, d), w, c);
            fill(
                r,
                rc(cx - w / 2.0, cy - d * 0.3, w, d * 0.3 + w),
                w / 2.0,
                c,
                None,
            );
            fill(r, rc(cx - w / 2.0, cy, d * 0.24 + w, w), w / 2.0, c, None);
        }
        Kind::Battery => {
            let (bw, bh) = (d, d * 0.54);
            let body = rc(cx - bw / 2.0, cy - bh / 2.0, bw, bh);
            fill(r, body, bh * 0.28, Color::TRANSPARENT, Some((c, w)));
            fill(r, inset(body, w * 1.8), bh * 0.12, c, None);
        }
        Kind::Wifi => {
            let bw = d * 0.17;
            for i in 0..4 {
                let h = d * (0.3 + 0.7 * i as f32 / 3.0);
                let x = cx - d / 2.0 + i as f32 * (d - bw) / 3.0;
                let col = if i == 3 { c.scale_alpha(0.45) } else { c };
                fill(r, rc(x, cy + d / 2.0 - h, bw, h), bw / 2.0, col, None);
            }
        }
        Kind::Users => {
            let head = d * 0.4;
            fill(r, sq(cx - head / 2.0, cy - d * 0.5, head), head / 2.0, c, None);
            let sh = d * 0.38;
            fill(
                r,
                rc(cx - d * 0.45, cy + d * 0.12, d * 0.9, sh),
                sh / 2.0,
                c,
                None,
            );
        }
        Kind::Password => {
            let sh = d * 0.5;
            ring(r, sq(cx - sh / 2.0, cy - d * 0.5, sh), w, c);
            let body = rc(cx - d * 0.4, cy - d * 0.08, d * 0.8, d * 0.58);
            fill(r, body, d * 0.14, c, None);
        }
        Kind::Greeting | Kind::NowPlaying => {
            let body = rc(cx - d * 0.5, cy - d * 0.34, d, d * 0.68);
            fill(r, body, d * 0.2, Color::TRANSPARENT, Some((c, w)));
            fill(r, rc(cx - d * 0.3, cy - d * 0.1, d * 0.6, w), w / 2.0, c, None);
            fill(r, rc(cx - d * 0.3, cy + d * 0.1, d * 0.36, w), w / 2.0, c, None);
        }
    }
}

/// The smallest a tile gets: a glyph, with the one short label beside or
/// under it when there is room, else the glyph alone.
fn glyph_card(r: &mut iced::Renderer, kind: Kind, b: Rectangle, clip: Rectangle, sk: &Skin, a: f32) {
    let c = sk.ink.scale_alpha(lp::TAG_ALPHA.max(0.8) * a);
    let g = (b.width.min(b.height) * t::GLYPH_SHARE).min(t::GLYPH_MAX);
    if g < t::GLYPH_MIN {
        return;
    }
    let label = kind_label(kind);
    let ls = ts(g * 0.55, t::LABEL_SIZE + 1.0);
    let lw = ls.map_or(0.0, |s| measure(label, s, font::UI_MEDIUM));
    let gap = g * 0.4;
    let (cx, cy) = (b.x + b.width / 2.0, b.y + b.height / 2.0);
    let stacked = ls.is_some_and(|s| lw <= b.width && b.height >= g + gap + s && b.height >= b.width * 0.8);
    let beside = ls.is_some() && g + gap + lw <= b.width;
    let sq = |x: f32, y: f32| Rectangle::new(Point::new(x, y), Size::new(g, g));
    match (ls, stacked, beside) {
        (Some(s), true, _) => {
            let top = cy - (g + gap + s) / 2.0;
            glyph(r, kind, sq(cx - g / 2.0, top), c);
            put(
                r,
                label,
                Rectangle::new(Point::new(b.x, top + g + gap), Size::new(b.width, s)),
                s,
                font::UI_MEDIUM,
                c,
                text::Alignment::Center,
                alignment::Vertical::Top,
                clip,
            );
        }
        (Some(s), _, true) => {
            let x0 = cx - (g + gap + lw) / 2.0;
            glyph(r, kind, sq(x0, cy - g / 2.0), c);
            put(
                r,
                label,
                Rectangle::new(Point::new(x0 + g + gap, cy - s / 2.0), Size::new(lw + 2.0, s)),
                s,
                font::UI_MEDIUM,
                c,
                text::Alignment::Left,
                alignment::Vertical::Top,
                clip,
            );
        }
        _ => glyph(r, kind, sq(cx - g / 2.0, cy - g / 2.0), c),
    }
}

/// What the greeter draws in a tile, small, degrading with the tile's size:
/// the whole miniature, then without its secondary text, then (when its main
/// text would be unreadable) a glyph and one short label. Text under the
/// floor is never drawn. Clipped to the tile.
fn mock(
    r: &mut iced::Renderer,
    kind: Kind,
    tile: Rectangle,
    b: Rectangle,
    clip: Rectangle,
    sk: &Skin,
    a: f32,
) {
    let ink = sk.ink.scale_alpha(a);
    let dim = sk.ink.scale_alpha(lp::TAG_ALPHA * a);
    let faint = sk.ink.scale_alpha(lp::FIELD_FILL * a);
    let left = text::Alignment::Left;
    let mid = alignment::Vertical::Center;
    let top = alignment::Vertical::Top;
    // The small-caps heading of a tile, only on one tall enough to carry it.
    let head_size = if b.height >= t::LEAN_H {
        ts(tile.height * 0.075, t::LABEL_SIZE)
    } else {
        None
    };
    let heading = |r: &mut iced::Renderer, s: &str, size: f32| {
        put(
            r,
            s,
            Rectangle::new(b.position(), Size::new(b.width, size)),
            size,
            font::DATA,
            dim,
            left,
            top,
            clip,
        );
    };
    if b.width < t::GLYPH_W || b.height < t::GLYPH_H {
        return glyph_card(r, kind, b, clip, sk, a);
    }
    match kind {
        Kind::Clock => {
            let ratio = measure(&sk.clock, 100.0, font::UI_MEDIUM) / 100.0;
            let by_w = if ratio > 0.0 { b.width / ratio } else { b.height };
            let size = (b.height * 0.52).min(by_w);
            if size < t::MIN_PRIMARY {
                return glyph_card(r, kind, b, clip, sk, a);
            }
            put(
                r,
                &sk.clock,
                Rectangle::new(b.position(), Size::new(b.width, size)),
                size,
                font::UI_MEDIUM,
                ink,
                left,
                top,
                clip,
            );
            if b.height > size * 1.9 {
                if let Some(s2) = fit_w(t::MOCK_TIME, (b.height * 0.13).min(13.0), b.width, font::UI) {
                    put(
                        r,
                        t::MOCK_TIME,
                        Rectangle::new(Point::new(b.x, b.y + b.height - s2), Size::new(b.width, s2)),
                        s2,
                        font::UI,
                        dim,
                        left,
                        top,
                        clip,
                    );
                }
            }
        }
        Kind::Greeting => {
            let size = (b.height * 0.62).min(15.0);
            if size < t::MIN_PRIMARY.min(t::MIN_TEXT + 1.0) {
                return glyph_card(r, kind, b, clip, sk, a);
            }
            let w = b.width - 2.0;
            let s = ellipsize(&sk.greeting, size, w, font::UI_MEDIUM);
            put(
                r,
                &s,
                Rectangle::new(Point::new(b.x + 2.0, b.y), Size::new(w, b.height)),
                size,
                font::UI_MEDIUM,
                ink,
                left,
                mid,
                clip,
            );
        }
        Kind::Users => {
            let head = head_size.map_or(0.0, |s| s + 5.0);
            if let Some(s) = head_size {
                heading(r, "ACCOUNTS", s);
            }
            let rh = (b.height * 0.24).clamp(14.0, 28.0).min(b.height);
            let gap = 4.0;
            let mut y = b.y + head;
            for name in t::MOCK_NAMES {
                if y + rh > b.y + b.height + 0.5 {
                    break;
                }
                let row = Rectangle::new(Point::new(b.x, y), Size::new(b.width, rh));
                fill(r, row, rh * 0.34, faint, None);
                let d = rh * 0.62;
                let av = Rectangle::new(
                    Point::new(row.x + rh * 0.2, row.y + (rh - d) / 2.0),
                    Size::new(d, d),
                );
                fill(r, av, d / 2.0, sk.ink.scale_alpha(t::DISC * a), None);
                if let Some(sz) = ts(rh * 0.5, 12.0) {
                    put(
                        r,
                        &name[..1].to_uppercase(),
                        av,
                        sz * 0.85,
                        font::UI_MEDIUM,
                        ink,
                        text::Alignment::Center,
                        mid,
                        clip,
                    );
                    put(
                        r,
                        name,
                        Rectangle::new(Point::new(av.x + d + rh * 0.25, row.y), Size::new(row.width, rh)),
                        sz,
                        font::UI_MEDIUM,
                        ink,
                        left,
                        mid,
                        clip,
                    );
                }
                y += rh + gap;
            }
        }
        Kind::Password => {
            let fh = (b.height * 0.5).clamp(12.0, 34.0).min(b.height);
            let Some(sz) = ts(fh * 0.45, 12.0) else {
                return glyph_card(r, kind, b, clip, sk, a);
            };
            let bw = (b.width * 0.28).min(96.0);
            let field = Rectangle::new(b.position(), Size::new(b.width - bw - 6.0, fh));
            fill(r, field, fh * 0.3, faint, None);
            let pad = fh * 0.35;
            if measure("Password", sz, font::UI) + 2.0 * pad <= field.width {
                put(
                    r,
                    "Password",
                    Rectangle::new(Point::new(field.x + pad, field.y), field.size()),
                    sz,
                    font::UI,
                    dim,
                    left,
                    mid,
                    clip,
                );
            }
            let btn = Rectangle::new(Point::new(field.x + field.width + 6.0, b.y), Size::new(bw, fh));
            fill(r, btn, fh * 0.3, sk.ink.scale_alpha(lp::PILL_FILL * a), None);
            if measure("Sign in", sz, font::UI_MEDIUM) + pad <= bw {
                put(
                    r,
                    "Sign in",
                    btn,
                    sz,
                    font::UI_MEDIUM,
                    dim,
                    text::Alignment::Center,
                    mid,
                    clip,
                );
            }
            if b.height > fh * 1.7 {
                if let Some(s2) = fit_w(
                    "Choose an account to sign in",
                    (b.height * 0.14).min(11.0),
                    b.width,
                    font::UI,
                ) {
                    put(
                        r,
                        "Choose an account to sign in",
                        Rectangle::new(Point::new(b.x, b.y + b.height - s2 - 1.0), Size::new(b.width, s2)),
                        s2,
                        font::UI,
                        dim,
                        left,
                        top,
                        clip,
                    );
                }
            }
        }
        Kind::Power => {
            let names = ["Suspend", "Restart", "Power off"];
            let gap = 6.0;
            let pw = (b.width - gap * 2.0) / 3.0;
            let ph = b.height.min(26.0);
            // One size for the three, set by the widest label.
            let Some(sz) = fit_w("Power off", (ph * 0.46).min(11.0), pw - ph * 0.5, font::UI_MEDIUM) else {
                return glyph_card(r, kind, b, clip, sk, a);
            };
            let y = b.y + (b.height - ph) / 2.0;
            for (i, n) in names.iter().enumerate() {
                let pill = Rectangle::new(Point::new(b.x + i as f32 * (pw + gap), y), Size::new(pw, ph));
                fill(r, pill, ph / 2.0, sk.ink.scale_alpha(lp::PILL_FILL * a), None);
                put(
                    r,
                    n,
                    pill,
                    sz,
                    font::UI_MEDIUM,
                    ink,
                    text::Alignment::Center,
                    mid,
                    clip,
                );
            }
        }
        Kind::Battery => {
            // The gauge row at its intrinsic proportions (`ec_layout::intrinsic`),
            // scaled to fit the tile and centred. Ink only, like every mock.
            use ec_greeter_ui::gauge::{paint, Colours, Spec};
            use ec_greeter_ui::tokens::gauge as gt;
            let n = layout_edit::PREVIEW_BATTERIES.len();
            let (cw, ch) = ec_layout::intrinsic::px(n);
            let s = (tile.width / cw as f32).min(tile.height / ch as f32);
            let side = gt::GAUGE_PX as f32 * s;
            let origin = Point::new(
                tile.x + (tile.width - cw as f32 * s) / 2.0 + gt::PAD_PX as f32 * s,
                tile.y + (tile.height - ch as f32 * s) / 2.0 + gt::PAD_PX as f32 * s,
            );
            let rgba = |c: Color| ec_theme::Rgba::new(c.r, c.g, c.b, c.a);
            let colours = Colours {
                ring: rgba(ink),
                track: rgba(sk.ink.scale_alpha(t::TRACK * a)),
                ink: rgba(ink),
                bolt: rgba(ink),
            };
            let mut frame = iced::widget::canvas::Frame::new(&*r, clip.size());
            let at = |i: usize| {
                Point::new(
                    origin.x - clip.x + i as f32 * (side + gt::GAUGE_GAP_PX as f32 * s),
                    origin.y - clip.y,
                )
            };
            for (i, (kind, percent, charging)) in layout_edit::PREVIEW_BATTERIES.iter().enumerate() {
                let spec = Spec {
                    kind: *kind,
                    percent: *percent,
                    charging: *charging,
                };
                paint(&mut frame, at(i), side, &spec, &colours);
            }
            r.with_translation(Vector::new(clip.x, clip.y), |r| {
                geometry::Renderer::draw_geometry(r, frame.into_geometry());
            });
            let line = gt::NUMERAL_PX as f32 * s;
            let size = (gt::NUMERAL_SIZE * s).max(t::MIN_TEXT);
            if line >= t::MIN_TEXT {
                for (i, (_, percent, _)) in layout_edit::PREVIEW_BATTERIES.iter().enumerate() {
                    put(
                        r,
                        &format!("{percent}%"),
                        Rectangle::new(
                            Point::new(
                                origin.x + i as f32 * (side + gt::GAUGE_GAP_PX as f32 * s),
                                origin.y + side,
                            ),
                            Size::new(side, line),
                        ),
                        size.min(line),
                        font::DATA,
                        ink,
                        text::Alignment::Center,
                        alignment::Vertical::Center,
                        clip,
                    );
                }
            }
        }
        Kind::Wifi => {
            let head = head_size.map_or(0.0, |s| s + 3.0);
            if let Some(s) = head_size {
                heading(r, "LINK", s);
            }
            let sz = ((b.height - head) * 0.5).min(30.0);
            if sz < t::MIN_PRIMARY {
                return glyph_card(r, kind, b, clip, sk, a);
            }
            put(
                r,
                "68%",
                Rectangle::new(Point::new(b.x, b.y + head), Size::new(b.width, sz)),
                sz,
                font::UI_MEDIUM,
                ink,
                left,
                top,
                clip,
            );
            // Signal bars, rising, the last one dim.
            let bh = (sz * 0.9).max(8.0);
            let bw = (bh * 0.16).max(2.0);
            let x0 = (b.x + b.width - 4.0 * (bw + 2.0)).max(b.x + sz * 2.2);
            for i in 0..4 {
                let h = bh * (0.35 + 0.22 * i as f32);
                let c = if i == 3 {
                    sk.ink.scale_alpha(t::DIM_BAR * a)
                } else {
                    ink
                };
                fill(
                    r,
                    Rectangle::new(
                        Point::new(x0 + i as f32 * (bw + 2.0), b.y + head + bh - h + sz * 0.05),
                        Size::new(bw, h),
                    ),
                    bw / 2.0,
                    c,
                    None,
                );
            }
        }
        _ => {}
    }
}

impl<'a, M: Clone + 'a> From<Editor<'a, M>> for Element<'a, M, Theme> {
    fn from(e: Editor<'a, M>) -> Self {
        Element::new(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Glyph width as a share of the size, for tests that have no renderer.
    fn est(s: &str, size: f32, _: iced::Font) -> f32 {
        s.chars().count() as f32 * size * 0.56
    }

    fn geo(width: f32, offered: &[Kind]) -> Geo {
        geo_with(width, offered, Some("Apply"), &est)
    }

    #[test]
    fn the_preview_is_16_by_9_and_the_tray_and_bar_follow_it() {
        let g = geo(700.0, &[Kind::Battery, Kind::Wifi]);
        assert!((g.pv.width / g.pv.height - 16.0 / 9.0).abs() < 1e-3);
        assert!(g.chips.iter().all(|(_, r)| r.y >= g.pv.y + g.pv.height));
        let bar = g.bar.unwrap();
        // Beside the tray's last row, never over a chip.
        for (_, c) in &g.chips {
            assert!(
                c.x + c.width <= bar.rect.x || c.y + c.height <= bar.rect.y,
                "{c:?} {bar:?}"
            );
        }
        assert!(g.h >= bar.rect.y + bar.rect.height);
        assert!(bar.revert.x + bar.revert.width < bar.apply.x);
        assert!(bar.apply.x + bar.apply.width <= bar.rect.x + bar.rect.width);
    }

    #[test]
    fn the_preview_uses_the_whole_width_up_to_the_maximum() {
        let g = geo(900.0, &[]);
        assert!((g.pv.width - (900.0 - 2.0 * t::PAD)).abs() < 1e-3);
    }

    #[test]
    fn a_narrow_tray_wraps_and_stays_inside() {
        let all = layout_edit::OPTIONAL;
        let g = geo(260.0, &all);
        assert!(g
            .chips
            .iter()
            .all(|(_, r)| r.x + r.width <= g.w - t::PAD + 0.01 || r.x == t::PAD));
        let rows = g
            .chips
            .iter()
            .map(|(_, r)| r.y as i32)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(rows.len() > 1);
        assert!(g.h >= g.chips.last().unwrap().1.y + t::CHIP_H);
    }

    #[test]
    fn it_never_exceeds_its_maximum_width() {
        assert_eq!(geo(5000.0, &[]).w, t::MAX_W);
    }

    #[test]
    fn a_tray_with_no_room_beside_it_puts_the_bar_on_its_own_row() {
        let g = geo(520.0, &layout_edit::OPTIONAL);
        let bar = g.bar.unwrap();
        let last = g.chips.last().unwrap().1;
        assert!(bar.rect.y >= last.y + last.height, "{bar:?} {last:?}");
        assert!(g.h >= bar.rect.y + bar.rect.height);
    }

    #[test]
    fn no_bar_no_slot() {
        let g = geo_with(700.0, &[], None, &est);
        assert!(g.bar.is_none());
    }

    #[test]
    fn cells_round_trip_through_pixels() {
        let g = geo(640.0, &[]);
        let r = g.rect_px(Rect::new(8, 5, 8, 4));
        let (cx, cy) = g.cell(Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0));
        assert!((cx - 12.0).abs() < 0.1 && (cy - 7.0).abs() < 0.1);
    }

    #[test]
    fn corners_win_over_edges_and_every_side_has_a_grip() {
        let tile = Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 60.0));
        assert_eq!(grip_at(tile, Point::new(95.0, 55.0)), Some(Handle::SouthEast));
        assert_eq!(grip_at(tile, Point::new(5.0, 55.0)), Some(Handle::SouthWest));
        assert_eq!(grip_at(tile, Point::new(5.0, 5.0)), Some(Handle::NorthWest));
        assert_eq!(grip_at(tile, Point::new(95.0, 5.0)), Some(Handle::NorthEast));
        assert_eq!(grip_at(tile, Point::new(97.0, 30.0)), Some(Handle::East));
        assert_eq!(grip_at(tile, Point::new(3.0, 30.0)), Some(Handle::West));
        assert_eq!(grip_at(tile, Point::new(50.0, 57.0)), Some(Handle::South));
        assert_eq!(grip_at(tile, Point::new(50.0, 3.0)), Some(Handle::North));
        assert_eq!(grip_at(tile, Point::new(50.0, 30.0)), None);
        assert_eq!(grip_at(tile, Point::new(140.0, 20.0)), None);
    }

    #[test]
    fn a_small_tile_keeps_a_body_to_drag_by() {
        let tile = Rectangle::new(Point::new(0.0, 0.0), Size::new(40.0, 28.0));
        assert_eq!(grip_at(tile, Point::new(20.0, 14.0)), None);
    }

    #[test]
    fn text_under_the_floor_is_hidden_not_shrunk() {
        assert_eq!(ts(t::MIN_TEXT - 0.1, 20.0), None);
        assert_eq!(ts(30.0, 11.0), Some(11.0));
    }
}
