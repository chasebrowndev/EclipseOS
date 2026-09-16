// SPDX-License-Identifier: AGPL-3.0-only
//! Compositor-drawn region selector (COMP-18 §1.1, §1.3).
//!
//! An addon that wants a rectangle must not be the thing that asks for it.
//! Letting Oracle-Eyes draw its own rubber-band would mean an untrusted client
//! grabbing the seat and painting over the whole screen on a chord it chose,
//! which is exactly the authority COMP-18 §1.3 denies it. So the compositor
//! runs the interaction itself and hands back only the four numbers, the same
//! way the overscan calibration overlay owns the seat for its own keys.
//!
//! Drawn as a backend-prepended pass, so it is invisible to `capture.rs` by
//! construction rather than by policy (ADR 0040) — a selector that showed up
//! in the capture it is about to trigger would be read back as screen content.

use smithay::{
    backend::renderer::element::{
        solid::{SolidColorBuffer, SolidColorRenderElement},
        Kind,
    },
    input::keyboard::Keysym,
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Size},
};

use super::AbyssRenderElement;

/// Shortest edge, in logical pixels, that counts as a deliberate drag. Below
/// this a release is a miss-click, and COMP-18 §1.3 gives an addon nothing on
/// a miss-click: a 0x0 rectangle is still a rectangle it could act on.
const MIN_EDGE: i32 = 8;

/// Everything outside the pending rectangle is dimmed, so the human can see
/// what the addon will be given before letting go of the button.
const DIM: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const DIM_ALPHA: f32 = 0.45;
const BAND: [f32; 4] = [1.0, 0.69, 0.16, 1.0];
const BAND_THICK: i32 = 2;

/// What a key means while the selector owns the seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKey {
    /// Leave selection mode and emit nothing.
    Cancel,
    /// A key with no meaning here. Swallowed anyway — the selector owns the
    /// seat outright, so nothing bound elsewhere may fire underneath it.
    Ignored,
}

/// Escape cancels, and so does a second press of the chord that started the
/// selection: the chord is a toggle, because a modal mode you can only leave
/// by a key you were not told about is a trap.
pub fn key(sym: Keysym, select_chord: bool) -> SelectKey {
    if select_chord || sym == Keysym::Escape {
        SelectKey::Cancel
    } else {
        SelectKey::Ignored
    }
}

/// The `keybind` payload for a committed region, in compositor-logical
/// coordinates — the same space `annotation_create` takes rectangles in, so
/// the addon can hand it straight back without knowing about output scale.
pub fn payload(rect: Rectangle<i32, Logical>) -> serde_json::Value {
    serde_json::json!({
        "action": "annotation-select",
        "region": {
            "x": rect.loc.x,
            "y": rect.loc.y,
            "w": rect.size.w,
            "h": rect.size.h,
        },
    })
}

/// A drag in progress, in global logical coordinates.
#[derive(Debug)]
struct Session {
    /// Where the button went down. `None` until it does: the mode is entered
    /// by a chord, which leaves the human free to move the pointer first.
    anchor: Option<Point<i32, Logical>>,
    cursor: Point<i32, Logical>,
}

/// Selection-mode state. Inactive is the default and costs a frame nothing.
#[derive(Debug, Default)]
pub struct RegionSelect {
    session: Option<Session>,
}

impl RegionSelect {
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// Enter selection mode with the pointer where it already is.
    pub fn start(&mut self, cursor: Point<i32, Logical>) {
        self.session = Some(Session { anchor: None, cursor });
    }

    /// Leave selection mode with nothing to report.
    pub fn cancel(&mut self) {
        self.session = None;
    }

    /// Returns whether the drawn shape changed, so a motion that only moves
    /// the (compositor-drawn) cursor does not cost a full redraw.
    pub fn motion(&mut self, pos: Point<i32, Logical>) -> bool {
        let Some(s) = self.session.as_mut() else {
            return false;
        };
        let moved = s.cursor != pos;
        s.cursor = pos;
        moved && s.anchor.is_some()
    }

    pub fn press(&mut self, pos: Point<i32, Logical>) {
        if let Some(s) = self.session.as_mut() {
            s.anchor = Some(pos);
            s.cursor = pos;
        }
    }

