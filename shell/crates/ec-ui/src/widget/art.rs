// SPDX-License-Identifier: AGPL-3.0-only
//! Drawn art: the icon tile a category carries in a sidebar, the vignette in
//! a page header, and the shapes both are made of.
//!
//! A widget and not a styled container because the picture is geometry (arcs,
//! rounded rects, a bezier) that a container cannot express, and because the
//! same drawing has to come out in the sidebar at 22px and in a header at 64
//! with the same weights and corners. There is no binary asset and no icon
//! font: every glyph is a handful of paths in a unit square ([`parts`]), so it
//! follows the scheme (the strokes mix the hue toward the palette's text) and
//! costs nothing while the window is idle (iced draws it when something else
//! redraws the window; there is no clock behind it).
//!
//! The art never carries meaning alone. A tile sits beside a label; the
//! vignette sits beside a title. Hues are restrained and one per sidebar
//! group ([`crate::tokens::art`]); the accent is not used.

use iced::widget::canvas::{self, path::Arc, Frame, LineCap, LineJoin, Path, Stroke};
use iced::{border, Color, Element, Length, Point, Radians, Rectangle, Size, Theme};

use crate::tokens::{art, color};

/// The hue of a sidebar group. Which group owns which hue is the app's call;
/// this is only the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hue {
    Home,
    Look,
    Devices,
    System,
    Privacy,
    Apps,
}

impl Hue {
    pub const ALL: &'static [Hue] = &[
        Hue::Home,
        Hue::Look,
        Hue::Devices,
        Hue::System,
        Hue::Privacy,
        Hue::Apps,
    ];

    pub fn color(self) -> Color {
        match self {
            Hue::Home => art::HUE_HOME,
            Hue::Look => art::HUE_LOOK,
            Hue::Devices => art::HUE_DEVICES,
            Hue::System => art::HUE_SYSTEM,
            Hue::Privacy => art::HUE_PRIVACY,
            Hue::Apps => art::HUE_APPS,
        }
    }
}

/// One picture. Section-level glyphs and the page-level ones a section's own
/// pages are told apart by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    Overview,
    Tiles,
    Gaps,
    Frame,
    Focus,
    Panes,
    Shadow,
    Corner,
    Motion,
    Sliders,
    Dots,
    Landscape,
    Bar,
    Search,
    Monitor,
    Wifi,
    Keycap,
    Mouse,
    Touchpad,
    Power,
    Chip,
    Lock,
    Person,
    Cube,
    Eye,
    Prompt,
}

impl Glyph {
    pub const ALL: &'static [Glyph] = &[
        Glyph::Overview,
        Glyph::Tiles,
        Glyph::Gaps,
        Glyph::Frame,
        Glyph::Focus,
        Glyph::Panes,
        Glyph::Shadow,
        Glyph::Corner,
        Glyph::Motion,
        Glyph::Sliders,
        Glyph::Dots,
        Glyph::Landscape,
        Glyph::Bar,
        Glyph::Search,
        Glyph::Monitor,
        Glyph::Wifi,
        Glyph::Keycap,
        Glyph::Mouse,
        Glyph::Touchpad,
        Glyph::Power,
        Glyph::Chip,
        Glyph::Lock,
        Glyph::Person,
        Glyph::Cube,
        Glyph::Eye,
        Glyph::Prompt,
    ];
}

/// How a [`Part`] is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// A stroke in the line colour.
    Line,
    /// Solid line colour: a dot.
    Fill,
    /// A closed shape: a stroke, and in the detailed (vignette) drawing a
    /// translucent fill inside it, so the shape reads as a pane of glass.
    Shell,
    /// A soft hue-tinted fill with no edge: a shadow, a wash.
    Wash,
}

/// One path of a glyph and how it is painted.
#[derive(Debug, Clone)]
pub struct Part {
    pub path: Path,
    pub ink: Ink,
}

struct Build {
    s: f32,
    parts: Vec<Part>,
}

impl Build {
    fn p(&self, x: f32, y: f32) -> Point {
        Point::new(x * self.s, y * self.s)
    }

    fn rr(&self, x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
        let r = (r * self.s).min(w * self.s / 2.0).min(h * self.s / 2.0).max(0.0);
        Path::rounded_rectangle(
            self.p(x, y),
            Size::new((w * self.s).max(0.0), (h * self.s).max(0.0)),
            border::Radius::from(r),
        )
    }

