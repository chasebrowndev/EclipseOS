// SPDX-License-Identifier: AGPL-3.0-only
//! The bar widget vocabulary (ADR 0065).
//!
//! Every widget on the taskbar is one object: a [`widget_shell`] holding a
//! [`drag_bar`] grip, a core, and an optional revealed section. The grip is
//! the drawer handle — drag it left and the revealed section slides out from
//! behind it; drag it right and the core slides in under it until the widget
//! is the grip alone. The parts ([`viz_bars`], [`mini_meter`],
//! [`transport`], [`volume_slider`], [`art_thumb`], [`track_label`]) are what
//! presets and custom widgets fill a core with, so a user's widget and a
//! shipped one read as the same instrument.
//!
//! Accent discipline: nothing in here is yellow unless the caller says so.
//! [`viz_bars`] is the natural live value (the music moving); the meters and
//! the volume track take an `accent` flag so a bar can spend its yellow
//! elsewhere, and only once.
//!
//! These are pure view functions over data and messages. No services, no IPC.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer::{self, Renderer as _};
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::{button, canvas, column, container, image, row, slider, text, Row, Space};
use iced::{
    mouse, touch, window, Alignment, Border, Color, Element, Event, Length, Point, Rectangle, Size, Theme,
    Vector,
};

use crate::tokens::{bar, color, font, size};
use crate::widget::quad;

// ------------------------------------------------------------------ grip

/// How a grip is drawn. The pointer drives `Hover` and `Active` on its own;
/// passing one here is a floor — for a preview, or for a drag the bar is
/// animating on the user's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Grip {
    #[default]
    Rest,
    Hover,
    Active,
}

impl Grip {
    fn dots(self) -> Color {
        match self {
            Grip::Rest => color::TEXT_TERTIARY,
            Grip::Hover => color::TEXT_SECONDARY,
            Grip::Active => color::TEXT,
        }
    }

    fn wash(self) -> Color {
        match self {
            Grip::Rest => Color::TRANSPARENT,
            Grip::Hover => color::LIFT_SOFT,
            Grip::Active => color::LIFT,
        }
    }
}

/// The grip: a 2×4 field of square dots in a [`bar::GRIP_W`] column.
///
/// A widget and not a styled container because it *is* a gesture: it owns the
/// press, follows the pointer (or a finger) outside its own bounds for the
/// length of the drag, reports the horizontal travel, and shows the grab
/// cursor. A container cannot keep a drag once the pointer leaves it, and a
/// grip that drops the drag when you overshoot is a grip nobody trusts.
///
/// The dot field is the one glyph every desktop already uses for "you can
/// pull this"; hard square dots keep it on the owner's hard-edged grid. It is
/// also the whole of a compressed widget, so it is drawn to stand alone: a
/// slim capsule with a texture in it, never an empty sliver.
pub struct DragBar<'a, Message> {
    floor: Grip,
    on_press: Option<Message>,
    on_drag: Option<Box<dyn Fn(f32) -> Message + 'a>>,
    on_release: Option<Message>,
    opacity: f32,
}

/// A grip at rest, with no gesture wired. See [`DragBar`].
pub fn drag_bar<'a, Message>() -> DragBar<'a, Message> {
    DragBar {
        floor: Grip::Rest,
        on_press: None,
        on_drag: None,
        on_release: None,
        opacity: 1.0,
    }
}

impl<'a, Message> DragBar<'a, Message> {
    /// Draw at least as `state`.
    pub fn state(mut self, state: Grip) -> Self {
        self.floor = state;
        self
    }

    /// The press that starts a drag (or a tap).
    pub fn on_press(mut self, message: Message) -> Self {
        self.on_press = Some(message);
        self
    }

    /// Horizontal travel since the press, in logical pixels; negative is
    /// leftward — the reveal direction.
    pub fn on_drag(mut self, f: impl Fn(f32) -> Message + 'a) -> Self {
        self.on_drag = Some(Box::new(f));
        self
    }

    /// The release that ends a drag, wherever the pointer is.
    pub fn on_release(mut self, message: Message) -> Self {
        self.on_release = Some(message);
        self
    }

    /// Draw the dots and the wash at `opacity` (`0.0..=1.0`): a grip whose
    /// widget is arriving or leaving fades with it instead of standing at
    /// full ink while its cell closes over it. See [`ShellFrame::grip_alpha`].
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    fn wired(&self) -> bool {
        self.on_press.is_some() || self.on_drag.is_some() || self.on_release.is_some()
    }
}

#[derive(Default)]
struct GripState {
    /// Where the press landed, while a drag is live.
    origin: Option<Point>,
    finger: Option<touch::Finger>,
    /// What was last drawn, so a change can ask for a frame.
    drawn: Grip,
}

