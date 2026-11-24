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

use super::layout_edit::{self, Handle, Screen};
use super::sample;
use ec_glance::tile::{self, BatteryPlan, WifiPlan};
use ec_glance::{cells, Charge};
use ec_theme::{Palette, Rgba};

/// What the mocks draw with: the previewed theme's colours and the texts the
/// page is about to send.
#[derive(Clone)]
pub struct Skin {
    pub base: Color,
    pub ink: Color,
    /// The palette the greeter derives for this theme: every colour a tile's
    /// content is drawn in comes from here, as on the real screen.
    pub pal: Palette,
    /// A chosen picture is behind the preview: the tiles are denser.
    pub dense: bool,
    pub clock: sample::ClockSample,
    /// Sample Glance data (display only), of the shapes the real ones take.
    pub batteries: Vec<ec_glance::Battery>,
    pub link: Option<u8>,
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
    screen: Screen,
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
        screen: Screen::Login,
        on_change: Box::new(on_change),
        bar: None,
    }
}

impl<'a, Message> Editor<'a, Message> {
    /// Edit the lock screen's layout instead of the login screen's: its own tile
    /// set, and its tiles drawn as the lock draws them.
    pub fn screen(mut self, screen: Screen) -> Self {
        self.screen = screen;
        self
    }

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
pub(super) fn measure(s: &str, size: f32, f: iced::Font) -> f32 {
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
            if layout_edit::removable(self.screen, tile.kind) && badge_rect(r).contains(p) {
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
                layout_edit::drag_to(self.screen, self.layout, k, x, y)
            }
            Hit::Grip(k, h) => {
                let (fx, fy) = ((cx + grab.off.0).round() as i32, (cy + grab.off.1).round() as i32);
                layout_edit::resize_to(self.screen, self.layout, k, h, fx, fy)
            }
            Hit::Chip(k) => {
                if !g.pv.contains(p) {
                    return None;
                }
                let (w, h) = layout_edit::default_size(self.screen, k);
                let at = (
                    (cx - w as f32 / 2.0).round() as i32,
                    (cy - h as f32 / 2.0).round() as i32,
                );
                layout_edit::add(self.screen, self.layout, k, Some(at))?.get(k)
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

pub(super) fn lerp(a: f32, b: f32, t: f32) -> f32 {
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
        let g = geo(
            width,
            &layout_edit::offered(self.screen, self.layout),
            self.bar_label(),
        );
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
        let g = geo(
            b.width,
            &layout_edit::offered(self.screen, self.layout),
            self.bar_label(),
        );
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
        let g = geo(
            b.width,
            &layout_edit::offered(self.screen, self.layout),
            self.bar_label(),
        );
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
            let removable = layout_edit::removable(self.screen, s.kind);
            renderer.with_layer(clip, |r| {
                if lifted {
                    r.fill_quad(
                        renderer::Quad {
                            bounds: rect,
                            border: border::rounded(tile_radius(rect, g.pv.width)),
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
                self.tile(r, s.kind, rect, base, hot, alpha, lifted, g.pv.width);
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
            let (w, h) = layout_edit::default_size(self.screen, k);
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
                        border: border::rounded(tile_radius(rect, g.pv.width)),
                        shadow: Shadow {
                            color: Color::BLACK.scale_alpha(t::SHADOW),
                            offset: Vector::new(0.0, t::SHADOW_DROP),
                            blur_radius: t::SHADOW_SPREAD * 2.0,
                        },
                        ..renderer::Quad::default()
                    },
                    Color::TRANSPARENT,
                );
                self.tile(r, k, rect, base, 1.0, 1.0, true, g.pv.width);
            });
        }

        // The tray.
        let offered = layout_edit::offered(self.screen, self.layout);
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

pub(super) fn fill(r: &mut iced::Renderer, b: Rectangle, radius: f32, c: Color, rim: Option<(Color, f32)>) {
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

/// The width of the box a line of text is set in: the caller's `w`, but never
/// narrower than the text when it is left-aligned, so the renderer cannot
/// wrap it. Centred and right-aligned text anchor on the box's width, so
/// those keep it.
fn one_line_width(w: f32, text_w: f32, ax: text::Alignment) -> f32 {
    match ax {
        text::Alignment::Left | text::Alignment::Default | text::Alignment::Justified => {
            w.max(text_w.ceil() + 2.0)
        }
        _ => w,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn put(
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
    // `Wrapping::None` is not honoured for cached text, which word-wraps in
    // its box; the screens' labels never wrap, so a left-aligned line gets a
    // box as wide as the text and overflows rather than breaks.
    let width = one_line_width(bounds.width, measure(s, size, f), ax);
    // ...and is cut at its own box's sides, as the floor on text size can make
    // a line wider than the real one the box was sized for.
    let clip = if width > bounds.width {
        Rectangle::new(Point::new(bounds.x, clip.y), Size::new(bounds.width, clip.height))
            .intersection(&clip)
            .unwrap_or(Rectangle::new(Point::ORIGIN, Size::ZERO))
    } else {
        clip
    };
    r.fill_text(
        text::Text {
            content: s.to_owned(),
            bounds: Size::new(width, size),
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
                    next = layout_edit::remove(self.screen, self.layout, k);
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
                        let (w, h) = layout_edit::default_size(self.screen, k);
                        (
                            (cx - w as f32 / 2.0).round() as i32,
                            (cy - h as f32 / 2.0).round() as i32,
                        )
                    })
                } else {
                    None
                };
                if !grab.dragging || at.is_some() {
                    next = layout_edit::add(self.screen, self.layout, k, at);
                }
            }
            Hit::Tile(k) | Hit::Grip(k, _) if grab.dragging => {
                next = grab
                    .target
                    .and_then(|r| layout_edit::commit(self.screen, self.layout, k, r));
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
        pv_w: f32,
    ) {
        let sk = &self.skin;
        if self.screen == Screen::Lock {
            return super::lock_mock::tile(r, kind, b, full, hot, alpha, lifted, pv_w, sk);
        }
        let ink = color::to_iced(sk.pal.text);
        let hot = if lifted { 1.0 } else { hot };
        // A battery pane is content-sized: the pane the greeter draws is as big
        // as its gauges (`ec_layout::intrinsic`), anchored at its footprint's
        // top-left, so that is the pane drawn here, not the whole footprint.
        let gauges = (kind == Kind::Battery)
            .then(|| self.gauge_pane(b, full, pv_w))
            .flatten();
        let pane = gauges.map_or(b, |g| g.0);
        let b = pane;
        let radius = tile_radius(b, pv_w);
        // Frosted ground, then the light tint a step brighter when held.
        fill(
            r,
            b,
            radius,
            sk.base.scale_alpha(
                if sk.dense {
                    lp::CARD_DENSE
                } else {
                    cells::CARD_BACK
                } * alpha,
            ),
            None,
        );
        fill(
            r,
            b,
            radius,
            ink.scale_alpha(lerp(cells::CARD.0, t::TILE_FILL_HOT, hot) * alpha),
            Some((
                ink.scale_alpha(lerp(cells::CARD.1, t::TILE_RIM_HOT, hot) * alpha),
                space::HAIRLINE,
            )),
        );
        let c = b.center();
        let full = Rectangle::new(
            Point::new(c.x - full.width / 2.0, c.y - full.height / 2.0),
            full.size(),
        );
        let k = pv_w / t::REAL_W;
        if let Some((_, n, scale)) = gauges {
            return gauge_mock(r, b, n, scale, sk, alpha, k);
        }
        mock(r, kind, full, b, sk, alpha, k);
    }

    /// The battery pane the greeter draws for the sample's devices: its
    /// rectangle in the preview, how many gauges fit the footprint
    /// (`ec_layout::fit`), and the scale they are drawn at (`content_scale`).
    /// `None` when there is nothing to show (the greeter draws no pane then).
    fn gauge_pane(&self, b: Rectangle, full: Rectangle, pv_w: f32) -> Option<(Rectangle, usize, f32)> {
        let k = pv_w / t::REAL_W;
        let devices = self.skin.batteries.len();
        let (_, n) = ec_layout::fit(self.layout, Kind::Battery, devices)?;
        if n == 0 || k <= 0.0 {
            return None;
        }
        let c = b.center();
        let (rw, rh) = real_size(full.size(), k);
        let scale = ec_greeter_ui::motion::content_scale(rw, rh, n);
        let (pw, ph) = ec_layout::intrinsic::px(n);
        // The footprint's top-left on the real screen is its cell edge plus the
        // real gap; the preview's tile keeps its own, smaller one.
        let x = c.x - full.width / 2.0 + (t::REAL_GAP * k - t::TILE_GAP);
        let y = c.y - full.height / 2.0 + (t::REAL_GAP * k - t::TILE_GAP);
        let pane = Rectangle::new(
            Point::new(x, y),
            Size::new(pw as f32 * scale * k, ph as f32 * scale * k),
        );
        Some((pane, n, scale))
    }
}

/// The greeter's battery pane: a row of gauges, each the greeter's own drawing
/// (`ec_greeter_ui::gauge::paint`) with its percentage under it, at the
/// intrinsic proportions of `ec_layout::intrinsic`, scaled by the preview's `k`.
fn gauge_mock(r: &mut iced::Renderer, pane: Rectangle, n: usize, scale: f32, sk: &Skin, a: f32, k: f32) {
    use ec_greeter_ui::gauge::{paint, Colours, Spec};
    use ec_greeter_ui::tokens::gauge as gt;
    let s = scale * k;
    let specs: Vec<Spec> = sk.batteries.iter().take(n).map(Spec::of).collect();
    let side = gt::GAUGE_PX as f32 * s;
    let origin = Point::new(pane.x + gt::PAD_PX as f32 * s, pane.y + gt::PAD_PX as f32 * s);
    let pitch = side + gt::GAUGE_GAP_PX as f32 * s;
    let row_pitch = (gt::CELL_H_PX + gt::ROW_GAP_PX) as f32 * s;
    let cell_at = |i: usize| {
        Point::new(
            origin.x + (i % gt::MAX_PER_ROW) as f32 * pitch,
            origin.y + (i / gt::MAX_PER_ROW) as f32 * row_pitch,
        )
    };
    let fade = |c: ec_theme::Rgba| c.with_alpha(c.0[3] * a);
    let mut frame = iced::widget::canvas::Frame::new(&*r, pane.size());
    for (i, spec) in specs.iter().enumerate() {
        let p = cell_at(i);
        let mut colours = Colours::of(&sk.pal, spec);
        colours.ring = fade(colours.ring);
        colours.track = fade(colours.track);
        colours.ink = fade(colours.ink);
        colours.bolt = fade(colours.bolt);
        paint(
            &mut frame,
            Point::new(p.x - pane.x, p.y - pane.y),
            side,
            spec,
            &colours,
        );
    }
    r.with_translation(Vector::new(pane.x, pane.y), |r| {
        geometry::Renderer::draw_geometry(r, frame.into_geometry());
    });
    let mut pen = Pen {
        r,
        k,
        clip: pane,
        a,
        pal: &sk.pal,
        lock: false,
    };
    let pal = pen.pal;
    let (line, numeral) = (gt::NUMERAL_PX as f32 * scale, gt::NUMERAL_SIZE * scale);
    for (i, spec) in specs.iter().enumerate() {
        let p = cell_at(i);
        // The numeral sits at the bottom of its box under the gauge.
        let y = p.y + side + (line - tile::line(numeral)) * k;
        pen.text(
            &format!("{}%", spec.percent.min(100)),
            p.x,
            y,
            side,
            numeral,
            font::DATA_MEDIUM,
            pal.text,
            text::Alignment::Center,
        );
    }
}

/// A tile's corner radius: the greeter's card radius scaled by the preview's
/// scale, clamped to half the short side as the greeter's panes are.
pub(super) fn tile_radius(b: Rectangle, pv_w: f32) -> f32 {
    cells::radius(cells::CARD_RADIUS * pv_w / t::REAL_W, b.width.min(b.height))
}

/// `size` if it is readable, else nothing: text below the floor is hidden, not
/// drawn small.
fn ts(want: f32, max: f32) -> Option<f32> {
    let s = want.min(max);
    (s >= t::MIN_TEXT).then_some(s)
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
pub(super) fn glyph_card(
    r: &mut iced::Renderer,
    kind: Kind,
    b: Rectangle,
    clip: Rectangle,
    sk: &Skin,
    a: f32,
) {
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

/// The text sizes the shared sizing rules are asked with: the greeter's.
pub(super) const GLANCE_TYPE: tile::Type = tile::Type {
    micro: ec_ui::tokens::size::MICRO,
    body_small: ec_ui::tokens::size::BODY_SMALL,
    body: ec_ui::tokens::size::BODY,
};

/// The tile's size on the real screen: the preview tile (which keeps
/// `TILE_GAP` from its cell) scaled up by `1/k`, less the gap the real screen
/// leaves (`ec_layout::Rect::to_px`).
pub(super) fn real_size(preview: Size, k: f32) -> (f32, f32) {
    let up = |v: f32| ((v + 2.0 * t::TILE_GAP) / k - 2.0 * t::REAL_GAP).max(0.0);
    (up(preview.width), up(preview.height))
}

/// Draws what the real greeter draws, at the real screen's numbers, scaled by
/// `k` (the preview's width over the real screen's).
///
/// Nothing here decides how a tile looks. The tile's box is the real one
/// ([`real_size`]), its padding and inner box are `ec_glance::tile`'s, and each
/// kind's variant, sizes and parts come from the plan the greeter asks for the
/// same inputs (`ec_glance::tile` for the clock and the Glances,
/// `ec_glance::cells` for the greeting, the accounts, the field and the power
/// pills). The colours are the greeter's palette for the previewed theme, the
/// faces and the words the greeter's. All this does is the scaling: positions
/// and sizes are the plan's times `k`, in the order the greeter's widgets lay
/// them out.
///
/// The deviations, all for legibility at a miniature, never for the layout:
/// text the scale would take under [`t::MIN_TEXT`] is drawn at the floor (its
/// line box keeps the scaled height, so the variant, the rows and the rails
/// are the real ones), a rail or bar is at least 2px thick, and a tile whose
/// inner box is too small to draw anything in is a glyph.
pub(super) struct Pen<'r> {
    pub(super) r: &'r mut iced::Renderer,
    pub(super) k: f32,
    pub(super) clip: Rectangle,
    pub(super) a: f32,
    pub(super) pal: &'r Palette,
    /// The lock screen's tiles: its Glances set their headings as `micro_label`
    /// does (small caps, tracked).
    pub(super) lock: bool,
}

impl Pen<'_> {
    pub(super) fn colour(&self, c: Rgba) -> Color {
        color::to_iced(c).scale_alpha(self.a)
    }

    /// Text at the real size `px`, scaled but not below the floor.
    pub(super) fn fs(&self, px: f32) -> f32 {
        (px * self.k).max(t::MIN_TEXT)
    }

    /// One line of text in a box of the real line height, scaled: `x`, `y` and
    /// `w` are preview px.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn text(
        &mut self,
        s: &str,
        x: f32,
        y: f32,
        w: f32,
        px: f32,
        f: iced::Font,
        c: Rgba,
        ax: text::Alignment,
    ) {
        let ink = self.colour(c);
        put(
            self.r,
            s,
            Rectangle::new(Point::new(x, y), Size::new(w.max(0.0), tile::line(px) * self.k)),
            self.fs(px),
            f,
            ink,
            ax,
            alignment::Vertical::Center,
            self.clip,
        );
    }

    /// A cell on a card, as the greeter paints one over the wallpaper: text
    /// ink at `cells::CELL`'s alphas, with a hairline. `ask` is the real
    /// radius, clamped to half the short side as the greeter's panes are.
    /// `fade` is the greeter's `Ground::fade`.
    pub(super) fn ground(&mut self, b: Rectangle, ask: f32, fade: f32) {
        let radius = cells::radius(ask * self.k, b.width.min(b.height));
        let ink = self.pal.text;
        let f = self.colour(ink.with_alpha(cells::CELL.0)).scale_alpha(fade);
        let rim = self.colour(ink.with_alpha(cells::CELL.1)).scale_alpha(fade);
        fill(self.r, b, radius, f, Some((rim, space::HAIRLINE)));
    }

    /// A Glance heading's ink: the greeter's tertiary, or the lock's secondary
    /// (its `micro`, which holds 4.5:1 on a light scheme).
    pub(super) fn head_ink(&self) -> ec_theme::Rgba {
        if self.lock {
            self.pal.text_secondary
        } else {
            self.pal.text_tertiary
        }
    }

    /// A Glance's heading: the greeter's plain caps, or the lock's `micro_label`.
    pub(super) fn heading(&self, greeter: &str, lock: &str) -> String {
        if self.lock {
            ec_ui::widget::tracked(lock)
        } else {
            greeter.to_owned()
        }
    }

    /// The charge rail: a track with the lit part in text ink.
    pub(super) fn rail(&mut self, x: f32, y: f32, w: f32, pct: u8, real_h: f32) {
        let h = (real_h * self.k).max(2.0);
        let bar = Rectangle::new(Point::new(x, y), Size::new(w.max(0.0), h));
        let (track, lit) = (self.colour(self.pal.track), self.colour(self.pal.text));
        fill(self.r, bar, h / 2.0, track, None);
        fill(
            self.r,
            Rectangle::new(
                bar.position(),
                Size::new(bar.width * f32::from(pct.min(100)) / 100.0, h),
            ),
            h / 2.0,
            lit,
            None,
        );
    }
}

/// What the greeter draws in `tile_px` (the tile's preview rectangle), for
/// every kind. See [`Pen`].
fn mock(r: &mut iced::Renderer, kind: Kind, tile_px: Rectangle, clip: Rectangle, sk: &Skin, a: f32, k: f32) {
    if k <= 0.0 {
        return;
    }
    let (real_w, real_h) = real_size(tile_px.size(), k);
    let pad = tile::pad_for(real_w, real_h);
    let inner = tile::inner(real_w, real_h);
    let (ox, oy) = (tile_px.x + pad * k, tile_px.y + pad * k);
    let (iw, ih) = (inner.w * k, inner.h * k);
    if iw < t::GLYPH_W || ih < t::GLYPH_H {
        let b = Rectangle::new(Point::new(ox, oy), Size::new(iw.max(1.0), ih.max(1.0)));
        return glyph_card(r, kind, b, clip, sk, a);
    }
    let mut pen = Pen {
        r,
        k,
        clip,
        a,
        pal: &sk.pal,
        lock: false,
    };
    let at = Rectangle::new(Point::new(ox, oy), Size::new(iw, ih));
    match kind {
        Kind::Clock => clock_mock(&mut pen, inner, at, sk),
        Kind::Battery => battery_mock(&mut pen, inner, at, sk),
        Kind::Wifi => wifi_mock(&mut pen, inner, at, sk),
        Kind::Greeting => greeting_mock(&mut pen, inner, at, sk),
        Kind::Users => users_mock(&mut pen, inner, at),
        Kind::Password => password_mock(&mut pen, inner, at),
        Kind::Power => power_mock(&mut pen, inner, at),
        // Never placed: it needs a session, so the editor does not offer it.
        Kind::NowPlaying => {}
    }
}

pub(super) fn clock_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle, sk: &Skin) {
    let k = p.k;
    let c = &sk.clock;
    let pal = p.pal;
    let plan = tile::clock(inner, c.twelve, c.has_date);
    let (tw, dw) = (
        measure(c.time, plan.numeral * k, font::UI_MEDIUM),
        plan.date.map_or(0.0, |d| measure(c.date, d * k, font::UI)),
    );
    let h = plan.used_h() * k;
    let (x, y) = match plan.align {
        tile::Align::BottomLeft => (at.x, at.y + at.height - h),
        tile::Align::Centre => (at.x + (at.width - tw.max(dw)) / 2.0, at.y + (at.height - h) / 2.0),
    };
    p.text(
        c.time,
        x,
        y,
        at.width,
        plan.numeral,
        font::UI_MEDIUM,
        pal.text,
        text::Alignment::Left,
    );
    if let Some(d) = plan.date {
        let dy = y + (tile::line(plan.numeral) + tile::CLOCK_GAP) * k;
        p.text(
            c.date,
            x,
            dy,
            at.width,
            d,
            font::UI,
            pal.text_secondary,
            text::Alignment::Left,
        );
    }
}

fn quiet_mock(p: &mut Pen<'_>, at: Rectangle, s: &str) {
    let pal = p.pal;
    // The greeter centres the one line in the box, whole.
    let h = tile::line(GLANCE_TYPE.body_small) * p.k;
    p.text(
        s,
        at.x,
        at.y + (at.height - h) / 2.0,
        at.width,
        GLANCE_TYPE.body_small,
        font::UI,
        pal.text_secondary,
        text::Alignment::Center,
    );
}

pub(super) fn battery_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle, sk: &Skin) {
    let k = p.k;
    let (ox, oy, iw, ih) = (at.x, at.y, at.width, at.height);
    let bottom = oy + ih;
    let pal = p.pal;
    let ty = GLANCE_TYPE;
    let left = text::Alignment::Left;
    let head = |p: &mut Pen<'_>| {
        let word = p.heading("BATTERY", cells::lock::HEAD_BATTERY);
        let ink = p.head_ink();
        p.text(&word, ox, oy, iw, ty.micro, font::DATA_MEDIUM, ink, left);
    };
    match (tile::battery(inner, &sk.batteries, ty), sk.batteries.first()) {
        (BatteryPlan::Empty, _) | (_, None) => quiet_mock(p, at, "No battery"),
        (
            BatteryPlan::Compact {
                figure,
                rail: with_rail,
            },
            Some(f),
        ) => {
            let h = tile::line(figure) * k;
            let y = oy + (ih - h) / 2.0;
            let s = format!("{}%", f.percent);
            let w = measure(&s, figure * k, font::DATA_MEDIUM);
            p.text(&s, ox, y, w + 2.0, figure, font::DATA_MEDIUM, pal.text, left);
            if with_rail {
                let rx = ox + w + tile::CELL_GAP * k;
                p.rail(
                    rx,
                    y + (h - tile::RAIL_H * k) / 2.0,
                    ox + iw - rx,
                    f.percent,
                    tile::RAIL_H,
                );
            }
        }
        (
            BatteryPlan::One {
                figure,
                rail: with_rail,
                note,
            },
            Some(f),
        ) => {
            head(p);
            let mut y = bottom;
            if note {
                y -= tile::line(ty.micro) * k;
                p.text(
                    tile::charge_note(f.charge),
                    ox,
                    y,
                    iw,
                    ty.micro,
                    font::UI,
                    pal.text_secondary,
                    left,
                );
                y -= tile::GAP * k;
            }
            if with_rail {
                y -= tile::RAIL_H * k;
                p.rail(ox, y, iw, f.percent, tile::RAIL_H);
                y -= tile::GAP * k;
            }
            y -= tile::line(figure) * k;
            p.text(
                &format!("{}%", f.percent),
                ox,
                y,
                iw,
                figure,
                font::DATA_MEDIUM,
                pal.text,
                left,
            );
        }
        (
            BatteryPlan::Many {
                shown,
                rail: with_rail,
                tag,
                gap,
                name_chars,
                scale,
            },
            Some(_),
        ) => {
            head(p);
            let st = ty.scaled(scale);
            let mut y = oy + (tile::line(ty.micro) + tile::GAP) * k;
            for b in sk.batteries.iter().take(shown) {
                let name = if b.internal {
                    "Internal".to_owned()
                } else {
                    ec_glance::clean(&b.name)
                };
                let pct = format!("{}%", b.percent);
                let pw = measure("100%", p.fs(st.body), font::DATA_MEDIUM);
                let cw = if tag && b.charge == Charge::Charging {
                    measure("Charging", p.fs(st.micro), font::UI)
                } else {
                    0.0
                };
                let gaps = tile::CELL_GAP * k * if cw > 0.0 { 2.0 } else { 1.0 };
                let room = (iw - pw - cw - gaps).max(0.0);
                // The screens cut a name by `truncate` only and clip the rest.
                let nm = tile::truncate(&name, name_chars);
                p.text(
                    &nm,
                    ox,
                    y,
                    room + 2.0,
                    st.body_small,
                    font::UI,
                    pal.text_secondary,
                    left,
                );
                if cw > 0.0 {
                    p.text(
                        "Charging",
                        ox + iw - pw - cw - tile::CELL_GAP * k,
                        y,
                        cw + 2.0,
                        st.micro,
                        font::UI,
                        pal.text_secondary,
                        left,
                    );
                }
                p.text(
                    &pct,
                    ox + iw - pw,
                    y,
                    pw,
                    st.body,
                    font::DATA_MEDIUM,
                    pal.text,
                    text::Alignment::Right,
                );
                y += tile::line(st.body) * k;
                if with_rail {
                    y += tile::GAP * k;
                    p.rail(ox, y, iw, b.percent, tile::RAIL_H * scale);
                    y += tile::RAIL_H * scale * k;
                }
                y += gap * k;
            }
        }
    }
}

pub(super) fn wifi_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle, sk: &Skin) {
    let k = p.k;
    let (ox, oy, iw, ih) = (at.x, at.y, at.width, at.height);
    let pal = p.pal;
    let left = text::Alignment::Left;
    let plan = tile::wifi(inner, sk.link, GLANCE_TYPE);
    let (Some(q), WifiPlan::Compact { figure, bars, gap } | WifiPlan::Full { figure, bars, gap }) =
        (sk.link, plan)
    else {
        return quiet_mock(p, at, "No link");
    };
    let h = tile::line(figure) * k;
    let y = if matches!(plan, WifiPlan::Full { .. }) {
        let head = p.heading("LINK", cells::lock::HEAD_LINK);
        p.text(
            &head,
            ox,
            oy,
            iw,
            GLANCE_TYPE.micro,
            font::DATA_MEDIUM,
            p.head_ink(),
            left,
        );
        oy + ih - h
    } else {
        oy + (ih - h) / 2.0
    };
    let s = format!("{q}%");
    let w = measure(&s, figure * k, font::DATA_MEDIUM);
    p.text(&s, ox, y, w + 2.0, figure, font::DATA_MEDIUM, pal.text, left);
    if let Some(b) = bars {
        let x0 = ox + w + gap * k;
        for i in 0..tile::WIFI_BARS {
            let (bw, bg) = (b.w * k, b.gap * k);
            let bh = tile::bar_h(figure, i) * k;
            let c = if i < b.lit { pal.text } else { pal.track };
            let col = p.colour(c);
            fill(
                p.r,
                Rectangle::new(
                    Point::new(x0 + i as f32 * (bw + bg), y + h - bh),
                    Size::new(bw, bh),
                ),
                bw / 2.0,
                col,
                None,
            );
        }
    }
}

