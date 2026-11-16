// SPDX-License-Identifier: AGPL-3.0-only
//! A password field that shows one `*` per typed character.
//!
//! iced's secure mode hardcodes `•`, and no style changes the glyph. So the
//! field inside is the real secure `text_input`: it owns the keys, the cursor,
//! the selection, the clicks and the placeholder, and it is drawn with its value,
//! caret and selection inked transparent. This widget then draws the stars, the
//! selection and a blinking caret itself, at the positions the input uses.
//!
//! The input does not store a scroll offset; it works one out each frame from
//! where the cursor's grapheme sits in its run of bullets. This widget repeats
//! that arithmetic on the bullet's own measured advance, so the stars, the
//! caret, the selection and a click all land on the same cell at any length.
//! Nothing here reads the value: `count` is the caller's character count.

use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{mouse, renderer, Clipboard, Renderer as _, Shell};
use iced::alignment::Vertical;
use iced::widget::text_input::{self, TextInput, Value};
use iced::{Background, Color, Element, Event, Length, Pixels, Point, Rectangle, Size, Theme};

use crate::theme;
use crate::tokens::{color, font, size, space};

type Para = <iced::Renderer as text::Renderer>::Paragraph;

/// The caret's blink half-period: iced's own, so the inner input's redraws
/// land on our edges.
const BLINK: Duration = Duration::from_millis(500);
/// The room iced keeps right of the cursor before it scrolls
/// (`measure_cursor_and_scroll_offset` in iced_widget 0.14). Mirrored, not
/// chosen: the stars only agree with the input's clicks if both scroll alike.
const SCROLL_SLACK: f32 = 5.0;
/// Bullets measured at once for the advance, so rounding spreads thin.
const PROBE: usize = 16;

pub struct Starred<'a, Message> {
    input: TextInput<'a, Message, Theme, iced::Renderer>,
    count: usize,
    live: bool,
}

/// What [`Starred`] keeps between frames.
struct State {
    /// One bullet's advance in the input's font, as the input lays it out.
    advance: f32,
    /// When the caret last moved or the value last changed: the blink restarts.
    since: Instant,
    key: (usize, usize, usize, bool),
    /// The last press, so a double click can be told from a single one.
    last_click: Option<mouse::Click>,
    /// Where a drag-select started, while the button is down.
    anchor: Option<usize>,
    shift: bool,
}

/// iced's scroll for a cursor at `focus` (in cells): just enough that the
/// focus edge sits inside a field `width` wide.
fn scroll_offset(focus: usize, adv: f32, width: f32) -> f32 {
    (focus as f32 * adv + SCROLL_SLACK - width).max(0.0)
}

/// The cell boundary nearest a pointer `x` pixels into the text area, on a run
/// scrolled by `offset`: what a click there means.
fn index_at(x: f32, offset: f32, adv: f32, count: usize) -> usize {
    if adv <= 0.0 {
        return 0;
    }
    let cells = ((x + offset) / adv).round();
    if cells <= 0.0 {
        0
    } else {
        (cells as usize).min(count)
    }
}

impl<'a, Message: Clone + 'a> Starred<'a, Message> {
    /// `input` carries its id, placeholder, value and handlers; this sets its
    /// font, size, padding and style. `live` is whether keys reach it now.
    pub fn new(input: TextInput<'a, Message, Theme, iced::Renderer>, count: usize, live: bool) -> Self {
        let input = input
            .secure(true)
            .font(font::DATA)
            .size(size::BODY)
            .padding(text_input::DEFAULT_PADDING)
            .style(|t: &Theme, status| text_input::Style {
                value: Color::TRANSPARENT,
                selection: Color::TRANSPARENT,
                ..theme::prompt_input(t, status)
            });
        Self { input, count, live }
    }
}