impl GripState {
    fn look(&self, floor: Grip, hovered: bool) -> Grip {
        let own = if self.origin.is_some() {
            Grip::Active
        } else if hovered {
            Grip::Hover
        } else {
            Grip::Rest
        };
        own.max(floor)
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for DragBar<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<GripState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(GripState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(bar::GRIP_W), Length::Fixed(bar::WIDGET_H))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::new(bar::GRIP_W, bar::WIDGET_H))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let st = tree.state.downcast_mut::<GripState>();
        if self.wired() {
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(p) = cursor.position_over(bounds) {
                        st.origin = Some(p);
                        st.finger = None;
                        if let Some(m) = &self.on_press {
                            shell.publish(m.clone());
                        }
                        shell.capture_event();
                    }
                }
                Event::Touch(touch::Event::FingerPressed { id, position }) if bounds.contains(*position) => {
                    st.origin = Some(*position);
                    st.finger = Some(*id);
                    if let Some(m) = &self.on_press {
                        shell.publish(m.clone());
                    }
                    shell.capture_event();
                }
                Event::Mouse(mouse::Event::CursorMoved { position }) if st.finger.is_none() => {
                    if let (Some(o), Some(f)) = (st.origin, &self.on_drag) {
                        shell.publish(f(position.x - o.x));
                        shell.capture_event();
                    }
                }
                Event::Touch(touch::Event::FingerMoved { id, position }) if st.finger == Some(*id) => {
                    if let (Some(o), Some(f)) = (st.origin, &self.on_drag) {
                        shell.publish(f(position.x - o.x));
                        shell.capture_event();
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if st.finger.is_none() => {
                    if st.origin.take().is_some() {
                        if let Some(m) = &self.on_release {
                            shell.publish(m.clone());
                        }
                        shell.capture_event();
                    }
                }
                Event::Touch(touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. })
                    if st.finger == Some(*id) =>
                {
                    st.origin = None;
                    st.finger = None;
                    if let Some(m) = &self.on_release {
                        shell.publish(m.clone());
                    }
                    shell.capture_event();
                }
                _ => {}
            }
        }
        let now = st.look(self.floor, self.wired() && cursor.is_over(bounds));
        if let Event::Window(window::Event::RedrawRequested(_)) = event {
            st.drawn = now;
        } else if now != st.drawn {
            shell.request_redraw();
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<GripState>();
        if !self.wired() {
            mouse::Interaction::None
        } else if st.origin.is_some() {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let b = layout.bounds();
        let st = tree.state.downcast_ref::<GripState>();
        let look = st.look(self.floor, self.wired() && cursor.is_over(b));

        // The wash sits inside the shell's hairline and follows its left
        // corners, so a hovered grip is a lit end of the capsule rather than
        // a square pasted onto it.
        if self.opacity <= 0.0 {
            return;
        }
        let wash = look.wash().scale_alpha(self.opacity);
        if wash.a > 0.0 {
            let inset = bar::HAIRLINE;
            let r = (bar::RADIUS_WIDGET - inset).max(0.0);
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: b.x + inset,
                        y: b.y + inset,
                        width: b.width - inset,
                        height: b.height - 2.0 * inset,
                    },
                    border: Border {
                        radius: iced::border::Radius::default().top_left(r).bottom_left(r),
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                wash,
            );
        }

        let (cols, rows) = (bar::GRIP_COLS as f32, bar::GRIP_ROWS as f32);
        let pitch = bar::GRIP_DOT + bar::GRIP_DOT_GAP;
        let field_w = cols * pitch - bar::GRIP_DOT_GAP;
        let field_h = rows * pitch - bar::GRIP_DOT_GAP;
        // Whole-pixel origin: a dot that straddles a pixel is a grey smear.
        let x0 = (b.x + (b.width - field_w) / 2.0).round();
        let y0 = (b.y + (b.height - field_h) / 2.0).round();
        let ink = look.dots().scale_alpha(self.opacity);
        for r in 0..bar::GRIP_ROWS {
            for c in 0..bar::GRIP_COLS {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            x: x0 + c as f32 * pitch,
                            y: y0 + r as f32 * pitch,
                            width: bar::GRIP_DOT,
                            height: bar::GRIP_DOT,
                        },
                        ..renderer::Quad::default()
                    },
                    ink,
                );
            }
        }
    }
}

impl<'a, Message: Clone + 'a> From<DragBar<'a, Message>> for Element<'a, Message, Theme> {
    fn from(g: DragBar<'a, Message>) -> Self {
        Element::new(g)
    }
}

// ------------------------------------------------------------------ clip

/// Which edge of a clipped child stays put as the window onto it narrows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipEdge {
    /// The child's left edge is pinned: a label read from the left keeps its
    /// first word while the cell narrows over its tail. Task chips.
    #[default]
    Left,
    /// The child's right edge is pinned: a widget's body slides out from
    /// under its grip. Widget shells.
    Right,
}

/// A window onto a child laid out at its natural width, optionally on a glass
/// cell ground.
///
/// A widget because iced's `container` resolves a fixed-width child against
/// its own maximum: shrink the container and the child is laid out narrower,
/// which is text reflowing on every frame of a width animation — the jitter
/// ADR 0065's motion exists to avoid. This lays the child out once at
/// `natural` and draws only `visible` of it, clipped by a layer, pinned at
/// `edge`.
///
/// With a `ground` it is also the cell: the chip ground of
/// [`crate::theme::bar_cell`] drawn at the *visible* bounds, so the glass and
/// its hairline animate with the width while the content stays whole and
/// clipped. It has to be drawn here rather than by a styled container around
/// it: the ground reads the pointer (hover and press lift it, as they do a
/// chip) and a container has no pointer state.
struct Clip<'a, Message> {
    content: Element<'a, Message, Theme>,
    natural: f32,
    visible: f32,
    height: f32,
    edge: ClipEdge,
    /// `Some(accent)` draws the bar cell ground.
    ground: Option<bool>,
    /// The ground's opacity: its fill and hairline scale by this.
    opacity: f32,
}

#[derive(Default)]
struct CellState {
    pressed: bool,
    finger: Option<touch::Finger>,
    /// What was last drawn, so a change can ask for a frame.
    drawn: Option<button::Status>,
}

impl CellState {
    fn status(&self, hovered: bool) -> button::Status {
        match (self.pressed, hovered) {
            (true, _) => button::Status::Pressed,
            (false, true) => button::Status::Hovered,
            (false, false) => button::Status::Active,
        }
    }
}

impl<'a, Message> Clip<'a, Message> {
    fn inner_cursor(&self, layout: Layout<'_>, cursor: mouse::Cursor) -> mouse::Cursor {
        if cursor.is_over(layout.bounds()) {
            cursor
        } else {
            cursor.levitate()
        }
    }