fn greeting_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle, sk: &Skin) {
    let px = cells::greeting(inner);
    let h = tile::line(px) * p.k;
    let pal = p.pal;
    p.text(
        &sk.greeting,
        at.x,
        at.y + (at.height - h) / 2.0,
        at.width,
        px,
        font::UI_MEDIUM,
        pal.text,
        text::Alignment::Left,
    );
}

fn users_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle) {
    let k = p.k;
    let pal = p.pal;
    let plan = cells::users(inner, t::MOCK_NAMES.len());
    let left = text::Alignment::Left;
    let micro = ec_ui::tokens::size::MICRO;
    p.text(
        cells::ACCOUNTS_HEAD,
        at.x,
        at.y,
        at.width,
        micro,
        font::DATA_MEDIUM,
        pal.text_tertiary,
        left,
    );
    let mut y = at.y + (tile::line(micro) + cells::LINE_GAP) * k;
    for name in t::MOCK_NAMES.iter().take(plan.shown) {
        let row = Rectangle::new(Point::new(at.x, y), Size::new(at.width, plan.row_h * k));
        // The list starts with no account chosen: every row is plain.
        p.ground(row, cells::CELL_RADIUS, 1.0);
        let d = plan.avatar * k;
        let av = Rectangle::new(
            Point::new(row.x + cells::ROW_PAD_X * k, row.y + (row.height - d) / 2.0),
            Size::new(d, d),
        );
        let lift = p.colour(pal.lift);
        fill(p.r, av, d / 2.0, lift, None);
        let initial = name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default();
        let ipx = plan.avatar * cells::INITIAL_FRAC;
        p.text(
            &initial,
            av.x,
            av.y + (d - tile::line(ipx) * k) / 2.0,
            d,
            ipx,
            font::UI_MEDIUM,
            pal.text,
            text::Alignment::Center,
        );
        let body = ec_ui::tokens::size::BODY;
        p.text(
            name,
            av.x + d + cells::LINE_GAP * k,
            row.y + (row.height - tile::line(body) * k) / 2.0,
            row.x + row.width - (av.x + d + cells::LINE_GAP * k),
            body,
            font::UI_MEDIUM,
            pal.text,
            left,
        );
        y += (plan.row_h + cells::ROW_GAP) * k;
    }
    if plan.more > 0 {
        p.text(
            &cells::more_label(plan.more),
            at.x,
            y,
            at.width,
            micro,
            font::DATA,
            pal.text_tertiary,
            left,
        );
    }
}

