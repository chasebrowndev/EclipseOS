// SPDX-License-Identifier: AGPL-3.0-only
//! A lead and a trail that sit side by side when there is room, and fold one
//! above the other when there is not.
//!
//! iced's `responsive` hands its closure the width, but it rebuilds the
//! children from scratch on every layout, so it cannot take an element the
//! caller has already built — and every settings row is exactly that: a label
//! and a control handed in whole. `Row` cannot do it either: it never
//! stacks, it squeezes, which is how a toggle ended up hanging past the edge
//! of its card. So the decision is made here, at layout, from the two
//! children's own natural sizes.
//!
//! [`Fold::halves`] is the same decision for two equal columns: side by side
//! while each half can hold `min` pixels, one above the other once it cannot.

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{Operation, Tree, Widget};
use iced::advanced::{mouse, overlay, renderer, Clipboard, Shell};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

pub struct Fold<'a, Message, Theme, Renderer> {
    children: [Element<'a, Message, Theme, Renderer>; 2],
    gap_x: f32,
    gap_y: f32,
    /// `Some(min)` for two equal halves instead of a lead and a trail.
    halves: Option<f32>,
}

impl<'a, Message, Theme, Renderer> Fold<'a, Message, Theme, Renderer> {
    pub fn new(
        lead: impl Into<Element<'a, Message, Theme, Renderer>>,
        trail: impl Into<Element<'a, Message, Theme, Renderer>>,
        gap_x: f32,
        gap_y: f32,
    ) -> Self {
        Self {
            children: [lead.into(), trail.into()],
            gap_x,
            gap_y,
            halves: None,
        }
    }

    /// Two equal columns that stack once either would be narrower than `min`.
    pub fn halves(
        left: impl Into<Element<'a, Message, Theme, Renderer>>,
        right: impl Into<Element<'a, Message, Theme, Renderer>>,
        gap_x: f32,
        gap_y: f32,
        min: f32,
    ) -> Self {
        Self {
            halves: Some(min),
            ..Self::new(left, right, gap_x, gap_y)
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Fold<'_, Message, Theme, Renderer>
where
    Renderer: iced::advanced::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let max_w = limits.max().width;
        let [lead, trail] = &mut self.children;
        let [lead_tree, trail_tree] = &mut tree.children[..] else {
            unreachable!("a fold has two children")
        };

        if let Some(min) = self.halves {
            let half = (max_w - self.gap_x) / 2.0;
            let side = half.is_finite() && half >= min;
            let w = if side { half } else { max_w };
            let within = layout::Limits::new(Size::ZERO, Size::new(w, f32::INFINITY)).width(Length::Fixed(w));
            let a = lead.as_widget_mut().layout(lead_tree, renderer, &within);
            let b = trail.as_widget_mut().layout(trail_tree, renderer, &within);
            let (sa, sb) = (a.size(), b.size());
            return if side {
                layout::Node::with_children(
                    Size::new(max_w, sa.height.max(sb.height)),
                    vec![a, b.move_to((half + self.gap_x, 0.0))],
                )
            } else {
                layout::Node::with_children(
                    Size::new(max_w, sa.height + self.gap_y + sb.height),
                    vec![a, b.move_to((0.0, sa.height + self.gap_y))],
                )
            };
        }

        // The lead's natural width is its one-line width: measured unbounded,
        // so a label that would wrap beside the control reads as "no room".
        let unbounded = layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, f32::INFINITY));
        let within = layout::Limits::new(Size::ZERO, Size::new(max_w, f32::INFINITY));
        let lead_node = lead.as_widget_mut().layout(lead_tree, renderer, &unbounded);
        let natural = lead_node.size();
        let trail_node = trail.as_widget_mut().layout(trail_tree, renderer, &within);
        let t = trail_node.size();

        let side = natural.width.is_finite() && natural.width + self.gap_x + t.width <= max_w;
        if side || !max_w.is_finite() {
            let l = natural;
            let h = l.height.max(t.height);
            let w = if max_w.is_finite() {
                max_w
            } else {
                l.width + self.gap_x + t.width
            };
            let lead_node = lead_node.move_to((0.0, (h - l.height) / 2.0));
            let trail_node = trail_node.move_to((w - t.width, (h - t.height) / 2.0));
            return layout::Node::with_children(Size::new(w, h), vec![lead_node, trail_node]);
        }

        let lead_node = lead.as_widget_mut().layout(lead_tree, renderer, &within);
        let l = lead_node.size();
        let trail_node = trail_node.move_to((0.0, l.height + self.gap_y));
        layout::Node::with_children(
            Size::new(max_w, l.height + self.gap_y + t.height),
            vec![lead_node, trail_node],
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child.as_widget_mut().operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child
                .as_widget_mut()
                .update(state, event, layout, cursor, renderer, clipboard, shell, viewport);
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
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self.children.iter().zip(&tree.children).zip(layout.children()) {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(&mut self.children, tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message, Theme, Renderer> From<Fold<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    fn from(fold: Fold<'a, Message, Theme, Renderer>) -> Self {
        Element::new(fold)
    }
}