    fn offset(&self) -> f32 {
        match self.edge {
            ClipEdge::Left => 0.0,
            ClipEdge::Right => self.visible - self.natural,
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Clip<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<CellState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(CellState::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.visible), Length::Fixed(self.height))
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
        _limits: &layout::Limits,
    ) -> layout::Node {
        let natural = Size::new(self.natural, self.height);
        let child = self
            .content
            .as_widget_mut()
            .layout(
                &mut tree.children[0],
                renderer,
                &layout::Limits::new(natural, natural),
            )
            .move_to(Point::new(self.offset(), 0.0));
        layout::Node::with_children(Size::new(self.visible, self.height), vec![child])
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
            layout.children().next().expect("clip has one child"),
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
        if self.ground.is_some() {
            // Observed, never captured: the press belongs to whatever inside
            // the cell it landed on; the ground only lights under it.
            let st = tree.state.downcast_mut::<CellState>();
            match event {
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if cursor.is_over(bounds) => {
                    st.pressed = true;
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if st.finger.is_none() => {
                    st.pressed = false;
                }
                Event::Touch(touch::Event::FingerPressed { id, position }) if bounds.contains(*position) => {
                    st.pressed = true;
                    st.finger = Some(*id);
                }
                Event::Touch(touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. })
                    if st.finger == Some(*id) =>
                {
                    st.pressed = false;
                    st.finger = None;
                }
                _ => {}
            }
            let now = st.status(cursor.is_over(bounds));
            if let Event::Window(window::Event::RedrawRequested(_)) = event {
                st.drawn = Some(now);
            } else if st.drawn != Some(now) {
                shell.request_redraw();
            }
        }
        // A touch that lands on the clipped-away part of the child is not a
        // touch on the child: the pointer path gets this from the levitated
        // cursor, a finger carries its own position and needs saying.
        if let Event::Touch(touch::Event::FingerPressed { position, .. }) = event {
            if !bounds.contains(*position) {
                return;
            }
        }
        let cursor = self.inner_cursor(layout, cursor);
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("clip has one child"),
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
            layout.children().next().expect("clip has one child"),
            self.inner_cursor(layout, cursor),
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
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        if let (Some(accent), true) = (self.ground, bounds.width >= 1.0 && self.opacity > 0.0) {
            let st = tree.state.downcast_ref::<CellState>();
            let look = crate::theme::bar_cell(accent)(theme, st.status(cursor.is_over(bounds)));
            // A cell narrower than its corners is a capsule, not a pinched
            // square: the radius never exceeds half the short side.
            let r = bar::RADIUS_CELL.min(bounds.width / 2.0).min(bounds.height / 2.0);
            let fill = look
                .background
                .unwrap_or(iced::Background::Color(Color::TRANSPARENT))
                .scale_alpha(self.opacity);
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        color: look.border.color.scale_alpha(self.opacity),
                        width: look.border.width,
                        radius: r.into(),
                    },
                    ..renderer::Quad::default()
                },
                fill,
            );
        }
        let cursor = self.inner_cursor(layout, cursor);
        renderer.with_layer(clip, |r| {
            self.content.as_widget().draw(
                &tree.children[0],
                r,
                theme,
                style,
                layout.children().next().expect("clip has one child"),
                cursor,
                &clip,
            );
        });
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
            layout.children().next().expect("clip has one child"),
            renderer,
            viewport,
            translation,
        )
    }
}

// ----------------------------------------------------------------- shell

/// Where a [`widget_shell`] is in its travel. Every field is `0.0..=1.0` and is
/// meant to come straight out of an [`crate::motion::Animated`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellFrame {
    /// How much of the core shows: 1 is the whole core, 0 is compressed to
    /// the grip.
    pub open: f32,
    /// How much of the revealed section shows.
    pub reveal: f32,
    /// The whole shell, grip included: 0 is zero width (Now Playing with
    /// nothing playing, animating out).
    pub presence: f32,
}

impl ShellFrame {
    pub const FULL: ShellFrame = ShellFrame {
        open: 1.0,
        reveal: 0.0,
        presence: 1.0,
    };
    pub const REVEALED: ShellFrame = ShellFrame {
        open: 1.0,
        reveal: 1.0,
        presence: 1.0,
    };
    pub const COMPRESSED: ShellFrame = ShellFrame {
        open: 0.0,
        reveal: 0.0,
        presence: 1.0,
    };
    pub const GONE: ShellFrame = ShellFrame {
        open: 0.0,
        reveal: 0.0,
        presence: 0.0,
    };

    /// The core's ink: faded by how open it is and by the shell's presence,
    /// each through [`lead`], so the core's glyphs are transparent before the
    /// closing edge reaches them and arrive only once there is room to read
    /// them.
    pub fn core_alpha(self) -> f32 {
        lead(self.open) * lead(self.presence)
    }

    /// The revealed section's ink, the same way.
    pub fn revealed_alpha(self) -> f32 {
        lead(self.reveal) * lead(self.presence)
    }

    /// The grip's ink: it goes and comes with the shell.
    pub fn grip_alpha(self) -> f32 {
        lead(self.presence)
    }

    /// The cell ground's opacity. The glass scales linearly with presence —
    /// it is the object's extent, and fading it ahead would leave content
    /// floating on nothing — and a shell compressed to its grip keeps only
    /// [`bar::GRIP_GROUND`] of it, so a run of grips is quiet.
    pub fn ground_alpha(self) -> f32 {
        let open = self.open.clamp(0.0, 1.0).max(self.reveal.clamp(0.0, 1.0));
        let kept = bar::GRIP_GROUND + (1.0 - bar::GRIP_GROUND) * open;
        kept * self.presence.clamp(0.0, 1.0)
    }
}