fn password_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle) {
    let k = p.k;
    let pal = p.pal;
    let plan = cells::password(inner);
    let left = text::Alignment::Left;
    let (body, small) = (ec_ui::tokens::size::BODY, ec_ui::tokens::size::BODY_SMALL);
    // The screen starts with no account chosen: the field and its pill are
    // inert, dimmed, the field not focused, no caret. The ground dims by one
    // share and the ink by the scheme's own (`tokens::disabled_ink`).
    let dim = cells::DISABLED;
    let ink_dim = ec_greeter_ui::tokens::disabled_ink(pal.scheme);
    let (fh, sw) = (plan.field_h * k, plan.submit_w * k);
    let gap = cells::LINE_GAP * k;
    let field = Rectangle::new(at.position(), Size::new((at.width - sw - gap).max(0.0), fh));
    p.ground(field, cells::CELL_RADIUS, dim);
    p.text(
        cells::PLACEHOLDER,
        field.x + cells::ROW_PAD_X * k,
        field.y + (fh - tile::line(body) * k) / 2.0,
        field.width - 2.0 * cells::ROW_PAD_X * k,
        body,
        font::UI,
        pal.text_tertiary.with_alpha(pal.text_tertiary.0[3] * ink_dim),
        left,
    );
    let submit = Rectangle::new(Point::new(field.x + field.width + gap, at.y), Size::new(sw, fh));
    p.ground(submit, cells::PILL_RADIUS, dim);
    p.text(
        cells::SIGN_IN,
        submit.x,
        submit.y + (fh - tile::line(small) * k) / 2.0,
        submit.width,
        small,
        font::UI_MEDIUM,
        pal.text.with_alpha(pal.text.0[3] * ink_dim),
        text::Alignment::Center,
    );
    p.text(
        cells::NOTE_IDLE,
        at.x,
        at.y + fh + cells::NOTE_GAP * k,
        at.width,
        small,
        font::UI,
        pal.text_tertiary,
        left,
    );
}

