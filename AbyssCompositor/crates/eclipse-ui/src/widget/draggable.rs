// SPDX-License-Identifier: AGPL-3.0-only
//! Drag and drop inside one window: an object you pick up ([`draggable`]),
//! the regions that report where they are so a drop can be aimed
//! ([`drop_zone`]), the ground a region wears while it can take a drop
//! ([`drop_well`]), and the look of the thing in flight ([`drag_ghost`]).
//!
//! Every point and every rectangle these report is in *layout space*: window
//! coordinates as the widget tree lays them out, before any scroll offset. A
//! drag point and a zone's bounds are therefore always comparable, however
//! far the pane is scrolled — the caller hit-tests one against the other and
//! never has to know about the scrollable they both sit in.
//!
//! The widgets decide nothing about what a drop means. They report press,
//! travel and release; the caller owns the hit test and the write.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::container;
use iced::{mouse, touch, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use crate::tokens::{bar, color, motion, space};

// ------------------------------------------------------------- draggable

/// An object that can be picked up: the whole element is the handle.
///
/// A widget and not a styled container because it *is* a gesture. It owns
/// the press, keeps following the pointer (or the finger) after it leaves its
/// own bounds for as long as the button is held, reports travel on both axes,
/// and tells a click from a drag by [`motion::TAP_SLOP`] — so one object can
/// be selected by a click and moved by a drag without a separate grip. While
/// it is held, a ghost of it follows the pointer on an overlay, above
/// everything, so what is being moved is never in doubt. The ghost hangs one
/// object-height below where it was taken, never on the pointer itself: the
/// drop mark a target draws under the pointer has to stay in sight.
///
/// The gesture logic is the bar grip's ([`crate::widget::DragBar`]), lifted
/// to two axes and to the whole object.
///
/// The press is not forwarded to `content`: a button inside is drawn (it
/// still lights under the pointer) but never fires. The click is this
/// widget's [`Draggable::on_click`].
pub struct Draggable<'a, Message> {
    /// `[content]` or `[content, ghost]`.
    children: Vec<Element<'a, Message, Theme>>,
    on_press: Option<Box<dyn Fn(Point) -> Message + 'a>>,
    on_move: Option<Box<dyn Fn(Point) -> Message + 'a>>,
    on_release: Option<Box<dyn Fn(Point) -> Message + 'a>>,
    on_click: Option<Message>,
    on_cancel: Option<Message>,
    lifted: Option<Point>,
}

/// `content`, as the handle of a drag. Nothing is wired until a callback is
/// set; an unwired draggable is its content and nothing more.
pub fn draggable<'a, Message>(content: impl Into<Element<'a, Message, Theme>>) -> Draggable<'a, Message> {
    Draggable {
        children: vec![content.into()],
        on_press: None,
        on_move: None,
        on_release: None,
        on_click: None,
        on_cancel: None,
        lifted: None,
    }
}

impl<'a, Message> Draggable<'a, Message> {
    /// What follows the pointer while this is held. Usually a
    /// [`drag_ghost`] of the object. Without one, nothing does.
    pub fn ghost(mut self, ghost: impl Into<Element<'a, Message, Theme>>) -> Self {
        self.children.truncate(1);
        self.children.push(ghost.into());
        self
    }

    /// The press landed, at this point. Not yet a drag: it may still be a
    /// click.
    pub fn on_press(mut self, f: impl Fn(Point) -> Message + 'a) -> Self {
        self.on_press = Some(Box::new(f));
        self
    }

    /// The pointer, while this is dragged. The first one arrives when the
    /// travel first passes [`motion::TAP_SLOP`]: that is the start of the
    /// drag, and a caller that keeps no state of its own can treat it so.
    pub fn on_move(mut self, f: impl Fn(Point) -> Message + 'a) -> Self {
        self.on_move = Some(Box::new(f));
        self
    }

    /// A drag ended, with the pointer here, wherever that is.
    pub fn on_release(mut self, f: impl Fn(Point) -> Message + 'a) -> Self {
        self.on_release = Some(Box::new(f));
        self
    }

    /// Pressed and released without passing the slop.
    pub fn on_click(mut self, message: Message) -> Self {
        self.on_click = Some(message);
        self
    }