/// How much ink a part shows at `travel` (`0.0..=1.0`) of its motion:
/// nothing through the first [`crate::tokens::motion::FADE_LEAD`], then a
/// smoothstep to full. Continuous in `travel`, so a retargeted motion never
/// makes the ink jump; its slope is zero at both ends, so a settle never
/// ends on a visible step.
pub fn lead(travel: f32) -> f32 {
    let l = crate::tokens::motion::FADE_LEAD;
    let t = if travel.is_finite() {
        ((travel - l) / (1.0 - l)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    t * t * (3.0 - 2.0 * t)
}

/// The widths a shell's parts take, for a layout solver that has to know a
/// widget's width before it draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellSpan {
    /// The core's own width, excluding shell padding.
    pub core: f32,
    /// The revealed section's own width; 0 for a widget with none.
    pub revealed: f32,
}

impl ShellSpan {
    /// The core with its padding: what `open` scales.
    fn core_run(self) -> f32 {
        self.core + 2.0 * bar::WIDGET_X
    }

    /// The revealed section with the gap that joins it to the core: what
    /// `reveal` scales.
    fn reveal_run(self) -> f32 {
        if self.revealed > 0.0 {
            self.revealed + bar::WIDGET_GAP
        } else {
            0.0
        }
    }

    /// The body (everything right of the grip) laid out whole.
    fn natural_body(self) -> f32 {
        self.core_run() + self.reveal_run()
    }

    /// The body's drawn width at `frame`.
    fn body_at(self, frame: ShellFrame) -> f32 {
        let f = |v: f32| v.clamp(0.0, 1.0);
        (f(frame.open) * self.core_run() + f(frame.reveal) * self.reveal_run()).round()
    }

    /// The shell's drawn width at `frame` — exactly what [`widget_shell`]
    /// will occupy.
    pub fn width_at(self, frame: ShellFrame) -> f32 {
        ((bar::GRIP_W + self.body_at(frame)) * frame.presence.clamp(0.0, 1.0)).round()
    }
}

/// A bar widget: `[grip][revealed][core]` on one glass cell.
///
/// The cell is the task chip's own ground ([`crate::theme::bar_cell`]): a
/// hairline over the bar's glass, lifted by the pointer. A widget beside a
/// window chip reads as the same family of object, and a compressed widget
/// is a slim glass chip holding its grip (owner direction, ADR 0065).
///
/// A widget and not a styled row because the shell is the animation: its
/// width is `span.width_at(frame)`, the ground and its hairline are drawn at
/// that width, and every part inside is laid out once at its natural width
/// and *clipped*, never squashed, as the width moves. The body is anchored to
/// the right edge, so opening the core slides it out from under the grip and
/// revealing slides the extra section out after it — the grip is a drawer
/// handle, and the drawer comes out where you pull.
///
/// `core` and `revealed` are laid out at exactly `span.core` and
/// `span.revealed`; text in them should not wrap.
pub fn widget_shell<'a, Message: 'a>(
    grip: impl Into<Element<'a, Message, Theme>>,
    core: impl Into<Element<'a, Message, Theme>>,
    revealed: Option<Element<'a, Message, Theme>>,
    span: ShellSpan,
    frame: ShellFrame,
) -> Element<'a, Message, Theme> {
    let fixed = |e: Element<'a, Message, Theme>, w: f32| {
        container(e)
            .width(Length::Fixed(w))
            .height(Length::Fill)
            .align_y(Alignment::Center)
    };

    let mut body = Row::new().height(Length::Fixed(bar::WIDGET_H));
    body = body.push(Space::new().width(Length::Fixed(bar::WIDGET_X)));
    if let (Some(r), true) = (revealed, span.revealed > 0.0) {
        body = body.push(fixed(r, span.revealed));
        body = body.push(Space::new().width(Length::Fixed(bar::WIDGET_GAP)));
    }
    body = body.push(fixed(core.into(), span.core));
    body = body.push(Space::new().width(Length::Fixed(bar::WIDGET_X)));

    let body_w = span.body_at(frame);
    let inner = row![
        grip.into(),
        Element::new(Clip {
            content: body.into(),
            natural: span.natural_body(),
            visible: body_w,
            height: bar::WIDGET_H,
            edge: ClipEdge::Right,
            ground: None,
            opacity: 1.0,
        }),
    ];
    let natural = bar::GRIP_W + body_w;
    let visible = span.width_at(frame);

    glass_cell_faded(
        inner,
        natural,
        visible,
        false,
        ClipEdge::Right,
        frame.ground_alpha(),
    )
}

/// A bare glass cell: `content` laid out at `natural` width, shown `visible`
/// wide on the bar cell ground, pinned at `edge`. `accent` is a window that
/// is up.
///
/// What [`widget_shell`] draws its ground with, exposed for the cells that
/// are not widgets — the window chips — and for a widget that has nothing to
/// drag (an important one with no revealed section), so every cell on the
/// bar animates its width the same way: ground and hairline at the moving
/// width, content whole and clipped.
pub fn glass_cell<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    natural: f32,
    visible: f32,
    accent: bool,
    edge: ClipEdge,
) -> Element<'a, Message, Theme> {
    glass_cell_faded(content, natural, visible, accent, edge, 1.0)
}

