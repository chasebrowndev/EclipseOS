// SPDX-License-Identifier: AGPL-3.0-only
//! The greeter's one primitive: a pane of ground under its content.
//!
//! Why a widget and not a styled container: a pane's ground is drawn by the
//! compositor, under the surface, so the client has to push the pane's box at
//! draw time, track hover and press across events, and publish a message on a
//! release inside the box. A styled container can do none of those. Without
//! glass the same widget paints its own opaque ground, so the screen reads the
//! same either way.

use iced::advanced::image::{Image, Renderer as _};
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, Clipboard, Renderer as _, Shell, Widget};
use iced::widget::image::FilterMethod;
use iced::{mouse, Background, Border, Element, Event, Length, Rectangle, Shadow, Size, Theme, Vector};

use crate::backdrop::Backdrop;
use crate::glass::{self, Pane, Tone, HOVERED, PRESSED};
use crate::tokens::{self, to_iced};
use ec_theme::Palette;

type Renderer = iced::Renderer;

/// Per-widget state: where the pointer is and whether a press is in progress.
#[derive(Default)]
struct Status {
    hover: bool,
    press: bool,
}

/// A pane of ground behind `content`. Not interactive by default; `on_press`
/// makes it a control that publishes once per press and release inside it.
pub struct Ground<'a, M> {
    content: Element<'a, M, Theme>,
    pal: Palette,
    radius: f32,
    tone: Tone,
    fade: f32,
    shadow: bool,
    frost: Option<Backdrop>,
    interactive: bool,
    on_press: Option<M>,
}

impl<'a, M: Clone + 'a> Ground<'a, M> {
    pub fn new(content: impl Into<Element<'a, M, Theme>>, pal: &Palette) -> Self {
        Ground {
            content: content.into(),
            pal: *pal,
            radius: tokens::CARD_RADIUS,
            tone: Tone::Plain,
            fade: 1.0,
            shadow: false,
            frost: None,
            interactive: false,
            on_press: None,
        }
    }

    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    /// How far the pane has appeared: 0 hidden, 1 in place.
    pub fn fade(mut self, fade: f32) -> Self {
        self.fade = fade.clamp(0.0, 1.0);
        self
    }

    /// A drop shadow for the fallback ground. Glass casts its own.
    pub fn shadow(mut self) -> Self {
        self.shadow = true;
        self
    }

    /// Without glass, show a blurred slice of the wallpaper behind this pane.
    pub fn frost(mut self, backdrop: Option<&Backdrop>) -> Self {
        self.frost = backdrop.cloned();
        self
    }

    pub fn on_press(mut self, message: M) -> Self {
        self.interactive = true;
        self.on_press = Some(message);
        self
    }

    /// The fill and rim this pane paints without glass, for its state.
    fn colours(&self, hover: bool, press: bool) -> (ec_theme::Rgba, ec_theme::Rgba) {
        let p = &self.pal;
        let focused = self.tone == Tone::Focused;
        let fill = match (focused, press, hover) {
            (true, true, _) => p.cell_focus_press,
            (true, false, true) => p.cell_focus_hover,
            (true, false, false) => p.cell_focus,
            (false, true, _) => p.cell_press,
            (false, false, true) => p.cell_hover,
            (false, false, false) => p.cell,
        };
        let rim = match (focused, hover) {
            (true, true) => p.cell_focus_rim_hover,
            (true, false) => p.cell_focus_rim,
            (false, true) => p.cell_rim_hover,
            (false, false) => p.cell_rim,
        };
        (fill, rim)
    }

    fn corner(&self, bounds: Rectangle) -> f32 {
        self.radius.min(bounds.width.min(bounds.height) / 2.0).max(0.0)
    }
}

impl<'a, M: Clone + 'a> From<Ground<'a, M>> for Element<'a, M, Theme> {
    fn from(ground: Ground<'a, M>) -> Self {
        Element::new(ground)
    }
}

