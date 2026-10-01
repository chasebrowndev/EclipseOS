// SPDX-License-Identifier: AGPL-3.0-only

//! Drag and drop inside one window: an object you pick up, with a ghost of
//! it following the pointer (FOG §Visual design, layer 4 "drag ghosts").
//!
//! The gesture is Eclipse Settings' (`eclipse-ui`'s `draggable`, D-05 §2),
//! ported rather than shared: Fog is its own workspace and takes no
//! dependency on the desktop's UI crate. What it keeps: the whole object is
//! the handle, a click and a drag are told apart by [`TAP_SLOP`], the ghost
//! hangs below the pointer so a drop mark under it stays in sight, and the
//! caller can drop the drag while the button is still held (Escape) by
//! passing `lifted(false)`. The widget decides nothing about what a drop
//! means: it reports press, travel and release.
//!
//! Two differences, both for a file list. A double click is reported (a
//! row opens on it), and the pointer's travel is *not* captured during a
//! drag: the targets under it (rows, places) keep their own hover states,
//! and that hover is how the caller aims the drop. So points are never
//! compared across the list's scroll and the rest of the window.
//!
//! Every message is taken from the object *as it was pressed*. A list can
//! move under a held button (a row inserted, the job tray folding away),
//! and the state of a press follows the widget's slot, not the object: a
//! release must still name what was picked up, never what slid under it.

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse::click;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::mouse;
use iced::{Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

/// How far a press may travel and still be a click, in logical pixels:
/// `eclipse-ui` tokens `motion::TAP_SLOP`.
pub const TAP_SLOP: f32 = 4.0;

/// An object that can be picked up; see the module note.
pub struct Draggable<'a, Message> {
    /// `[content]` or `[content, ghost]`.
    children: Vec<Element<'a, Message, Theme>>,
    on_drag: Option<Message>,
    on_release: Option<Message>,
    on_click: Option<Message>,
    on_double_click: Option<Message>,
    lifted: bool,
    hang: f32,
}

/// `content`, as the handle of a drag.
pub fn draggable<'a, Message>(
    content: impl Into<Element<'a, Message, Theme>>,
) -> Draggable<'a, Message> {
    Draggable {
        children: vec![content.into()],
        on_drag: None,
        on_release: None,
        on_click: None,
        on_double_click: None,
        lifted: true,
        hang: 0.0,
    }
}

impl<'a, Message> Draggable<'a, Message> {
    /// What follows the pointer while this is dragged, `gap` below the
    /// object as it was taken (above it where the window ends), held at the
    /// point it was taken as far as the ghost is wide, and kept on screen.
    pub fn ghost(mut self, ghost: impl Into<Element<'a, Message, Theme>>, gap: f32) -> Self {
        self.children.truncate(1);
        self.children.push(ghost.into());
        self.hang = gap;
        self
    }

    /// Published on every move while this is dragged. The first arrives
    /// when the travel first passes [`TAP_SLOP`]: the drag starts.
    pub fn on_drag(mut self, message: Message) -> Self {
        self.on_drag = Some(message);
        self
    }

    /// A drag ended, wherever the pointer is.
    pub fn on_release(mut self, message: Message) -> Self {
        self.on_release = Some(message);
        self
    }

    /// Pressed and released without passing the slop.
    pub fn on_click(mut self, message: Message) -> Self {
        self.on_click = Some(message);
        self
    }

    /// The second press of a double click. It starts no drag.
    pub fn on_double_click(mut self, message: Message) -> Self {
        self.on_double_click = Some(message);
        self
    }

    /// Whether the caller still holds the drag. `false` hides the ghost at
    /// once, button still down (Escape dropped it).
    pub fn lifted(mut self, lifted: bool) -> Self {
        self.lifted = lifted;
        self
    }
}

#[derive(Debug, Clone)]
struct Press<Message> {
    origin: Point,
    /// Where on the object it was taken, and how tall it was then: the
    /// ghost hangs from this point, below the object's edge.
    grab: Vector,
    height: f32,
    at: Point,
    dragging: bool,
    /// The object's messages at the press; see the module note.
    click: Option<Message>,
    drag: Option<Message>,
    release: Option<Message>,
}

impl<Message> Press<Message> {
    fn dragging(&self) -> bool {
        self.dragging
    }
}

struct DragState<Message> {
    press: Option<Press<Message>>,
    last_click: Option<click::Click>,
}

impl<Message> Default for DragState<Message> {
    fn default() -> Self {
        Self {
            press: None,
            last_click: None,
        }
    }
}

/// Where the ghost goes: `size` big, the pointer at `at` holding the object
/// `grab` into it, `drop` below that point, all inside `screen`.
///
/// The grab is measured on the object (a row the list's width); the ghost
/// is a label, so across it the grab is clamped to at most half its width.
/// Where there is no room below, the ghost goes above the object.
fn ghost_place(at: Point, grab: Vector, drop: f32, rise: f32, size: Size, screen: Size) -> Point {
    let x = at.x - grab.x.clamp(0.0, size.width / 2.0);
    let below = at.y + drop;
    let y = if below + size.height <= screen.height {
        below
    } else {
        at.y - rise - size.height
    };
    Point::new(
        x.clamp(0.0, (screen.width - size.width).max(0.0)),
        y.clamp(0.0, (screen.height - size.height).max(0.0)),
    )
}