/// [`glass_cell`] with its ground (fill and hairline) at `opacity`, for a
/// cell that is arriving or leaving: the glass fades as it narrows rather
/// than a full-strength hairline shrinking to a bright sliver. The content
/// fades itself — iced has no layer opacity — so pass it inks already scaled
/// (see [`ShellFrame::core_alpha`]).
pub fn glass_cell_faded<'a, Message: 'a>(
    content: impl Into<Element<'a, Message, Theme>>,
    natural: f32,
    visible: f32,
    accent: bool,
    edge: ClipEdge,
    opacity: f32,
) -> Element<'a, Message, Theme> {
    let visible = visible.max(0.0);
    container(Element::new(Clip {
        content: content.into(),
        natural,
        visible,
        height: bar::WIDGET_H,
        edge,
        ground: Some(accent),
        opacity: opacity.clamp(0.0, 1.0),
    }))
    .width(Length::Fixed(visible))
    .height(Length::Fixed(bar::WIDGET_H))
    .into()
}

// ------------------------------------------------------------ visualizer

/// Sixteen band levels drawn as hard columns mirrored about the midline.
///
/// A widget (drawn quads, like [`crate::widget::BarChart`]) rather than
/// sixteen containers because it redraws at the audio tap's ~60 Hz and must
/// cost sixteen quads, not sixteen layout nodes. Mirrored, because at bar
/// height a spectrum standing on a floor reads as a tiny chart; mirrored it
/// reads as sound. A silent band keeps [`bar::VIZ_FLOOR`], so a quiet passage
/// is a fine dotted rule and not an empty hole in the widget.
struct VizBars {
    levels: [f32; bar::VIZ_BANDS],
    ink: Color,
}

/// The visualizer. `live` spends the accent: pass it only while media is
/// playing and nothing else on the bar is yellow.
pub fn viz_bars<'a, Message: 'a>(levels: &[f32; bar::VIZ_BANDS], live: bool) -> Element<'a, Message, Theme> {
    viz_bars_faded(levels, live, 1.0)
}

/// [`viz_bars`] at `alpha` of its ink, for a shell in motion.
pub fn viz_bars_faded<'a, Message: 'a>(
    levels: &[f32; bar::VIZ_BANDS],
    live: bool,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let ink = if live { color::ACCENT } else { color::TEXT_TERTIARY };
    Element::new(VizBars {
        levels: *levels,
        ink: ink.scale_alpha(alpha.clamp(0.0, 1.0)),
    })
}

/// The drawn height of one band: whole pixels either side of the midline.
fn band_height(level: f32) -> f32 {
    let l = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let h = bar::VIZ_FLOOR + l * (bar::VIZ_H - bar::VIZ_FLOOR);
    ((h / 2.0).round() * 2.0).max(bar::VIZ_FLOOR)
}

impl<Message> Widget<Message, Theme, iced::Renderer> for VizBars {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(bar::VIZ_W), Length::Fixed(bar::VIZ_H))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &iced::Renderer,
        _limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(Size::new(bar::VIZ_W, bar::VIZ_H))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        if self.ink.a <= 0.0 {
            return;
        }
        let b = layout.bounds();
        let mid = (b.y + b.height / 2.0).round();
        let x0 = b.x.round();
        for (i, level) in self.levels.iter().enumerate() {
            let h = band_height(*level);
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: x0 + i as f32 * (bar::VIZ_BAR_W + bar::VIZ_GAP),
                        y: mid - h / 2.0,
                        width: bar::VIZ_BAR_W,
                        height: h,
                    },
                    ..renderer::Quad::default()
                },
                self.ink,
            );
        }
    }
}

// ----------------------------------------------------------------- meter

/// A labelled fraction: `CPU  42%` over a hairline track.
///
/// A widget because the pairing and the fixed width are the rule: a reading
/// without its track is body text (a coverage smell), a track without its
/// reading is decoration, and a meter whose width followed its digits would
/// shove its neighbours every second. `accent` is the bar's one yellow — leave
/// it off unless this reading is the live value.
pub fn mini_meter<'a, Message: 'a>(label: &str, fraction: f32, accent: bool) -> Element<'a, Message, Theme> {
    mini_meter_faded(label, fraction, accent, 1.0)
}

/// [`mini_meter`] at `alpha` of its ink, for a shell in motion.
pub fn mini_meter_faded<'a, Message: 'a>(
    label: &str,
    fraction: f32,
    accent: bool,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let a = alpha.clamp(0.0, 1.0);
    let f = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (fill, reading) = if accent {
        (color::ACCENT, color::ACCENT_TEXT)
    } else {
        (color::TEXT_SECONDARY, color::TEXT)
    };
    let (fill, reading) = (fill.scale_alpha(a), reading.scale_alpha(a));
    let lit = (bar::METER_W * f).round();
    let head = row![
        text(label.to_uppercase())
            .font(font::DATA_MEDIUM)
            .size(size::MICRO)
            .color(color::TEXT_TERTIARY.scale_alpha(a))
            .wrapping(text::Wrapping::None),
        Space::new().width(Length::Fill),
        text(format!("{}%", (f * 100.0).round() as u32))
            .font(font::DATA)
            .size(size::MICRO)
            .color(reading)
            .wrapping(text::Wrapping::None),
    ]
    .align_y(Alignment::Center);
    let track = row![
        quad(Length::Fixed(lit), Length::Fixed(bar::METER_H), fill, 0.0),
        quad(
            Length::Fixed(bar::METER_W - lit),
            Length::Fixed(bar::METER_H),
            color::TRACK.scale_alpha(a),
            0.0
        ),
    ];
    column![head, track]
        .spacing(bar::METER_GAP)
        .width(Length::Fixed(bar::METER_W))
        .into()
}

// ------------------------------------------------------------- transport

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Prev,
    Play,
    Pause,
    Next,
}

struct GlyphMark {
    glyph: Glyph,
    ink: Color,
    hover_ink: Color,
}

