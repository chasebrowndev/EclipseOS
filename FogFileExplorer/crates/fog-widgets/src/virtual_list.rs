// SPDX-License-Identifier: AGPL-3.0-only

//! A virtualized, fixed-row-height list (FOG §Performance model, item 6).
//!
//! # Why this exists (the spec's open question)
//!
//! *Does the pinned iced have a virtual list that handles 100k rows?* No.
//! Checked against the `iced_widget` 0.14.2 source:
//!
//! - `scrollable` lays out and diffs its whole content. A `column` of 100k
//!   rows builds 100k elements in `view`, diffs 100k tree nodes and lays out
//!   100k children on every rebuild; only drawing is culled to the viewport.
//! - `lazy` caches a subtree keyed by a hash of its dependencies. It skips
//!   rebuilding an *unchanged* subtree, but the cached subtree still holds
//!   every row, so the first build and every change (a sort, a diff from
//!   `fogd`, a selection move) cost O(n).
//! - `responsive` hands its closure the available size during layout, which
//!   is half of what a virtual list needs; it knows nothing about the scroll
//!   offset.
//!
//! So `fog-widgets` ships its own. It is a thin custom [`Widget`] wrapped
//! around iced's own `scrollable`, which keeps the scrollbar, wheel, drag,
//! kinetic and `scroll_to` behaviour identical to every other iced list:
//!
//! - Rows are materialized in **`layout`**, like `responsive` does: the
//!   viewport height comes from the layout limits, so the very first frame and
//!   every resize are correct without waiting for a scroll event.
//! - The scroll offset is read back from the inner scrollable after every
//!   event it handles (a one-node widget [`Operation`] reads its translation).
//!   When the visible rows leave the materialized window the widget
//!   invalidates layout, and iced re-lays-out *within the same event*, before
//!   drawing. There is no message round trip through the application, so a
//!   fast fling or a scrollbar drag never shows blank rows for a frame.
//! - The content is `[spacer, rows[start..end], spacer]` whose height is
//!   always `len * row_height`, so the scrollbar is exact.
//!
//! Cost per rebuild is O(visible rows + 2 × overscan), independent of `len`.
//! Materialized rows are re-diffed by position, so row widgets should not
//! keep interaction state that matters across scrolling (a hover highlight is
//! fine; an embedded text input is not). Rows are clipped to `row_height`.
//!
//! The list must be given a bounded height (the default `Fill` inside a
//! window is). Nested inside another vertical scrollable it would receive an
//! unbounded viewport and materialize every row.

use std::ops::Range;
use std::rc::Rc;

use iced::advanced::layout::{self, Layout};
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::text;
use iced::advanced::widget::operation::scrollable::{AbsoluteOffset, Scrollable as ScrollState};
use iced::advanced::widget::{self, tree, Operation, Tree};
use iced::advanced::{Clipboard, Shell, Widget};
use iced::widget::scrollable::{self, Viewport};
use iced::widget::{container, space, Column, Scrollable};
use iced::{Element, Event, Length, Rectangle, Size, Task, Vector};

/// Screens of rows materialized above and below the viewport by default.
pub const DEFAULT_OVERSCAN_SCREENS: f32 = 1.0;

/// The rows that must exist as widgets for a viewport at `offset_y` of height
/// `viewport_h`, plus `overscan_screens` viewports' worth above and below.
///
/// The offset is clamped exactly as a scrollable clamps it (to
/// `0..=len * row_height - viewport_h`), so an offset past the end yields the
/// last screen. Degenerate input (no rows, a non-positive row height, a zero
/// viewport) yields an empty range. An unbounded viewport yields every row.
pub fn visible_range(
    len: usize,
    row_height: f32,
    offset_y: f32,
    viewport_h: f32,
    overscan_screens: f32,
) -> Range<usize> {
    let positive = |x: f32| x.partial_cmp(&0.0) == Some(std::cmp::Ordering::Greater);
    if len == 0 || !positive(row_height) || !positive(viewport_h) {
        return 0..0;
    }

    let content_h = len as f32 * row_height;
    let max_offset = (content_h - viewport_h).max(0.0);
    let offset = if offset_y.is_nan() {
        0.0
    } else {
        offset_y.clamp(0.0, max_offset)
    };
    let overscan = if overscan_screens > 0.0 {
        viewport_h * overscan_screens
    } else {
        0.0
    };

    let top = (offset - overscan).max(0.0);
    let bottom = offset + viewport_h + overscan;

    // `as usize` saturates: +inf becomes usize::MAX, clamped by `len`.
    let end = ((bottom / row_height).ceil() as usize).min(len);
    let start = ((top / row_height).floor() as usize).min(end);
    start..end
}

