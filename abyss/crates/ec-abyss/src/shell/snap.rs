// SPDX-License-Identifier: AGPL-3.0-only
//! Window snapping while moving and resizing under `mode "de"` (COMP-05 §3.1,
//! amendment C-24).
//!
//! A moved window's frame edges snap to the edges of the output's usable area
//! and to the frame edges of other visible windows, within
//! `general.snap-distance` logical px; a resized window's moving edges do the
//! same. Frames are the outer rectangles (`Floating::rect`: titlebar, content
//! and border), so two snapped windows touch exactly.
//!
//! **Who counts.** Only floating windows on the dragged window's own output
//! and its active workspace, so a minimized window (not in the layout at all),
//! a window on another workspace and a window that is rolled up to its bar
//! (its content is not there to line up with) are ignored, as are fullscreen
//! windows. **Never off the area.** A snap that would leave the frame further
//! outside the usable area than it already was is refused.
//!
//! **Shift** held at the time of the motion disables snapping for that motion
//! (KDE's modifier too). Shift is not part of the Alt+drag move/resize
//! binds or the titlebar drag, so it never conflicts with the grab; edge
//! tiling is a separate gesture and is not suppressed by it.
//!
//! The pointer path allocates nothing: the other windows are walked in place.

use smithay::{
    desktop::Window,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    utils::{Logical, Point, Rectangle},
};

use crate::state::AbyssState;

use super::{quicktile::Edges, titlebar, usable_area};

/// Which edges of a rectangle move, and so may snap.
fn moving(edges: ResizeEdge) -> Edges {
    use ResizeEdge as E;
    Edges {
        left: matches!(edges, E::Left | E::TopLeft | E::BottomLeft),
        right: matches!(edges, E::Right | E::TopRight | E::BottomRight),
        top: matches!(edges, E::Top | E::TopLeft | E::TopRight),
        bottom: matches!(edges, E::Bottom | E::BottomLeft | E::BottomRight),
    }
}

/// How far `lo..hi` hangs outside `ulo..uhi`.
fn overflow(lo: i32, hi: i32, ulo: i32, uhi: i32) -> i32 {
    (ulo - lo).max(0) + (hi - uhi).max(0)
}

/// Snap one axis. `lo..hi` is the frame's extent on it, `ulo..uhi` the usable
/// extent, `targets` the coordinates of other edges (already filtered to the
/// windows near on the other axis). `lo_moves` / `hi_moves` say which edges
/// may snap; `both` is a move, where the whole span shifts together. Returns
/// the shift to apply to the moving edge(s): the smallest within `dist` that
/// does not push the span further out of the usable area; 0 if none.
fn snap_axis(
    (lo, hi): (i32, i32),
    (ulo, uhi): (i32, i32),
    targets: impl Iterator<Item = i32>,
    dist: i32,
    (lo_moves, hi_moves): (bool, bool),
    both: bool,
) -> i32 {
    let before = overflow(lo, hi, ulo, uhi);
    let mut best: Option<i32> = None;
    let mut consider = |d: i32| {
        if d.abs() > dist || best.is_some_and(|b| b.abs() <= d.abs()) {
            return;
        }
        let (nlo, nhi) = match (both, lo_moves) {
            (true, _) => (lo + d, hi + d),
            (false, true) => (lo + d, hi),
            (false, false) => (lo, hi + d),
        };
        if nhi > nlo && overflow(nlo, nhi, ulo, uhi) <= before {
            best = Some(d);
        }
    };
    let mut each = |t: i32| {
        if lo_moves {
            consider(t - lo);
        }
        if hi_moves {
            consider(t - hi);
        }
    };
    each(ulo);
    each(uhi);
    for t in targets {
        each(t);
    }
    best.unwrap_or(0)
}

