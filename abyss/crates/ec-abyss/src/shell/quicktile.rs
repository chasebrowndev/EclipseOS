// SPDX-License-Identifier: AGPL-3.0-only
//! Quick Tile under `mode "de"` (COMP-05 §3.1, §7.1, amendment C-24, ADR 0090).
//!
//! Quick Tile is a *floating placement*, not a tiling layout: `de` stays pure
//! stacking. A tiled window is a floating window whose [`Placement`] is
//! `QuickTile { zone, restore }`; its frame rectangle is derived on every
//! arrange from the output's usable area (the output minus layer-shell
//! exclusive zones), so a panel appearing or an output resizing is followed.
//! The frame, titlebar included, fills the zone; the content rect and the
//! configure follow from it as in `shell::titlebar`. It is drawn square and
//! without a border, like a maximized window. `restore` is the outer rectangle
//! the window had before it was first tiled or maximized.
//!
//! **What clients are told.** An `xdg_toplevel` gets the content size and the
//! `Tiled*` states for exactly the edges its zone touches (a left half: left,
//! top, bottom; the top-left quarter: left, top), sent by `arrange` and cleared
//! by the next arrange once the window leaves the placement. `Maximized` is
//! never set for a tile. An **X11** window is placed the same way (the content
//! rectangle goes out in a `ConfigureNotify`) and gets no tiled state at all:
//! `_NET_WM_STATE` has no tiled bits, and `Maximized` is reserved for a window
//! that really is maximized.
//!
//! **Keys.** [`bind`] moves the focused window one step toward a direction
//! along each axis, where an axis has three positions: low (left / top), the
//! whole span, and high (right / bottom). A floating window is at the whole
//! span on both axes. The step is clamped, so:
//!
//! - Left from floating is the left half; Up from floating is the top half;
//!   Down the bottom half (KWin maximizes on Up and minimizes on Down; this
//!   keeps `de` to one rule).
//! - Left then Up is the top-left quarter, in either order.
//! - The opposite direction steps back: left half then Right is floating
//!   again (the restore rect comes back), the top-left quarter then Right is
//!   the top half.
//! - The same direction again does nothing at the end of the axis.
//!
//! A maximized window counts as floating for the step (the tile replaces the
//! maximize and keeps its restore rect). Fullscreen wins: the bind does
//! nothing while a window is fullscreen. Super+arrows are the `de` default
//! binds (`mode_default_binds`); an explicit bind on the chord wins, and the
//! reserved chords are not involved.
//!
//! **Dragging.** While a window is dragged ([`drag_preview`], called from the
//! move grab), a pointer at an *outer* edge of the output layout shows a
//! preview of the target and the drop applies it ([`drop_preview`]). Top:
//! maximize (KDE default). Left and right: that half, or the quarter when the
//! pointer is in the upper or lower quarter of the edge. An edge shared with
//! another output never triggers. The preview is a compositor overlay
//! (`ec_abyss_render::drop::preview_elements`) and is never in a capture pass
//! list. A drop (or key) onto a zone another tile occupies displaces that tile
//! so tiles never overlap: it takes the mirror zone (a swap of halves), else the
//! free zone sharing most of its area, else its free rectangle.

use smithay::{
    desktop::{layer_map_for_output, Window},
    output::Output,
    utils::{Logical, Point, Rectangle},
};

use crate::{input::Direction, state::AbyssState};

use super::{anim, arrange, clamp_size, maximize, titlebar, usable_area, workspace::Placement};

/// Where a Quick Tiled window sits in the usable area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// The edges of the usable area a zone touches: what the client's `Tiled*`
/// states are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    /// Every edge: a tile of the Radiant layout.
    pub const ALL: Edges = Edges {
        left: true,
        right: true,
        top: true,
        bottom: true,
    };
    pub const NONE: Edges = Edges {
        left: false,
        right: false,
        top: false,
        bottom: false,
    };
}

impl Zone {
    /// The position on each axis: -1 low (left, top), 0 the whole span, 1 high.
    pub fn axes(self) -> (i8, i8) {
        match self {
            Zone::Left => (-1, 0),
            Zone::Right => (1, 0),
            Zone::Top => (0, -1),
            Zone::Bottom => (0, 1),
            Zone::TopLeft => (-1, -1),
            Zone::TopRight => (1, -1),
            Zone::BottomLeft => (-1, 1),
            Zone::BottomRight => (1, 1),
        }
    }