/// The offset that brings row `index` fully into a viewport currently at
/// `offset_y` with height `viewport_h`, or `None` if it is already fully
/// visible.
///
/// Scrolls the minimum distance: a row above the viewport is aligned to the
/// top, a row below it to the bottom. A viewport shorter than a row aligns
/// the row's top.
pub fn reveal_offset(index: usize, row_height: f32, offset_y: f32, viewport_h: f32) -> Option<f32> {
    let top = index as f32 * row_height;
    let bottom = top + row_height;

    if top < offset_y || viewport_h < row_height {
        (top != offset_y).then_some(top)
    } else if bottom > offset_y + viewport_h {
        Some(bottom - viewport_h)
    } else {
        None
    }
}

/// A [`Task`] that scrolls the [`VirtualList`] with the given `id` so that row
/// `index` is fully visible, moving the minimum distance (see
/// [`reveal_offset`]).
///
/// The offset and viewport height are read from the live scrollable when the
/// operation runs, so the application does not have to track them. It is a
/// no-op when the row is already visible or no list has that id.
pub fn scroll_into_view<T>(id: impl Into<widget::Id>, index: usize, row_height: f32) -> Task<T>
where
    T: Send + 'static,
{
    struct ScrollIntoView {
        target: widget::Id,
        index: usize,
        row_height: f32,
    }

    impl<T> Operation<T> for ScrollIntoView {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            id: Option<&widget::Id>,
            bounds: Rectangle,
            _content_bounds: Rectangle,
            translation: Vector,
            state: &mut dyn ScrollState,
        ) {
            if Some(&self.target) != id {
                return;
            }
            if let Some(y) =
                reveal_offset(self.index, self.row_height, translation.y, bounds.height)
            {
                state.scroll_to(AbsoluteOffset {
                    x: None,
                    y: Some(y),
                });
            }
        }
    }

    widget::operate(ScrollIntoView {
        target: id.into(),
        index,
        row_height,
    })
}

/// Creates a [`VirtualList`] of `len` rows, each `row_height` logical pixels
/// tall, built on demand by `view_row(index)`.
pub fn virtual_list<'a, Message, Theme, Renderer>(
    len: usize,
    row_height: f32,
    view_row: impl Fn(usize) -> Element<'a, Message, Theme, Renderer> + 'a,
) -> VirtualList<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    VirtualList::new(len, row_height, view_row)
}

/// A vertically scrolling list that only creates widgets for the visible rows
/// plus overscan. See the [module docs](self) for how and why.
pub struct VirtualList<'a, Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    len: usize,
    row_height: f32,
    overscan_screens: f32,
    id: Option<widget::Id>,
    width: Length,
    height: Length,
    view_row: Box<dyn Fn(usize) -> Element<'a, Message, Theme, Renderer> + 'a>,
    on_scroll: Option<Rc<dyn Fn(Viewport) -> Message + 'a>>,
    content: Element<'a, Message, Theme, Renderer>,
}

