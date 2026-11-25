// SPDX-License-Identifier: AGPL-3.0-only
//! The progress gauge: a rounded-square ring that fills around its perimeter.
//!
//! The geometry is the battery glance's: one stroked rounded-rectangle path.
//! A faint track runs all the way round; the reading is the same path walked
//! from the top centre clockwise for `fraction` of its length, with round
//! caps. The walk is exact ([`walk`]), not sampled points, so 50% lands on the
//! middle of the bottom edge.
//!
//! When the amount of work is unknown there is no fraction to draw. A short
//! sweep (a leader and two fading steps) travels round the same path instead,
//! following the corners; it is not a percent. [`progress_sweep_bar`] is the
//! straight twin for a job row.
//!
//! Colour: the neutral ink the caller passes, over a track of the palette's
//! tertiary ink. No accent, no gradient, no glow.

use std::f32::consts::{FRAC_PI_2, PI};

use iced::widget::canvas::{self, path::Arc, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::{mouse, Color, Element, Length, Point, Radians, Rectangle, Renderer, Size, Theme};

use crate::tokens::{color, console, drawer};

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

    /// The piece from `lo` to `hi` of its own length.
    fn between(&self, lo: f32, hi: f32) -> Seg {
        let len = self.len();
        let (f0, f1) = if len > 0.0 {
            ((lo / len).clamp(0.0, 1.0), (hi / len).clamp(0.0, 1.0))
        } else {
            (0.0, 1.0)
        };
        match *self {
            Seg::Line(a, b) => {
                let at = |f: f32| Point::new(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f);
                Seg::Line(at(f0), at(f1))
            }
            Seg::Arc(c, r, s, sweep) => Seg::Arc(c, r, s + sweep * f0, sweep * (f1 - f0)),
        }
    }

    fn start(&self) -> Point {
        match *self {
            Seg::Line(a, _) => a,
            Seg::Arc(c, r, s, _) => Point::new(c.x + r * s.cos(), c.y + r * s.sin()),
        }
    }

    #[cfg(test)]
    fn end(&self) -> Point {
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

/// The outline from distance `from` to distance `to` along it, whole pieces
/// with a cut one at each end.
pub fn span(segs: &[Seg], from: f32, to: f32) -> Vec<Seg> {
    let mut out = Vec::new();
    let mut at = 0.0;
    for s in segs {
        let len = s.len();
        let (lo, hi) = ((from - at).max(0.0), (to - at).min(len));
        if hi > lo {
            out.push(s.between(lo, hi));
        }
        at += len;
        if at >= to {
            break;
        }
    }
    out
}

/// The first `dist` of the outline.
pub fn walk(segs: &[Seg], dist: f32) -> Vec<Seg> {
    span(segs, 0.0, dist)
}

fn path_of(segs: &[Seg]) -> Path {
    Path::new(|b| {
        if let Some(first) = segs.first() {
            b.move_to(first.start());
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

/// The unlit track: the palette's tertiary ink, thinned.
fn rest() -> Color {
    color::text_tertiary().scale_alpha(drawer::GAUGE_REST)
}

/// The ink of sweep step `k` (0 = the leader): full, stepping evenly down to
/// [`drawer::SWEEP_TAIL`] at the last step.
fn step_ink(tint: Color, k: usize) -> Color {
    let last = (drawer::SWEEP_STEPS - 1).max(1) as f32;
    let t = k as f32 / last;
    tint.scale_alpha(1.0 - (1.0 - drawer::SWEEP_TAIL) * t)
}

fn stroke(c: Color, width: f32, cap: LineCap) -> Stroke<'static> {
    Stroke {
        style: canvas::Style::Solid(c),
        width,
        line_cap: cap,
        line_join: LineJoin::Round,
        ..Stroke::default()
    }
}

/// A progress gauge: a rounded-square ring `side` px square, `stroke` px thick,
/// filled clockwise from the top centre to `fraction`, or, when the amount of
/// work is unknown (`None`), a soft sweep travelling round it at `phase`
/// (revolutions).
///
/// A widget because the bar's progress cell and its drawer's hero must show a
/// job the same way, and the ring is a path walked exactly, which a styled
/// container cannot draw. Neutral ink, no glow: the caller passes the tint.
pub fn progress_squircle<'a, Message: 'a>(
    fraction: Option<f32>,
    phase: f32,
    side: f32,
    stroke: f32,
    tint: Color,
) -> Element<'a, Message, Theme> {
    iced::widget::canvas(Squircle {
        fraction,
        phase,
        stroke,
        tint,
    })
    .width(Length::Fixed(side))
    .height(Length::Fixed(side))
    .into()
}

struct Squircle {
    fraction: Option<f32>,
    phase: f32,
    stroke: f32,
    tint: Color,
}

impl<Message> canvas::Program<Message> for Squircle {
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
        let side = bounds.width.min(bounds.height);
        let w = self.stroke;
        let ring = Rectangle::new(
            Point::new(w / 2.0, w / 2.0),
            Size::new((side - w).max(0.0), (side - w).max(0.0)),
        );
        let r = ring.width * drawer::GAUGE_CORNER;
        let full = Path::rounded_rectangle(ring.position(), ring.size(), r.into());
        frame.stroke(&full, stroke(rest(), w, LineCap::Round));
        let segs = outline(ring, r);
        let perimeter: f32 = segs.iter().map(Seg::len).sum();
        match self.fraction {
            Some(f) if f >= 1.0 => frame.stroke(&full, stroke(self.tint, w, LineCap::Round)),
            Some(f) if f > 0.0 => frame.stroke(
                &path_of(&walk(&segs, perimeter * f)),
                stroke(self.tint, w, LineCap::Round),
            ),
            Some(_) => {}
            None => {
                let step = perimeter * drawer::SWEEP_STEP;
                let lead = self.phase.rem_euclid(1.0) * perimeter;
                for k in 0..drawer::SWEEP_STEPS {
                    let hi = lead - step * k as f32;
                    let lo = hi - step;
                    let pieces = if lo >= 0.0 {
                        span(&segs, lo, hi)
                    } else {
                        // Wrapped past the top centre: the end of the path
                        // and the start of it.
                        let mut v = span(&segs, perimeter + lo, perimeter);
                        v.extend(span(&segs, 0.0, hi));
                        v
                    };
                    // Each piece is stroked alone so the wrap never draws a
                    // line across the ring.
                    for piece in pieces {
                        frame.stroke(
                            &path_of(&[piece]),
                            stroke(step_ink(self.tint, k), w, LineCap::Butt),
                        );
                    }
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

/// The straight twin of the sweep, for a job row whose fraction is unknown: a
/// track the width of the row, [`console::BAR_H`] tall, with the same leader
/// and two fading steps running along it at `phase` (cycles). Pair of
/// [`super::progress_bar`], so every job is the same silhouette.
///
/// A widget because the bar must be drawn to whatever width the drawer gives
/// it; a row of quads would need a measuring pass.
pub fn progress_sweep_bar<'a, Message: 'a>(phase: f32, tint: Color) -> Element<'a, Message, Theme> {
    iced::widget::canvas(SweepBar { phase, tint })
        .width(Length::Fill)
        .height(Length::Fixed(console::BAR_H))
        .into()
}

struct SweepBar {
    phase: f32,
    tint: Color,
}

impl<Message> canvas::Program<Message> for SweepBar {
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
        let (w, h) = (bounds.width, bounds.height);
        let r = drawer::RADIUS_METER;
        let bar = |x0: f32, x1: f32, c: Color| {
            if x1 > x0 {
                frame.fill(
                    &Path::rounded_rectangle(Point::new(x0, 0.0), Size::new(x1 - x0, h), r.into()),
                    c,
                );
            }
        };
        let mut bar = bar;
        bar(0.0, w, rest());
        let step = w * drawer::SWEEP_STEP;
        let lead = self.phase.rem_euclid(1.0) * (w + step * drawer::SWEEP_STEPS as f32);
        for k in 0..drawer::SWEEP_STEPS {
            let hi = lead - step * k as f32;
            bar((hi - step).max(0.0), hi.min(w), step_ink(self.tint, k));
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn square() -> (Rectangle, f32) {
        (
            Rectangle::new(Point::new(10.0, 20.0), Size::new(100.0, 100.0)),
            30.0,
        )
    }

    fn total(o: &[Seg]) -> f32 {
        o.iter().map(Seg::len).sum()
    }

    #[test]
    fn the_outline_is_closed_and_has_the_exact_perimeter() {
        let (b, r) = square();
        let o = outline(b, r);
        let want = 4.0 * (b.width - 2.0 * r) + TAU * r;
        assert!((total(&o) - want).abs() < 1e-3);
        let start = o[0].start();
        assert_eq!(start, Point::new(60.0, 20.0));
        assert!(o[8].end().distance(start) < 1e-3);
        for w in o.windows(2) {
            assert!(w[0].end().distance(w[1].start()) < 1e-3);
        }
    }

    #[test]
    fn the_walk_lands_on_the_edge_midpoints() {
        let (b, r) = square();
        let o = outline(b, r);
        let t = total(&o);
        let at = |f: f32| walk(&o, t * f).last().unwrap().end();
        let near = |p: Point, x: f32, y: f32| (p.x - x).abs() < 1e-2 && (p.y - y).abs() < 1e-2;
        assert!(near(at(0.25), 110.0, 70.0));
        assert!(near(at(0.5), 60.0, 120.0));
        assert!(near(at(0.75), 10.0, 70.0));
    }

    #[test]
    fn a_walk_is_as_long_as_asked_and_never_longer_than_the_outline() {
        let (b, r) = square();
        let o = outline(b, r);
        let t = total(&o);
        for f in [0.0, 0.01, 0.1, 0.33, 0.5, 0.99, 1.0, 2.0] {
            let want = (t * f).min(t);
            assert!((total(&walk(&o, t * f)) - want).abs() < 1e-2, "{f}");
        }
        assert!(walk(&o, 0.0).is_empty() && walk(&o, -5.0).is_empty());
    }

    #[test]
    fn a_span_is_the_stretch_between_its_ends() {
        let (b, r) = square();
        let o = outline(b, r);
        let t = total(&o);
        let s = span(&o, t * 0.2, t * 0.45);
        assert!((total(&s) - t * 0.25).abs() < 1e-2);
        // It starts where a walk of 0.2 ends.
        let from = walk(&o, t * 0.2).last().unwrap().end();
        assert!(s[0].start().distance(from) < 1e-2);
        assert!(span(&o, t * 0.5, t * 0.5).is_empty());
    }

    #[test]
    fn the_sweep_fades_from_the_leader_to_the_tail() {
        let tint = Color::WHITE;
        assert_eq!(step_ink(tint, 0).a, 1.0);
        assert!((step_ink(tint, drawer::SWEEP_STEPS - 1).a - drawer::SWEEP_TAIL).abs() < 1e-6);
        assert!(step_ink(tint, 1).a < step_ink(tint, 0).a);
        assert!(step_ink(tint, 1).a > step_ink(tint, 2).a);
    }
}
