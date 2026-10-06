// SPDX-License-Identifier: AGPL-3.0-only
//! The arrangement canvas: a spatial hero (COMPOSITION.md) in which each tile
//! is a rectangle at its real place in a layout, and a tile can be picked up
//! and dropped somewhere else.
//!
//! A widget and not a styled container because it is a gesture and a
//! coordinate system: it owns the press, the travel and the release, it maps
//! logical space to pixels and back, and while a tile is held it lays that
//! tile out at the place [`snap`] would drop it — the live ghost — leaving an
//! outline where it started. Nothing is published per mouse-move; the caller
//! hears about a press (select) and about the drop (one message, with the
//! snapped logical position), and never about the travel in between.
//!
//! What a tile *looks* like is the caller's: each tile carries its own
//! content element, laid out at the size its rectangle has on the canvas.
//! The widget adds only the hover light and the outline left behind.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{border, mouse, touch, Color, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use crate::tokens::{canvas, color, motion, radius};
use crate::widget::arrangement_geometry::{fit_layout, hit_test, snap, Rect, Viewport};

/// What a drop reports: the tile's id and its snapped logical `(x, y)`.
type OnDrop<'a, Message> = Box<dyn Fn(u64, i64, i64) -> Message + 'a>;

/// One rectangle on the canvas.
pub struct Tile<'a, Message> {
    pub id: u64,
    /// Where it is, in logical space.
    pub rect: Rect,
    pub content: Element<'a, Message, Theme>,
}

pub struct Arrangement<'a, Message> {
    tiles: Vec<Tile<'a, Message>>,
    on_select: Option<Box<dyn Fn(u64) -> Message + 'a>>,
    on_drop: Option<OnDrop<'a, Message>>,
}

pub fn arrangement<'a, Message>(tiles: Vec<Tile<'a, Message>>) -> Arrangement<'a, Message> {
    Arrangement {
        tiles,
        on_select: None,
        on_drop: None,
    }
}