impl<M: Clone> Widget<M, Theme, Renderer> for Ground<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Status>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Status::default())
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

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let Some(child_tree) = tree.children.first_mut() else {
            return layout::Node::new(limits.max());
        };
        let child = self.content.as_widget_mut().layout(child_tree, renderer, limits);
        let size = child.size();
        layout::Node::with_children(size, vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if let (Some(child_tree), Some(child)) = (tree.children.first_mut(), layout.children().next()) {
            self.content
                .as_widget_mut()
                .operate(child_tree, child, renderer, operation);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let inside = cursor.is_over(layout.bounds());
        let status = tree.state.downcast_mut::<Status>();
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Mouse(mouse::Event::CursorEntered)
            | Event::Mouse(mouse::Event::CursorLeft) => {
                if status.hover != inside {
                    status.hover = inside;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) if inside && self.interactive => {
                status.press = true;
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if status.press => {
                status.press = false;
                shell.request_redraw();
                if inside {
                    if let Some(message) = self.on_press.clone() {
                        shell.publish(message);
                    }
                }
            }
            _ => {}
        }
        if let (Some(child_tree), Some(child)) = (tree.children.first_mut(), layout.children().next()) {
            self.content.as_widget_mut().update(
                child_tree, event, child, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.interactive && cursor.is_over(layout.bounds()) {
            return mouse::Interaction::Pointer;
        }
        match (tree.children.first(), layout.children().next()) {
            (Some(child_tree), Some(child)) => self
                .content
                .as_widget()
                .mouse_interaction(child_tree, child, cursor, viewport, renderer),
            _ => mouse::Interaction::None,
        }
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
        let bounds = layout.bounds();
        let status = tree.state.downcast_ref::<Status>();
        // Only a control shows hover and press; a plain pane has no state.
        let hover = status.hover && self.interactive;
        let press = status.press && self.interactive;
        let radius = self.corner(bounds);

        if glass::available() {
            let mut state = 0;
            if hover {
                state |= HOVERED;
            }
            if press {
                state |= PRESSED;
            }
            let alpha = (self.fade * 255.0).round().clamp(0.0, 255.0) as u8;
            glass::push(Pane {
                rect: bounds,
                radius,
                tone: self.tone,
                state,
                alpha,
            });
        } else if self.fade > 0.0 {
            let (fill, rim) = self.colours(hover, press);
            let mut fill = to_iced(fill);
            fill.a *= self.fade;
            let mut rim = to_iced(rim);
            rim.a *= self.fade;
            let shadow = if !self.shadow {
                Shadow::default()
            } else if self.radius <= tokens::CELL_RADIUS {
                Shadow {
                    color: to_iced(self.pal.cell_shadow),
                    offset: Vector::new(0.0, tokens::CELL_SHADOW_Y),
                    blur_radius: tokens::CELL_SHADOW_BLUR,
                }
            } else {
                Shadow {
                    color: to_iced(self.pal.cell_shadow),
                    offset: Vector::new(0.0, tokens::CARD_SHADOW_Y),
                    blur_radius: tokens::CARD_SHADOW_BLUR,
                }
            };
            let ink = |a: f32| to_iced(self.pal.text.with_alpha(a * self.fade));
            if let (Some(b), true) = (&self.frost, self.shadow) {
                // A card. The renderer draws a layer's quads, then its images,
                // then its text, so each step that must sit over the last is
                // its own layer: the shadow, the blurred slice of the
                // wallpaper, then the veil, the rim and the content.
                let reach = tokens::CARD_SHADOW_BLUR + tokens::CARD_SHADOW_Y;
                let outer = Rectangle {
                    x: bounds.x - reach,
                    y: bounds.y - reach,
                    width: bounds.width + 2.0 * reach,
                    height: bounds.height + 2.0 * reach,
                };
                let round = Border {
                    radius: radius.into(),
                    ..Border::default()
                };
                renderer.with_layer(outer, |r| {
                    r.fill_quad(
                        Quad {
                            bounds,
                            border: round,
                            shadow,
                            snap: true,
                        },
                        Background::Color(to_iced(self.pal.base.with_alpha(self.fade))),
                    );
                });
                renderer.with_layer(bounds, |r| {
                    let mut image = Image::new(b.frost.clone())
                        .filter_method(FilterMethod::Linear)
                        .opacity(self.fade);
                    image.border_radius = radius.into();
                    r.draw_image(image, b.cover(*viewport), bounds);
                });
                let (veil, edge) = tokens::backdrop::CARD;
                renderer.with_layer(bounds, |r| {
                    r.fill_quad(
                        Quad {
                            bounds,
                            border: round,
                            shadow: Shadow::default(),
                            snap: true,
                        },
                        Background::Color(to_iced(
                            self.pal.base.with_alpha(tokens::backdrop::CARD_BACK * self.fade),
                        )),
                    );
                    r.fill_quad(
                        Quad {
                            bounds,
                            border: Border {
                                color: ink(edge),
                                width: tokens::RIM,
                                radius: radius.into(),
                            },
                            shadow: Shadow::default(),
                            snap: true,
                        },
                        Background::Color(ink(veil)),
                    );
                    if let (Some(child_tree), Some(child)) = (tree.children.first(), layout.children().next())
                    {
                        self.content
                            .as_widget()
                            .draw(child_tree, r, theme, style, child, cursor, viewport);
                    }
                });
                return;
            } else if self.frost.is_some() {
                // A cell on a card: ink lifts the frost beneath it.
                use tokens::backdrop as k;
                let focused = self.tone == Tone::Focused;
                let (a, e) = match (focused, press, hover) {
                    (true, true, _) => k::FOCUS_PRESS,
                    (true, false, true) => k::FOCUS_HOVER,
                    (true, false, false) => k::FOCUS,
                    (false, true, _) => k::CELL_PRESS,
                    (false, false, true) => k::CELL_HOVER,
                    (false, false, false) => k::CELL,
                };
                renderer.fill_quad(
                    Quad {
                        bounds,
                        border: Border {
                            color: ink(e),
                            width: tokens::RIM,
                            radius: radius.into(),
                        },
                        shadow: Shadow::default(),
                        snap: true,
                    },
                    Background::Color(ink(a)),
                );
            } else {
                renderer.fill_quad(
                    Quad {
                        bounds,
                        border: Border {
                            color: rim,
                            width: tokens::RIM,
                            radius: radius.into(),
                        },
                        shadow,
                        snap: true,
                    },
                    Background::Color(fill),
                );
            }
        }

        if let (Some(child_tree), Some(child)) = (tree.children.first(), layout.children().next()) {
            self.content
                .as_widget()
                .draw(child_tree, renderer, theme, style, child, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        let child_layout = layout.children().next()?;
        let child_tree = tree.children.first_mut()?;
        self.content
            .as_widget_mut()
            .overlay(child_tree, child_layout, renderer, viewport, translation)
    }
}

/// A plain colour swatch of a given box: a dot, a rail, a caret, an accent.
pub fn swatch<'a, M: 'a>(
    colour: ec_theme::Rgba,
    radius: f32,
    width: Length,
    height: Length,
) -> Element<'a, M, Theme> {
    iced::widget::container(iced::widget::Space::new().width(width).height(height))
        .width(width)
        .height(height)
        .style(move |_| iced::widget::container::Style {
            background: Some(Background::Color(to_iced(colour))),
            border: Border {
                radius: radius.into(),
                ..Border::default()
            },
            ..iced::widget::container::Style::default()
        })
        .into()
}
