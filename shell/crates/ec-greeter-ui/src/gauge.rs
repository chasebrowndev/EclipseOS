// SPDX-License-Identifier: AGPL-3.0-only
//! The battery gauge: a device's icon inside a rounded-square progress ring,
//! its percentage in numerals below (the iOS battery widget, with squircles
//! where it has circles).
//!
//! The ring is one stroked rounded-rectangle path. A faint track runs all the
//! way round; the charge is the same path walked from the top centre
//! clockwise for `percent` of its length, with round caps. The walk is exact
//! ([`walk`]), not an approximation by sampled points, so 50% lands on the
//! middle of the bottom edge.
//!
//! Colour comes from the palette only: ring in the accent (its text step on
//! light glass, where the gold would wash out), danger when low and not
//! charging, ink for the icon and numerals. No gradient, no glow: glassy, not
//! glowy. A bolt in the accent marks charging.
//!
//! Reuse: [`gauge`] and [`cell`] are the widgets; [`paint`] draws onto any
//! canvas [`Frame`], which is what the Settings layout editor's preview does.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use ec_glance::{Battery, Charge};
use ec_theme::{Palette, Rgba};
use iced::widget::canvas::{self, path::Arc, Canvas, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::widget::{column, container, text};
use iced::{alignment, mouse, Element, Length, Point, Radians, Rectangle, Renderer, Size, Theme};

use crate::tokens::{self, font, gauge as g, to_iced};

/// What a battery is, for the icon. `ec_glance` reports only whether it is the
/// machine's own and a device-supplied name, so connected devices are told apart
/// by words in that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Laptop,
    Mouse,
    Keyboard,
    Headphones,
    Phone,
    Generic,
}

impl DeviceKind {
    pub fn of(b: &Battery) -> Self {
        Self::from_name(b.internal, &b.name)
    }

    /// The name is untrusted text: it is only compared, never drawn.
    pub fn from_name(internal: bool, name: &str) -> Self {
        if internal {
            return DeviceKind::Laptop;
        }
        let n = name.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
        if has(&[
            "mouse",
            "trackball",
            "trackpad",
            "touchpad",
            "mx master",
            "mx anywhere",
        ]) {
            DeviceKind::Mouse
        } else if has(&["keyboard", "keypad", "keychron", "kbd"]) {
            DeviceKind::Keyboard
        } else if has(&[
            "headphone",
            "headset",
            "earbud",
            "buds",
            "airpods",
            "earphone",
            "wh-1000",
            "wf-1000",
        ]) {
            DeviceKind::Headphones
        } else if has(&["phone", "iphone", "pixel", "galaxy", "android"]) {
            DeviceKind::Phone
        } else {
            DeviceKind::Generic
        }
    }
}

/// One gauge's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub kind: DeviceKind,
    pub percent: u8,
    pub charging: bool,
}

impl Spec {
    pub fn of(b: &Battery) -> Self {
        Spec {
            kind: DeviceKind::of(b),
            percent: b.percent.min(100),
            charging: b.charge == Charge::Charging,
        }
    }

    /// Low and not on charge: the ring turns to the danger colour.
    pub fn low(&self) -> bool {
        self.percent <= g::LOW_PERCENT && !self.charging
    }
}

/// The four colours a gauge uses, from the palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colours {
    pub ring: Rgba,
    pub track: Rgba,
    pub ink: Rgba,
    pub bolt: Rgba,
}

impl Colours {
    pub fn of(pal: &Palette, spec: &Spec) -> Self {
        let accent = tokens::typed_mark(pal);
        Colours {
            ring: if spec.low() { pal.danger } else { accent },
            track: pal.track,
            ink: pal.text,
            bolt: accent,
        }
    }
}

// ------------------------------------------------------------------ the walk

/// One piece of the ring's outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seg {
    Line(Point, Point),
    /// A clockwise (on screen) arc: centre, radius, start angle and sweep.
    Arc(Point, f32, f32, f32),
}

impl Seg {
    pub fn len(&self) -> f32 {
        match *self {
            Seg::Line(a, b) => a.distance(b),
            Seg::Arc(_, r, _, sweep) => r * sweep,
        }
    }

    /// The piece cut to `d` of its length.
    fn upto(&self, d: f32) -> Seg {
        let f = if self.len() > 0.0 {
            (d / self.len()).clamp(0.0, 1.0)
        } else {
            1.0
        };
        match *self {
            Seg::Line(a, b) => Seg::Line(a, Point::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f)),
            Seg::Arc(c, r, s, sweep) => Seg::Arc(c, r, s, sweep * f),
        }
    }

    pub fn end(&self) -> Point {
        match *self {
            Seg::Line(_, b) => b,
            Seg::Arc(c, r, s, sweep) => {
                let a = s + sweep;
                Point::new(c.x + r * a.cos(), c.y + r * a.sin())
            }
        }
    }
}