    fn poly(&self, pts: &[(f32, f32)], close: bool) -> Path {
        Path::new(|b| {
            for (i, (x, y)) in pts.iter().enumerate() {
                let at = self.p(*x, *y);
                if i == 0 {
                    b.move_to(at);
                } else {
                    b.line_to(at);
                }
            }
            if close {
                b.close();
            }
        })
    }

    fn arc(&self, cx: f32, cy: f32, r: f32, from: f32, to: f32, close: bool) -> Path {
        Path::new(|b| {
            b.arc(Arc {
                center: self.p(cx, cy),
                radius: (r * self.s).max(0.0),
                start_angle: Radians(from),
                end_angle: Radians(to),
            });
            if close {
                b.close();
            }
        })
    }

    fn push(&mut self, path: Path, ink: Ink) {
        self.parts.push(Part { path, ink });
    }

    fn line(&mut self, pts: &[(f32, f32)]) {
        let path = self.poly(pts, false);
        self.push(path, Ink::Line);
    }

    fn shell(&mut self, path: Path) {
        self.push(path, Ink::Shell);
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let path = self.rr(x, y, w, h, r);
        self.shell(path);
    }

    fn circle(&mut self, x: f32, y: f32, r: f32) {
        let path = Path::circle(self.p(x, y), (r * self.s).max(0.0));
        self.shell(path);
    }

    fn dot(&mut self, x: f32, y: f32, r: f32) {
        let path = Path::circle(self.p(x, y), (r * self.s).max(0.0));
        self.push(path, Ink::Fill);
    }

    fn wash(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let path = self.rr(x, y, w, h, r);
        self.push(path, Ink::Wash);
    }
}