impl<Message> canvas::Program<Message> for GlyphMark {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let g = bar::TRANSPORT_GLYPH;
        let s = bar::TRANSPORT_STROKE;
        let o = Point::new(
            ((bounds.width - g) / 2.0).round(),
            ((bounds.height - g) / 2.0).round(),
        );
        let p = |x: f32, y: f32| Point::new(o.x + x, o.y + y);
        let tri = |a: Point, b: Point, c: Point| {
            canvas::Path::new(|path| {
                path.move_to(a);
                path.line_to(b);
                path.line_to(c);
                path.close();
            })
        };
        let bar_at = |x: f32, w: f32| canvas::Path::rectangle(p(x, 0.0), Size::new(w, g));
        let paths = match self.glyph {
            Glyph::Play => vec![tri(p(1.0, 0.0), p(g, g / 2.0), p(1.0, g))],
            Glyph::Pause => vec![bar_at(1.0, s + 1.0), bar_at(g - s - 2.0, s + 1.0)],
            Glyph::Next => vec![tri(p(0.0, 0.0), p(g - s, g / 2.0), p(0.0, g)), bar_at(g - s, s)],
            Glyph::Prev => vec![bar_at(0.0, s), tri(p(g, 0.0), p(s, g / 2.0), p(g, g))],
        };
        let ink = if cursor.is_over(bounds) {
            self.hover_ink
        } else {
            self.ink
        };
        for path in paths {
            frame.fill(&path, ink);
        }
        vec![frame.into_geometry()]
    }
}

fn transport_lift(alpha: f32) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_t: &Theme, status: button::Status| transport_lift_at(status, alpha)
}

fn transport_lift_at(status: button::Status, alpha: f32) -> button::Style {
    let background = match status {
        button::Status::Hovered => color::LIFT_SOFT,
        button::Status::Pressed => color::LIFT,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(iced::Background::Color(background.scale_alpha(alpha))),
        text_color: color::TEXT,
        border: iced::border::rounded(bar::RADIUS_TRANSPORT),
        ..button::Style::default()
    }
}

fn transport_button<'a, Message: Clone + 'a>(
    glyph: Glyph,
    primary: bool,
    on_press: Option<Message>,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let (ink, hover_ink) = match (on_press.is_some(), primary) {
        (false, _) => (color::TEXT_TERTIARY, color::TEXT_TERTIARY),
        (true, true) => (color::TEXT, color::TEXT),
        (true, false) => (color::TEXT_SECONDARY, color::TEXT),
    };
    let mark = canvas(GlyphMark {
        glyph,
        ink: ink.scale_alpha(alpha),
        hover_ink: hover_ink.scale_alpha(alpha),
    })
    .width(Length::Fixed(bar::TRANSPORT_BTN))
    .height(Length::Fixed(bar::TRANSPORT_BTN));
    let b = button(mark)
        .padding(iced::Padding::ZERO)
        .width(Length::Fixed(bar::TRANSPORT_BTN))
        .height(Length::Fixed(bar::TRANSPORT_BTN))
        .style(transport_lift(alpha));
    match on_press {
        Some(m) => b.on_press(m).into(),
        None => b.into(),
    }
}

/// Previous, play/pause, next.
///
/// A widget because the three are one control with one rule: the middle
/// button is the verb that is possible *now* (pause while playing, play while
/// not) and is drawn in primary ink; the skips are secondary; a verb the
/// player refuses (`None`) stays in place, tertiary and inert, so the play
/// button never slides sideways when a player lacks a skip. The glyphs are
/// drawn geometry — the desktop ships no icon font — on hard edges.
pub fn transport<'a, Message: Clone + 'a>(
    playing: bool,
    prev: Option<Message>,
    play_pause: Option<Message>,
    next: Option<Message>,
) -> Element<'a, Message, Theme> {
    transport_faded(playing, prev, play_pause, next, 1.0)
}

/// [`transport`] at `alpha` of its ink, for a revealed section in motion.
pub fn transport_faded<'a, Message: Clone + 'a>(
    playing: bool,
    prev: Option<Message>,
    play_pause: Option<Message>,
    next: Option<Message>,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let a = alpha.clamp(0.0, 1.0);
    row![
        transport_button(Glyph::Prev, false, prev, a),
        transport_button(
            if playing { Glyph::Pause } else { Glyph::Play },
            true,
            play_pause,
            a
        ),
        transport_button(Glyph::Next, false, next, a),
    ]
    .spacing(bar::TRANSPORT_GAP)
    .align_y(Alignment::Center)
    .into()
}

/// The width [`transport`] takes, for a [`ShellSpan`].
pub const TRANSPORT_W: f32 = 3.0 * bar::TRANSPORT_BTN + 2.0 * bar::TRANSPORT_GAP;

// ---------------------------------------------------------------- volume

/// A compact volume track with its reading.
///
/// A widget because at bar height the stock slider's round knob is a bead on
/// a wire; this is a 2px rail with a hard vertical tick, and the reading sits
/// in a fixed box so the rail never moves when `9` becomes `10`. `volume` is
/// the sink level (1.0 is 100%), `max` the ceiling (`bar.widgets.volume
/// .max-percent` / 100). Muted keeps the level visible but drops it to
/// tertiary ink — the level is still the level you will get back. `accent`
/// makes the fill the bar's one yellow.
pub fn volume_slider<'a, Message: Clone + 'a>(
    volume: f32,
    max: f32,
    muted: bool,
    accent: bool,
    on_change: impl Fn(f32) -> Message + 'a,
    on_release: Option<Message>,
) -> Element<'a, Message, Theme> {
    volume_slider_faded(volume, max, muted, accent, on_change, on_release, 1.0)
}