/// The outline of the rounded square `b` with corner radius `r`, from the top
/// centre clockwise, as nine pieces (the top edge is split at the centre).
pub fn outline(b: Rectangle, r: f32) -> [Seg; 9] {
    let (x0, y0, x1, y1) = (b.x, b.y, b.x + b.width, b.y + b.height);
    let cx = (x0 + x1) / 2.0;
    let p = Point::new;
    [
        Seg::Line(p(cx, y0), p(x1 - r, y0)),
        Seg::Arc(p(x1 - r, y0 + r), r, -FRAC_PI_2, FRAC_PI_2),
        Seg::Line(p(x1, y0 + r), p(x1, y1 - r)),
        Seg::Arc(p(x1 - r, y1 - r), r, 0.0, FRAC_PI_2),
        Seg::Line(p(x1 - r, y1), p(x0 + r, y1)),
        Seg::Arc(p(x0 + r, y1 - r), r, FRAC_PI_2, FRAC_PI_2),
        Seg::Line(p(x0, y1 - r), p(x0, y0 + r)),
        Seg::Arc(p(x0 + r, y0 + r), r, PI, FRAC_PI_2),
        Seg::Line(p(x0 + r, y0), p(cx, y0)),
    ]
}

/// The first `dist` of the outline, whole pieces then a cut one.
pub fn walk(segs: &[Seg], dist: f32) -> Vec<Seg> {
    let mut left = dist.max(0.0);
    let mut out = Vec::new();
    for s in segs {
        if left <= 0.0 {
            break;
        }
        if s.len() <= left {
            out.push(*s);
            left -= s.len();
        } else {
            out.push(s.upto(left));
            break;
        }
    }
    out
}

fn path_of(segs: &[Seg]) -> Path {
    Path::new(|b| {
        if let Some(first) = segs.first() {
            b.move_to(match *first {
                Seg::Line(a, _) => a,
                Seg::Arc(c, r, s, _) => Point::new(c.x + r * s.cos(), c.y + r * s.sin()),
            });
        }
        for s in segs {
            match *s {
                Seg::Line(_, e) => b.line_to(e),
                Seg::Arc(center, radius, start, sweep) => b.arc(Arc {
                    center,
                    radius,
                    start_angle: Radians(start),
                    end_angle: Radians(start + sweep),
                }),
            }
        }
    })
}

// ------------------------------------------------------------------ painting

fn stroke(c: Rgba, width: f32, cap: LineCap) -> Stroke<'static> {
    Stroke {
        style: canvas::Style::Solid(to_iced(c)),
        width,
        line_cap: cap,
        line_join: LineJoin::Round,
        ..Stroke::default()
    }
}

/// Draw one gauge in the `side` x `side` square whose top-left is `origin`.
pub fn paint(frame: &mut Frame, origin: Point, side: f32, spec: &Spec, c: &Colours) {
    let w = (side * g::STROKE_FRAC).max(1.0);
    let ring = Rectangle::new(
        Point::new(origin.x + w / 2.0, origin.y + w / 2.0),
        Size::new((side - w).max(0.0), (side - w).max(0.0)),
    );
    let r = ring.width * g::CORNER_FRAC;
    let outline = outline(ring, r);
    // The track is the whole path, closed.
    frame.stroke(
        &Path::rounded_rectangle(ring.position(), ring.size(), r.into()),
        stroke(c.track, w, LineCap::Round),
    );
    let perimeter: f32 = outline.iter().map(Seg::len).sum();
    let charge = f32::from(spec.percent.min(100)) / 100.0;
    if spec.percent >= 100 {
        frame.stroke(
            &Path::rounded_rectangle(ring.position(), ring.size(), r.into()),
            stroke(c.ring, w, LineCap::Round),
        );
    } else if spec.percent > 0 {
        frame.stroke(
            &path_of(&walk(&outline, perimeter * charge)),
            stroke(c.ring, w, LineCap::Round),
        );
    }
    let icon = side * g::ICON_FRAC;
    let at = Point::new(origin.x + (side - icon) / 2.0, origin.y + (side - icon) / 2.0);
    draw_icon(frame, spec.kind, at, icon, c.ink);
    if spec.charging {
        let s = side * g::BOLT_FRAC;
        let at = Point::new(origin.x + side * g::BOLT_AT, origin.y + side * g::BOLT_AT);
        frame.fill(&bolt(at, s), to_iced(c.bolt));
    }
}

