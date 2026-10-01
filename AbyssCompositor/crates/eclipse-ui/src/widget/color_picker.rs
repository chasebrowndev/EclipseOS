// SPDX-License-Identifier: AGPL-3.0-only
//! A colour picker: a saturation/value square, a hue strip and, for a colour
//! that carries one, an alpha strip.
//!
//! A canvas and not a stack of styled containers because the square and the
//! strips are gradients by nature, and because one drag has to follow the
//! pointer out of the region it started in. The gradients are the colour
//! space itself, not decoration.
//!
//! The picker holds no colour of its own: it is handed bytes, draws them, and
//! reports the bytes a drag arrives at. The hue is kept in the canvas state
//! for the length of a drag, because a grey or a black has no hue to read
//! back from its bytes and the strip would otherwise snap to red.

use iced::mouse;
use iced::widget::canvas::{self, gradient::Linear, Canvas, Gradient, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Size};

use crate::tokens::{color, radius, space};
use iced::Theme;

/// Hue in degrees `0..360`, saturation and value in `0..=1`.
pub type Hsv = (f32, f32, f32);

/// Bytes to hue/saturation/value. A colour with no chroma reports hue 0.
pub fn rgb_to_hsv(r: u8, g: u8, b: u8) -> Hsv {
    let (r, g, b) = (f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    (h, s, max)
}

/// Hue/saturation/value to bytes. Hue wraps; the other two clamp.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u8 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let byte = |n: f32| ((n + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b)]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Region {
    Square,
    Hue,
    Alpha,
}

#[derive(Default)]
struct State {
    drag: Option<Region>,
    /// The colour as the drag has it, so the hue survives a grey.
    held: Option<(Hsv, u8)>,
}

struct Picker<M> {
    rgba: [u8; 4],
    alpha: bool,
    on_change: Box<dyn Fn([u8; 4]) -> M>,
    on_release: M,
}

fn height(alpha: bool) -> f32 {
    let bars = if alpha { 2.0 } else { 1.0 };
    space::PICKER_SV_H + bars * (space::PICKER_GAP + space::PICKER_BAR_H)
}

fn region_rect(region: Region, w: f32) -> Rectangle {
    let step = space::PICKER_BAR_H + space::PICKER_GAP;
    match region {
        Region::Square => Rectangle::new(Point::ORIGIN, Size::new(w, space::PICKER_SV_H)),
        Region::Hue => Rectangle::new(
            Point::new(0.0, space::PICKER_SV_H + space::PICKER_GAP),
            Size::new(w, space::PICKER_BAR_H),
        ),
        Region::Alpha => Rectangle::new(
            Point::new(0.0, space::PICKER_SV_H + space::PICKER_GAP + step),
            Size::new(w, space::PICKER_BAR_H),
        ),
    }
}

impl<M> Picker<M> {
    fn current(&self, state: &State) -> (Hsv, u8) {
        state
            .held
            .unwrap_or_else(|| (rgb_to_hsv(self.rgba[0], self.rgba[1], self.rgba[2]), self.rgba[3]))
    }

    fn region_at(&self, p: Point, w: f32) -> Option<Region> {
        [Region::Square, Region::Hue, Region::Alpha]
            .into_iter()
            .filter(|r| *r != Region::Alpha || self.alpha)
            .find(|r| region_rect(*r, w).contains(p))
    }

    /// Move the value the drag is on to `p` (clamped into its region).
    fn apply(&self, state: &mut State, region: Region, p: Point, w: f32) -> [u8; 4] {
        let ((mut h, mut s, mut v), mut a) = self.current(state);
        let r = region_rect(region, w);
        let fx = ((p.x - r.x) / r.width).clamp(0.0, 1.0);
        let fy = ((p.y - r.y) / r.height).clamp(0.0, 1.0);
        match region {
            Region::Square => {
                s = fx;
                v = 1.0 - fy;
            }
            Region::Hue => h = fx * 360.0,
            Region::Alpha => a = (fx * 255.0).round() as u8,
        }
        state.held = Some(((h, s, v), a));
        let [r, g, b] = hsv_to_rgb(h, s, v);
        [r, g, b, a]
    }
}

fn hue_color(h: f32) -> Color {
    let [r, g, b] = hsv_to_rgb(h, 1.0, 1.0);
    Color::from_rgb8(r, g, b)
}

fn linear(from: Point, to: Point, stops: &[(f32, Color)]) -> Gradient {
    let mut g = Linear::new(from, to);
    for (at, c) in stops {
        g = g.add_stop(*at, *c);
    }
    Gradient::Linear(g)
}