fn power_mock(p: &mut Pen<'_>, inner: tile::Inner, at: Rectangle) {
    let k = p.k;
    let pal = p.pal;
    let plan = cells::power(inner);
    let small = ec_ui::tokens::size::BODY_SMALL;
    let gap = cells::ROW_GAP * k;
    let pw = (at.width - 2.0 * gap) / 3.0;
    let ph = plan.pill_h * k;
    let y = at.y + (at.height - ph) / 2.0;
    for (i, (full, short)) in cells::POWER.iter().enumerate() {
        let pill = Rectangle::new(Point::new(at.x + i as f32 * (pw + gap), y), Size::new(pw, ph));
        p.ground(pill, cells::PILL_RADIUS, 1.0);
        p.text(
            if plan.full { full } else { short },
            pill.x,
            y + (ph - tile::line(small) * k) / 2.0,
            pw,
            small,
            font::UI_MEDIUM,
            pal.text,
            text::Alignment::Center,
        );
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

    /// The preview asks the real screen's tile size, not its own.
    #[test]
    fn the_preview_tile_maps_back_to_the_real_pixel_size() {
        let pv_w = 700.0;
        let k = pv_w / t::REAL_W;
        for r in [
            Rect::new(0, 0, 12, 6),
            Rect::new(12, 3, 5, 4),
            Rect::new(4, 0, 3, 1),
        ] {
            let (_, _, rw, rh) = r.to_px(1920, 1080, 8);
            let cell = |c: i32, n: i32, total: f32| c as f32 * total / n as f32;
            let preview = Size::new(
                (cell(r.x + r.w, COLS, pv_w) - cell(r.x, COLS, pv_w)) - 2.0 * t::TILE_GAP,
                (cell(r.y + r.h, ROWS, pv_w * t::ASPECT) - cell(r.y, ROWS, pv_w * t::ASPECT))
                    - 2.0 * t::TILE_GAP,
            );
            let (w, h) = real_size(preview, k);
            assert!(
                (w - rw as f32).abs() < 3.0 && (h - rh as f32).abs() < 3.0,
                "{r:?}: {w}x{h} vs {rw}x{rh}"
            );
        }
    }

    /// The Glance mocks hold no sizing of their own: the old hand-drawn
    /// literals are gone and the shared module is asked for each plan.
    #[test]
    fn the_glance_mocks_ask_the_shared_module() {
        let src = include_str!("layout_view.rs");
        let body = src.split("#[cfg(test)]").next().unwrap_or(src);
        for gone in [
            "(\"Internal\", 0.72)",
            "\"68%\"",
            "\"Choose an account to sign in\"",
            "[\"Suspend\", \"Restart\", \"Power off\"]",
            "fn fit_w(",
            "b.height * 0.24",
        ] {
            assert!(!body.contains(gone), "{gone} is back");
        }
        for asked in [
            "tile::battery(",
            "tile::wifi(",
            "tile::clock(",
            "tile::inner(",
            "cells::greeting(",
            "cells::users(",
            "cells::password(",
            "cells::power(",
        ] {
            assert!(body.contains(asked), "{asked} is not asked");
        }
    }

    /// The lock's tiles hold no sizing of their own either: the shared plans
    /// are asked, and the lock's words and numbers are `ec_glance::cells::lock`.
    #[test]
    fn the_lock_mocks_ask_the_shared_plans() {
        let src = include_str!("lock_mock.rs");
        let body = src.split("#[cfg(test)]").next().unwrap_or(src);
        for asked in [
            "cells::lock_frame(",
            "cells::lock_greeting(",
            "cells::lock_password(",
            "clock_mock(",
            "battery_mock(",
            "wifi_mock(",
            "lock::NOTE_READY",
            "lock::HINTS",
            "lock::WORD_LOCKED",
            "cells::lock::UNLOCK",
            "cells::PLACEHOLDER",
        ] {
            assert!(body.contains(asked), "{asked} is not asked");
        }
        for gone in [
            "\"Enter your password to unlock\"",
            "\"LOCKED\"",
            "\"Unlock\"",
            "0.62",
        ] {
            assert!(!body.contains(gone), "{gone} is a literal in the lock mock");
        }
    }

    /// Every shape the sample picker offers gives a plan that fits the tile.
    #[test]
    fn every_sample_shape_plans_inside_its_tile() {
        for (w, h) in [(40.0, 30.0), (200.0, 60.0), (300.0, 150.0), (900.0, 400.0)] {
            let i = tile::Inner { w, h };
            for b in sample::Batteries::ALL {
                let p = tile::battery(i, &b.data(), GLANCE_TYPE);
                assert!(p.used_h(GLANCE_TYPE) <= h + 0.01 || matches!(p, BatteryPlan::Empty));
            }
            for l in sample::Link::ALL {
                let p = tile::wifi(i, l.quality(), GLANCE_TYPE);
                assert!(p.used_h(GLANCE_TYPE) <= h + 0.01 && p.used_w() <= w + 0.01);
            }
        }
    }

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
    fn a_left_line_is_never_set_in_a_box_narrower_than_itself() {
        let left = text::Alignment::Left;
        // A name column squeezed by its percentage and tag: the line must not
        // break, as the lock's `Wrapping::None` label does not.
        assert!(one_line_width(20.0, 52.4, left) >= 52.4);
        assert_eq!(one_line_width(80.0, 52.4, left), 80.0);
        assert_eq!(one_line_width(20.0, 52.4, text::Alignment::Right), 20.0);
        assert_eq!(one_line_width(20.0, 52.4, text::Alignment::Center), 20.0);
    }

    #[test]
    fn text_under_the_floor_is_hidden_not_shrunk() {
        assert_eq!(ts(t::MIN_TEXT - 0.1, 20.0), None);
        assert_eq!(ts(30.0, 11.0), Some(11.0));
    }
}
