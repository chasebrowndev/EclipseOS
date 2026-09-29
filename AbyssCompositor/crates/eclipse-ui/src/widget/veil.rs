// SPDX-License-Identifier: AGPL-3.0-only
//! Content drawn at reduced strength that stays fully interactive.
//!
//! A settings row that does not apply right now — a Liquid Glass tunable while
//! the blur mode is Frost — must still be shown and still be editable (the
//! settings app never hides a setting). Restyling every control it might hold
//! to a "dimmed" variant would mean a second style for the toggle, the slider,
//! the pills and the text field, each able to drift from the first. iced has
//! no opacity on a subtree either. So this draws the content as it is and then
//! repaints the panel's own ground over it, translucent, in a layer of its own so
//! the quad lands above the content's text: everything beneath reads at the
//! same reduced strength, and every event still reaches the content untouched.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{mouse, overlay, renderer, Clipboard, Shell};
use iced::{Background, Color, Element, Event, Length, Rectangle, Size, Vector};

pub struct Veil<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    /// `None` draws the content untouched, so a caller can toggle the veil
    /// without changing the widget tree (and losing a field's focus).
    ground: Option<&'a [Color]>,
    /// The veil's corners: soft, so that wherever the ground it repaints
    /// differs from what is really behind it the edge reads as glass, not a
    /// cut.
    radius: f32,
}

impl<'a, Message, Theme, Renderer> Veil<'a, Message, Theme, Renderer> {
    /// `ground` is painted over the content in order, each layer the full
    /// size of the content.
    pub fn new(
        content: impl Into<Element<'a, Message, Theme, Renderer>>,
        ground: Option<&'a [Color]>,
        radius: f32,
    ) -> Self {
        Self {
            content: content.into(),
            ground,
            radius,
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Veil<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(child.size(), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("a veil has one child"),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("a veil has one child"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("a veil has one child"),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().expect("a veil has one child"),
            cursor,
            viewport,
        );
        if let Some(ground) = self.ground {
            let bounds = layout.bounds();
            renderer.with_layer(bounds, |renderer| {
                for layer in ground {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds,
                            border: iced::border::rounded(self.radius),
                            ..renderer::Quad::default()
                        },
                        Background::Color(*layer),
                    );
                }
            });
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("a veil has one child"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<Veil<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(veil: Veil<'a, Message, Theme, Renderer>) -> Self {
        Element::new(veil)
    }
}