    /// Ends the session either way. `Some` only for a deliberate drag: a
    /// press-release with no travel is a cancel, not a 0x0 rectangle.
    pub fn release(&mut self, pos: Point<i32, Logical>) -> Option<Rectangle<i32, Logical>> {
        let s = self.session.take()?;
        let anchor = s.anchor?;
        let rect = normalize(anchor, pos);
        (rect.size.w >= MIN_EDGE && rect.size.h >= MIN_EDGE).then_some(rect)
    }

    /// The rubber-band as it currently stands, if the drag has started.
    pub fn rect(&self) -> Option<Rectangle<i32, Logical>> {
        let s = self.session.as_ref()?;
        Some(normalize(s.anchor?, s.cursor))
    }
}

/// Two drag points in any order into a rectangle with non-negative extents.
pub fn normalize(a: Point<i32, Logical>, b: Point<i32, Logical>) -> Rectangle<i32, Logical> {
    let loc: Point<i32, Logical> = (a.x.min(b.x), a.y.min(b.y)).into();
    let size: Size<i32, Logical> = ((a.x - b.x).abs(), (a.y - b.y).abs()).into();
    Rectangle::new(loc, size)
}

fn phys(p: Point<i32, Logical>, scale: Scale<f64>) -> Point<i32, Physical> {
    p.to_f64().to_physical(scale).to_i32_round()
}

fn quad(
    loc: Point<i32, Logical>,
    size: Size<i32, Logical>,
    scale: Scale<f64>,
    color: [f32; 4],
    alpha: f32,
    out: &mut Vec<AbyssRenderElement>,
) {
    if size.w <= 0 || size.h <= 0 {
        return;
    }
    let buffer = SolidColorBuffer::new(size, color);
    out.push(AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        phys(loc, scale),
        scale,
        alpha,
        Kind::Unspecified,
    )));
}