impl<M: Clone> canvas::Program<M> for Picker<M> {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<M>> {
        let at = cursor
            .position()
            .map(|p| Point::new(p.x - bounds.x, p.y - bounds.y));
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = at.filter(|_| cursor.is_over(bounds))?;
                let region = self.region_at(p, bounds.width)?;
                state.drag = Some(region);
                state.held = None;
                let rgba = self.apply(state, region, p, bounds.width);
                Some(canvas::Action::publish((self.on_change)(rgba)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let region = state.drag?;
                let rgba = self.apply(state, region, at?, bounds.width);
                Some(canvas::Action::publish((self.on_change)(rgba)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.drag.take()?;
                state.held = None;
                Some(canvas::Action::publish(self.on_release.clone()).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &State,
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let ((h, s, v), a) = self.current(state);
        let hovered = state.drag.is_some() || cursor.is_over(bounds);
        let ring = if hovered {
            color::TEXT
        } else {
            color::TEXT_SECONDARY
        };
        let round = |r: Rectangle| Path::rounded_rectangle(r.position(), r.size(), radius::CHIP.into());
        let knob = |frame: &mut canvas::Frame, at: Point| {
            let c = Path::circle(at, space::PICKER_KNOB);
            frame.stroke(
                &c,
                Stroke::default()
                    .with_color(color::BASE)
                    .with_width(space::PICKER_KNOB_EDGE * 2.0 + space::HAIRLINE * 2.0),
            );
            frame.stroke(
                &c,
                Stroke::default()
                    .with_color(ring)
                    .with_width(space::HAIRLINE * 2.0),
            );
        };

        // Square: white to the full hue across, then clear to black down.
        let sq = region_rect(Region::Square, w);
        let clear = Color::TRANSPARENT;
        frame.fill(
            &round(sq),
            linear(
                sq.position(),
                Point::new(sq.x + sq.width, sq.y),
                &[(0.0, Color::WHITE), (1.0, hue_color(h))],
            ),
        );
        frame.fill(
            &round(sq),
            linear(
                sq.position(),
                Point::new(sq.x, sq.y + sq.height),
                &[(0.0, clear), (1.0, Color::BLACK)],
            ),
        );
        knob(
            &mut frame,
            Point::new(sq.x + s * sq.width, sq.y + (1.0 - v) * sq.height),
        );

        // Hue: the six corners of the colour wheel, laid out flat.
        let hue = region_rect(Region::Hue, w);
        let stops: Vec<(f32, Color)> = (0..=6)
            .map(|i| (i as f32 / 6.0, hue_color(i as f32 * 60.0)))
            .collect();
        frame.fill(
            &round(hue),
            linear(hue.position(), Point::new(hue.x + hue.width, hue.y), &stops),
        );
        knob(
            &mut frame,
            Point::new(hue.x + h / 360.0 * hue.width, hue.y + hue.height / 2.0),
        );

        if self.alpha {
            let al = region_rect(Region::Alpha, w);
            let [r, g, b] = hsv_to_rgb(h, s, v);
            frame.fill(&round(al), color::TRACK);
            frame.fill(
                &round(al),
                linear(
                    al.position(),
                    Point::new(al.x + al.width, al.y),
                    &[
                        (0.0, Color::from_rgba8(r, g, b, 0.0)),
                        (1.0, Color::from_rgb8(r, g, b)),
                    ],
                ),
            );
            knob(
                &mut frame,
                Point::new(al.x + f32::from(a) / 255.0 * al.width, al.y + al.height / 2.0),
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let over = cursor
            .position()
            .filter(|_| cursor.is_over(bounds))
            .and_then(|p| self.region_at(Point::new(p.x - bounds.x, p.y - bounds.y), bounds.width))
            .is_some();
        if state.drag.is_some() || over {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

/// The picker for `rgba`. `alpha` adds the alpha strip; without it the fourth
/// byte is passed through untouched. `on_change` fires for every pointer
/// move of a drag, `on_release` once when it ends.
pub fn color_picker<'a, M: Clone + 'a>(
    rgba: [u8; 4],
    alpha: bool,
    on_change: impl Fn([u8; 4]) -> M + 'static,
    on_release: M,
) -> Element<'a, M, Theme> {
    Canvas::new(Picker {
        rgba,
        alpha,
        on_change: Box::new(on_change),
        on_release,
    })
    .width(Length::Fixed(space::PICKER_W))
    .height(Length::Fixed(height(alpha)))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primaries_and_greys() {
        assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), [255, 0, 0]);
        assert_eq!(hsv_to_rgb(120.0, 1.0, 1.0), [0, 255, 0]);
        assert_eq!(hsv_to_rgb(240.0, 1.0, 1.0), [0, 0, 255]);
        assert_eq!(hsv_to_rgb(77.0, 0.0, 1.0), [255, 255, 255]);
        assert_eq!(hsv_to_rgb(77.0, 1.0, 0.0), [0, 0, 0]);
        assert_eq!(rgb_to_hsv(0, 0, 0), (0.0, 0.0, 0.0));
        assert_eq!(rgb_to_hsv(255, 255, 255), (0.0, 0.0, 1.0));
    }

    #[test]
    fn hue_wraps() {
        assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), hsv_to_rgb(0.0, 1.0, 1.0));
        assert_eq!(hsv_to_rgb(-60.0, 1.0, 1.0), hsv_to_rgb(300.0, 1.0, 1.0));
    }

    #[test]
    fn every_byte_triple_round_trips() {
        for r in (0..=255u8).step_by(5) {
            for g in (0..=255u8).step_by(5) {
                for b in (0..=255u8).step_by(5) {
                    let (h, s, v) = rgb_to_hsv(r, g, b);
                    assert_eq!(hsv_to_rgb(h, s, v), [r, g, b], "{r},{g},{b}");
                }
            }
        }
    }
}