    /// The zone at `(x, y)`; `None` for `(0, 0)`, which is floating.
    pub fn from_axes(x: i8, y: i8) -> Option<Zone> {
        Some(match (x, y) {
            (-1, 0) => Zone::Left,
            (1, 0) => Zone::Right,
            (0, -1) => Zone::Top,
            (0, 1) => Zone::Bottom,
            (-1, -1) => Zone::TopLeft,
            (1, -1) => Zone::TopRight,
            (-1, 1) => Zone::BottomLeft,
            (1, 1) => Zone::BottomRight,
            _ => return None,
        })
    }

    /// One key press toward `dir` from `from` (`None` is floating): the
    /// documented rule of this module. `None` back means floating.
    pub fn step(from: Option<Zone>, dir: Direction) -> Option<Zone> {
        let (x, y) = from.map_or((0, 0), Zone::axes);
        let (dx, dy) = match dir {
            Direction::Left => (-1, 0),
            Direction::Right => (1, 0),
            Direction::Up => (0, -1),
            Direction::Down => (0, 1),
        };
        Zone::from_axes((x + dx).clamp(-1, 1), (y + dy).clamp(-1, 1))
    }

    /// The zone's frame rectangle in `usable`. Halves split at `size / 2`; the
    /// high side takes the odd pixel, so two neighbours always tile exactly.
    pub fn rect(self, usable: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        let (ax, ay) = self.axes();
        let span = |lo: i32, len: i32, a: i8| match a {
            -1 => (lo, len / 2),
            1 => (lo + len / 2, len - len / 2),
            _ => (lo, len),
        };
        let (x, w) = span(usable.loc.x, usable.size.w, ax);
        let (y, h) = span(usable.loc.y, usable.size.h, ay);
        Rectangle::new(Point::from((x, y)), (w.max(1), h.max(1)).into())
    }

    /// The edges of the usable area this zone touches.
    pub fn edges(self) -> Edges {
        let (ax, ay) = self.axes();
        Edges {
            left: ax <= 0,
            right: ax >= 0,
            top: ay <= 0,
            bottom: ay >= 0,
        }
    }
}

/// [`Zone::rect`] held to the client's size hints, as `arrange` configures it:
/// the hints apply to the content, `bar` shorter than the frame.
pub(super) fn fitted_zone(
    window: &Window,
    usable: Rectangle<i32, Logical>,
    zone: Zone,
    bar: i32,
) -> Rectangle<i32, Logical> {
    let mut rect = zone.rect(usable);
    let content = clamp_size(window, (rect.size.w, (rect.size.h - bar).max(1)).into(), false);
    rect.size = (content.w, content.h + bar).into();
    rect
}

/// The frame a placement derives for `window`, or `None` for a free window.
pub(super) fn placed_rect(
    window: &Window,
    placement: Placement,
    usable: Rectangle<i32, Logical>,
    border: i32,
    bar: i32,
) -> Option<Rectangle<i32, Logical>> {
    match placement {
        Placement::Free => None,
        Placement::Maximized { h, v, restore } => {
            Some(maximize::fitted_rect(window, usable, border, restore, h, v, bar))
        }
        Placement::QuickTile { zone, .. } => Some(fitted_zone(window, usable, zone, bar)),
    }
}

/// The Quick Tile placement of `window`, if it has one. No allocation.
pub fn zone_of(state: &AbyssState, window: &Window) -> Option<Zone> {
    match maximize::placement_of(state, window) {
        Placement::QuickTile { zone, .. } => Some(zone),
        _ => None,
    }
}

/// A `tile-*` bind (Super+arrows under `de`): step the focused window.
pub fn bind(state: &mut AbyssState, dir: Direction) {
    if !state.config.mode.profile().quick_tile {
        return;
    }
    let Some(window) = state.focus.clone() else { return };
    step(state, &window, dir);
}

/// One step of `window` toward `dir` (see the module docs). False when nothing
/// was done: not `de`, fullscreen, or not floating on a workspace.
pub fn step(state: &mut AbyssState, window: &Window, dir: Direction) -> bool {
    let next = Zone::step(zone_of(state, window), dir);
    set(state, window, next)
}

