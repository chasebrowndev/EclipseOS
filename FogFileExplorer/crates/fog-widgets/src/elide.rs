// SPDX-License-Identifier: AGPL-3.0-only

//! One line of text that ends in `…` when it does not fit.
//!
//! A widget and not a styled `text` because iced 0.14 cannot elide: a
//! `text` that is too wide is either wrapped or cut mid-glyph by its
//! container's clip, and a file name cut mid-glyph reads as a different
//! name. Where the cut goes depends on the shaped width of a proportional
//! face, which only exists at layout time, so it has to be decided there,
//! against the paragraph the renderer will draw.
//!
//! Cost: a line that fits is one paragraph, like `text`. A line that does
//! not is shaped a few more times (a binary search over its length), once
//! per change of its content or of the width it is given, not per frame.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self, paragraph::Plain, Paragraph as _};
use iced::advanced::widget::{tree, Tree, Widget};
use iced::alignment;
use iced::mouse;
use iced::{Color, Element, Length, Pixels, Rectangle, Size};

const ELLIPSIS: char = '…';

pub struct Elide<'a, Renderer: text::Renderer> {
    content: std::borrow::Cow<'a, str>,
    size: Option<Pixels>,
    font: Option<Renderer::Font>,
    color: Option<Color>,
    width: Length,
}

/// `content` on one line, elided with `…` to the width it is laid out in.
pub fn elide<'a, Renderer: text::Renderer>(
    content: impl Into<std::borrow::Cow<'a, str>>,
) -> Elide<'a, Renderer> {
    Elide {
        content: content.into(),
        size: None,
        font: None,
        color: None,
        width: Length::Fill,
    }
}

impl<Renderer: text::Renderer> Elide<'_, Renderer> {
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.font = Some(font.into());
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

struct State<P: text::Paragraph> {
    /// The content and width the shown text was fitted to.
    fitted: Option<(String, f32)>,
    shown: String,
    paragraph: Plain<P>,
}

/// The longest prefix of `full`, trailing spaces dropped, that fits in `max`
/// with `…` after it; `…` alone if nothing does. `width` measures a line.
pub fn fit(full: &str, max: f32, mut width: impl FnMut(&str) -> f32) -> String {
    if width(full) <= max {
        return full.to_owned();
    }
    let ends: Vec<usize> = full
        .char_indices()
        .map(|(i, _)| i)
        .skip(1)
        .chain(std::iter::once(full.len()))
        .collect();
    let cut = |n: usize| {
        let mut s = full[..ends[n]].trim_end().to_owned();
        s.push(ELLIPSIS);
        s
    };
    // The largest n whose cut fits: widths grow with n.
    let (mut lo, mut hi) = (0usize, ends.len() - 1);
    let mut best = None;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let s = cut(mid);
        if width(&s) <= max {
            best = Some(s);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    best.unwrap_or_else(|| ELLIPSIS.to_string())
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Elide<'_, Renderer>
where
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer::Paragraph> {
            fitted: None,
            shown: String::new(),
            paragraph: Plain::default(),
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let st = tree.state.downcast_mut::<State<Renderer::Paragraph>>();
        let size = self.size.unwrap_or_else(|| renderer.default_size());
        let font = self.font.unwrap_or_else(|| renderer.default_font());
        layout::sized(limits, self.width, Length::Shrink, |limits| {
            let max = limits.max();
            fn line<F>(content: &str, bounds: Size, size: Pixels, font: F) -> text::Text<&str, F> {
                text::Text {
                    content,
                    bounds,
                    size,
                    line_height: text::LineHeight::default(),
                    font,
                    align_x: text::Alignment::Default,
                    align_y: alignment::Vertical::Top,
                    shaping: text::Shaping::Advanced,
                    wrapping: text::Wrapping::None,
                }
            }
            let bounds = Size::new(f32::INFINITY, max.height);
            let key = (self.content.as_ref(), max.width);
            if st.fitted.as_ref().map(|(c, w)| (c.as_str(), *w)) != Some(key) {
                st.shown = fit(&self.content, max.width, |s| {
                    Renderer::Paragraph::with_text(line(s, bounds, size, font)).min_width()
                });
                st.fitted = Some((self.content.clone().into_owned(), max.width));
            }
            st.paragraph.update(line(&st.shown, bounds, size, font));
            st.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let st = tree.state.downcast_ref::<State<Renderer::Paragraph>>();
        renderer.fill_paragraph(
            st.paragraph.raw(),
            layout.bounds().position(),
            self.color.unwrap_or(style.text_color),
            *viewport,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Elide<'a, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer + 'a,
{
    fn from(e: Elide<'a, Renderer>) -> Self {
        Element::new(e)
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    /// Every char one unit wide.
    fn w(s: &str) -> f32 {
        s.chars().count() as f32
    }

    #[test]
    fn a_line_that_fits_is_kept_whole() {
        assert_eq!(fit("notes.md", 8.0, w), "notes.md");
    }

    #[test]
    fn a_long_line_ends_in_an_ellipsis_within_the_width() {
        let s = fit("Quarterly report.pdf", 10.0, w);
        assert_eq!(s, "Quarterly…");
        assert!(w(&s) <= 10.0);
        // A cut never leaves a space before the ellipsis.
        assert_eq!(fit("ab cd", 4.0, w), "ab…");
    }

    #[test]
    fn multibyte_names_are_cut_on_char_boundaries() {
        assert_eq!(fit("überlänge", 4.0, w), "übe…");
        assert_eq!(fit("文件名很长", 3.0, w), "文件…");
    }

    #[test]
    fn nothing_fits_but_the_ellipsis() {
        assert_eq!(fit("abc", 0.5, w), "…");
    }
}