impl<'a, Message, Theme, Renderer> VirtualList<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer,
{
    /// See [`virtual_list`].
    pub fn new(
        len: usize,
        row_height: f32,
        view_row: impl Fn(usize) -> Element<'a, Message, Theme, Renderer> + 'a,
    ) -> Self {
        Self {
            len,
            row_height,
            overscan_screens: DEFAULT_OVERSCAN_SCREENS,
            id: None,
            width: Length::Fill,
            height: Length::Fill,
            view_row: Box::new(view_row),
            on_scroll: None,
            content: Element::new(space()),
        }
    }

    /// Sets the id of the inner scrollable, for [`scroll_into_view`] and
    /// iced's own scroll operations.
    pub fn id(mut self, id: impl Into<widget::Id>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Screens of rows kept above and below the viewport. Default `1.0`.
    pub fn overscan(mut self, screens: f32) -> Self {
        self.overscan_screens = screens;
        self
    }

    /// Emits a message whenever the viewport moves. Not needed for rendering;
    /// the list tracks its own offset.
    pub fn on_scroll(mut self, f: impl Fn(Viewport) -> Message + 'a) -> Self {
        self.on_scroll = Some(Rc::new(f));
        self
    }

    /// Sets the width. Default [`Length::Fill`].
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the height. Default [`Length::Fill`]; must resolve to a bounded
    /// height.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }
}

impl<'a, Message, Theme, Renderer> VirtualList<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: scrollable::Catalog + container::Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn build(&self, range: Range<usize>) -> Element<'a, Message, Theme, Renderer> {
        let above = range.start as f32 * self.row_height;
        let below = (self.len - range.end) as f32 * self.row_height;

        let mut rows = Column::with_capacity(range.len() + 2)
            .width(Length::Fill)
            .push(space().height(above));
        for index in range {
            rows = rows.push(
                container((self.view_row)(index))
                    .width(Length::Fill)
                    .height(self.row_height)
                    .clip(true),
            );
        }
        rows = rows.push(space().height(below));

        // Embedded scrollbar: it takes layout width instead of floating over
        // the right-aligned columns of a file list.
        let mut list = Scrollable::new(rows)
            .spacing(0.0)
            .width(Length::Fill)
            .height(Length::Fill);
        if let Some(id) = &self.id {
            list = list.id(id.clone());
        }
        if let Some(on_scroll) = &self.on_scroll {
            let on_scroll = Rc::clone(on_scroll);
            list = list.on_scroll(move |viewport| on_scroll(viewport));
        }
        list.into()
    }
}

/// Persistent state: the last offset read back from the scrollable and the
/// rows currently materialized.
#[derive(Debug, Default)]
struct State {
    offset_y: f32,
    built: Range<usize>,
}

/// Reads the offset and viewport height of the first scrollable it meets
/// without descending into its rows.
#[derive(Default)]
struct Probe {
    found: Option<(f32, f32)>,
}

impl Operation for Probe {
    fn traverse(&mut self, _operate: &mut dyn FnMut(&mut dyn Operation)) {}

    fn scrollable(
        &mut self,
        _id: Option<&widget::Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn ScrollState,
    ) {
        if self.found.is_none() {
            self.found = Some((translation.y, bounds.height));
        }
    }
}