fn bolt(at: Point, s: f32) -> Path {
    let pts = [
        (0.62, 0.0),
        (0.12, 0.58),
        (0.46, 0.58),
        (0.38, 1.0),
        (0.88, 0.38),
        (0.54, 0.38),
    ];
    Path::new(|b| {
        for (i, (x, y)) in pts.iter().enumerate() {
            let p = Point::new(at.x + x * s, at.y + y * s);
            if i == 0 {
                b.move_to(p);
            } else {
                b.line_to(p);
            }
        }
        b.close();
    })
}

/// A line icon for `kind` in the square at `at` of side `s`: vector paths in
/// the unit square, stroked in `ink`. No image, no icon font.
fn draw_icon(frame: &mut Frame, kind: DeviceKind, at: Point, s: f32, ink: Rgba) {
    let w = (s * g::ICON_STROKE_FRAC).max(1.0);
    let st = || stroke(ink, w, LineCap::Round);
    let p = |x: f32, y: f32| Point::new(at.x + x * s, at.y + y * s);
    let rr = |x: f32, y: f32, ww: f32, hh: f32, r: f32| {
        Path::rounded_rectangle(p(x, y), Size::new(ww * s, hh * s), (r * s).into())
    };
    let line = |a: Point, b: Point| Path::line(a, b);
    match kind {
        DeviceKind::Laptop => {
            frame.stroke(&rr(0.14, 0.16, 0.72, 0.52, 0.07), st());
            frame.stroke(&line(p(0.04, 0.84), p(0.96, 0.84)), st());
        }
        DeviceKind::Mouse => {
            frame.stroke(&rr(0.25, 0.04, 0.5, 0.92, 0.25), st());
            frame.stroke(&line(p(0.5, 0.06), p(0.5, 0.34)), st());
        }
        DeviceKind::Keyboard => {
            frame.stroke(&rr(0.04, 0.24, 0.92, 0.52, 0.1), st());
            for x in [0.22, 0.41, 0.59, 0.78] {
                frame.stroke(&line(p(x, 0.4), p(x, 0.4)), st());
            }
            frame.stroke(&line(p(0.3, 0.6), p(0.7, 0.6)), st());
        }
        DeviceKind::Headphones => {
            let arc = Path::new(|b| {
                b.arc(Arc {
                    center: p(0.5, 0.55),
                    radius: 0.36 * s,
                    start_angle: Radians(PI),
                    end_angle: Radians(TAU),
                })
            });
            frame.stroke(&arc, st());
            frame.stroke(&rr(0.1, 0.55, 0.16, 0.32, 0.07), st());
            frame.stroke(&rr(0.74, 0.55, 0.16, 0.32, 0.07), st());
        }
        DeviceKind::Phone => {
            frame.stroke(&rr(0.26, 0.04, 0.48, 0.92, 0.12), st());
            frame.stroke(&line(p(0.43, 0.82), p(0.57, 0.82)), st());
        }
        DeviceKind::Generic => {
            frame.stroke(&rr(0.06, 0.28, 0.78, 0.44, 0.09), st());
            frame.stroke(&line(p(0.93, 0.42), p(0.93, 0.58)), st());
        }
    }
}

// ------------------------------------------------------------------- widgets

struct Face {
    spec: Spec,
    colours: Colours,
}

impl<Msg> canvas::Program<Msg> for Face {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        paint(
            &mut frame,
            Point::ORIGIN,
            bounds.width.min(bounds.height),
            &self.spec,
            &self.colours,
        );
        vec![frame.into_geometry()]
    }
}

/// The ring and icon alone, `side` px square.
pub fn gauge<'a, Msg: 'a>(spec: Spec, side: f32, pal: &Palette) -> Element<'a, Msg> {
    Canvas::new(Face {
        spec,
        colours: Colours::of(pal, &spec),
    })
    .width(Length::Fixed(side))
    .height(Length::Fixed(side))
    .into()
}