/// Quick Tile `window` into `zone`, or give it back its free rectangle
/// (`None`). False when nothing was done.
pub fn set(state: &mut AbyssState, window: &Window, zone: Option<Zone>) -> bool {
    if !state.config.mode.profile().quick_tile || state.fullscreen.contains_key(window) {
        return false;
    }
    let Some(zone) = zone else {
        // Leaving a tile is the restore the maximize code already does; a
        // maximized window stepping "back" to floating is restored the same.
        return maximize::set(state, window, None);
    };
    // A rolled-up window is unrolled by a tile, as by maximize (`winops`).
    super::winops::unshade(state, window);
    let mut done = false;
    anim::toggle(state, Some(window.clone()), |state| {
        done = set_now(state, window, zone);
        if done {
            displace_occupants(state, window, zone);
        }
    });
    done
}

/// Tiles do not overlap: every other Quick Tiled window on `window`'s output
/// and workspace whose zone overlaps `zone` moves to a free zone, preferring
/// the mirror image of its own (so a drag onto an occupied half swaps the
/// halves), else the free zone sharing most of its old area. With no free zone
/// left it is given back its free rectangle.
fn displace_occupants(state: &mut AbyssState, window: &Window, zone: Zone) {
    let Some((id, ws)) = state.outputs.iter().find_map(|e| {
        e.workspaces
            .iter()
            .position(|w| w.floating.iter().any(|f| &f.window == window))
            .map(|ws| (e.id, ws))
    }) else {
        return;
    };
    let output = state.outputs.get(id).expect("just found").output.clone();
    let usable = usable_area(state, &output);
    let target = zone.rect(usable);
    let tiled = |state: &AbyssState| -> Vec<(Window, Zone)> {
        state.outputs.get(id).map_or_else(Vec::new, |e| {
            e.workspaces[ws]
                .floating
                .iter()
                .filter_map(|f| match f.placement {
                    Placement::QuickTile { zone, .. } if &f.window != window => {
                        Some((f.window.clone(), zone))
                    }
                    _ => None,
                })
                .collect()
        })
    };
    let (tx, ty) = zone.axes();
    for (other, old) in tiled(state) {
        let old_rect = old.rect(usable);
        if !old_rect.overlaps(target) {
            continue;
        }
        let (ox, oy) = old.axes();
        // Mirror across the axis the target splits; a half spanning the other
        // axis keeps its shape.
        let mirror = Zone::from_axes(if tx != 0 { -ox } else { ox }, if ty != 0 { -oy } else { oy });
        let taken: Vec<Rectangle<i32, Logical>> = tiled(state)
            .into_iter()
            .filter(|(w, _)| w != &other)
            .map(|(_, z)| z.rect(usable))
            .chain(std::iter::once(target))
            .collect();
        let best = ALL_ZONES
            .iter()
            .copied()
            .filter(|z| !taken.iter().any(|t| z.rect(usable).overlaps(*t)))
            .max_by_key(|z| {
                let shared = z
                    .rect(usable)
                    .intersection(old_rect)
                    .map_or(0, |r| r.size.w * r.size.h);
                (Some(*z) == mirror, shared)
            });
        match best {
            Some(z) => {
                set_now(state, &other, z);
            }
            None => {
                maximize::set(state, &other, None);
            }
        }
    }
}

const ALL_ZONES: [Zone; 8] = [
    Zone::Left,
    Zone::Right,
    Zone::Top,
    Zone::Bottom,
    Zone::TopLeft,
    Zone::TopRight,
    Zone::BottomLeft,
    Zone::BottomRight,
];

fn set_now(state: &mut AbyssState, window: &Window, zone: Zone) -> bool {
    let located = state.outputs.iter().find_map(|e| {
        e.workspaces
            .iter()
            .position(|ws| ws.floating.iter().any(|f| &f.window == window))
            .map(|ws| (e.id, ws))
    });
    let Some((id, ws)) = located else {
        return false;
    };
    let output = state.outputs.get(id).expect("just found").output.clone();
    layer_map_for_output(&output).arrange();
    let usable = usable_area(state, &output);
    let bar = titlebar::bar_height(state, window);
    let entry = state.outputs.get_mut(id).expect("just found");
    let f = entry.workspaces[ws]
        .floating
        .iter_mut()
        .find(|f| &f.window == window)
        .expect("just found");
    let restore = f.placement.restore_rect().unwrap_or(f.rect);
    f.placement = Placement::QuickTile { zone, restore };
    f.rect = fitted_zone(&f.window, usable, zone, bar);
    // An X11 window carries no tiled state, but must not keep a maximized one
    // from before the tile.
    if let Some(x11) = window.x11_surface() {
        let _ = x11.set_maximized(false);
    }
    arrange(state);
    true
}