fn covers(built: &Range<usize>, needed: &Range<usize>) -> bool {
    needed.is_empty() || (built.start <= needed.start && needed.end <= built.end)
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for VirtualList<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: scrollable::Catalog + container::Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn diff(&self, _tree: &mut Tree) {
        // Deferred to layout, where the rows are built.
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = limits.width(self.width).height(self.height);
        let viewport_h = limits.max().height;

        let state = tree.state.downcast_mut::<State>();
        let range = visible_range(
            self.len,
            self.row_height,
            state.offset_y,
            viewport_h,
            self.overscan_screens,
        );
        state.built = range.clone();

        self.content = self.build(range);
        tree.diff_children(std::slice::from_ref(&self.content));

        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits);
        let size = limits.resolve(self.width, self.height, node.size());

        layout::Node::with_children(size, vec![node])
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
        let content_layout = layout.children().next().unwrap();
        let content = self.content.as_widget_mut();

        content.update(
            &mut tree.children[0],
            event,
            content_layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        // Covers wheel, drag, keyboard and `scroll_to` operations alike:
        // whatever moved the scrollable, read where it is now.
        let mut probe = Probe::default();
        content.operate(&mut tree.children[0], content_layout, renderer, &mut probe);

        if let Some((offset_y, viewport_h)) = probe.found {
            let state = tree.state.downcast_mut::<State>();
            state.offset_y = offset_y;

            let needed = visible_range(self.len, self.row_height, offset_y, viewport_h, 0.0);
            if !covers(&state.built, &needed) {
                shell.invalidate_layout();
                shell.request_redraw();
            }
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout.children().next().unwrap(),
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout.children().next().unwrap(),
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            operation,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().unwrap(),
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<VirtualList<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: scrollable::Catalog + container::Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(list: VirtualList<'a, Message, Theme, Renderer>) -> Self {
        Element::new(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::clipboard;
    use iced::mouse::ScrollDelta;
    use iced::Point;
    use std::cell::RefCell;

    type Counter = Rc<RefCell<Vec<usize>>>;

    const ROW: f32 = 20.0;

    #[test]
    fn empty_and_degenerate_inputs() {
        assert_eq!(visible_range(0, ROW, 0.0, 600.0, 1.0), 0..0);
        assert_eq!(visible_range(100, 0.0, 0.0, 600.0, 1.0), 0..0);
        assert_eq!(visible_range(100, -1.0, 0.0, 600.0, 1.0), 0..0);
        assert_eq!(visible_range(100, f32::NAN, 0.0, 600.0, 1.0), 0..0);
        assert_eq!(visible_range(100, ROW, 0.0, 0.0, 1.0), 0..0);
        assert_eq!(visible_range(100, ROW, 0.0, f32::NAN, 1.0), 0..0);
    }

    #[test]
    fn top_of_list_has_overscan_below_only() {
        // 600px viewport = 30 rows; one screen of overscan below.
        assert_eq!(visible_range(100_000, ROW, 0.0, 600.0, 1.0), 0..60);
        assert_eq!(visible_range(100_000, ROW, 0.0, 600.0, 0.0), 0..30);
    }

    #[test]
    fn middle_has_overscan_both_sides() {
        let r = visible_range(100_000, ROW, 1_000_000.0, 600.0, 1.0);
        assert_eq!(r, 49_970..50_060);
        assert_eq!(r.len(), 90);
    }

    #[test]
    fn partial_rows_are_included() {
        // Offset 10px into row 0: rows 0..=30 are (partly) visible.
        assert_eq!(visible_range(1000, ROW, 10.0, 600.0, 0.0), 0..31);
    }

    #[test]
    fn offset_beyond_end_shows_last_screen() {
        let r = visible_range(100_000, ROW, 1e12, 600.0, 0.0);
        assert_eq!(r, 99_970..100_000);
        let r = visible_range(100_000, ROW, 1e12, 600.0, 1.0);
        assert_eq!(r, 99_940..100_000);
    }

    #[test]
    fn negative_and_nan_offsets_clamp_to_top() {
        assert_eq!(visible_range(1000, ROW, -500.0, 600.0, 0.0), 0..30);
        assert_eq!(visible_range(1000, ROW, f32::NAN, 600.0, 0.0), 0..30);
    }

    #[test]
    fn short_list_fits_entirely() {
        assert_eq!(visible_range(5, ROW, 300.0, 600.0, 1.0), 0..5);
    }

    #[test]
    fn unbounded_viewport_materializes_everything() {
        assert_eq!(visible_range(1000, ROW, 0.0, f32::INFINITY, 1.0), 0..1000);
    }

    #[test]
    fn cost_is_independent_of_len() {
        let small = visible_range(100, ROW, 0.0, 600.0, 1.0).len();
        let huge = visible_range(1_000_000, ROW, 0.0, 600.0, 1.0).len();
        assert_eq!(small, 60);
        assert_eq!(huge, 60);
    }

    #[test]
    fn reveal_offset_moves_minimum_distance() {
        // Viewport [200, 800) shows rows 10..40.
        assert_eq!(reveal_offset(20, ROW, 200.0, 600.0), None);
        assert_eq!(reveal_offset(10, ROW, 200.0, 600.0), None);
        assert_eq!(reveal_offset(39, ROW, 200.0, 600.0), None);
        // Above: align top.
        assert_eq!(reveal_offset(9, ROW, 200.0, 600.0), Some(180.0));
        // Below: align bottom.
        assert_eq!(reveal_offset(40, ROW, 200.0, 600.0), Some(220.0));
        assert_eq!(reveal_offset(99, ROW, 200.0, 600.0), Some(1400.0));
        // Partly visible at the top.
        assert_eq!(reveal_offset(9, ROW, 190.0, 600.0), Some(180.0));
        // Viewport shorter than a row: align top.
        assert_eq!(reveal_offset(3, ROW, 0.0, 10.0), Some(60.0));
        assert_eq!(reveal_offset(3, ROW, 60.0, 10.0), None);
    }

    type TestList<'a> = VirtualList<'a, (), iced::Theme, ()>;

    fn counting_list(len: usize, built: &Counter) -> TestList<'static> {
        let built = Rc::clone(built);
        virtual_list(len, ROW, move |i| {
            built.borrow_mut().push(i);
            Element::new(space().height(ROW))
        })
    }

    fn lay_out(element: &mut Element<'_, (), iced::Theme, ()>, tree: &mut Tree) -> layout::Node {
        element.as_widget_mut().layout(
            tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(400.0, 600.0)),
        )
    }

    #[test]
    fn layout_builds_only_the_window_for_100k_rows() {
        let built: Counter = Rc::default();
        let mut element: Element<'_, (), iced::Theme, ()> = counting_list(100_000, &built).into();
        let mut tree = Tree::new(&element);

        let node = lay_out(&mut element, &mut tree);

        // First layout already knows the 600px viewport: 30 visible + 30 below.
        assert_eq!(*built.borrow(), (0..60).collect::<Vec<_>>());
        // The scrollable's content spans every row.
        let content = &node.children()[0].children()[0];
        assert_eq!(content.size().height, 100_000.0 * ROW);
    }

    #[test]
    fn wheel_past_the_window_rebuilds_before_draw() {
        let built: Counter = Rc::default();
        let mut element: Element<'_, (), iced::Theme, ()> = counting_list(100_000, &built).into();
        let mut tree = Tree::new(&element);
        let node = lay_out(&mut element, &mut tree);
        built.borrow_mut().clear();

        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Pixels {
                x: 0.0,
                y: -5_000.0,
            },
        });
        element.as_widget_mut().update(
            &mut tree,
            &wheel,
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(100.0, 100.0)),
            &(),
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::new(Point::ORIGIN, Size::new(400.0, 600.0)),
        );
        assert!(shell.is_layout_invalid(), "left the window: must relayout");

        lay_out(&mut element, &mut tree);
        // Offset 5000 = row 250; one screen (30 rows) either side.
        assert_eq!(*built.borrow(), (220..310).collect::<Vec<_>>());
    }

    #[test]
    fn small_scroll_inside_overscan_does_not_rebuild() {
        let built: Counter = Rc::default();
        let mut element: Element<'_, (), iced::Theme, ()> = counting_list(100_000, &built).into();
        let mut tree = Tree::new(&element);
        let node = lay_out(&mut element, &mut tree);

        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        let wheel = Event::Mouse(mouse::Event::WheelScrolled {
            delta: ScrollDelta::Pixels { x: 0.0, y: -200.0 },
        });
        element.as_widget_mut().update(
            &mut tree,
            &wheel,
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(100.0, 100.0)),
            &(),
            &mut clipboard::Null,
            &mut shell,
            &Rectangle::new(Point::ORIGIN, Size::new(400.0, 600.0)),
        );
        assert!(!shell.is_layout_invalid());
    }
}