/// The paths of `glyph` in a square of side `side`, origin at its corner.
/// Empty for a side too small to draw or not a number, so a tile squeezed to
/// nothing paints nothing rather than panicking.
pub fn parts(glyph: Glyph, side: f32) -> Vec<Part> {
    use std::f32::consts::{FRAC_PI_2, PI, TAU};
    if !side.is_finite() || side < 1.0 {
        return Vec::new();
    }
    let mut b = Build {
        s: side,
        parts: Vec::new(),
    };
    match glyph {
        Glyph::Overview => {
            for (x, y) in [(0.14, 0.14), (0.55, 0.14), (0.14, 0.55), (0.55, 0.55)] {
                b.rect(x, y, 0.31, 0.31, 0.08);
            }
        }
        Glyph::Tiles => {
            b.rect(0.1, 0.16, 0.8, 0.68, 0.1);
            b.line(&[(0.5, 0.16), (0.5, 0.84)]);
            b.line(&[(0.5, 0.5), (0.9, 0.5)]);
        }
        Glyph::Gaps => {
            b.rect(0.1, 0.26, 0.28, 0.48, 0.07);
            b.rect(0.62, 0.26, 0.28, 0.48, 0.07);
            b.line(&[(0.44, 0.5), (0.56, 0.5)]);
        }
        Glyph::Frame => {
            b.rect(0.1, 0.14, 0.8, 0.72, 0.12);
            b.rect(0.27, 0.3, 0.46, 0.4, 0.06);
        }
        Glyph::Focus => {
            b.circle(0.5, 0.5, 0.25);
            b.dot(0.5, 0.5, 0.07);
            b.line(&[(0.5, 0.1), (0.5, 0.2)]);
            b.line(&[(0.5, 0.8), (0.5, 0.9)]);
            b.line(&[(0.1, 0.5), (0.2, 0.5)]);
            b.line(&[(0.8, 0.5), (0.9, 0.5)]);
        }
        Glyph::Panes => {
            b.rect(0.1, 0.1, 0.5, 0.5, 0.11);
            b.rect(0.4, 0.4, 0.5, 0.5, 0.11);
        }
        Glyph::Shadow => {
            b.wash(0.26, 0.28, 0.6, 0.6, 0.14);
            b.rect(0.12, 0.12, 0.54, 0.54, 0.12);
        }
        Glyph::Corner => {
            let p = b.poly(&[(0.12, 0.88), (0.12, 0.5)], false);
            b.push(p, Ink::Line);
            let a = b.arc(0.5, 0.5, 0.38, PI, PI + FRAC_PI_2, false);
            b.push(a, Ink::Line);
            b.line(&[(0.5, 0.12), (0.88, 0.12)]);
        }
        Glyph::Motion => {
            let curve = Path::new(|p| {
                p.move_to(b.p(0.14, 0.78));
                p.bezier_curve_to(b.p(0.55, 0.78), b.p(0.45, 0.22), b.p(0.86, 0.22));
            });
            b.push(curve, Ink::Line);
            b.dot(0.14, 0.78, 0.06);
            b.dot(0.86, 0.22, 0.06);
        }
        Glyph::Sliders => {
            for (y, x) in [(0.28, 0.66), (0.5, 0.34), (0.72, 0.56)] {
                b.line(&[(0.1, y), (0.9, y)]);
                b.circle(x, y, 0.09);
            }
        }
        Glyph::Dots => {
            for y in [0.25, 0.5, 0.75] {
                for x in [0.25, 0.5, 0.75] {
                    b.dot(x, y, 0.065);
                }
            }
        }
        Glyph::Landscape => {
            b.rect(0.1, 0.18, 0.8, 0.64, 0.1);
            b.dot(0.33, 0.42, 0.065);
            b.line(&[(0.14, 0.74), (0.4, 0.5), (0.56, 0.66), (0.68, 0.56), (0.86, 0.74)]);
        }
        Glyph::Bar => {
            b.rect(0.1, 0.62, 0.8, 0.2, 0.1);
            b.rect(0.2, 0.18, 0.6, 0.3, 0.07);
            for x in [0.24, 0.36, 0.76] {
                b.dot(x, 0.72, 0.035);
            }
        }
        Glyph::Search => {
            b.circle(0.44, 0.44, 0.27);
            b.line(&[(0.64, 0.64), (0.88, 0.88)]);
        }
        Glyph::Monitor => {
            b.rect(0.08, 0.16, 0.84, 0.54, 0.08);
            b.line(&[(0.5, 0.7), (0.5, 0.82)]);
            b.line(&[(0.32, 0.84), (0.68, 0.84)]);
        }
        Glyph::Wifi => {
            b.dot(0.5, 0.76, 0.055);
            for r in [0.2, 0.36, 0.52] {
                let a = b.arc(0.5, 0.76, r, -PI * 0.75, -PI * 0.25, false);
                b.push(a, Ink::Line);
            }
        }
        Glyph::Keycap => {
            b.rect(0.1, 0.12, 0.8, 0.76, 0.18);
            b.rect(0.26, 0.26, 0.48, 0.4, 0.1);
        }
        Glyph::Mouse => {
            b.rect(0.28, 0.1, 0.44, 0.8, 0.22);
            b.line(&[(0.5, 0.1), (0.5, 0.4)]);
            b.line(&[(0.28, 0.4), (0.72, 0.4)]);
        }
        Glyph::Touchpad => {
            b.rect(0.08, 0.2, 0.84, 0.6, 0.1);
            b.line(&[(0.08, 0.62), (0.92, 0.62)]);
            b.line(&[(0.5, 0.62), (0.5, 0.8)]);
        }
        Glyph::Power => {
            let gap = 0.62;
            let a = b.arc(0.5, 0.56, 0.32, -FRAC_PI_2 + gap, 3.0 * FRAC_PI_2 - gap, false);
            b.push(a, Ink::Line);
            b.line(&[(0.5, 0.12), (0.5, 0.5)]);
        }
        Glyph::Chip => {
            b.rect(0.26, 0.26, 0.48, 0.48, 0.07);
            for t in [0.38, 0.5, 0.62] {
                b.line(&[(t, 0.1), (t, 0.26)]);
                b.line(&[(t, 0.74), (t, 0.9)]);
                b.line(&[(0.1, t), (0.26, t)]);
                b.line(&[(0.74, t), (0.9, t)]);
            }
        }
        Glyph::Lock => {
            b.rect(0.2, 0.44, 0.6, 0.42, 0.1);
            let a = b.arc(0.5, 0.36, 0.17, PI, TAU, false);
            b.push(a, Ink::Line);
            b.line(&[(0.33, 0.36), (0.33, 0.44)]);
            b.line(&[(0.67, 0.36), (0.67, 0.44)]);
            b.dot(0.5, 0.65, 0.045);
        }
        Glyph::Person => {
            b.circle(0.5, 0.33, 0.18);
            let body = b.arc(0.5, 0.88, 0.34, PI, TAU, true);
            b.shell(body);
        }
        Glyph::Cube => {
            let hex = [
                (0.5, 0.1),
                (0.84, 0.3),
                (0.84, 0.7),
                (0.5, 0.9),
                (0.16, 0.7),
                (0.16, 0.3),
            ];
            let p = b.poly(&hex, true);
            b.shell(p);
            b.line(&[(0.16, 0.3), (0.5, 0.5), (0.84, 0.3)]);
            b.line(&[(0.5, 0.5), (0.5, 0.9)]);
        }
        Glyph::Eye => {
            let almond = Path::new(|p| {
                p.move_to(b.p(0.08, 0.5));
                p.quadratic_curve_to(b.p(0.5, 0.1), b.p(0.92, 0.5));
                p.quadratic_curve_to(b.p(0.5, 0.9), b.p(0.08, 0.5));
                p.close();
            });
            b.shell(almond);
            b.circle(0.5, 0.5, 0.16);
            b.dot(0.5, 0.5, 0.06);
        }
        Glyph::Prompt => {
            b.rect(0.08, 0.16, 0.84, 0.68, 0.1);
            b.line(&[(0.26, 0.38), (0.4, 0.5), (0.26, 0.62)]);
            b.line(&[(0.48, 0.64), (0.68, 0.64)]);
        }
    }
    b.parts
}