/// [`volume_slider`] at `alpha` of its ink, for a revealed section in motion.
pub fn volume_slider_faded<'a, Message: Clone + 'a>(
    volume: f32,
    max: f32,
    muted: bool,
    accent: bool,
    on_change: impl Fn(f32) -> Message + 'a,
    on_release: Option<Message>,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let a = alpha.clamp(0.0, 1.0);
    let max = if max.is_finite() && max > 0.0 { max } else { 1.0 };
    let v = if volume.is_finite() {
        volume.clamp(0.0, max)
    } else {
        0.0
    };
    let (fill, reading) = match (muted, accent) {
        (true, _) => (color::TEXT_TERTIARY, color::TEXT_TERTIARY),
        (false, true) => (color::ACCENT, color::ACCENT_TEXT),
        (false, false) => (color::TEXT_SECONDARY, color::TEXT),
    };
    let (fill, reading) = (fill.scale_alpha(a), reading.scale_alpha(a));
    let mut track = slider(0.0..=max, v, on_change)
        .step(0.01_f32)
        .width(Length::Fixed(bar::VOLUME_W))
        .height(bar::VOLUME_KNOB_H)
        .style(move |_t: &Theme, status: slider::Status| {
            let knob = match status {
                slider::Status::Active => fill,
                _ if muted => color::TEXT_SECONDARY.scale_alpha(a),
                _ => color::TEXT.scale_alpha(a),
            };
            slider::Style {
                rail: slider::Rail {
                    backgrounds: (
                        iced::Background::Color(fill),
                        iced::Background::Color(color::TRACK.scale_alpha(a)),
                    ),
                    width: bar::VOLUME_RAIL,
                    border: Border::default(),
                },
                handle: slider::Handle {
                    shape: slider::HandleShape::Rectangle {
                        width: bar::VOLUME_KNOB_W,
                        border_radius: 0.0.into(),
                    },
                    background: iced::Background::Color(knob),
                    border_color: Color::TRANSPARENT,
                    border_width: 0.0,
                },
            }
        });
    if let Some(m) = on_release {
        track = track.on_release(m);
    }
    let shown = if muted {
        "mute".to_string()
    } else {
        format!("{}", (v * 100.0).round() as u32)
    };
    row![
        track,
        container(
            text(shown)
                .font(font::DATA)
                .size(size::MICRO)
                .color(reading)
                .wrapping(text::Wrapping::None)
        )
        .width(Length::Fixed(bar::VOLUME_READOUT_W))
        .align_x(Alignment::End),
    ]
    .spacing(bar::VOLUME_GAP)
    .align_y(Alignment::Center)
    .into()
}

/// The width [`volume_slider`] takes, for a [`ShellSpan`].
pub const VOLUME_SLIDER_W: f32 = bar::VOLUME_W + bar::VOLUME_GAP + bar::VOLUME_READOUT_W;

// ------------------------------------------------------------------- art

/// Decode-once handle for album art bytes. Keep it: a handle made per frame
/// is a new texture per frame.
pub fn art_handle(bytes: &[u8]) -> image::Handle {
    image::Handle::from_bytes(bytes.to_vec())
}

/// Album art: a [`bar::ART`] square, cropped to fill, hard-edged or on
/// [`bar::RADIUS_ART`]. With no art it is a quiet square of track ink, so a
/// track change does not shift the title sideways.
///
/// A widget because the square, the crop and the fallback are the rule — art
/// arrives in every aspect ratio, and a letterboxed thumbnail at 24px is a
/// thin stripe.
pub fn art_thumb<'a, Message: 'a>(art: Option<&image::Handle>, hard: bool) -> Element<'a, Message, Theme> {
    let r = if hard { 0.0 } else { bar::RADIUS_ART };
    match art {
        Some(h) => image(h.clone())
            .width(Length::Fixed(bar::ART))
            .height(Length::Fixed(bar::ART))
            .content_fit(iced::ContentFit::Cover)
            .border_radius(r)
            .into(),
        None => quad(Length::Fixed(bar::ART), Length::Fixed(bar::ART), color::TRACK, r),
    }
}

/// The art square while a cover arrives: the [`art_thumb`] placeholder under
/// it, the cover over it at `arrival` (`0.0..=1.0`, from an animation), the
/// whole at `alpha` of its ink.
///
/// A widget because the cover lands after the track — the service fetches
/// it, sometimes over the network — and the slot is already reserved, so the
/// cover must *develop* in place over the square that held its room. An
/// image swapped in on the frame it decoded is a pop, and ADR 0065's motion
/// exists to remove pops.
pub fn art_slot<'a, Message: 'a>(
    art: Option<&image::Handle>,
    hard: bool,
    arrival: f32,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let r = if hard { 0.0 } else { bar::RADIUS_ART };
    let a = alpha.clamp(0.0, 1.0);
    let arrival = arrival.clamp(0.0, 1.0);
    let under = |ink: Color| quad(Length::Fixed(bar::ART), Length::Fixed(bar::ART), ink, r);
    match art {
        Some(h) if arrival > 0.0 => iced::widget::stack![
            under(color::TRACK.scale_alpha(a * (1.0 - arrival))),
            image(h.clone())
                .width(Length::Fixed(bar::ART))
                .height(Length::Fixed(bar::ART))
                .content_fit(iced::ContentFit::Cover)
                .border_radius(r)
                .opacity(a * arrival),
        ]
        .into(),
        _ => under(color::TRACK.scale_alpha(a)),
    }
}

// ----------------------------------------------------------------- label

/// Cut `s` to at most `max` characters, with an ellipsis if it was cut.
pub fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

