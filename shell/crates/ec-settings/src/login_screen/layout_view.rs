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
use iced::advanced::image::Renderer as _;
use iced::advanced::layout::{self, Layout as NodeLayout};
use iced::advanced::renderer;
use iced::advanced::text::{self, LineHeight, Renderer as _, Shaping, Wrapping};
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

pub struct Editor<'a, Message> {
    layout: &'a Layout,
    skin: Skin,
    on_change: Box<dyn Fn(Layout) -> Message + 'a>,
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

fn chip_width(kind: Kind) -> f32 {
    let n = kind_label(kind).chars().count() as f32;
    // The plus mark, then the label.
    2.0 * t::CHIP_PAD + t::CHIP_SIZE + n * t::CHIP_SIZE * t::CHIP_GLYPH
}

fn geo(width: f32, offered: &[Kind]) -> Geo {
    let w = width.clamp(0.0, t::MAX_W);
    let pw = (w - 2.0 * t::PAD).max(1.0);
    let pv = Rectangle::new(Point::new(t::PAD, t::PAD), Size::new(pw, pw * t::ASPECT));
    let tray_y = pv.y + pv.height + t::TRAY_GAP;
    let (mut x, mut y) = (t::PAD, tray_y);
    let mut chips = Vec::with_capacity(offered.len());
    for &k in offered {
        let cw = chip_width(k);
        if x > t::PAD && x + cw > w - t::PAD {
            x = t::PAD;
            y += t::CHIP_H + t::CHIP_GAP;
        }
        chips.push((k, Rectangle::new(Point::new(x, y), Size::new(cw, t::CHIP_H))));
        x += cw + t::CHIP_GAP;
    }
    Geo {
        w,
        h: y + t::CHIP_H + t::PAD,
        cw: pv.width / COLS as f32,
        ch: pv.height / ROWS as f32,
        pv,
        chips,
        tray_y,
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

/// Which grip, if any, `p` is over on a tile: the corner wins over the edges.
fn grip_at(tile: Rectangle, p: Point) -> Option<Handle> {
    if !tile.contains(p) {
        return None;
    }
    let (dx, dy) = (tile.x + tile.width - p.x, tile.y + tile.height - p.y);
    let corner = t::HIT_CORNER.min(tile.width / 2.0).min(tile.height / 2.0);
    if dx <= corner && dy <= corner {
        Some(Handle::SouthEast)
    } else if dx <= t::HIT_EDGE.min(tile.width / 3.0) {
        Some(Handle::East)
    } else if dy <= t::HIT_EDGE.min(tile.height / 3.0) {
        Some(Handle::South)
    } else {
        None
    }
}

// ------------------------------------------------------------------- state

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
        self.x.set_target_under(r.x as f32, m, now);
        self.y.set_target_under(r.y as f32, m, now);
        self.w.set_target_under(r.w as f32, m, now);
        self.h.set_target_under(r.h as f32, m, now);
    }

    fn aim_f(&mut self, x: f32, y: f32, m: Motion, now: Instant) {
        self.x.set_target_under(x, m, now);
        self.y.set_target_under(y, m, now);
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
}

impl Hit {
    fn kind(self) -> Option<Kind> {
        match self {
            Hit::Nothing => None,
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
                    s.pop.set_target_under(1.0, LAND, now);
                }
                None => {
                    let pop = if self.mounted { 0.0 } else { 1.0 };
                    self.slots.push(Slot::new(tile.kind, tile.rect, pop));
                }
            }
        }
        for s in &mut self.slots {
            if layout.get(s.kind).is_none() {
                s.pop.set_target_under(0.0, LAND, now);
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
        if let Some(d) = &mut self.dots {
            d.tick(now);
            if d.value() <= 0.01 && !d.animating() && d.target() == 0.0 {
                self.dots = None;
            }
        }
    }

    fn busy(&self) -> bool {
        self.slots.iter().any(Slot::busy)
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
                    g.x.set_target_under(r.x as f32, HOP, now);
                    g.y.set_target_under(r.y as f32, HOP, now);
                    g.w.set_target_under(r.w as f32, HOP, now);
                    g.h.set_target_under(r.h as f32, HOP, now);
                }
                g.shown.set_target_under(1.0, HOP, now);
            }
            (Some(r), None) => {
                let a = |v: i32| Animated::new(v as f32, HOP);
                let mut shown = Animated::new(0.0, HOP);
                shown.set_target_under(1.0, HOP, now);
                self.ghost = Some(Ghost {
                    rect: r,
                    x: a(r.x),
                    y: a(r.y),
                    w: a(r.w),
                    h: a(r.h),
                    shown,
                });
            }
            (None, Some(g)) => g.shown.set_target_under(0.0, HOP, now),
            (None, None) => {}
        }
    }

    fn set_dots(&mut self, on: bool, now: Instant) {
        let d = self.dots.get_or_insert_with(|| Animated::new(0.0, DOTS));
        d.set_target_under(if on { 1.0 } else { 0.0 }, DOTS, now);
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
            self.hov(k)
                .set_target_under(if on { 1.0 } else { 0.0 }, HOVER, now);
        }
        if let Some(k) = lit {
            self.hov(k)
                .set_target_under(if held.is_none_or(|h| h == k) { 1.0 } else { 0.0 }, HOVER, now);
        }
    }
}