// ------------------------------------------------------------ edge dragging

/// What a pointer at a screen edge would do on release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeTarget {
    Tile(Zone),
    Maximize,
}

/// Which edges of an output are *outer* edges of the layout: nothing is
/// adjacent beyond them where the pointer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outer {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Where a pointer at `pos` on an output of geometry `bounds` triggers, with
/// `dist` the trigger distance (0 disables). Pure. Left and right take
/// precedence over the top, so a corner is a quarter and only the middle of
/// the top is maximize; a quarter is the outer quarter of the edge's length.
pub fn edge_target(
    bounds: Rectangle<i32, Logical>,
    outer: Outer,
    pos: Point<f64, Logical>,
    dist: i32,
) -> Option<EdgeTarget> {
    if dist <= 0 {
        return None;
    }
    let d = f64::from(dist);
    let (left, top) = (f64::from(bounds.loc.x), f64::from(bounds.loc.y));
    let (right, bottom) = (left + f64::from(bounds.size.w), top + f64::from(bounds.size.h));
    if pos.x < left || pos.x >= right || pos.y < top || pos.y >= bottom {
        return None;
    }
    let side: Option<i8> = if outer.left && pos.x < left + d {
        Some(-1)
    } else if outer.right && pos.x >= right - d {
        Some(1)
    } else {
        None
    };
    if let Some(x) = side {
        let quarter = f64::from(bounds.size.h) / 4.0;
        let y = if outer.top && pos.y < top + quarter {
            -1
        } else if outer.bottom && pos.y >= bottom - quarter {
            1
        } else {
            0
        };
        return Zone::from_axes(x, y).map(EdgeTarget::Tile);
    }
    (outer.top && pos.y < top + d).then_some(EdgeTarget::Maximize)
}

/// The edges of `bounds` with no other output beyond them where the pointer is.
/// Allocation free.
fn outer_edges(state: &AbyssState, bounds: Rectangle<i32, Logical>, pos: Point<f64, Logical>) -> Outer {
    let beyond = |p: Point<f64, Logical>| {
        !state.space.outputs().any(|o| {
            state
                .space
                .output_geometry(o)
                .is_some_and(|g| g.to_f64().contains(p))
        })
    };
    let (l, t) = (f64::from(bounds.loc.x), f64::from(bounds.loc.y));
    let (r, b) = (l + f64::from(bounds.size.w), t + f64::from(bounds.size.h));
    Outer {
        left: beyond(Point::from((l - 1.0, pos.y))),
        right: beyond(Point::from((r, pos.y))),
        top: beyond(Point::from((pos.x, t - 1.0))),
        bottom: beyond(Point::from((pos.x, b))),
    }
}

/// The preview a drag shows: which window, on which output, what the release
/// will do and the rectangle it would fill (global logical).
#[derive(Debug, Clone)]
pub struct Preview {
    pub window: Window,
    pub output: Output,
    pub target: EdgeTarget,
    pub rect: Rectangle<i32, Logical>,
}

fn target_at(
    state: &AbyssState,
    pointer: Point<f64, Logical>,
) -> Option<(Output, EdgeTarget, Rectangle<i32, Logical>)> {
    let g = &state.config.general;
    if !state.config.mode.profile().quick_tile
        || !g.edge_tile
        || g.edge_tile_distance <= 0
        || state.lock.locked
    {
        return None;
    }
    let output = super::output_at(state, pointer)?;
    let bounds = state.space.output_geometry(&output)?;
    let outer = outer_edges(state, bounds, pointer);
    let target = edge_target(bounds, outer, pointer, g.edge_tile_distance)?;
    let usable = usable_area(state, &output);
    let rect = match target {
        EdgeTarget::Tile(zone) => zone.rect(usable),
        EdgeTarget::Maximize => usable,
    };
    Some((output, target, rect))
}