    /// A drag that ended without a release: the finger was lost.
    pub fn on_cancel(mut self, message: Message) -> Self {
        self.on_cancel = Some(message);
        self
    }

    /// Draw as held, the ghost at `at`, when no press of its own is live.
    /// For a drag the caller is replaying (a debug preview), or one it has
    /// to keep drawing after the tree was rebuilt under it.
    ///
    /// While a press of its own is dragging, this is the caller's say on
    /// whether the drag is still on: `None` hides the ghost at once (the
    /// caller cancelled it), `Some` leaves it on the pointer. A caller with
    /// a ghost therefore passes `Some` for as long as it holds the drag.
    pub fn lifted(mut self, at: Option<Point>) -> Self {
        self.lifted = at;
        self
    }

    fn wired(&self) -> bool {
        self.on_press.is_some()
            || self.on_move.is_some()
            || self.on_release.is_some()
            || self.on_click.is_some()
    }
}

#[derive(Debug, Clone, Copy)]
struct Press {
    origin: Point,
    /// Where on the object it was taken: the ghost keeps this point under
    /// the pointer, so it does not jump to a corner when the drag starts.
    grab: Vector,
    at: Point,
    dragging: bool,
    finger: Option<touch::Finger>,
}

#[derive(Default)]
struct DragState {
    press: Option<Press>,
}