impl<Message> Starred<'_, Message> {
    /// What a mouse event does to the cursor: `(anchor, at)` to place once the
    /// input has handled the event, or `None` to leave the input's own result.
    /// Double clicks and shift-clicks stay the input's (select all, extend).
    fn pointer(
        &self,
        tree: &mut Tree,
        event: &Event,
        child: Layout<'_>,
        cursor_at: mouse::Cursor,
    ) -> Option<(usize, usize)> {
        let text_bounds = child.children().next()?.bounds();
        let (focus, _, _) = cursor(tree, self.count);
        let offset = if inner(tree).is_focused() {
            scroll_offset(
                focus,
                tree.state.downcast_ref::<State>().advance,
                text_bounds.width,
            )
        } else {
            0.0
        };
        let st = tree.state.downcast_mut::<State>();
        let at = |x: f32| index_at(x - text_bounds.x, offset, st.advance, self.count);
        match event {
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(m)) => {
                st.shift = m.shift();
                None
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor_at.position_over(child.bounds())?;
                let click = mouse::Click::new(p, mouse::Button::Left, st.last_click);
                st.last_click = Some(click);
                if !self.live || st.shift || click.kind() != mouse::click::Kind::Single {
                    st.anchor = None;
                    return None;
                }
                let i = at(p.x);
                st.anchor = Some(i);
                Some((i, i))
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => st.anchor.map(|a| (a, at(position.x))),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                st.anchor = None;
                None
            }
            _ => None,
        }
    }
}

fn inner(tree: &Tree) -> &text_input::State<Para> {
    tree.children[0].state.downcast_ref::<text_input::State<Para>>()
}