/// The selector's elements for one output, front-to-back. Empty when no
/// selection is running, which is the overwhelmingly common case.
pub fn selector_elements(
    select: &RegionSelect,
    output: &Output,
    output_loc: Point<i32, Logical>,
) -> Vec<AbyssRenderElement> {
    if !select.active() {
        return Vec::new();
    }
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let logical: Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    let bounds = Rectangle::new(Point::from((0, 0)), logical);

    // Output-local, because each output draws its own share of a rectangle
    // that lives in the global space.
    let local = select
        .rect()
        .map(|r| Rectangle::new(r.loc - output_loc, r.size))
        .and_then(|r| r.intersection(bounds));

    let mut out = Vec::new();
    let Some(r) = local else {
        // Nothing dragged yet: dim the lot, so the mode is unmistakable.
        quad(bounds.loc, bounds.size, scale, DIM, DIM_ALPHA, &mut out);
        return out;
    };

    // Rubber-band first: front-to-back, and it sits on top of the dim.
    let t = BAND_THICK.min(r.size.w).min(r.size.h);
    for (loc, size) in [
        (r.loc, Size::from((r.size.w, t))),
        (
            Point::from((r.loc.x, r.loc.y + r.size.h - t)),
            Size::from((r.size.w, t)),
        ),
        (r.loc, Size::from((t, r.size.h))),
        (
            Point::from((r.loc.x + r.size.w - t, r.loc.y)),
            Size::from((t, r.size.h)),
        ),
    ] {
        quad(loc, size, scale, BAND, 1.0, &mut out);
    }

    let (right, bottom) = (r.loc.x + r.size.w, r.loc.y + r.size.h);
    for (loc, size) in [
        (Point::from((0, 0)), Size::from((logical.w, r.loc.y))),
        (
            Point::from((0, bottom)),
            Size::from((logical.w, logical.h - bottom)),
        ),
        (Point::from((0, r.loc.y)), Size::from((r.loc.x, r.size.h))),
        (
            Point::from((right, r.loc.y)),
            Size::from((logical.w - right, r.size.h)),
        ),
    ] {
        quad(loc, size, scale, DIM, DIM_ALPHA, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> Point<i32, Logical> {
        (x, y).into()
    }

    #[test]
    fn a_drag_normalises_the_same_way_in_every_direction() {
        let want = Rectangle::new(p(10, 20), Size::from((30, 40)));
        for (a, b) in [
            (p(10, 20), p(40, 60)),
            (p(40, 60), p(10, 20)),
            (p(40, 20), p(10, 60)),
            (p(10, 60), p(40, 20)),
        ] {
            assert_eq!(normalize(a, b), want, "{a:?} -> {b:?}");
        }
    }

    #[test]
    fn a_drag_that_never_moved_is_a_cancel_not_a_zero_rect() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(100, 100));
        assert_eq!(s.release(p(100, 100)), None);
        assert!(!s.active(), "a release always ends the session");
    }

    #[test]
    fn a_sub_threshold_drag_is_a_cancel() {
        for end in [p(103, 140), p(140, 103), p(103, 103)] {
            let mut s = RegionSelect::default();
            s.start(p(0, 0));
            s.press(p(100, 100));
            assert_eq!(s.release(end), None, "{end:?}");
        }
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(100, 100));
        assert_eq!(
            s.release(p(100 + MIN_EDGE, 100 + MIN_EDGE)),
            Some(Rectangle::new(p(100, 100), Size::from((MIN_EDGE, MIN_EDGE))))
        );
    }

    #[test]
    fn a_release_before_any_press_commits_nothing() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        assert_eq!(s.release(p(500, 500)), None);
    }

    #[test]
    fn escape_and_the_chord_cancel_and_everything_else_is_swallowed() {
        assert_eq!(key(Keysym::Escape, false), SelectKey::Cancel);
        assert_eq!(key(Keysym::o, true), SelectKey::Cancel);
        assert_eq!(key(Keysym::a, false), SelectKey::Ignored);
        assert_eq!(key(Keysym::Return, false), SelectKey::Ignored);
    }

    #[test]
    fn cancelling_leaves_no_rectangle_to_emit() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(10, 10));
        s.motion(p(400, 400));
        assert!(s.rect().is_some(), "a live drag has a band to draw");
        s.cancel();
        assert!(!s.active());
        assert_eq!(s.rect(), None);
        // And the only thing that ever produces a payload is `release`.
        assert_eq!(s.release(p(400, 400)), None);
    }

    #[test]
    fn a_committed_rectangle_has_strictly_positive_extents() {
        let mut s = RegionSelect::default();
        s.start(p(0, 0));
        s.press(p(400, 400));
        let r = s.release(p(100, 100)).expect("a backwards drag still commits");
        assert!(r.size.w > 0 && r.size.h > 0);
        assert_eq!(r, Rectangle::new(p(100, 100), Size::from((300, 300))));
    }

    #[test]
    fn the_payload_carries_the_rectangle_in_logical_coordinates() {
        let v = payload(Rectangle::new(p(12, 34), Size::from((56, 78))));
        assert_eq!(
            v,
            serde_json::json!({
                "action": "annotation-select",
                "region": {"x": 12, "y": 34, "w": 56, "h": 78},
            })
        );
    }

    #[test]
    fn the_capture_pass_cannot_see_the_selector() {
        let src = include_str!("capture.rs");
        assert!(
            !src.contains("selector_elements") && !src.contains("region_select"),
            "capture.rs names the region selector; a selector in a capture would be read back as screen content"
        );
    }

    #[test]
    fn the_selector_sits_between_annotations_and_the_trusted_indicator_in_every_backend() {
        for (src, path, indicator_first) in [
            (include_str!("../backend/drm.rs"), "drm.rs", true),
            (include_str!("../backend/winit.rs"), "winit.rs", false),
            (include_str!("../backend/headless.rs"), "headless.rs", false),
        ] {
            let ann = src
                .find("annotation::annotation_elements")
                .unwrap_or_else(|| panic!("{path} does not draw annotations"));
            let sel = src
                .find("select::selector_elements")
                .unwrap_or_else(|| panic!("{path} does not draw the region selector"));
            let ind = src
                .find("capture::indicator")
                .unwrap_or_else(|| panic!("{path} does not draw the indicator"));
            // drm appends top-first; winit and headless splice each pass in at
            // index 0, which reverses source order.
            assert_eq!(
                ind < sel,
                indicator_first,
                "{path}: selector is on the wrong side of trusted UI"
            );
            assert_eq!(
                sel < ann,
                indicator_first,
                "{path}: selector is on the wrong side of the annotation pass"
            );
        }
    }
}
