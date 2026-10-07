// SPDX-License-Identifier: AGPL-3.0-only
//! A rectangle of fixed size that stays empty and says where it is.
//!
//! The compositor draws a commit slot inside a protected surface (COMP-19 §5):
//! the client chooses the position, the compositor chooses the size, and the
//! client keeps that rectangle free of its own content. This is the client's
//! half. It lays out as exactly the size it is told, paints nothing, and
//! reports its top-left corner in window coordinates whenever that differs
//! from the position the caller last knew.
//!
//! A widget and not a styled container for two reasons. A container cannot
//! tell the application where it ended up: the position only exists after
//! layout, and the slot request needs it in surface-local logical pixels.
//! And a container invites a background, a border, a label: everything the
//! spec forbids here ("must not draw anything that imitates it"). This one has
//! no `draw` body to put them in.
//!
//! It reports from `RedrawRequested`, which iced sends after layout and
//! before drawing, so the position is the one the frame shows. The caller
//! stores what it is told and passes it back as `known`; once they agree the
//! widget is silent, so a settled window costs nothing.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{Tree, Widget};
use iced::advanced::{mouse, Shell};
use iced::window;
use iced::{Element, Event, Length, Point, Rectangle, Size};

/// See the module docs.
pub struct Hole<'a, Message> {
    size: Size,
    known: Option<Point>,
    on_place: Box<dyn Fn(Point) -> Message + 'a>,
}

impl<'a, Message> Hole<'a, Message> {
    /// A hole of `size` logical pixels. `known` is the position the caller
    /// last stored; `on_place` turns a new position into a message.
    pub fn new(size: Size, known: Option<Point>, on_place: impl Fn(Point) -> Message + 'a) -> Self {
        Self {
            size,
            known,
            on_place: Box::new(on_place),
        }
    }
}

/// Positions closer than this are the same position: sub-pixel layout noise
/// must not move a slot, because moving it disarms it.
const SAME: f32 = 0.5;

fn moved(known: Option<Point>, now: Point) -> bool {
    match known {
        None => true,
        Some(k) => (k.x - now.x).abs() >= SAME || (k.y - now.y).abs() >= SAME,
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Hole<'_, Message>
where
    Renderer: renderer::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size.width), Length::Fixed(self.size.height))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, _limits: &layout::Limits) -> layout::Node {
        layout::Node::new(self.size)
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            let now = layout.position();
            if moved(self.known, now) {
                shell.publish((self.on_place)(now));
            }
        }
    }

    /// Deliberately empty. Whatever is here belongs to the compositor.
    fn draw(
        &self,
        _tree: &Tree,
        _renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
    }
}

impl<'a, Message, Theme, Renderer> From<Hole<'a, Message>> for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(hole: Hole<'a, Message>) -> Self {
        Element::new(hole)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_position_is_always_news() {
        assert!(moved(None, Point::new(0.0, 0.0)));
    }

    #[test]
    fn sub_pixel_noise_is_not_a_move() {
        let k = Some(Point::new(100.0, 200.0));
        assert!(!moved(k, Point::new(100.2, 199.8)));
        assert!(moved(k, Point::new(101.0, 200.0)));
        assert!(moved(k, Point::new(100.0, 199.0)));
    }
}