// -------------------------------------------------------------- the widget

impl<M> Editor<'_, M> {
    fn hit(&self, g: &Geo, p: Point) -> Hit {
        for tile in &self.layout.tiles {
            let r = g.rect_px(tile.rect);
            if layout_edit::removable(tile.kind) && badge_rect(r).contains(p) {
                return Hit::Badge(tile.kind);
            }
        }
        for tile in &self.layout.tiles {
            let r = g.rect_px(tile.rect);
            if r.contains(p) {
                return match grip_at(r, p) {
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
        if matches!(grab.hit, Hit::Badge(_)) {
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
        let g = geo(width, &layout_edit::offered(self.layout));
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
        let g = geo(b.width, &layout_edit::offered(self.layout));
        let st = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        st.sync(self.layout, now);
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
                        shell.request_redraw();
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if st.grab.is_none() && st.hover.is_some() => {
                st.hover = None;
                st.light(None, now);
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
            Some(Hit::Grip(_, Handle::East)) => mouse::Interaction::ResizingHorizontally,
            Some(Hit::Grip(_, Handle::South)) => mouse::Interaction::ResizingVertically,
            Some(Hit::Grip(_, Handle::SouthEast)) => mouse::Interaction::ResizingDiagonallyDown,
            Some(Hit::Badge(_)) => mouse::Interaction::Pointer,
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
        let g = geo(b.width, &layout_edit::offered(self.layout));
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
                self.tile(r, s.kind, rect, hot, alpha, lifted);
                let grips = ink.scale_alpha(lerp(t::GRIP_REST, 1.0, hot) * alpha);
                grip_marks(r, rect, grips);
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
            let rect = scaled(inset(rect, t::TILE_GAP), t::LIFT);
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
                self.tile(r, k, rect, 1.0, 1.0, true);
            });
        }

        // The tray.
        let offered = layout_edit::offered(self.layout);
        renderer.with_layer(clip, |r| {
            if offered.is_empty() {
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

/// The corner bracket and the two edge bars of a tile: what says "pull me".
fn grip_marks(r: &mut iced::Renderer, tile: Rectangle, c: Color) {
    let w = t::GRIP_W;
    let (right, bottom) = (
        tile.x + tile.width - t::TILE_RADIUS * 0.55,
        tile.y + tile.height - t::TILE_RADIUS * 0.55,
    );
    let len = t::GRIP_LEN.min(tile.width / 3.0).min(tile.height / 2.5);
    fill(
        r,
        Rectangle::new(Point::new(right - len, bottom - w), Size::new(len, w)),
        w / 2.0,
        c,
        None,
    );
    fill(
        r,
        Rectangle::new(Point::new(right - w, bottom - len), Size::new(w, len)),
        w / 2.0,
        c,
        None,
    );
    // Edge bars sit on the tile's own edge, only when there is room for them.
    let edge = t::GRIP_EDGE_LEN;
    if tile.height > 2.0 * edge + len {
        fill(
            r,
            Rectangle::new(
                Point::new(
                    tile.x + tile.width - 2.0 * w - 1.0,
                    tile.y + (tile.height - edge) / 2.0 - len / 3.0,
                ),
                Size::new(w, edge),
            ),
            w / 2.0,
            c.scale_alpha(0.7),
            None,
        );
    }
    if tile.width > 2.0 * edge + len {
        fill(
            r,
            Rectangle::new(
                Point::new(
                    tile.x + (tile.width - edge) / 2.0 - len / 3.0,
                    tile.y + tile.height - 2.0 * w - 1.0,
                ),
                Size::new(edge, w),
            ),
            w / 2.0,
            c.scale_alpha(0.7),
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
            Hit::Grip(k, _) => self
                .layout
                .get(k)
                .map_or((0.0, 0.0), |r| ((r.x + r.w) as f32 - cx, (r.y + r.h) as f32 - cy)),
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
        shell.request_redraw();
        shell.capture_event();
    }

    fn cancel(&self, st: &mut State, now: Instant) {
        st.grab = None;
        st.float = None;
        st.set_ghost(None, now);
        st.set_dots(false, now);
        st.light(None, now);
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
        st.light(self.hover_after(g, rel), now);
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
    fn tile(&self, r: &mut iced::Renderer, kind: Kind, b: Rectangle, hot: f32, alpha: f32, lifted: bool) {
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
        let inner = inset(b, (b.height.min(b.width) * 0.09).clamp(3.0, 12.0));
        mock(r, kind, b, inner, sk, alpha);
    }
}

/// Text sized to a tile, never below the floor.
fn fit(h: f32, share: f32, max: f32) -> f32 {
    (h * share).clamp(t::MIN_TEXT, max)
}

/// What the greeter draws in a tile, small. Clipped to the tile.
fn mock(r: &mut iced::Renderer, kind: Kind, tile: Rectangle, b: Rectangle, sk: &Skin, a: f32) {
    let ink = sk.ink.scale_alpha(a);
    let dim = sk.ink.scale_alpha(lp::TAG_ALPHA * a);
    let faint = sk.ink.scale_alpha(lp::FIELD_FILL * a);
    let left = text::Alignment::Left;
    let mid = alignment::Vertical::Center;
    let top = alignment::Vertical::Top;
    let label = |r: &mut iced::Renderer, s: &str, at: Point, w: f32| {
        let size = fit(tile.height, 0.075, t::LABEL_SIZE);
        put(
            r,
            s,
            Rectangle::new(at, Size::new(w, size)),
            size,
            font::DATA,
            dim,
            left,
            top,
            tile,
        );
    };
    match kind {
        Kind::Clock => {
            let chars = sk.clock.chars().count().max(1) as f32;
            let size = (b.height * 0.52).min(b.width * 1.7 / chars).max(t::MIN_TEXT);
            put(
                r,
                &sk.clock,
                Rectangle::new(b.position(), Size::new(b.width, size)),
                size,
                font::UI_MEDIUM,
                ink,
                left,
                top,
                tile,
            );
            if b.height > size * 1.9 {
                let s2 = fit(b.height, 0.13, 13.0);
                put(
                    r,
                    t::MOCK_TIME,
                    Rectangle::new(Point::new(b.x, b.y + b.height - s2), Size::new(b.width, s2)),
                    s2,
                    font::UI,
                    dim,
                    left,
                    top,
                    tile,
                );
            }
        }
        Kind::Greeting => {
            let size = fit(b.height, 0.62, 15.0);
            put(
                r,
                &sk.greeting,
                Rectangle::new(Point::new(b.x + 2.0, b.y), Size::new(b.width, b.height)),
                size,
                font::UI_MEDIUM,
                ink,
                left,
                mid,
                tile,
            );
        }
        Kind::Users => {
            label(r, "ACCOUNTS", b.position(), b.width);
            let head = fit(tile.height, 0.075, t::LABEL_SIZE) + 5.0;
            let rh = (b.height * 0.24).clamp(14.0, 28.0);
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
                let sz = fit(rh, 0.5, 12.0);
                put(
                    r,
                    &name[..1].to_uppercase(),
                    av,
                    sz * 0.85,
                    font::UI_MEDIUM,
                    ink,
                    text::Alignment::Center,
                    mid,
                    tile,
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
                    tile,
                );
                y += rh + gap;
            }
        }
        Kind::Password => {
            let fh = (b.height * 0.5).clamp(12.0, 34.0).min(b.height);
            let bw = (b.width * 0.28).min(96.0);
            let field = Rectangle::new(b.position(), Size::new(b.width - bw - 6.0, fh));
            fill(r, field, fh * 0.3, faint, None);
            let sz = fit(fh, 0.45, 12.0);
            put(
                r,
                "Password",
                Rectangle::new(Point::new(field.x + fh * 0.35, field.y), field.size()),
                sz,
                font::UI,
                dim,
                left,
                mid,
                tile,
            );
            let btn = Rectangle::new(Point::new(field.x + field.width + 6.0, b.y), Size::new(bw, fh));
            fill(r, btn, fh * 0.3, sk.ink.scale_alpha(lp::PILL_FILL * a), None);
            put(
                r,
                "Unlock",
                btn,
                sz,
                font::UI_MEDIUM,
                dim,
                text::Alignment::Center,
                mid,
                tile,
            );
            if b.height > fh * 1.7 {
                let s2 = fit(b.height, 0.14, 11.0);
                put(
                    r,
                    "Choose an account to sign in",
                    Rectangle::new(Point::new(b.x, b.y + b.height - s2 - 1.0), Size::new(b.width, s2)),
                    s2,
                    font::UI,
                    dim,
                    left,
                    top,
                    tile,
                );
            }
        }
        Kind::Power => {
            let names = ["Suspend", "Restart", "Power off"];
            let gap = 6.0;
            let pw = (b.width - gap * 2.0) / 3.0;
            let ph = b.height.min(26.0);
            let y = b.y + (b.height - ph) / 2.0;
            for (i, n) in names.iter().enumerate() {
                let pill = Rectangle::new(Point::new(b.x + i as f32 * (pw + gap), y), Size::new(pw, ph));
                fill(r, pill, ph / 2.0, sk.ink.scale_alpha(lp::PILL_FILL * a), None);
                put(
                    r,
                    n,
                    pill,
                    fit(ph, 0.46, 11.0),
                    font::UI_MEDIUM,
                    ink,
                    text::Alignment::Center,
                    mid,
                    tile,
                );
            }
        }
        Kind::Battery => {
            label(r, "BATTERY", b.position(), b.width);
            let head = fit(tile.height, 0.075, t::LABEL_SIZE) + 5.0;
            let rows = [("Internal", 0.72), ("Mouse", 0.40), ("Keyboard", 0.85)];
            let rh = (b.height * 0.3).clamp(15.0, 30.0);
            let mut y = b.y + head;
            for (name, v) in rows {
                if y + rh > b.y + b.height + 0.5 {
                    break;
                }
                let sz = fit(rh, 0.42, 11.0);
                put(
                    r,
                    &format!("{name}  {}%", (v * 100.0) as u32),
                    Rectangle::new(Point::new(b.x, y), Size::new(b.width, sz)),
                    sz,
                    font::UI,
                    ink,
                    left,
                    top,
                    tile,
                );
                let bar = Rectangle::new(Point::new(b.x, y + sz + 3.0), Size::new(b.width, 3.5));
                fill(r, bar, 1.75, sk.ink.scale_alpha(t::TRACK * a), None);
                fill(
                    r,
                    Rectangle::new(bar.position(), Size::new(bar.width * v, bar.height)),
                    1.75,
                    ink,
                    None,
                );
                y += rh;
            }
        }
        Kind::Wifi => {
            label(r, "LINK", b.position(), b.width);
            let head = fit(tile.height, 0.075, t::LABEL_SIZE) + 3.0;
            let sz = fit(b.height - head, 0.5, 30.0);
            put(
                r,
                "68%",
                Rectangle::new(Point::new(b.x, b.y + head), Size::new(b.width, sz)),
                sz,
                font::UI_MEDIUM,
                ink,
                left,
                top,
                tile,
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

    #[test]
    fn the_preview_is_16_by_9_and_the_tray_follows_it() {
        let g = geo(700.0, &[Kind::Battery, Kind::Wifi]);
        assert!((g.pv.width / g.pv.height - 16.0 / 9.0).abs() < 1e-3);
        assert!(g.chips.iter().all(|(_, r)| r.y >= g.pv.y + g.pv.height));
        assert!(g.h > g.pv.y + g.pv.height);
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
    fn cells_round_trip_through_pixels() {
        let g = geo(640.0, &[]);
        let r = g.rect_px(Rect::new(8, 5, 8, 4));
        let (cx, cy) = g.cell(Point::new(r.x + r.width / 2.0, r.y + r.height / 2.0));
        assert!((cx - 12.0).abs() < 0.1 && (cy - 7.0).abs() < 0.1);
    }

    #[test]
    fn the_corner_grip_wins_over_the_edges() {
        let tile = Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 60.0));
        assert_eq!(grip_at(tile, Point::new(95.0, 55.0)), Some(Handle::SouthEast));
        assert_eq!(grip_at(tile, Point::new(95.0, 20.0)), Some(Handle::East));
        assert_eq!(grip_at(tile, Point::new(40.0, 56.0)), Some(Handle::South));
        assert_eq!(grip_at(tile, Point::new(40.0, 20.0)), None);
        assert_eq!(grip_at(tile, Point::new(140.0, 20.0)), None);
    }
}
