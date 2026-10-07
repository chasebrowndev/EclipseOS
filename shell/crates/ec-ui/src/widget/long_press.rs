// SPDX-License-Identifier: AGPL-3.0-only
//! Long press: the touch stand-in for a right click.
//!
//! iced 0.14's `mouse_area` has `on_right_press` and nothing for a finger,
//! which has no right button. [`long_press`] wraps an element and publishes
//! one message once a finger has rested on it for [`motion::LONG_PRESS_MS`]
//! without travelling past [`motion::LONG_PRESS_SLOP`]. A lift or a lost
//! finger before then cancels it.
//!
//! A widget and not a styled container because it is a gesture with a clock:
//! it owns the finger's identity and asks the shell for a wake-up at the
//! deadline. The timing and slop rules live in [`LongPressTracker`], a pure
//! struct with the clock passed in, so they are unit-tested without a window.
//!
//! The press still reaches the content (a button inside lights as pressed).
//! When the long press fires, the content is told the finger was lost, so it
//! does not also fire its own click on lift. Mouse input is untouched: the
//! right button keeps working through `mouse_area` as before.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{mouse, touch, window, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use crate::tokens::motion;

/// What one finger has done so far.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rest {
    finger: touch::Finger,
    origin: Point,
    since: Instant,
    fired: bool,
}

/// The long-press state machine for one widget: pure, clock passed in.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LongPressTracker {
    rest: Option<Rest>,
}

impl LongPressTracker {
    /// A finger came down at `at`. A second finger while one is tracked is
    /// ignored: two fingers are a different gesture.
    pub fn pressed(&mut self, finger: touch::Finger, at: Point, now: Instant) {
        if self.rest.is_none() {
            self.rest = Some(Rest {
                finger,
                origin: at,
                since: now,
                fired: false,
            });
        }
    }

    /// The finger moved. Past the slop (measured from where it came down)
    /// it is no longer a press. Returns whether this move cancelled it.
    pub fn moved(&mut self, finger: touch::Finger, at: Point) -> bool {
        match self.rest {
            Some(r) if r.finger == finger && !r.fired => {
                let (dx, dy) = (at.x - r.origin.x, at.y - r.origin.y);
                if dx * dx + dy * dy > motion::LONG_PRESS_SLOP * motion::LONG_PRESS_SLOP {
                    self.rest = None;
                    return true;
                }
                false
            }
            _ => false,
        }
    }

    /// The finger lifted or was lost. Returns whether it was the tracked
    /// one and the press had already fired.
    pub fn released(&mut self, finger: touch::Finger) -> bool {
        match self.rest {
            Some(r) if r.finger == finger => {
                self.rest = None;
                r.fired
            }
            _ => false,
        }
    }

    /// Whether the press fires at `now`. True exactly once per press.
    pub fn poll(&mut self, now: Instant) -> bool {
        match &mut self.rest {
            Some(r) if !r.fired && now.saturating_duration_since(r.since) >= Self::delay() => {
                r.fired = true;
                true
            }
            _ => false,
        }
    }

    /// When to wake up next, if a press is waiting to fire.
    pub fn deadline(&self) -> Option<Instant> {
        self.rest.filter(|r| !r.fired).map(|r| r.since + Self::delay())
    }

    /// The finger currently tracked, fired or not.
    pub fn finger(&self) -> Option<touch::Finger> {
        self.rest.map(|r| r.finger)
    }

    fn delay() -> Duration {
        Duration::from_millis(motion::LONG_PRESS_MS)
    }
}

/// `content`, publishing `message` when a finger long-presses it.
pub fn long_press<'a, Message>(
    content: impl Into<Element<'a, Message, Theme>>,
    message: Message,
) -> LongPress<'a, Message> {
    LongPress {
        content: content.into(),
        message,
    }
}