/// `window` is being moved and the pointer is at `pointer`: show, move or clear
/// the preview. Redraws only when the target changes; moving away cancels it.
pub fn drag_preview(state: &mut AbyssState, window: &Window, pointer: Point<f64, Logical>) {
    let want = if state.fullscreen.contains_key(window) {
        None
    } else {
        target_at(state, pointer)
    };
    let same = match (&state.tile_preview, &want) {
        (None, None) => true,
        (Some(p), Some((o, t, r))) => &p.window == window && &p.output == o && p.target == *t && p.rect == *r,
        _ => false,
    };
    if same {
        return;
    }
    state.tile_preview = want.map(|(output, target, rect)| Preview {
        window: window.clone(),
        output,
        target,
        rect,
    });
    crate::backend::damage_all(state);
}

/// Drop the preview without applying it (a cancelled drag, a mode flip).
pub fn clear_preview(state: &mut AbyssState) {
    if state.tile_preview.take().is_some() {
        crate::backend::damage_all(state);
    }
}

/// The drag of `window` ended: apply what the preview showed, if anything.
pub fn drop_preview(state: &mut AbyssState, window: &Window) {
    let Some(preview) = state.tile_preview.take() else {
        return;
    };
    crate::backend::damage_all(state);
    if &preview.window != window {
        return;
    }
    match preview.target {
        EdgeTarget::Tile(zone) => {
            set(state, window, Some(zone));
        }
        EdgeTarget::Maximize => {
            maximize::set(state, window, Some((true, true)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), (w, h).into())
    }

    #[test]
    fn zones_split_the_usable_area_after_a_panel() {
        // A 30 px panel on top and a 48 px one on the left.
        let u = r(48, 30, 1872, 1050);
        assert_eq!(Zone::Left.rect(u), r(48, 30, 936, 1050));
        assert_eq!(Zone::Right.rect(u), r(984, 30, 936, 1050));
        assert_eq!(Zone::Top.rect(u), r(48, 30, 1872, 525));
        assert_eq!(Zone::Bottom.rect(u), r(48, 555, 1872, 525));
        assert_eq!(Zone::TopLeft.rect(u), r(48, 30, 936, 525));
        assert_eq!(Zone::TopRight.rect(u), r(984, 30, 936, 525));
        assert_eq!(Zone::BottomLeft.rect(u), r(48, 555, 936, 525));
        assert_eq!(Zone::BottomRight.rect(u), r(984, 555, 936, 525));
    }

    #[test]
    fn an_odd_size_gives_the_extra_pixel_to_the_high_side() {
        let u = r(0, 0, 101, 51);
        assert_eq!(Zone::Left.rect(u), r(0, 0, 50, 51));
        assert_eq!(Zone::Right.rect(u), r(50, 0, 51, 51));
        assert_eq!(Zone::Top.rect(u), r(0, 0, 101, 25));
        assert_eq!(Zone::Bottom.rect(u), r(0, 25, 101, 26));
    }

    #[test]
    fn zones_follow_a_resized_area() {
        let before = Zone::Right.rect(r(0, 0, 1000, 800));
        let after = Zone::Right.rect(r(0, 40, 1400, 760));
        assert_eq!(before, r(500, 0, 500, 800));
        assert_eq!(after, r(700, 40, 700, 760));
    }

    #[test]
    fn edges_are_the_ones_the_zone_touches() {
        let e = |z: Zone| {
            let e = z.edges();
            (e.left, e.right, e.top, e.bottom)
        };
        assert_eq!(e(Zone::Left), (true, false, true, true));
        assert_eq!(e(Zone::Right), (false, true, true, true));
        assert_eq!(e(Zone::Top), (true, true, true, false));
        assert_eq!(e(Zone::Bottom), (true, true, false, true));
        assert_eq!(e(Zone::TopLeft), (true, false, true, false));
        assert_eq!(e(Zone::BottomRight), (false, true, false, true));
    }

    #[test]
    fn the_step_rule_composes_and_steps_back() {
        use Direction::*;
        let s = Zone::step;
        assert_eq!(s(None, Left), Some(Zone::Left));
        assert_eq!(s(None, Right), Some(Zone::Right));
        assert_eq!(s(None, Up), Some(Zone::Top));
        assert_eq!(s(None, Down), Some(Zone::Bottom));
        // Left then Up, and Up then Left, are the same quarter.
        assert_eq!(s(s(None, Left), Up), Some(Zone::TopLeft));
        assert_eq!(s(s(None, Up), Left), Some(Zone::TopLeft));
        assert_eq!(s(s(None, Right), Down), Some(Zone::BottomRight));
        // The opposite direction returns to floating.
        assert_eq!(s(Some(Zone::Left), Right), None);
        assert_eq!(s(Some(Zone::Top), Down), None);
        // From a quarter it steps back one axis.
        assert_eq!(s(Some(Zone::TopLeft), Right), Some(Zone::Top));
        assert_eq!(s(Some(Zone::TopLeft), Down), Some(Zone::Left));
        // The same direction at the end of an axis does nothing.
        assert_eq!(s(Some(Zone::Left), Left), Some(Zone::Left));
        assert_eq!(s(Some(Zone::TopLeft), Up), Some(Zone::TopLeft));
    }

    #[test]
    fn axes_round_trip() {
        for z in [
            Zone::Left,
            Zone::Right,
            Zone::Top,
            Zone::Bottom,
            Zone::TopLeft,
            Zone::TopRight,
            Zone::BottomLeft,
            Zone::BottomRight,
        ] {
            let (x, y) = z.axes();
            assert_eq!(Zone::from_axes(x, y), Some(z));
        }
        assert_eq!(Zone::from_axes(0, 0), None);
    }

    const ALL: Outer = Outer {
        left: true,
        right: true,
        top: true,
        bottom: true,
    };

    fn at(x: f64, y: f64) -> Option<EdgeTarget> {
        edge_target(r(0, 0, 1920, 1080), ALL, Point::from((x, y)), 20)
    }

    #[test]
    fn the_top_edge_maximizes_the_sides_tile_a_half_and_corners_a_quarter() {
        assert_eq!(at(900.0, 0.0), Some(EdgeTarget::Maximize));
        assert_eq!(at(900.0, 19.0), Some(EdgeTarget::Maximize));
        assert_eq!(at(900.0, 20.0), None);
        assert_eq!(at(0.0, 540.0), Some(EdgeTarget::Tile(Zone::Left)));
        assert_eq!(at(1919.0, 540.0), Some(EdgeTarget::Tile(Zone::Right)));
        assert_eq!(at(19.0, 540.0), Some(EdgeTarget::Tile(Zone::Left)));
        assert_eq!(at(20.0, 540.0), None);
        assert_eq!(at(0.0, 0.0), Some(EdgeTarget::Tile(Zone::TopLeft)));
        assert_eq!(at(0.0, 200.0), Some(EdgeTarget::Tile(Zone::TopLeft)));
        assert_eq!(at(0.0, 270.0), Some(EdgeTarget::Tile(Zone::Left)));
        assert_eq!(at(1919.0, 0.0), Some(EdgeTarget::Tile(Zone::TopRight)));
        assert_eq!(at(0.0, 1079.0), Some(EdgeTarget::Tile(Zone::BottomLeft)));
        assert_eq!(at(1919.0, 1079.0), Some(EdgeTarget::Tile(Zone::BottomRight)));
        // The bottom edge alone does nothing.
        assert_eq!(at(900.0, 1079.0), None);
    }

    #[test]
    fn a_zero_distance_disables_it() {
        assert_eq!(
            edge_target(r(0, 0, 1920, 1080), ALL, Point::from((0.0, 0.0)), 0),
            None
        );
    }

    #[test]
    fn an_edge_shared_with_another_output_does_not_trigger() {
        let b = r(0, 0, 1920, 1080);
        let p = |x, y| Point::from((x, y));
        // Another output on the right: its edge is not an outer edge.
        let no_right = Outer { right: false, ..ALL };
        assert_eq!(edge_target(b, no_right, p(1919.0, 540.0), 20), None);
        assert_eq!(
            edge_target(b, no_right, p(0.0, 540.0), 20),
            Some(EdgeTarget::Tile(Zone::Left))
        );
        // One above: the top is not outer, so the top maximizes nothing and a
        // side near the top is the half, not the quarter.
        let no_top = Outer { top: false, ..ALL };
        assert_eq!(edge_target(b, no_top, p(900.0, 0.0), 20), None);
        assert_eq!(
            edge_target(b, no_top, p(0.0, 10.0), 20),
            Some(EdgeTarget::Tile(Zone::Left))
        );
    }

    #[test]
    fn outside_the_output_nothing_triggers() {
        assert_eq!(at(-1.0, 100.0), None);
        assert_eq!(at(1920.0, 100.0), None);
    }
}