/// `a` moved `t` of the way to `b`, channel by channel.
fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.0,
    }
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// The line colour of a hue on the current palette: the hue pulled toward the
/// text ink, pastel on dark and deep on light.
pub fn line_color(hue: Hue) -> Color {
    mix(hue.color(), color::text(), art::LINE_MIX)
}

/// Paint `glyph` into `frame` in the square `at` (origin and side), the
/// detailed way (glass fills) or as a monoline mark.
fn paint(frame: &mut Frame, glyph: Glyph, hue: Hue, at: Point, side: f32, detail: bool) {
    let line = line_color(hue);
    let width = (side * if detail { art::STROKE_DETAIL } else { art::STROKE }).max(art::STROKE_MIN);
    let stroke = |c: Color| {
        Stroke::default()
            .with_color(c)
            .with_width(width)
            .with_line_cap(LineCap::Round)
            .with_line_join(LineJoin::Round)
    };
    frame.with_save(|frame| {
        frame.translate(iced::Vector::new(at.x, at.y));
        for part in parts(glyph, side) {
            match part.ink {
                Ink::Line => frame.stroke(&part.path, stroke(line)),
                Ink::Fill => frame.fill(&part.path, line),
                Ink::Shell => {
                    if detail {
                        frame.fill(&part.path, with_alpha(line, art::VIGNETTE_GLASS));
                    }
                    frame.stroke(&part.path, stroke(line));
                }
                Ink::Wash => frame.fill(&part.path, with_alpha(hue.color(), art::VIGNETTE_WASH * 1.5)),
            }
        }
    });
}

struct Tile {
    glyph: Glyph,
    hue: Hue,
    lit: bool,
}

