// SPDX-License-Identifier: AGPL-3.0-only
//! A card's ground, handed to the compositor at a given opacity.
//!
//! A widget and not a styled container because the pane needs the card's
//! surface-local bounds and the pointer at draw time, and because it carries
//! an opacity: `ec_ui::widget::glass_card` always sends alpha 255, so a card
//! fading out kept a full-strength pane until it was removed and the glass
//! popped off in one frame (GLASS-05). This is `glass_card`'s twin with the
//! alpha passed through and the hover state read from the cursor; it belongs
//! beside it in `ec-ui` and moves there when that crate is open to change.
//!
//! With no glass it draws nothing: the content's own container style is the
//! ground then.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::button;
use iced::{mouse, window, Element, Event, Length, Rectangle, Size, Theme, Vector};

use ec_ui::theme::CellTone;

/// Wrap `content` so its bounds are a glass pane of `tone` at `alpha`
/// (`0.0..=1.0`) when glass is on. Pair the content's container style with
/// `ec_ui::theme::bare_container_if_glass`.
pub fn pane<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    tone: CellTone,
    radius: f32,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    Element::new(Pane {
        content: content.into(),
        tone,
        radius,
        alpha: alpha.clamp(0.0, 1.0),
    })
}

struct Pane<'a, Message> {
    content: Element<'a, Message, Theme>,
    tone: CellTone,
    radius: f32,
    alpha: f32,
}

#[derive(Default)]
struct State {
    /// Whether the pointer was over it at the last draw, so a change asks for
    /// a frame.
    drawn: Option<bool>,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Pane<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
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
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("pane has one child"),
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
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let over = cursor.is_over(layout.bounds());
        let st = tree.state.downcast_mut::<State>();
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            st.drawn = Some(over);
        } else if st.drawn != Some(over) {
            shell.request_redraw();
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("pane has one child"),
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
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("pane has one child"),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if ec_ui::glass::available() && self.alpha > 0.0 && bounds.intersects(viewport) {
            let r = self.radius.min(bounds.width / 2.0).min(bounds.height / 2.0);
            let status = if cursor.is_over(bounds) {
                button::Status::Hovered
            } else {
                button::Status::Active
            };
            ec_ui::glass::push_in(
                ec_ui::glass::Pane::new(bounds, r, self.tone)
                    .status(status)
                    .alpha((self.alpha * 255.0).round() as u8),
                viewport,
            );
        }
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().expect("pane has one child"),
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("pane has one child"),
            renderer,
            viewport,
            translation,
        )
    }
}
