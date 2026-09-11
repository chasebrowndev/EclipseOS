// SPDX-License-Identifier: AGPL-3.0-only
//! The 44×25 toggle pill.
//!
//! iced ships a `toggler`, but its geometry is its own and the spec's is exact:
//! a 44×25 pill whose knob is the *window base colour* when on, so that the
//! accent reads as a filled area rather than as a coloured outline. That is one
//! `fill_quad` for the track and one for the knob; a style function could not
//! have produced it.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{mouse, Shell};
use iced::{border, Color, Element, Event, Length, Rectangle, Size};

use crate::tokens::color;

const WIDTH: f32 = 44.0;
const HEIGHT: f32 = 25.0;
const PAD: f32 = 3.0;

/// A two-state switch. `on_toggle` receives the state the user asked for, not
/// the state we are in — the caller applies it (over IPC, usually) and feeds
/// the result back, so the switch never shows a change that did not land.
pub struct Toggle<'a, Message> {
    is_on: bool,
    on_toggle: Option<Box<dyn Fn(bool) -> Message + 'a>>,
}

impl<'a, Message> Toggle<'a, Message> {
    pub fn new(is_on: bool, on_toggle: impl Fn(bool) -> Message + 'a) -> Self {
        Self {
            is_on,
            on_toggle: Some(Box::new(on_toggle)),
        }
    }

    /// A toggle that shows state but cannot be driven — a policy-owned setting,
    /// for instance, which the viewer renders and the GUI may not write.
    pub fn locked(is_on: bool) -> Self {
        Self {
            is_on,
            on_toggle: None,
        }
    }
}

#[derive(Default)]
struct State {
    hovered: bool,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Toggle<'_, Message>
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(WIDTH), Length::Fixed(HEIGHT))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, _limits: &layout::Limits) -> layout::Node {
        layout::Node::new(Size::new(WIDTH, HEIGHT))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let over = cursor.is_over(layout.bounds());
        if state.hovered != over {
            state.hovered = over;
            shell.request_redraw();
        }

        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
            if over {
                if let Some(on_toggle) = &self.on_toggle {
                    shell.publish(on_toggle(!self.is_on));
                    shell.capture_event();
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.on_toggle.is_some() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let state = tree.state.downcast_ref::<State>();
        let live = self.on_toggle.is_some();
        // A locked toggle is dimmed rather than hidden: the setting still has a
        // value, and the value is what the user came to see.
        let alpha = if live { 1.0 } else { 0.45 };

        let track = if self.is_on {
            Color {
                a: alpha,
                ..color::ACCENT
            }
        } else {
            Color {
                a: (if state.hovered && live { 0.18 } else { 0.13 }) * alpha,
                ..Color::WHITE
            }
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: border::rounded(HEIGHT / 2.0),
                ..renderer::Quad::default()
            },
            track,
        );

        let d = HEIGHT - PAD * 2.0;
        let x = if self.is_on {
            bounds.x + bounds.width - PAD - d
        } else {
            bounds.x + PAD
        };

        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x,
                    y: bounds.y + PAD,
                    width: d,
                    height: d,
                },
                border: border::rounded(d / 2.0),
                ..renderer::Quad::default()
            },
            if self.is_on {
                Color {
                    a: alpha,
                    ..color::BASE
                }
            } else {
                Color {
                    a: 0.62 * alpha,
                    ..Color::WHITE
                }
            },
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Toggle<'a, Message>> for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(toggle: Toggle<'a, Message>) -> Self {
        Element::new(toggle)
    }
}
