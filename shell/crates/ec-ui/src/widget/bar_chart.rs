// SPDX-License-Identifier: AGPL-3.0-only
//! A flat history bar chart — the 24h charge histogram, a per-second CPU
//! trace, notification counts by hour.
//!
//! Flat rectangles with a 2.5px top radius, history in translucent white and
//! the accent spent on exactly one bar: the current sample, or the peak. That
//! rule is why this is a widget and not a row of styled containers — "which
//! one bar is yellow" is a property of the series, not of any one bar.

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Tree, Widget};
use iced::{Border, Color, Element, Length, Rectangle, Size};

use crate::tokens::{color, radius};

/// Which bar, if any, gets the accent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    /// The last sample — a live reading.
    Current,
    /// The largest sample — a record.
    Peak,
    /// None. Use this when the pane already spends its yellow elsewhere.
    None,
}

/// Bars are drawn from `values`, each clamped into `0.0..=1.0` against `max`.
pub struct BarChart {
    values: Vec<f32>,
    max: f32,
    height: f32,
    gap: f32,
    highlight: Highlight,
}

impl BarChart {
    pub fn new(values: impl IntoIterator<Item = f32>) -> Self {
        let values: Vec<f32> = values.into_iter().collect();
        // An all-zero series still has to draw as a flat floor rather than
        // dividing by zero into NaN widths.
        let max = values.iter().copied().fold(0.0f32, f32::max).max(f32::EPSILON);
        Self {
            values,
            max,
            height: 58.0,
            gap: 3.0,
            highlight: Highlight::Current,
        }
    }

    /// Pin the top of the scale, so a chart that is read against a known
    /// ceiling (percent, a link rate) does not rescale as samples arrive.
    pub fn max(mut self, max: f32) -> Self {
        self.max = max.max(f32::EPSILON);
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub fn highlight(mut self, highlight: Highlight) -> Self {
        self.highlight = highlight;
        self
    }

    fn accented(&self) -> Option<usize> {
        match self.highlight {
            Highlight::None => None,
            Highlight::Current => self.values.len().checked_sub(1),
            Highlight::Peak => self
                .values
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(i, _)| i),
        }
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for BarChart
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::stateless()
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(self.height))
    }

    fn layout(&mut self, _tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let width = limits.max().width;
        layout::Node::new(Size::new(width, self.height))
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        if self.values.is_empty() || bounds.width <= 0.0 {
            return;
        }

        let n = self.values.len() as f32;
        let slot = bounds.width / n;
        let bar_w = (slot - self.gap).max(1.0);
        let accented = self.accented();

        for (i, value) in self.values.iter().enumerate() {
            let frac = (value / self.max).clamp(0.0, 1.0);
            // A zero sample still draws a 1px floor, so a gap in the history
            // reads as "nothing happened" rather than as missing data.
            let h = (frac * bounds.height).max(1.0);
            let rect = Rectangle {
                x: bounds.x + i as f32 * slot,
                y: bounds.y + bounds.height - h,
                width: bar_w,
                height: h,
            };
            if !rect.intersects(viewport) {
                continue;
            }

            let r = radius::BAR.min(h / 2.0);
            renderer.fill_quad(
                renderer::Quad {
                    bounds: rect,
                    border: Border {
                        radius: iced::border::Radius::default().top(r),
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                if Some(i) == accented {
                    color::ACCENT
                } else {
                    Color {
                        a: 0.13,
                        ..Color::WHITE
                    }
                },
            );
        }
    }
}

impl<'a, Message, Theme, Renderer> From<BarChart> for Element<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer + 'a,
{
    fn from(chart: BarChart) -> Self {
        Element::new(chart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_bar_is_ever_accented() {
        let v = [0.2, 0.9, 0.4];
        assert_eq!(BarChart::new(v).highlight(Highlight::Current).accented(), Some(2));
        assert_eq!(BarChart::new(v).highlight(Highlight::Peak).accented(), Some(1));
        assert_eq!(BarChart::new(v).highlight(Highlight::None).accented(), None);
    }

    #[test]
    fn an_empty_or_flat_series_does_not_divide_by_zero() {
        assert_eq!(BarChart::new([]).accented(), None);
        let flat = BarChart::new([0.0, 0.0]);
        assert!(flat.max > 0.0);
    }
}