impl<Message: Clone + 'static> Widget<Message, Theme, iced::Renderer> for Draggable<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<DragState<Message>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(DragState::<Message>::default())
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
        let child =
            self.children[0]
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
        let st = tree.state.downcast_mut::<DragState<Message>>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                // A press while one is held: the release was lost (the
                // pointer went away with the button down). The old press
                // is over, and says nothing: a drop never lands on a
                // release this widget did not see.
                st.press = None;
                if let Some(p) = cursor.position_over(bounds) {
                    let c = click::Click::new(p, mouse::Button::Left, st.last_click);
                    st.last_click = Some(c);
                    if c.kind() == click::Kind::Double {
                        if let Some(m) = &self.on_double_click {
                            shell.publish(m.clone());
                            shell.capture_event();
                            return;
                        }
                    }
                    st.press = Some(Press {
                        origin: p,
                        grab: p - bounds.position(),
                        height: bounds.height,
                        at: p,
                        dragging: false,
                        click: self.on_click.clone(),
                        drag: self.on_drag.clone(),
                        release: self.on_release.clone(),
                    });
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some(press), Some(p)) = (st.press.as_mut(), cursor.land().position()) {
                    press.at = p;
                    if !press.dragging && press.origin.distance(p) > TAP_SLOP {
                        press.dragging = true;
                    }
                    if press.dragging {
                        if let Some(m) = &press.drag {
                            shell.publish(m.clone());
                        }
                        // The ghost's place is layout.
                        shell.invalidate_layout();
                        shell.request_redraw();
                    }
                    // Not captured: the targets under the pointer track it.
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(press) = st.press.take() {
                    let m = if press.dragging {
                        press.release
                    } else {
                        press.click
                    };
                    if let Some(m) = m {
                        shell.publish(m);
                    }
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                    return;
                }
            }
            _ => {}
        }
        // Anything else reaches the content, the pointer lifted off it while
        // it is dragged: the object in hand does not light up under itself.
        let dragging = st.press.as_ref().is_some_and(Press::dragging);
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
        let st = tree.state.downcast_ref::<DragState<Message>>();
        if st.press.as_ref().is_some_and(Press::dragging) && self.lifted {
            mouse::Interaction::Grabbing
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
        let st = tree.state.downcast_ref::<DragState<Message>>();
        let cursor = if st.press.as_ref().is_some_and(Press::dragging) {
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
        let held = tree
            .state
            .downcast_ref::<DragState<Message>>()
            .press
            .as_ref()
            .filter(|p| p.dragging && self.lifted)
            .map(|p| (p.at, p.grab, p.height));
        let (content, ghost) = self.children.split_at_mut(1);
        let (content_tree, ghost_tree) = tree.children.split_at_mut(1);
        if let (Some((at, grab, height)), Some(element), Some(tree)) =
            (held, ghost.first_mut(), ghost_tree.first_mut())
        {
            return Some(overlay::Element::new(Box::new(Ghost {
                element,
                tree,
                // Overlays are placed on the screen; the pointer is in
                // layout space. `translation` is the scroll between the two.
                at: at + translation,
                grab,
                drop: height - grab.y + self.hang,
                rise: grab.y + self.hang,
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

impl<'a, Message: Clone + 'static> From<Draggable<'a, Message>> for Element<'a, Message, Theme> {
    fn from(d: Draggable<'a, Message>) -> Self {
        Element::new(d)
    }
}

/// The thing in hand, on the overlay. Inert: it takes no events, so the
/// rows and places under it keep the pointer and their hover states.
struct Ghost<'a, 'b, Message> {
    element: &'b mut Element<'a, Message, Theme>,
    tree: &'b mut Tree,
    /// The pointer, on the screen, and the rest of [`ghost_place`]'s say.
    at: Point,
    grab: Vector,
    drop: f32,
    rise: f32,
}

impl<Message> overlay::Overlay<Message, Theme, iced::Renderer> for Ghost<'_, '_, Message> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let node = self.element.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        let at = ghost_place(
            self.at,
            self.grab,
            self.drop,
            self.rise,
            node.size(),
            bounds,
        );
        node.move_to(at)
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

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Size = Size::new(800.0, 600.0);
    const GHOST: Size = Size::new(130.0, 24.0);

    #[test]
    fn a_row_taken_far_right_keeps_its_ghost_at_the_pointer() {
        // A full-width row taken 600px in: the ghost is 130 wide, so it
        // hangs from its middle, not 600px left of the pointer.
        let at = Point::new(620.0, 100.0);
        let p = ghost_place(at, Vector::new(600.0, 10.0), 30.0, 16.0, GHOST, SCREEN);
        assert_eq!(p, Point::new(620.0 - 65.0, 130.0));
        // Taken near its left edge, the grab holds.
        let p = ghost_place(at, Vector::new(12.0, 10.0), 30.0, 16.0, GHOST, SCREEN);
        assert_eq!(p.x, 608.0);
    }

    #[test]
    fn the_ghost_stays_on_screen() {
        // Against the right edge.
        let p = ghost_place(
            Point::new(795.0, 100.0),
            Vector::new(4.0, 10.0),
            30.0,
            16.0,
            GHOST,
            SCREEN,
        );
        assert_eq!(p.x, SCREEN.width - GHOST.width);
        // Against the left edge.
        let p = ghost_place(
            Point::new(10.0, 100.0),
            Vector::new(60.0, 10.0),
            30.0,
            16.0,
            GHOST,
            SCREEN,
        );
        assert_eq!(p.x, 0.0);
        // No room below: it goes above the row, clear of the pointer.
        let at = Point::new(400.0, 590.0);
        let p = ghost_place(at, Vector::new(4.0, 10.0), 30.0, 16.0, GHOST, SCREEN);
        assert_eq!(p.y, 590.0 - 16.0 - GHOST.height);
        assert!(p.y + GHOST.height < at.y);
    }
}