impl<Message> canvas::Program<Message> for Tile {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let side = bounds.width.min(bounds.height);
        if side >= 1.0 {
            let (fill, rim) = if self.lit {
                (art::TILE_FILL_LIT, art::TILE_RIM_LIT)
            } else {
                (art::TILE_FILL, art::TILE_RIM)
            };
            let hue = self.hue.color();
            let r = side * art::TILE_R;
            let body = Path::rounded_rectangle(
                Point::new(0.5, 0.5),
                Size::new(side - 1.0, side - 1.0),
                border::Radius::from(r),
            );
            frame.fill(&body, with_alpha(hue, fill));
            frame.stroke(
                &body,
                Stroke::default().with_color(with_alpha(hue, rim)).with_width(1.0),
            );
            let inner = side * (1.0 - 2.0 * art::PAD);
            paint(
                &mut frame,
                self.glyph,
                self.hue,
                Point::new(side * art::PAD, side * art::PAD),
                inner,
                false,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The icon tile of a category: a glass squircle with a hue in it and the
/// glyph drawn in line. `lit` is the current page: a brighter fill and a
/// crisper rim, never a glow.
pub fn tile<'a, Message: 'a>(glyph: Glyph, hue: Hue, side: f32, lit: bool) -> Element<'a, Message, Theme> {
    canvas::Canvas::new(Tile { glyph, hue, lit })
        .width(Length::Fixed(side))
        .height(Length::Fixed(side))
        .into()
}

struct Vignette {
    glyph: Glyph,
    hue: Hue,
}

impl<Message> canvas::Program<Message> for Vignette {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        if w >= 1.0 && h >= 1.0 {
            let hue = self.hue.color();
            let r = art::VIGNETTE_R.min(w / 2.0).min(h / 2.0);
            let body = Path::rounded_rectangle(
                Point::new(0.5, 0.5),
                Size::new((w - 1.0).max(0.0), (h - 1.0).max(0.0)),
                border::Radius::from(r),
            );
            frame.fill(&body, with_alpha(hue, art::VIGNETTE_WASH));
            frame.stroke(
                &body,
                Stroke::default()
                    .with_color(with_alpha(hue, art::TILE_RIM))
                    .with_width(1.0),
            );
            // Two rings behind the glyph, clipped to the card by what fits:
            // a quiet suggestion of depth, not an object.
            let glyph_side = h * art::VIGNETTE_GLYPH;
            let cx = w * 0.5;
            let cy = h * 0.5;
            let ring = Stroke::default()
                .with_color(with_alpha(line_color(self.hue), art::VIGNETTE_RING))
                .with_width(1.0);
            frame.with_clip(Rectangle::new(Point::ORIGIN, Size::new(w, h)), |f| {
                f.stroke(&Path::circle(Point::new(cx, cy), h * art::RING_INNER), ring);
                f.stroke(&Path::circle(Point::new(cx, cy), h * art::RING_OUTER), ring);
            });
            paint(
                &mut frame,
                self.glyph,
                self.hue,
                Point::new(cx - glyph_side / 2.0, cy - glyph_side / 2.0),
                glyph_side,
                true,
            );
        }
        vec![frame.into_geometry()]
    }
}

/// A page's header art: a glass card washed in the group's hue with the
/// glyph drawn large and glassy over two quiet rings. Fixed at
/// [`art::VIGNETTE_W`] by [`art::VIGNETTE_H`], a hint the caller may drop
/// when the window is narrow.
pub fn vignette<'a, Message: 'a>(glyph: Glyph, hue: Hue) -> Element<'a, Message, Theme> {
    canvas::Canvas::new(Vignette { glyph, hue })
        .width(Length::Fixed(art::VIGNETTE_W))
        .height(Length::Fixed(art::VIGNETTE_H))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finite(p: &Path) -> bool {
        p.raw().iter().all(|e| {
            use iced::widget::canvas::path::lyon_path::Event::*;
            let ok =
                |p: iced::widget::canvas::path::lyon_path::math::Point| p.x.is_finite() && p.y.is_finite();
            match e {
                Begin { at } => ok(at),
                Line { from, to } => ok(from) && ok(to),
                Quadratic { from, ctrl, to } => ok(from) && ok(ctrl) && ok(to),
                Cubic {
                    from,
                    ctrl1,
                    ctrl2,
                    to,
                } => ok(from) && ok(ctrl1) && ok(ctrl2) && ok(to),
                End { last, first, .. } => ok(last) && ok(first),
            }
        })
    }

    #[test]
    fn every_glyph_draws_at_every_size_without_panicking() {
        for &glyph in Glyph::ALL {
            for side in [
                f32::NAN,
                f32::INFINITY,
                f32::NEG_INFINITY,
                -4.0,
                0.0,
                0.3,
                1.0,
                1.5,
                3.0,
                8.0,
                18.0,
                22.0,
                64.0,
                4096.0,
            ] {
                let ps = parts(glyph, side);
                if side.is_finite() && side >= 1.0 {
                    assert!(!ps.is_empty(), "{glyph:?} draws nothing at {side}");
                    assert!(ps.iter().all(|p| finite(&p.path)), "{glyph:?} at {side}");
                } else {
                    assert!(ps.is_empty(), "{glyph:?} at {side}");
                }
            }
        }
    }

    #[test]
    fn every_hue_is_opaque_and_distinct() {
        for (i, a) in Hue::ALL.iter().enumerate() {
            assert_eq!(a.color().a, 1.0);
            for b in &Hue::ALL[i + 1..] {
                assert_ne!(a.color(), b.color());
            }
        }
    }

    #[test]
    fn glyphs_are_distinct_pictures() {
        let shape = |g: Glyph| parts(g, 64.0).len();
        assert!(Glyph::ALL.iter().any(|g| shape(*g) != shape(Glyph::Overview)));
        assert_eq!(Glyph::ALL.len(), 26);
    }
}