impl<Message: Clone> Draggable<'_, Message> {
    fn begin(&self, st: &mut DragState, p: Point, bounds: Rectangle, finger: Option<touch::Finger>) {
        st.press = Some(Press {
            origin: p,
            grab: p - bounds.position(),
            at: p,
            dragging: false,
            finger,
        });
    }

    fn travel(&self, st: &mut DragState, p: Point, shell: &mut Shell<'_, Message>) {
        let Some(press) = st.press.as_mut() else {
            return;
        };
        press.at = p;
        if !press.dragging && press.origin.distance(p) > motion::TAP_SLOP {
            press.dragging = true;
        }
        if press.dragging {
            if let Some(f) = &self.on_move {
                shell.publish(f(p));
            }
            // The ghost's place is layout: without this it would be drawn
            // where it was until something else asked for a relayout.
            shell.invalidate_layout();
            shell.request_redraw();
        }
        shell.capture_event();
    }

    fn end(&self, st: &mut DragState, shell: &mut Shell<'_, Message>) {
        let Some(press) = st.press.take() else {
            return;
        };
        if press.dragging {
            if let Some(f) = &self.on_release {
                shell.publish(f(press.at));
            }
        } else if let Some(m) = &self.on_click {
            shell.publish(m.clone());
        }
        shell.invalidate_layout();
        shell.request_redraw();
        shell.capture_event();
    }

    /// Where the ghost's top-left goes, in layout space, if it is drawn: as
    /// taken, one object-height and a gap lower.
    fn ghost_origin(&self, st: &DragState, bounds: Rectangle) -> Option<Point> {
        if self.children.len() < 2 {
            return None;
        }
        let hang = Vector::new(0.0, bounds.height + space::CHIP_GAP);
        match st.press {
            // The caller dropped the drag (Escape) while the button is
            // still down: the ghost goes now, not on release.
            Some(p) if p.dragging => self.lifted.map(|_| p.at - p.grab + hang),
            Some(_) => None,
            None => self
                .lifted
                .map(|at| at - Vector::new(bounds.width / 2.0, bounds.height / 2.0) + hang),
        }
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Draggable<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<DragState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(DragState::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        self.children[0].as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.children[0].as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child = self.children[0]
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
        self.children[0].as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("draggable has its content"),
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
        let bounds = layout.bounds();
        // The cursor, not the event, carries the point: a scrollable hands
        // its children a cursor already moved into layout space, and iced
        // moves the cursor with a finger too.
        let here = |fallback: Point| cursor.land().position().unwrap_or(fallback);
        let st = tree.state.downcast_mut::<DragState>();
        let held = st.press.is_some();
        if self.wired() {
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if !held => {
                    if let Some(p) = cursor.position_over(bounds) {
                        self.begin(st, p, bounds, None);
                        if let Some(f) = &self.on_press {
                            shell.publish(f(p));
                        }
                        shell.capture_event();
                        return;
                    }
                }
                Event::Touch(touch::Event::FingerPressed { id, position }) if !held => {
                    let p = here(*position);
                    if bounds.contains(p) {
                        self.begin(st, p, bounds, Some(*id));
                        if let Some(f) = &self.on_press {
                            shell.publish(f(p));
                        }
                        shell.capture_event();
                        return;
                    }
                }
                Event::Mouse(mouse::Event::CursorMoved { position })
                    if st.press.is_some_and(|p| p.finger.is_none()) =>
                {
                    self.travel(st, here(*position), shell);
                    return;
                }
                Event::Touch(touch::Event::FingerMoved { id, position })
                    if st.press.is_some_and(|p| p.finger == Some(*id)) =>
                {
                    self.travel(st, here(*position), shell);
                    return;
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                    if st.press.is_some_and(|p| p.finger.is_none()) =>
                {
                    self.end(st, shell);
                    return;
                }
                Event::Touch(touch::Event::FingerLifted { id, .. })
                    if st.press.is_some_and(|p| p.finger == Some(*id)) =>
                {
                    self.end(st, shell);
                    return;
                }
                Event::Touch(touch::Event::FingerLost { id, .. })
                    if st.press.is_some_and(|p| p.finger == Some(*id)) =>
                {
                    if st.press.take().is_some_and(|p| p.dragging) {
                        if let Some(m) = &self.on_cancel {
                            shell.publish(m.clone());
                        }
                    }
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }
                _ => {}
            }
        }
        // Anything else reaches the content, with the pointer lifted off it
        // while it is held: the object in hand does not light up under
        // itself.
        let dragging = st.press.is_some_and(|p| p.dragging);
        let inner = if dragging { cursor.levitate() } else { cursor };
        self.children[0].as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("draggable has its content"),
            inner,
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
        let st = tree.state.downcast_ref::<DragState>();
        if st.press.is_some_and(|p| p.dragging) {
            mouse::Interaction::Grabbing
        } else if self.wired() && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Grab
        } else {
            self.children[0].as_widget().mouse_interaction(
                &tree.children[0],
                layout.children().next().expect("draggable has its content"),
                cursor,
                viewport,
                renderer,
            )
        }
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
        let st = tree.state.downcast_ref::<DragState>();
        let cursor = if st.press.is_some_and(|p| p.dragging) {
            cursor.levitate()
        } else {
            cursor
        };
        self.children[0].as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().expect("draggable has its content"),
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
        let origin = self.ghost_origin(tree.state.downcast_ref::<DragState>(), layout.bounds());
        let (content, ghost) = self.children.split_at_mut(1);
        let (content_tree, ghost_tree) = tree.children.split_at_mut(1);
        if let (Some(origin), Some(element), Some(tree)) = (origin, ghost.first_mut(), ghost_tree.first_mut())
        {
            return Some(overlay::Element::new(Box::new(Ghost {
                element,
                tree,
                // Overlays are placed on the screen; the point is in layout
                // space. `translation` is the scroll between the two.
                at: origin + translation,
            })));
        }
        content[0].as_widget_mut().overlay(
            &mut content_tree[0],
            layout.children().next().expect("draggable has its content"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'a> From<Draggable<'a, Message>> for Element<'a, Message, Theme> {
    fn from(d: Draggable<'a, Message>) -> Self {
        Element::new(d)
    }
}

/// The thing in hand, drawn on the overlay at the pointer. Inert: it takes
/// no events and reports no interaction, so the base layer under it keeps
/// the pointer and its hover states while the drag passes over them.
struct Ghost<'a, 'b, Message> {
    element: &'b mut Element<'a, Message, Theme>,
    tree: &'b mut Tree,
    at: Point,
}

impl<Message> overlay::Overlay<Message, Theme, iced::Renderer> for Ghost<'_, '_, Message> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        self.element
            .as_widget_mut()
            .layout(self.tree, renderer, &layout::Limits::new(Size::ZERO, bounds))
            .move_to(self.at)
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
    ) {
        let bounds = layout.bounds();
        self.element.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            layout,
            mouse::Cursor::Unavailable,
            &bounds,
        );
    }
}

