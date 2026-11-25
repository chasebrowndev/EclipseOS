// SPDX-License-Identifier: AGPL-3.0-only
//! The silence control's mark: a bell with a slash through it, drawn in line.
//!
//! A canvas and not an asset: this desktop draws its icons in-process
//! (`docs/STYLE.md`, Art), and no glyph of `ec_ui::widget::art` is a bell.

use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Theme};

/// The mark's square, in px.
pub const SIDE: f32 = 14.0;
/// Stroke width as a share of the side.
const STROKE: f32 = 0.11;

struct Bell {
    ink: Color,
}

/// A bell-off mark in `ink`.
pub fn bell_off<'a, Message: 'a>(ink: Color) -> Element<'a, Message, Theme> {
    canvas::Canvas::new(Bell { ink })
        .width(Length::Fixed(SIDE))
        .height(Length::Fixed(SIDE))
        .into()
}

impl<Message> canvas::Program<Message> for Bell {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let u = bounds.width;
        let p = |x: f32, y: f32| Point::new(x * u, y * u);
        let stroke = Stroke::default()
            .with_color(self.ink)
            .with_width(STROKE * u)
            .with_line_cap(LineCap::Round);
        let body = Path::new(|b| {
            b.move_to(p(0.16, 0.74));
            b.line_to(p(0.30, 0.62));
            b.line_to(p(0.30, 0.42));
            b.bezier_curve_to(p(0.30, 0.18), p(0.70, 0.18), p(0.70, 0.42));
            b.line_to(p(0.70, 0.62));
            b.line_to(p(0.84, 0.74));
            b.close();
        });
        frame.stroke(&body, stroke);
        frame.stroke(&Path::line(p(0.42, 0.88), p(0.58, 0.88)), stroke);
        frame.stroke(&Path::line(p(0.12, 0.10), p(0.88, 0.90)), stroke);
        vec![frame.into_geometry()]
    }
}