/// A two-line label in a fixed width: a title over a mono subtitle.
///
/// A widget because a core's text has to be cut before layout — iced has no
/// eliding text, and a label that wrapped or grew would change the shell's
/// width with every song. It is elided to the width's character budget and
/// then clipped, so the worst case is a lost character, never a reflow.
pub fn track_label<'a, Message: 'a>(title: &str, subtitle: &str, width: f32) -> Element<'a, Message, Theme> {
    track_label_faded(title, subtitle, width, 1.0)
}

/// [`track_label`] at `alpha` of its ink, for a shell in motion.
pub fn track_label_faded<'a, Message: 'a>(
    title: &str,
    subtitle: &str,
    width: f32,
    alpha: f32,
) -> Element<'a, Message, Theme> {
    let a = alpha.clamp(0.0, 1.0);
    let budget = (width / bar::CHAR_W).floor() as usize;
    // The mono face at MICRO is narrower per char than the UI face at 12px.
    let mono_budget = (width / (bar::CHAR_W * size::MICRO / size::BODY_SMALL)).floor() as usize;
    container(
        column![
            text(elide(title, budget))
                .font(font::UI_MEDIUM)
                .size(size::BODY_SMALL)
                .color(color::TEXT.scale_alpha(a))
                .wrapping(text::Wrapping::None),
            text(elide(subtitle, mono_budget))
                .font(font::DATA)
                .size(size::MICRO)
                .color(color::TEXT_SECONDARY.scale_alpha(a))
                .wrapping(text::Wrapping::None),
        ]
        .spacing(bar::LABEL_LINE_GAP),
    )
    .width(Length::Fixed(width))
    .clip(true)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: ShellSpan = ShellSpan {
        core: 120.0,
        revealed: 80.0,
    };

    #[test]
    fn a_compressed_shell_is_its_grip_and_a_gone_one_is_nothing() {
        assert_eq!(SPAN.width_at(ShellFrame::COMPRESSED), bar::GRIP_W);
        assert_eq!(SPAN.width_at(ShellFrame::GONE), 0.0);
    }

    #[test]
    fn revealing_adds_exactly_the_revealed_run() {
        let full = SPAN.width_at(ShellFrame::FULL);
        let open = SPAN.width_at(ShellFrame::REVEALED);
        assert_eq!(open - full, SPAN.revealed + bar::WIDGET_GAP);
        assert_eq!(full, bar::GRIP_W + SPAN.core + 2.0 * bar::WIDGET_X);
    }

    #[test]
    fn shell_width_is_monotonic_in_every_fraction() {
        let mut prev = 0.0;
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            let w = SPAN.width_at(ShellFrame {
                open: t,
                reveal: 0.0,
                presence: 1.0,
            });
            assert!(w >= prev);
            prev = w;
        }
        let w = SPAN.width_at(ShellFrame {
            open: 5.0,
            reveal: -1.0,
            presence: 2.0,
        });
        assert_eq!(w, SPAN.width_at(ShellFrame::FULL));
    }

    #[test]
    fn band_heights_are_even_and_floored() {
        for l in [0.0, 0.01, 0.33, 0.5, 1.0, 7.0, -1.0, f32::NAN] {
            let h = band_height(l);
            assert!((bar::VIZ_FLOOR..=bar::VIZ_H).contains(&h), "{l}: {h}");
            assert_eq!(h % 2.0, 0.0, "{l}: {h}");
        }
    }

    #[test]
    fn elide_keeps_short_labels_and_marks_cut_ones() {
        assert_eq!(elide("abc", 3), "abc");
        assert_eq!(elide("abcdef", 4), "abc\u{2026}");
        assert_eq!(elide("abc", 0), "");
    }

    #[test]
    fn a_grip_floor_never_hides_a_live_drag() {
        let st = GripState {
            origin: Some(Point::ORIGIN),
            ..GripState::default()
        };
        assert_eq!(st.look(Grip::Hover, false), Grip::Active);
        assert_eq!(GripState::default().look(Grip::Hover, false), Grip::Hover);
        assert_eq!(GripState::default().look(Grip::Rest, true), Grip::Hover);
    }

    /// The ink lead is silent through `FADE_LEAD`, whole at the end, and
    /// continuous and monotonic between: a retargeted motion never makes a
    /// glyph flicker.
    #[test]
    fn the_ink_lead_is_continuous_and_monotonic() {
        let l = crate::tokens::motion::FADE_LEAD;
        assert_eq!(lead(0.0), 0.0);
        assert_eq!(lead(l), 0.0);
        assert_eq!(lead(1.0), 1.0);
        assert_eq!(lead(f32::NAN), 0.0);
        let mut prev = 0.0;
        for i in 0..=1000 {
            let a = lead(i as f32 / 1000.0);
            assert!(a >= prev, "monotonic at {i}");
            assert!(a - prev < 0.01, "continuous at {i}: {prev} -> {a}");
            prev = a;
        }
    }

    /// A compressed shell keeps a quiet ground, a gone one none, an open one
    /// all of it; the grip's ink follows presence alone.
    #[test]
    fn a_compressed_ground_is_quiet_and_a_gone_one_is_nothing() {
        assert_eq!(ShellFrame::COMPRESSED.ground_alpha(), bar::GRIP_GROUND);
        assert_eq!(ShellFrame::GONE.ground_alpha(), 0.0);
        assert_eq!(ShellFrame::FULL.ground_alpha(), 1.0);
        assert_eq!(ShellFrame::REVEALED.ground_alpha(), 1.0);
        assert_eq!(ShellFrame::COMPRESSED.core_alpha(), 0.0);
        assert_eq!(ShellFrame::COMPRESSED.grip_alpha(), 1.0);
        assert_eq!(ShellFrame::FULL.revealed_alpha(), 0.0);
        assert_eq!(ShellFrame::REVEALED.revealed_alpha(), 1.0);
    }
}
