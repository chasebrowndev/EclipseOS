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
//!
//! Panes can be joined into one piece of glass (ADR 0130): [`pane_linked`]
//! takes a link key, and the second pane drawn under one key is joined to the
//! first, so a card and the drawer attached to it, or a folded group's face and
//! the strips beneath it, are one shape. [`frame`] clears the keys once a
//! frame, so an index from the last frame is never joined to.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::Renderer as _;
use iced::advanced::{Clipboard, Shell};
use iced::widget::button;
use iced::{mouse, window, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use ec_ui::theme::CellTone;

thread_local! {
    /// The index of the last pane drawn under each link key this frame.
    static LINKS: std::cell::RefCell<std::collections::HashMap<u64, usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// A link key and the fillet the join is drawn with, in logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    pub key: u64,
    pub fillet: f32,
}

/// Wrap `content` so its bounds are a glass pane of `tone` at `alpha`
/// (`0.0..=1.0`) when glass is on. Pair the content's container style with
/// `ec_ui::theme::bare_container_if_glass`.
pub fn pane<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    tone: CellTone,
    radius: f32,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    pane_linked(content, tone, radius, alpha, None, false)
}

/// [`pane`] that joins the pane drawn before it under the same `link` key.
///
/// The panes must touch, and neither may be a cell (a pane inside another
/// pane); the compositor draws a join of panes that do not as two shapes, so
/// the caller keeps the pair adjacent. Without glass nothing is sent and the
/// two content styles are the ground.
pub fn pane_linked<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    tone: CellTone,
    radius: f32,
    alpha: f32,
    link: Option<Link>,
    lit: bool,
) -> Element<'a, Message, Theme> {
    Element::new(Pane {
        content: content.into(),
        tone,
        radius,
        alpha: alpha.clamp(0.0, 1.0),
        link,
        lit,
    })
}

/// The root of a frame's panes: clears the link keys before anything is drawn.
pub fn frame<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    Element::new(Lane {
        content: content.into(),
        offset: Vector::ZERO,
        reset: true,
        sized: None,
    })
}

/// `content` laid out where it would be, then moved `dx` px along x (negative
/// is left). A widget because iced's row cannot place a child at a negative
/// offset: its size is clamped to its limits, and a card sliding in from the
/// left edge has to start left of its own slot. The slot around it clips.
pub fn shifted<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    dx: f32,
) -> Element<'a, Message, Theme> {
    Element::new(Lane {
        content: content.into(),
        offset: Vector::new(dx, 0.0),
        reset: false,
        sized: None,
    })
}

/// `content` laid out at its `full` size and shown through a window of
/// `outer` size, moved `offset` inside it and clipped to it: how a drawer
/// grows from its anchor without its rows being squeezed on the way.
pub fn reveal<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    full: Size,
    outer: Size,
    offset: Vector,
) -> Element<'a, Message, Theme> {
    Element::new(Lane {
        content: content.into(),
        offset,
        reset: false,
        sized: Some((outer, full)),
    })
}

struct Pane<'a, Message> {
    content: Element<'a, Message, Theme>,
    tone: CellTone,
    radius: f32,
    alpha: f32,
    link: Option<Link>,
    /// Drawn hovered whether or not the pointer is on it: a panel that is part
    /// of a card the pointer is on takes the card's glass.
    lit: bool,
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
            let status = if self.lit || cursor.is_over(bounds) {
                button::Status::Hovered
            } else {
                button::Status::Active
            };
            let index = ec_ui::glass::push_in(
                ec_ui::glass::Pane::new(bounds, r, self.tone)
                    .status(status)
                    .alpha((self.alpha * 255.0).round() as u8),
                viewport,
            );
            if let (Some(link), Some(index)) = (self.link, index) {
                let before = LINKS.with(|l| l.borrow_mut().insert(link.key, index));
                if let Some(before) = before {
                    ec_ui::glass::join(before, index, link.fillet);
                }
            }
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

/// See [`frame`] and [`shifted`].
struct Lane<'a, Message> {
    content: Element<'a, Message, Theme>,
    offset: Vector,
    reset: bool,
    /// `(outer, full)` for [`reveal`].
    sized: Option<(Size, Size)>,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Lane<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn state(&self) -> tree::State {
        tree::State::None
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
        let inner = match self.sized {
            Some((_, full)) => layout::Limits::new(Size::ZERO, full),
            None => *limits,
        };
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &inner)
            .move_to(Point::new(self.offset.x, self.offset.y));
        let size = self.sized.map_or_else(|| child.size(), |(outer, _)| outer);
        layout::Node::with_children(size, vec![child])
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
            layout.children().next().expect("lane has one child"),
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
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("lane has one child"),
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
            layout.children().next().expect("lane has one child"),
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
        if self.reset {
            LINKS.with(|l| l.borrow_mut().clear());
        }
        let child = layout.children().next().expect("lane has one child");
        let draw = |renderer: &mut iced::Renderer| {
            self.content
                .as_widget()
                .draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
        };
        if self.sized.is_some() {
            renderer.with_layer(layout.bounds(), draw);
        } else {
            draw(renderer);
        }
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
            layout.children().next().expect("lane has one child"),
            renderer,
            viewport,
            translation,
        )
    }
}