// ------------------------------------------------------------- drop zone

/// `content`, reporting where it is laid out whenever that changes.
///
/// A widget because iced has no way to ask where something ended up, and a
/// drop has to be aimed at something: a region that can take a drop, or the
/// objects inside one whose order a drop decides. iced's own `sensor`
/// reports a size and never a position, which is half of what a hit test
/// needs. The bounds are in layout space (see the module note), the same
/// space a [`draggable`] reports its pointer in.
pub struct DropZone<'a, Message> {
    content: Element<'a, Message, Theme>,
    on_bounds: Box<dyn Fn(Rectangle) -> Message + 'a>,
}

/// `content`, calling `on_bounds` with its bounds the first time they are
/// known and every time they change.
pub fn drop_zone<'a, Message>(
    content: impl Into<Element<'a, Message, Theme>>,
    on_bounds: impl Fn(Rectangle) -> Message + 'a,
) -> DropZone<'a, Message> {
    DropZone {
        content: content.into(),
        on_bounds: Box::new(on_bounds),
    }
}

#[derive(Default)]
struct ZoneState {
    told: Option<Rectangle>,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for DropZone<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ZoneState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ZoneState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<ZoneState>();
        if st.told != Some(bounds) {
            st.told = Some(bounds);
            shell.publish((self.on_bounds)(bounds));
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
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
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a> From<DropZone<'a, Message>> for Element<'a, Message, Theme> {
    fn from(z: DropZone<'a, Message>) -> Self {
        Element::new(z)
    }
}

// ------------------------------------------------------------------ looks

/// Where a [`drop_well`] stands in a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Well {
    /// No drag that could land here is live.
    #[default]
    Idle,
    /// Something that could land here is in hand.
    Armed,
    /// …and the pointer is over this well: releasing lands it here.
    Hot,
}

/// A region that takes drops: flat at rest, an inset ground whose edge
/// answers the drag while one is live.
///
/// A widget and not a styled container at each call site because the three
/// states are a contract between every drop target in the desktop — armed
/// draws the ground and edge, hot lifts both — and a target that
/// answered differently from its neighbour would not read as a target at
/// all. Never yellow: a drop target is a place, not the pane's live value.
pub fn drop_well<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    state: Well,
) -> container::Container<'a, Message, Theme> {
    container(content)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .style(move |_t: &Theme| {
            let (fill, edge) = match state {
                // At rest a well is not drawn at all: three idle targets
                // stacked would read as three panels, which is the look the
                // pane must not have.
                Well::Idle => (iced::Color::TRANSPARENT, iced::Color::TRANSPARENT),
                Well::Armed => (color::LIFT_SOFT, color::BORDER_STRONG),
                Well::Hot => (color::LIFT, color::TEXT_SECONDARY),
            };
            container::Style {
                background: Some(iced::Background::Color(fill)),
                border: iced::Border {
                    color: edge,
                    width: space::HAIRLINE,
                    radius: crate::tokens::radius::INSET.into(),
                },
                ..container::Style::default()
            }
        })
}

/// The object in hand: `content` on the nearly opaque menu ground with a
/// strong edge, at the object's own `radius`.
///
/// A widget because a ghost floats over anything — the bar picture, a list,
/// the sidebar — and has to stay legible over all of it; the ground that
/// guarantees that is [`color::MENU_GROUND`], and a ghost assembled at each
/// call site would pick a thinner one. It is drawn by a [`draggable`], on
/// its overlay.
pub fn drag_ghost<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    radius: f32,
) -> Element<'a, Message, Theme> {
    container(content)
        .padding([0.0, bar::WIDGET_X])
        .height(Length::Fixed(bar::WIDGET_H))
        .align_y(iced::Alignment::Center)
        .style(move |_t: &Theme| container::Style {
            background: Some(iced::Background::Color(color::MENU_GROUND)),
            border: iced::Border {
                color: color::BORDER_STRONG,
                width: space::HAIRLINE,
                radius: radius.into(),
            },
            ..container::Style::default()
        })
        .into()
}