/// The cursor as (focus index, selection start, selection end).
fn cursor(tree: &Tree, count: usize) -> (usize, usize, usize) {
    let bullets = Value::new(&"•".repeat(count));
    let c = inner(tree).cursor();
    match c.state(&bullets) {
        text_input::cursor::State::Index(i) => (i, i, i),
        text_input::cursor::State::Selection { start, end } => (end, start.min(end), start.max(end)),
    }
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for Starred<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            advance: 0.0,
            since: Instant::now(),
            key: (0, 0, 0, false),
            last_click: None,
            anchor: None,
            shift: false,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(
            &self.input as &dyn Widget<Message, Theme, iced::Renderer>,
        )]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children[0].diff(&self.input as &dyn Widget<Message, Theme, iced::Renderer>);
    }

    fn size(&self) -> Size<Length> {
        Widget::size(&self.input)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let probe = Para::with_text(text::Text {
            content: &"•".repeat(PROBE),
            bounds: Size::INFINITE,
            size: Pixels(size::BODY),
            line_height: text::LineHeight::default(),
            font: font::DATA,
            align_x: text::Alignment::Default,
            align_y: Vertical::Center,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        });
        let width = probe
            .grapheme_position(0, PROBE)
            .map_or(probe.min_width(), |p| p.x);
        tree.state.downcast_mut::<State>().advance = width / PROBE as f32;
        let child = Widget::layout(&mut self.input, &mut tree.children[0], renderer, limits);
        layout::Node::with_children(child.size(), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        let child = layout.children().next().expect("one child");
        Widget::operate(&mut self.input, &mut tree.children[0], child, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor_at: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let child = layout.children().next().expect("one child");
        // The pointer is mapped here, on the scroll the field is drawn with,
        // and applied over the input's own placement after it has run: the
        // input's hit test does not agree with its scrolled run.
        let place = self.pointer(tree, event, child, cursor_at);
        Widget::update(
            &mut self.input,
            &mut tree.children[0],
            event,
            child,
            cursor_at,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if let Some((anchor, at)) = place {
            let input = tree.children[0].state.downcast_mut::<text_input::State<Para>>();
            if anchor == at {
                input.move_cursor_to(at);
            } else {
                input.select_range(anchor, at);
            }
        }
        let (_, a, b) = cursor(tree, self.count);
        let focused = inner(tree).is_focused();
        let key = (self.count, a, b, focused);
        let st = tree.state.downcast_mut::<State>();
        if st.key != key {
            st.key = key;
            st.since = Instant::now();
        }
        if let Event::Window(iced::window::Event::RedrawRequested(now)) = event {
            if focused && self.live {
                let into = now.saturating_duration_since(st.since).as_millis() % BLINK.as_millis();
                let left = BLINK.as_millis() - into;
                shell.request_redraw_at(*now + Duration::from_millis(left as u64));
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor_at: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let child = layout.children().next().expect("one child");
        Widget::mouse_interaction(
            &self.input,
            &tree.children[0],
            child,
            cursor_at,
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
        cursor_at: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let child = layout.children().next().expect("one child");
        Widget::draw(
            &self.input,
            &tree.children[0],
            renderer,
            theme,
            style,
            child,
            cursor_at,
            viewport,
        );
        let Some(text_bounds) = child.children().next().map(|l| l.bounds()) else {
            return;
        };
        let st = tree.state.downcast_ref::<State>();
        let adv = st.advance;
        if adv <= 0.0 {
            return;
        }
        let focused = inner(tree).is_focused();
        let (focus, start, end) = cursor(tree, self.count);
        let offset = if focused {
            scroll_offset(focus, adv, text_bounds.width)
        } else {
            0.0
        };
        let x_at = |i: usize| text_bounds.x + i as f32 * adv - offset;
        let ink = color::text();

        renderer.with_layer(text_bounds, |r| {
            if focused && start != end {
                r.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: x_at(start),
                            y: text_bounds.y,
                            width: (end - start) as f32 * adv,
                            height: text_bounds.height,
                        },
                        ..renderer::Quad::default()
                    },
                    Background::Color(
                        theme::prompt_input(theme, text_input::Status::Focused { is_hovered: false })
                            .selection,
                    ),
                );
            }
            // Only the cells in view: a run of a thousand is drawn as fifty.
            let first = (offset / adv).floor() as usize;
            let shown = (text_bounds.width / adv).ceil() as usize + 1;
            for i in first..self.count.min(first + shown) {
                r.fill_text(
                    text::Text {
                        content: "*".to_owned(),
                        bounds: Size::new(adv, text_bounds.height),
                        size: Pixels(size::BODY),
                        line_height: text::LineHeight::default(),
                        font: font::DATA,
                        align_x: text::Alignment::Center,
                        align_y: Vertical::Center,
                        shaping: text::Shaping::Basic,
                        wrapping: text::Wrapping::None,
                    },
                    Point::new(x_at(i) + adv / 2.0, text_bounds.center_y()),
                    ink,
                    text_bounds,
                );
            }
            let lit =
                (Instant::now().saturating_duration_since(st.since).as_millis() / BLINK.as_millis()) % 2 == 0;
            if focused && self.live && start == end && lit {
                r.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: x_at(focus).floor(),
                            y: text_bounds.y,
                            width: space::HAIRLINE,
                            height: text_bounds.height,
                        },
                        ..renderer::Quad::default()
                    },
                    Background::Color(ink),
                );
            }
        });
    }
}

impl<'a, Message: Clone + 'a> From<Starred<'a, Message>> for Element<'a, Message, Theme, iced::Renderer> {
    fn from(s: Starred<'a, Message>) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_maps_through_the_scroll() {
        let (adv, width) = (7.0, 70.0);
        // Unscrolled: the nearest boundary, clamped to the run.
        assert_eq!(index_at(0.0, 0.0, adv, 5), 0);
        assert_eq!(index_at(10.0, 0.0, adv, 5), 1);
        assert_eq!(index_at(11.0, 0.0, adv, 5), 2);
        assert_eq!(index_at(500.0, 0.0, adv, 5), 5);
        assert_eq!(index_at(-3.0, 0.0, adv, 5), 0);
        // Forty cells, caret at the end: the run is scrolled so the caret
        // sits just inside the right edge, and a click at the left edge lands
        // on the first cell in view, not on cell zero.
        let off = scroll_offset(40, adv, width);
        assert_eq!(off, 40.0 * adv + SCROLL_SLACK - width);
        assert_eq!(index_at(0.0, off, adv, 40), 31);
        assert_eq!(index_at(width - SCROLL_SLACK, off, adv, 40), 40);
        // Short runs never scroll.
        assert_eq!(scroll_offset(3, adv, width), 0.0);
    }
}
