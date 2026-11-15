// SPDX-License-Identifier: AGPL-3.0-only
//! Motion pieces for a floating sheet: a cell whose hover and pressed tones
//! ease instead of snapping, a subtree that fades and rises in, and a
//! chevron that turns.
//!
//! iced's `button` restyles on the frame its status changes, and has no
//! opacity or rotation for a subtree, so each of these is a widget: the cell
//! keeps its own eased hover and press amounts and mixes the rest, hover and
//! pressed looks of its style by them; the fade translates its content and
//! washes it with the ground it sits on (and fades the compositor's panes
//! through [`crate::glass::with_fade`]); the chevron is a stroked path drawn
//! at an angle.

use std::f32::consts::FRAC_PI_2;
use std::time::{Duration, Instant};

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::{button, canvas};
use iced::{
    mouse, window, Background, Border, Color, Element, Event, Length, Padding, Point, Rectangle, Size, Theme,
    Vector,
};

use crate::theme::CellTone;

type StyleFn<'a> = Box<dyn Fn(&Theme, button::Status) -> button::Style + 'a>;

/// A pressable (or, without a message, static) glass cell whose ground eases
/// between its style's rest, hover and pressed looks over `ease`.
///
/// With compositor glass on, the ground is a pane of `tone` whose hover and
/// pressed bits flip half way through the ease; the client then draws no
/// fill or rim unless `keep_fill` (a gold ground, which the compositor never
/// draws) and the ink alone follows the mix. Content inherits the mixed text
/// colour, as it does in a `button`.
pub struct EasedCell<'a, Message> {
    content: Element<'a, Message, Theme>,
    on_press: Option<Message>,
    style: StyleFn<'a>,
    tone: CellTone,
    radius: f32,
    padding: Padding,
    ease: Duration,
    keep_fill: bool,
    keep_rim: bool,
}

impl<'a, Message: Clone + 'a> EasedCell<'a, Message> {
    pub fn new(
        content: impl Into<Element<'a, Message, Theme>>,
        style: impl Fn(&Theme, button::Status) -> button::Style + 'a,
        tone: CellTone,
        radius: f32,
    ) -> Self {
        Self {
            content: content.into(),
            on_press: None,
            style: Box::new(style),
            tone,
            radius,
            padding: Padding::ZERO,
            ease: Duration::ZERO,
            keep_fill: false,
            keep_rim: false,
        }
    }

    pub fn on_press(mut self, message: Message) -> Self {
        self.on_press = Some(message);
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    /// How long a change of tone takes; zero snaps (reduce-motion).
    pub fn ease(mut self, ease: Duration) -> Self {
        self.ease = ease;
        self
    }

    /// Keep the client's fill over the compositor's pane: the gold of a lit
    /// pill or a warning chip.
    pub fn keep_fill(mut self, keep: bool) -> Self {
        self.keep_fill = keep;
        self
    }

    /// Keep the client's rim over the compositor's pane: a gold-tinted rim
    /// that marks the default action (the compositor draws no colour).
    pub fn keep_rim(mut self, keep: bool) -> Self {
        self.keep_rim = keep;
        self
    }
}

#[derive(Debug, Default)]
struct CellState {
    over: bool,
    pressed: bool,
    /// Eased amounts, `0.0..=1.0`.
    hover: f32,
    press: f32,
    last: Option<Instant>,
}

impl CellState {
    fn settled(&self) -> bool {
        self.hover == f32::from(u8::from(self.over || self.pressed))
            && self.press == f32::from(u8::from(self.pressed))
    }