/// One cell of the battery row: the gauge with its percentage under it, at
/// `scale` times the reference size ([`g::GAUGE_PX`] wide, [`g::CELL_H_PX`] tall).
pub fn cell<'a, Msg: 'a>(spec: Spec, scale: f32, pal: &Palette) -> Element<'a, Msg> {
    let side = g::GAUGE_PX as f32 * scale;
    let line = g::NUMERAL_PX as f32 * scale;
    let numeral = text(format!("{}%", spec.percent.min(100)))
        .size(g::NUMERAL_SIZE * scale)
        .font(font::DATA_MEDIUM)
        .color(to_iced(pal.text))
        .wrapping(text::Wrapping::None);
    column![
        gauge(spec, side, pal),
        container(numeral)
            .width(Length::Fixed(side))
            .height(Length::Fixed(line))
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Bottom),
    ]
    .width(Length::Fixed(side))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> (Rectangle, f32) {
        (
            Rectangle::new(Point::new(10.0, 20.0), Size::new(100.0, 100.0)),
            30.0,
        )
    }

    #[test]
    fn the_outline_is_closed_and_has_the_exact_perimeter() {
        let (b, r) = square();
        let o = outline(b, r);
        let total: f32 = o.iter().map(Seg::len).sum();
        let want = 4.0 * (b.width - 2.0 * r) + TAU * r;
        assert!((total - want).abs() < 1e-3, "{total} vs {want}");
        // Starts at the top centre and ends there.
        let start = match o[0] {
            Seg::Line(a, _) => a,
            _ => unreachable!(),
        };
        assert_eq!(start, Point::new(60.0, 20.0));
        let end = o[8].end();
        assert!(end.distance(start) < 1e-3);
        // Every piece joins the next.
        for w in o.windows(2) {
            let a = w[0].end();
            let b = match w[1] {
                Seg::Line(a, _) => a,
                Seg::Arc(c, r, s, _) => Point::new(c.x + r * s.cos(), c.y + r * s.sin()),
            };
            assert!(a.distance(b) < 1e-3, "{a:?} {b:?}");
        }
    }

    #[test]
    fn a_quarter_of_the_walk_ends_at_the_right_edge_midpoint() {
        let (b, r) = square();
        let o = outline(b, r);
        let total: f32 = o.iter().map(Seg::len).sum();
        let at = |f: f32| walk(&o, total * f).last().unwrap().end();
        let mid_right = at(0.25);
        assert!(
            (mid_right.x - 110.0).abs() < 1e-2 && (mid_right.y - 70.0).abs() < 1e-2,
            "{mid_right:?}"
        );
        let bottom = at(0.5);
        assert!(
            (bottom.x - 60.0).abs() < 1e-2 && (bottom.y - 120.0).abs() < 1e-2,
            "{bottom:?}"
        );
        let left = at(0.75);
        assert!(
            (left.x - 10.0).abs() < 1e-2 && (left.y - 70.0).abs() < 1e-2,
            "{left:?}"
        );
    }

    #[test]
    fn a_walk_is_as_long_as_asked_and_never_longer_than_the_outline() {
        let (b, r) = square();
        let o = outline(b, r);
        let total: f32 = o.iter().map(Seg::len).sum();
        for f in [0.0, 0.01, 0.1, 0.33, 0.5, 0.99, 1.0, 2.0] {
            let got: f32 = walk(&o, total * f).iter().map(Seg::len).sum();
            let want = (total * f).min(total);
            assert!((got - want).abs() < 1e-2, "{f}: {got} vs {want}");
        }
        assert!(walk(&o, 0.0).is_empty() && walk(&o, -5.0).is_empty());
    }

    #[test]
    fn the_icon_follows_the_kind_of_device() {
        let k = DeviceKind::from_name;
        assert_eq!(k(true, "BAT0"), DeviceKind::Laptop);
        assert_eq!(k(false, "MX Master 3"), DeviceKind::Mouse);
        assert_eq!(k(false, "Logitech Mouse"), DeviceKind::Mouse);
        assert_eq!(k(false, "Keychron K2"), DeviceKind::Keyboard);
        assert_eq!(k(false, "WH-1000XM5"), DeviceKind::Headphones);
        assert_eq!(k(false, "AirPods Pro"), DeviceKind::Headphones);
        assert_eq!(k(false, "Pixel 8"), DeviceKind::Phone);
        assert_eq!(k(false, "hid-0003"), DeviceKind::Generic);
    }

    #[test]
    fn low_means_at_or_under_twenty_and_not_charging() {
        let s = |percent, charging| Spec {
            kind: DeviceKind::Generic,
            percent,
            charging,
        };
        assert!(s(20, false).low() && s(3, false).low());
        assert!(!s(21, false).low() && !s(3, true).low());
    }

    #[test]
    fn a_low_ring_is_danger_and_otherwise_the_accent_mark() {
        let pal = tokens::palette(&ec_theme::DARK_SEEDS, ec_theme::Scheme::Dark);
        let s = |percent, charging| Spec {
            kind: DeviceKind::Mouse,
            percent,
            charging,
        };
        assert_eq!(Colours::of(&pal, &s(10, false)).ring, pal.danger);
        assert_eq!(Colours::of(&pal, &s(10, true)).ring, tokens::typed_mark(&pal));
        assert_eq!(Colours::of(&pal, &s(80, false)).ring, tokens::typed_mark(&pal));
    }
}