/// The shift (dx, dy) that snaps `frame`. `others` are the frames of the
/// windows to line up with. `edges` are the moving edges; `all` a move.
pub fn snap_offset(
    frame: Rectangle<i32, Logical>,
    usable: Rectangle<i32, Logical>,
    others: impl Iterator<Item = Rectangle<i32, Logical>> + Clone,
    dist: i32,
    edges: Edges,
    all: bool,
) -> (i32, i32) {
    if dist <= 0 {
        return (0, 0);
    }
    let (l, t) = (frame.loc.x, frame.loc.y);
    let (r, b) = (l + frame.size.w, t + frame.size.h);
    let near_y = |o: &Rectangle<i32, Logical>| o.loc.y < b + dist && o.loc.y + o.size.h > t - dist;
    let dx = if all || edges.left || edges.right {
        snap_axis(
            (l, r),
            (usable.loc.x, usable.loc.x + usable.size.w),
            others
                .clone()
                .filter(near_y)
                .flat_map(|o| [o.loc.x, o.loc.x + o.size.w]),
            dist,
            (all || edges.left, all || edges.right),
            all,
        )
    } else {
        0
    };
    // The vertical pass sees the frame as already shifted horizontally.
    let (l, r) = if all { (l + dx, r + dx) } else { (l, r) };
    let near_x = |o: &Rectangle<i32, Logical>| o.loc.x < r + dist && o.loc.x + o.size.w > l - dist;
    let dy = if all || edges.top || edges.bottom {
        snap_axis(
            (t, b),
            (usable.loc.y, usable.loc.y + usable.size.h),
            others.filter(near_x).flat_map(|o| [o.loc.y, o.loc.y + o.size.h]),
            dist,
            (all || edges.top, all || edges.bottom),
            all,
        )
    } else {
        0
    };
    (dx, dy)
}

fn shift_held(state: &AbyssState) -> bool {
    state
        .seat
        .get_keyboard()
        .is_some_and(|k| k.modifier_state().shift)
}

/// Snapping is on: `de`, the flag, a distance, and Shift up.
fn enabled(state: &AbyssState) -> bool {
    let g = &state.config.general;
    state.config.mode.profile().quick_tile && g.snap && g.snap_distance > 0 && !shift_held(state)
}