    fn step(&mut self, now: Instant, ease: Duration) {
        let dt = self
            .last
            .map_or(Duration::ZERO, |l| now.saturating_duration_since(l));
        self.last = Some(now);
        let k = if ease.is_zero() {
            1.0
        } else {
            dt.as_secs_f32() / ease.as_secs_f32()
        };
        let toward = |v: f32, t: bool| {
            let t = f32::from(u8::from(t));
            if v < t {
                (v + k).min(t)
            } else {
                (v - k).max(t)
            }
        };
        self.hover = toward(self.hover, self.over || self.pressed);
        self.press = toward(self.press, self.pressed);
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Color::from_rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

fn fill(s: &button::Style) -> Color {
    match s.background {
        Some(Background::Color(c)) => c,
        _ => Color::TRANSPARENT,
    }
}

/// An ease-out on a linear amount: quick to answer, soft to land.
fn soft(t: f32) -> f32 {
    crate::motion::ease_out(t.clamp(0.0, 1.0))
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for EasedCell<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<CellState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(CellState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::padded(limits, Length::Shrink, Length::Shrink, self.padding, |limits| {
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, limits)
        })
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
            layout.children().next().expect("cell has one child"),
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
            layout.children().next().expect("cell has one child"),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<CellState>();
        let over = cursor.is_over(bounds);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if over && self.on_press.is_some() =>
            {
                st.pressed = true;
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if st.pressed => {
                st.pressed = false;
                if over {
                    if let Some(m) = &self.on_press {
                        shell.publish(m.clone());
                    }
                    shell.capture_event();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                st.step(*now, self.ease);
            }
            _ => {}
        }
        st.over = over;
        if !st.settled() {
            if st.last.is_none() {
                st.last = Some(Instant::now());
            }
            shell.request_redraw();
        } else {
            st.last = None;
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
        if self.on_press.is_some() && cursor.is_over(layout.bounds()) {
            return mouse::Interaction::Pointer;
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().expect("cell has one child"),
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
        let st = tree.state.downcast_ref::<CellState>();
        let (h, p) = (soft(st.hover), soft(st.press));
        let rest = (self.style)(theme, button::Status::Active);
        let hot = (self.style)(theme, button::Status::Hovered);
        let down = (self.style)(theme, button::Status::Pressed);
        let ground = mix(mix(fill(&rest), fill(&hot), h), fill(&down), p);
        let rim = mix(mix(rest.border.color, hot.border.color, h), down.border.color, p);
        let ink = mix(mix(rest.text_color, hot.text_color, h), down.text_color, p);
        let r = self.radius.min(bounds.width / 2.0).min(bounds.height / 2.0);

        let glass = crate::glass::available();
        if glass {
            let mut pane = crate::glass::Pane::new(bounds, r, self.tone);
            if h >= 0.5 {
                pane.state |= crate::glass::HOVERED;
            }
            if p >= 0.5 {
                pane.state |= crate::glass::PRESSED;
            }
            crate::glass::push_in(pane, viewport);
        }
        let (with_fill, with_rim) = (!glass || self.keep_fill, !glass || self.keep_rim);
        if with_fill || with_rim {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        color: if with_rim { rim } else { Color::TRANSPARENT },
                        width: rest.border.width,
                        radius: r.into(),
                    },
                    shadow: if glass {
                        iced::Shadow::default()
                    } else {
                        rest.shadow
                    },
                    ..renderer::Quad::default()
                },
                Background::Color(if with_fill { ground } else { Color::TRANSPARENT }),
            );
        }
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            &renderer::Style { text_color: ink },
            layout.children().next().expect("cell has one child"),
            cursor,
            viewport,
        );
        let _ = style;
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
            layout.children().next().expect("cell has one child"),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: Clone + 'a> From<EasedCell<'a, Message>> for Element<'a, Message, Theme> {
    fn from(cell: EasedCell<'a, Message>) -> Self {
        Element::new(cell)
    }
}

/// Content drawn `amount` of the way in (`0.0..=1.0`): lowered by
/// `(1 - amount) * rise` and washed with `wash` — the ground it sits on —
/// at `1 - amount`, its compositor panes faded and lowered with it. Layout
/// and input are untouched, so nothing reflows while it plays.
pub struct Fade<'a, Message> {
    content: Element<'a, Message, Theme>,
    amount: f32,
    rise: f32,
    wash: Color,
}

impl<'a, Message> Fade<'a, Message> {
    pub fn new(content: impl Into<Element<'a, Message, Theme>>, amount: f32, rise: f32, wash: Color) -> Self {
        Self {
            content: content.into(),
            amount: amount.clamp(0.0, 1.0),
            rise,
            wash,
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Fade<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
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
        self.content.as_widget_mut().layout(tree, renderer, limits)
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
            .operate(tree, layout, renderer, operation);
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
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);
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
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        if self.amount <= 0.0 {
            return;
        }
        let dy = (1.0 - self.amount) * self.rise;
        crate::glass::with_fade(self.amount, dy, || {
            renderer.with_translation(Vector::new(0.0, dy), |r| {
                self.content
                    .as_widget()
                    .draw(tree, r, theme, style, layout, cursor, viewport);
            });
        });
        if self.amount < 1.0 {
            let bounds = layout.bounds();
            renderer.with_layer(bounds.expand(self.rise), |r| {
                r.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            height: bounds.height + self.rise,
                            ..bounds
                        },
                        ..renderer::Quad::default()
                    },
                    Background::Color(self.wash.scale_alpha(1.0 - self.amount)),
                );
            });
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
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a> From<Fade<'a, Message>> for Element<'a, Message, Theme> {
    fn from(fade: Fade<'a, Message>) -> Self {
        Element::new(fade)
    }
}

/// A chevron `turn` of the way from pointing right (folded, 0) to pointing
/// down (open, 1), stroked in `ink` in a square of `side`.
pub struct Turn {
    pub turn: f32,
    pub side: f32,
    pub stroke: f32,
    pub ink: Color,
}

impl<Message> canvas::Program<Message> for Turn {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        // The down-pointing V, centred on the origin: half as tall as wide.
        let w = (self.side - 2.0 * self.stroke) / 2.0;
        let h = w / 2.0;
        frame.translate(Vector::new(bounds.width / 2.0, bounds.height / 2.0));
        frame.rotate(-(1.0 - self.turn.clamp(0.0, 1.0)) * FRAC_PI_2);
        let v = canvas::Path::new(|p| {
            p.move_to(Point::new(-w, -h));
            p.line_to(Point::new(0.0, h));
            p.line_to(Point::new(w, -h));
        });
        frame.stroke(
            &v,
            canvas::Stroke::default()
                .with_color(self.ink)
                .with_width(self.stroke)
                .with_line_cap(canvas::LineCap::Round)
                .with_line_join(canvas::LineJoin::Round),
        );
        vec![frame.into_geometry()]
    }
}