/// See [`long_press`].
pub struct LongPress<'a, Message> {
    content: Element<'a, Message, Theme>,
    message: Message,
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for LongPress<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<LongPressTracker>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(LongPressTracker::default())
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
            layout.children().next().expect("long_press has its content"),
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
        // The cursor, not the event, carries the point inside a scrollable.
        let here = |fallback: Point| cursor.land().position().unwrap_or(fallback);
        let st = tree.state.downcast_mut::<LongPressTracker>();
        // The content hears the finger's loss in the same update the press
        // fires, so its own click does not follow the menu.
        let mut cancel_content: Option<Event> = None;
        match event {
            Event::Touch(touch::Event::FingerPressed { id, position }) => {
                let p = here(*position);
                if bounds.contains(p) {
                    st.pressed(*id, p, Instant::now());
                    if let Some(at) = st.deadline() {
                        shell.request_redraw_at(at);
                    }
                }
            }
            Event::Touch(touch::Event::FingerMoved { id, position }) => {
                st.moved(*id, here(*position));
            }
            Event::Touch(touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. }) => {
                st.released(*id);
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                if st.poll(*now) {
                    shell.publish(self.message.clone());
                    if let Some(id) = st.finger() {
                        cancel_content = Some(Event::Touch(touch::Event::FingerLost {
                            id,
                            position: bounds.center(),
                        }));
                    }
                    shell.request_redraw();
                } else if let Some(at) = st.deadline() {
                    shell.request_redraw_at(at);
                }
            }
            _ => {}
        }
        let event = cancel_content.as_ref().unwrap_or(event);
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("long_press has its content"),
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
            layout.children().next().expect("long_press has its content"),
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().expect("long_press has its content"),
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
            layout.children().next().expect("long_press has its content"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'a> From<LongPress<'a, Message>> for Element<'a, Message, Theme> {
    fn from(l: LongPress<'a, Message>) -> Self {
        Element::new(l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }
    const F: touch::Finger = touch::Finger(1);

    #[test]
    fn fires_once_after_the_delay() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        t.pressed(F, at(10.0, 10.0), t0);
        assert!(!t.poll(t0 + Duration::from_millis(499)));
        assert!(t.poll(t0 + Duration::from_millis(500)));
        assert!(!t.poll(t0 + Duration::from_millis(900)), "once per press");
        assert_eq!(t.deadline(), None);
    }

    #[test]
    fn deadline_is_press_plus_delay() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        assert_eq!(t.deadline(), None);
        t.pressed(F, at(0.0, 0.0), t0);
        assert_eq!(t.deadline(), Some(t0 + Duration::from_millis(500)));
    }

    #[test]
    fn drift_inside_the_slop_still_fires() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        t.pressed(F, at(0.0, 0.0), t0);
        assert!(!t.moved(F, at(5.0, 5.0)));
        assert!(t.poll(t0 + Duration::from_millis(600)));
    }

    #[test]
    fn travel_past_the_slop_cancels() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        t.pressed(F, at(0.0, 0.0), t0);
        assert!(t.moved(F, at(9.0, 0.0)));
        assert!(!t.poll(t0 + Duration::from_secs(2)));
    }

    #[test]
    fn slop_is_measured_from_the_origin_not_the_last_move() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        t.pressed(F, at(0.0, 0.0), t0);
        assert!(!t.moved(F, at(5.0, 0.0)));
        assert!(t.moved(F, at(10.0, 0.0)));
    }

    #[test]
    fn lift_before_the_delay_cancels() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        t.pressed(F, at(0.0, 0.0), t0);
        assert!(!t.released(F));
        assert!(!t.poll(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn lift_after_firing_reports_it() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        t.pressed(F, at(0.0, 0.0), t0);
        assert!(t.poll(t0 + Duration::from_millis(500)));
        assert!(t.released(F));
    }

    #[test]
    fn a_second_finger_is_ignored() {
        let t0 = Instant::now();
        let mut t = LongPressTracker::default();
        let g = touch::Finger(2);
        t.pressed(F, at(0.0, 0.0), t0);
        t.pressed(g, at(50.0, 50.0), t0 + Duration::from_millis(400));
        assert!(!t.moved(g, at(500.0, 500.0)));
        assert!(!t.released(g));
        assert!(t.poll(t0 + Duration::from_millis(500)));
    }
}