impl<'a, Message> Arrangement<'a, Message> {
    /// A tile was pressed (whether or not it then moves).
    pub fn on_select(mut self, f: impl Fn(u64) -> Message + 'a) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    /// A tile was dragged and let go somewhere new: its id and its snapped
    /// logical position.
    pub fn on_drop(mut self, f: impl Fn(u64, i64, i64) -> Message + 'a) -> Self {
        self.on_drop = Some(Box::new(f));
        self
    }

    fn rects(&self) -> Vec<Rect> {
        self.tiles.iter().map(|t| t.rect).collect()
    }

    fn viewport(&self, size: Size) -> Viewport {
        fit_layout(
            &self.rects(),
            f64::from(size.width),
            f64::from(size.height),
            f64::from(canvas::ARRANGE_PAD),
            canvas::ARRANGE_SLACK,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct Press {
    index: usize,
    /// Where on the tile it was taken, in canvas pixels.
    grab: Vector,
    origin: Point,
    dragging: bool,
    /// Where it would land if released now, in logical space.
    target: Rect,
    finger: Option<touch::Finger>,
}

#[derive(Default)]
struct State {
    press: Option<Press>,
    hover: Option<usize>,
}

impl<Message> Arrangement<'_, Message> {
    fn travel(&self, st: &mut State, bounds: Rectangle, p: Point) {
        let Some(press) = st.press.as_mut() else { return };
        if !press.dragging && press.origin.distance(p) > motion::TAP_SLOP {
            press.dragging = true;
        }
        if !press.dragging {
            return;
        }
        let vp = self.viewport(bounds.size());
        let tile = &self.tiles[press.index];
        let (lx, ly) = vp.to_logical(
            f64::from(p.x - bounds.x - press.grab.x),
            f64::from(p.y - bounds.y - press.grab.y),
        );
        let want = tile.rect.at(lx.round() as i64, ly.round() as i64);
        let others: Vec<Rect> = self
            .tiles
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != press.index)
            .map(|(_, t)| t.rect)
            .collect();
        let threshold = (canvas::ARRANGE_SNAP_PX / vp.scale).round() as i64;
        let (x, y) = snap(want, &others, threshold);
        press.target = tile.rect.at(x, y);
    }
}

impl<Message: Clone> Widget<Message, Theme, iced::Renderer> for Arrangement<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.tiles.iter().map(|t| Tree::new(&t.content)).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children_custom(
            &self.tiles,
            |tree, tile| tree.diff(&tile.content),
            |tile| Tree::new(&tile.content),
        );
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(canvas::ARRANGE_H))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fixed(canvas::ARRANGE_H), Size::ZERO);
        let vp = self.viewport(size);
        let held = tree.state.downcast_ref::<State>().press.filter(|p| p.dragging);
        let mut nodes = Vec::with_capacity(self.tiles.len());
        for (i, tile) in self.tiles.iter_mut().enumerate() {
            let rect = match held {
                Some(p) if p.index == i => p.target,
                _ => tile.rect,
            };
            let (x, y, w, h) = vp.rect_to_screen(rect);
            let tile_size = Size::new(w as f32, h as f32);
            let node = tile
                .content
                .as_widget_mut()
                .layout(
                    &mut tree.children[i],
                    renderer,
                    &layout::Limits::new(tile_size, tile_size),
                )
                .move_to(Point::new(x as f32, y as f32));
            nodes.push(node);
        }
        layout::Node::with_children(size, nodes)
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
        let here = |fallback: Point| cursor.land().position().unwrap_or(fallback);
        let st = tree.state.downcast_mut::<State>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if st.press.is_none() => {
                if let Some(p) = cursor.position_over(bounds) {
                    self.begin(st, bounds, p, None, shell);
                }
            }
            Event::Touch(touch::Event::FingerPressed { id, position }) if st.press.is_none() => {
                let p = here(*position);
                if bounds.contains(p) {
                    self.begin(st, bounds, p, Some(*id), shell);
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let p = here(*position);
                if st.press.is_some_and(|p| p.finger.is_none()) {
                    self.travel(st, bounds, p);
                    shell.invalidate_layout();
                    shell.request_redraw();
                    shell.capture_event();
                } else {
                    let vp = self.viewport(bounds.size());
                    let hover = bounds
                        .contains(p)
                        .then(|| {
                            hit_test(
                                &self.rects(),
                                vp,
                                f64::from(p.x - bounds.x),
                                f64::from(p.y - bounds.y),
                            )
                        })
                        .flatten();
                    if hover != st.hover {
                        st.hover = hover;
                        shell.request_redraw();
                    }
                }
            }
            Event::Touch(touch::Event::FingerMoved { id, position })
                if st.press.is_some_and(|p| p.finger == Some(*id)) =>
            {
                self.travel(st, bounds, here(*position));
                shell.invalidate_layout();
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if st.press.is_some_and(|p| p.finger.is_none()) =>
            {
                self.end(st, shell);
            }
            Event::Touch(touch::Event::FingerLifted { id, .. })
                if st.press.is_some_and(|p| p.finger == Some(*id)) =>
            {
                self.end(st, shell);
            }
            Event::Touch(touch::Event::FingerLost { id, .. })
                if st.press.is_some_and(|p| p.finger == Some(*id)) =>
            {
                st.press = None;
                shell.invalidate_layout();
                shell.request_redraw();
            }
            _ => {}
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
        let st = tree.state.downcast_ref::<State>();
        if st.press.is_some_and(|p| p.dragging) {
            mouse::Interaction::Grabbing
        } else if layout.children().any(|c| cursor.is_over(c.bounds())) {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::None
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
        use iced::advanced::Renderer as _;
        let bounds = layout.bounds();
        let st = tree.state.downcast_ref::<State>();
        let held = st.press.filter(|p| p.dragging).map(|p| p.index);
        let vp = self.viewport(bounds.size());

        // Where the held tile started: an outline, so it is plain what moved.
        if let Some(i) = held {
            let (x, y, w, h) = vp.rect_to_screen(self.tiles[i].rect);
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle::new(
                        Point::new(bounds.x + x as f32, bounds.y + y as f32),
                        Size::new(w as f32, h as f32),
                    ),
                    border: border::rounded(radius::INSET)
                        .color(color::BORDER)
                        .width(canvas::ARRANGE_RIM),
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
            );
        }

        let mut order: Vec<(usize, Layout<'_>)> = layout.children().enumerate().collect();
        // The held tile last, so it is above its neighbours.
        order.sort_by_key(|(i, _)| Some(*i) == held);
        for (i, child) in order {
            self.tiles[i].content.as_widget().draw(
                &tree.children[i],
                renderer,
                theme,
                style,
                child,
                cursor,
                viewport,
            );
            if held.is_none() && st.hover == Some(i) {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: child.bounds(),
                        border: border::rounded(radius::INSET),
                        ..renderer::Quad::default()
                    },
                    color::LIFT_SOFT,
                );
            }
        }
    }
}

impl<Message: Clone> Arrangement<'_, Message> {
    fn begin(
        &self,
        st: &mut State,
        bounds: Rectangle,
        p: Point,
        finger: Option<touch::Finger>,
        shell: &mut Shell<'_, Message>,
    ) {
        let vp = self.viewport(bounds.size());
        let rel = Point::new(p.x - bounds.x, p.y - bounds.y);
        let Some(index) = hit_test(&self.rects(), vp, f64::from(rel.x), f64::from(rel.y)) else {
            return;
        };
        let (x, y, ..) = vp.rect_to_screen(self.tiles[index].rect);
        st.press = Some(Press {
            index,
            grab: Vector::new(rel.x - x as f32, rel.y - y as f32),
            origin: p,
            dragging: false,
            target: self.tiles[index].rect,
            finger,
        });
        if let Some(f) = &self.on_select {
            shell.publish(f(self.tiles[index].id));
        }
        shell.capture_event();
    }

    fn end(&self, st: &mut State, shell: &mut Shell<'_, Message>) {
        let Some(press) = st.press.take() else { return };
        let moved = self.tiles[press.index].rect;
        if press.dragging && (press.target.x, press.target.y) != (moved.x, moved.y) {
            if let Some(f) = &self.on_drop {
                shell.publish(f(self.tiles[press.index].id, press.target.x, press.target.y));
            }
        }
        shell.invalidate_layout();
        shell.request_redraw();
        shell.capture_event();
    }
}

impl<'a, Message: Clone + 'a> From<Arrangement<'a, Message>> for Element<'a, Message, Theme> {
    fn from(a: Arrangement<'a, Message>) -> Self {
        Element::new(a)
    }
}