/// Snap `content` (the window geometry a grab wants to place) and return the
/// content rectangle to place instead. `edges` is `None` for a move, the
/// dragged edges for a resize. Nothing is allocated.
pub fn snap_content(
    state: &AbyssState,
    window: &Window,
    content: Rectangle<i32, Logical>,
    edges: Option<ResizeEdge>,
) -> Rectangle<i32, Logical> {
    if !enabled(state) {
        return content;
    }
    let border = state.config.general.border_size;
    let bar = titlebar::bar_height(state, window);
    // The window's output, and its active workspace's floating windows.
    let Some(entry) = state.outputs.iter().find(|e| {
        e.workspaces
            .iter()
            .any(|ws| ws.floating.iter().any(|f| &f.window == window))
    }) else {
        return content;
    };
    let ws = &entry.workspaces[entry.active];
    if !ws.floating.iter().any(|f| &f.window == window) {
        return content;
    }
    let frame = titlebar::outer_of_content(content, border, bar);
    let usable = usable_area(state, &entry.output);
    let others = ws
        .floating
        .iter()
        .filter(|f| &f.window != window && !state.winops.is_shaded(&f.window))
        .filter(|f| !state.fullscreen.contains_key(&f.window))
        .map(|f| f.rect);
    let (all, mv) = match edges {
        None => (true, Edges::NONE),
        Some(e) => (false, moving(e)),
    };
    let (dx, dy) = snap_offset(frame, usable, others, state.config.general.snap_distance, mv, all);
    if all {
        return Rectangle::new(content.loc + Point::from((dx, dy)), content.size);
    }
    let mut r = content;
    if mv.left {
        r.loc.x += dx;
        r.size.w -= dx;
    }
    if mv.right {
        r.size.w += dx;
    }
    if mv.top {
        r.loc.y += dy;
        r.size.h -= dy;
    }
    if mv.bottom {
        r.size.h += dy;
    }
    r.size.w = r.size.w.max(1);
    r.size.h = r.size.h.max(1);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), (w, h).into())
    }

    const USABLE: fn() -> Rectangle<i32, Logical> = || r(0, 30, 1000, 770);

    fn mv(frame: Rectangle<i32, Logical>, others: &[Rectangle<i32, Logical>]) -> (i32, i32) {
        snap_offset(frame, USABLE(), others.iter().copied(), 10, Edges::NONE, true)
    }

    #[test]
    fn a_move_snaps_to_the_usable_area_edges() {
        // Left and top (the panel's strip is the area's top).
        assert_eq!(mv(r(6, 36, 300, 200), &[]), (-6, -6));
        // Right and bottom.
        assert_eq!(mv(r(695, 595, 300, 200), &[]), (5, 5));
        // Out of range: nothing.
        assert_eq!(mv(r(40, 80, 300, 200), &[]), (0, 0));
    }

    #[test]
    fn a_move_snaps_to_another_windows_frame_both_adjacent_and_aligned() {
        let other = [r(400, 300, 200, 150)];
        // My right edge near its left edge: adjacent.
        assert_eq!(mv(r(95, 320, 300, 100), &other), (5, 0));
        // My left edge near its right edge.
        assert_eq!(mv(r(607, 320, 300, 100), &other), (-7, 0));
        // My top near its bottom, my left aligned with its left.
        assert_eq!(mv(r(403, 455, 100, 100), &other), (-3, -5));
    }

    #[test]
    fn a_distant_or_unrelated_window_does_not_pull() {
        // Far away on the other axis: vertical ranges do not meet.
        let other = [r(400, 700, 200, 50)];
        assert_eq!(mv(r(95, 310, 300, 100), &other), (0, 0));
        // Out of snap distance.
        let other = [r(400, 300, 200, 150)];
        assert_eq!(mv(r(80, 310, 300, 100), &other), (0, 0));
    }

    #[test]
    fn the_nearest_target_wins() {
        let others = [r(310, 40, 100, 100), r(305, 40, 100, 100)];
        // Right edge at 300: 305 is 5 away, 310 is 10.
        assert_eq!(mv(r(100, 40, 200, 100), &others).0, 5);
    }

    #[test]
    fn a_snap_never_pushes_a_window_further_off_the_area() {
        // Another window hangs off the left of the area; snapping to its
        // right edge at -3 would put this one off the area.
        let hanging = [r(-300, 100, 297, 100)];
        assert_eq!(
            mv(r(2, 100, 200, 100), &hanging).0,
            -2,
            "the area's own edge instead"
        );
        // A window already partly off may snap inward but not outward.
        assert_eq!(mv(r(-4, 100, 200, 100), &[]).0, 4);
        let d = snap_offset(
            r(-30, 100, 200, 100),
            USABLE(),
            [r(-300, 100, 267, 100)].into_iter(),
            10,
            Edges::NONE,
            true,
        );
        assert!(d.0 >= 0, "never further left: {d:?}");
    }

    #[test]
    fn a_resize_snaps_only_the_edges_that_move() {
        let right = Edges {
            right: true,
            ..Edges::NONE
        };
        let d = snap_offset(r(100, 100, 395, 200), USABLE(), [].into_iter(), 10, right, false);
        assert_eq!(d, (0, 0), "500 is not an edge of anything");
        let other = [r(500, 100, 100, 100)];
        let d = snap_offset(
            r(100, 100, 395, 200),
            USABLE(),
            other.into_iter(),
            10,
            right,
            false,
        );
        assert_eq!(d, (5, 0));
        // The left edge is not dragged, so a neighbour near it does not pull.
        let other = [r(0, 100, 97, 100)];
        let d = snap_offset(
            r(100, 100, 395, 200),
            USABLE(),
            other.into_iter(),
            10,
            right,
            false,
        );
        assert_eq!(d, (0, 0));
        // A bottom-right drag snaps both axes to the area.
        let both = Edges {
            right: true,
            bottom: true,
            ..Edges::NONE
        };
        let d = snap_offset(r(100, 100, 897, 695), USABLE(), [].into_iter(), 10, both, false);
        assert_eq!(d, (3, 5));
    }

    #[test]
    fn a_zero_distance_snaps_nothing() {
        let d = snap_offset(r(1, 31, 100, 100), USABLE(), [].into_iter(), 0, Edges::NONE, true);
        assert_eq!(d, (0, 0));
    }

    #[test]
    fn edges_follow_the_resize_edge() {
        let e = moving(ResizeEdge::TopLeft);
        assert!(e.left && e.top && !e.right && !e.bottom);
        let e = moving(ResizeEdge::Bottom);
        assert!(e.bottom && !e.top && !e.left && !e.right);
        let e = moving(ResizeEdge::None);
        assert_eq!(e, Edges::NONE);
    }
}
