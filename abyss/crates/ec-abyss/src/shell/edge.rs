// SPDX-License-Identifier: AGPL-3.0-only
//! The resize band of server-side-decorated floating windows under `mode "de"`
//! (COMP-05 §3.1, §7.1, C-24, ADR 0090).
//!
//! A window with a compositor titlebar has a grab band just *outside* its frame
//! (the frame is `Floating.rect`: bar, content and border). Pressing the left
//! button in the band starts the existing resize grab (`input::grabs`) on the
//! edge or corner nearest; hovering sets the matching compositor cursor, and the
//! cursor is put back when the pointer leaves the band or the grab ends.
//!
//! Who has no band: a maximized or fullscreen window, a shaded one (it cannot be
//! resized), and a client-side-decorated or override-redirect window, which has
//! no titlebar and so keeps its own edges. The band is part of the window, so a
//! window stacked above covers it, and a top layer surface hides it. While the
//! pointer is in a band no client is under it (`shell::hit_under`), so no client
//! can set a cursor over it; everything is the compositor's, the same as a bar.
//!
//! Hit-testing is pure ([`edges_at`]) and nothing on the motion path allocates.

use smithay::{
    desktop::Window,
    input::pointer::{CursorIcon, CursorImageStatus},
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    utils::{Logical, Point, Rectangle},
};

use crate::state::AbyssState;

use super::{maximize, titlebar};

/// Band width outside the frame, logical px.
pub const BAND: i32 = 8;
/// How far along an edge from a corner still counts as the corner, logical px.
pub const CORNER: i32 = 16;

/// Hover state: the band the pointer is in and whether the cursor is ours.
#[derive(Debug, Default)]
pub struct EdgeUi {
    hover: Option<(Window, ResizeEdge)>,
    cursor_set: bool,
}

impl EdgeUi {
    pub fn forget(&mut self, window: &Window) {
        if self.hover.as_ref().is_some_and(|(w, _)| w == window) {
            self.hover = None;
        }
    }

    #[cfg(test)]
    pub fn hovered(&self) -> Option<ResizeEdge> {
        self.hover.as_ref().map(|(_, e)| *e)
    }

    #[cfg(test)]
    pub fn cursor_is_ours(&self) -> bool {
        self.cursor_set
    }
}

/// Which edge or corner of `frame` the point `pos` grabs, if it is in the band
/// around it. `band` is the width outside the frame; `corner` is how far along
/// an edge from the corner the corner reaches. The frame itself is not band.
pub fn edges_at(
    frame: Rectangle<i32, Logical>,
    pos: Point<f64, Logical>,
    band: i32,
    corner: i32,
) -> Option<ResizeEdge> {
    let (left, top) = (f64::from(frame.loc.x), f64::from(frame.loc.y));
    let (right, bottom) = (
        f64::from(frame.loc.x + frame.size.w),
        f64::from(frame.loc.y + frame.size.h),
    );
    let (band, corner) = (f64::from(band), f64::from(corner));
    if pos.x < left - band || pos.x >= right + band || pos.y < top - band || pos.y >= bottom + band {
        return None;
    }
    let inside_x = pos.x >= left && pos.x < right;
    let inside_y = pos.y >= top && pos.y < bottom;
    if inside_x && inside_y {
        return None;
    }
    // Outside the frame on a side, or within the corner reach of a side along
    // the edge that is outside on the other axis.
    let near_left = pos.x < left + corner;
    let near_right = pos.x >= right - corner;
    let near_top = pos.y < top + corner;
    let near_bottom = pos.y >= bottom - corner;
    let west = pos.x < left || (!inside_y && near_left);
    let east = pos.x >= right || (!inside_y && near_right);
    let north = pos.y < top || (!inside_x && near_top);
    let south = pos.y >= bottom || (!inside_x && near_bottom);
    Some(match (north, south, west, east) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

/// The compositor cursor for dragging `edge`.
pub fn icon_for(edge: ResizeEdge) -> Option<CursorIcon> {
    Some(match edge {
        ResizeEdge::Top => CursorIcon::NResize,
        ResizeEdge::Bottom => CursorIcon::SResize,
        ResizeEdge::Left => CursorIcon::WResize,
        ResizeEdge::Right => CursorIcon::EResize,
        ResizeEdge::TopLeft => CursorIcon::NwResize,
        ResizeEdge::TopRight => CursorIcon::NeResize,
        ResizeEdge::BottomLeft => CursorIcon::SwResize,
        ResizeEdge::BottomRight => CursorIcon::SeResize,
        _ => return None,
    })
}

/// May `window` have a resize band right now? Needs a titlebar (so it is server
/// side decorated, under `de`), and to be neither maximized, fullscreen nor
/// shaded.
fn has_band(state: &AbyssState, window: &Window) -> bool {
    titlebar::bar_of(window).is_some()
        && !maximize::is_maximized(state, window)
        && !state.fullscreen.contains_key(window)
        && super::winops::can_resize(state, window)
}

/// The band under `pos` (global logical) and the window it belongs to, if it is
/// the topmost thing there. No allocation.
pub fn band_at(state: &AbyssState, pos: Point<f64, Logical>) -> Option<(Window, ResizeEdge)> {
    if !state.config.mode.profile().titlebars || state.space.elements().len() == 0 {
        return None;
    }
    let border = state.config.general.border_size;
    for w in state.space.elements().rev() {
        let Some(content) = state.space.element_geometry(w) else {
            continue;
        };
        // Its frame: bar, content and border for a decorated window; whatever
        // the client shows for any other (which covers the point or not).
        let bar = titlebar::bar_of(w).map_or(0, |b| b.height);
        // A shaded window shows only its bar, so only that covers the point:
        // its hidden content takes no input and must not shield a window under.
        let frame = if bar > 0 {
            let mut shown = content;
            if state.winops.is_shaded(w) {
                shown.size.h = 0;
            }
            titlebar::outer_of_content(shown, border, bar)
        } else {
            content
        };
        if frame.to_f64().contains(pos) {
            return None;
        }
        if has_band(state, w) {
            if let Some(edge) = edges_at(frame, pos, BAND, CORNER) {
                if super::top_layer_at(state, pos) {
                    return None;
                }
                return Some((w.clone(), edge));
            }
        }
    }
    None
}

/// Show the resize cursor for `edges` (a grab started or the band is hovered).
/// Only under a mode with compositor chrome: `wm` and `hybrid` leave the
/// cursor exactly as it was during a resize grab.
pub fn show_cursor(state: &mut AbyssState, edges: ResizeEdge) {
    if !state.config.mode.profile().titlebars {
        return;
    }
    if let Some(icon) = icon_for(edges) {
        state.cursor_status = CursorImageStatus::Named(icon);
        state.edge.cursor_set = true;
        crate::backend::damage_all(state);
    }
}

/// Put the cursor back if it is the band's. Called when the band is left, a
/// resize grab ends, the session locks, or the mode is left.
pub fn restore_cursor(state: &mut AbyssState) {
    if state.edge.cursor_set {
        state.cursor_status = CursorImageStatus::default_named();
        state.edge.cursor_set = false;
        crate::backend::damage_all(state);
    }
}

/// The resize grab ended: the cursor goes back and the band is looked for again
/// at the next motion.
pub fn grab_ended(state: &mut AbyssState) {
    state.edge.hover = None;
    restore_cursor(state);
}

/// The pointer is at `pos` and the seat has been told: update the band hover and
/// the cursor. Must run *after* the seat's own motion, whose focus change
/// resets the cursor to the default. Nothing to do during a grab (a resize sets
/// its own cursor) or when the mode has no bands.
pub fn motion(state: &mut AbyssState, pos: Point<f64, Logical>) {
    if state.pointer_grab_active || !state.config.mode.profile().titlebars {
        if !state.pointer_grab_active && (state.edge.hover.is_some() || state.edge.cursor_set) {
            state.edge.hover = None;
            restore_cursor(state);
        }
        return;
    }
    let now = band_at(state, pos);
    if now == state.edge.hover {
        // The window under the cursor closed: the hover went with it, but the
        // cursor is still the band's.
        if now.is_none() && state.edge.cursor_set {
            restore_cursor(state);
        }
        // A focus change since may have reset the cursor to the default.
        if let Some((_, e)) = &now {
            if !matches!(&state.cursor_status, CursorImageStatus::Named(i) if Some(*i) == icon_for(*e)) {
                show_cursor(state, *e);
            }
        }
        return;
    }
    state.edge.hover = now.clone();
    tracing::info!(edge = ?now.as_ref().map(|(_, e)| *e), "resize band hover changed");
    match now {
        Some((_, edge)) => show_cursor(state, edge),
        None => restore_cursor(state),
    }
}

/// A pointer button. True when the band took it: the left press starts the
/// resize, any other press is eaten (and its release, via the bar's swallow
/// list). A release is never taken here: it reaches the resize grab it ends.
pub fn button(state: &mut AbyssState, button: u32, pressed: bool) -> bool {
    const BTN_LEFT: u32 = 0x110;
    if !pressed || !state.config.mode.profile().titlebars {
        return false;
    }
    let pos = state.pointer_location;
    let Some((window, edge)) = band_at(state, pos) else {
        return false;
    };
    if !state.popup_grabs.is_empty() {
        super::popup_grab_button_press(state, None);
    }
    super::focus::focus_window_raising(state, &window, true);
    if button == BTN_LEFT && crate::input::grabs::start_resize_edges(state, window, button, edge) {
        return true;
    }
    state.bar_ui.swallow(button);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((100, 100)), (400, 300).into())
    }

    fn at(x: f64, y: f64) -> Option<ResizeEdge> {
        edges_at(frame(), Point::from((x, y)), BAND, CORNER)
    }

    #[test]
    fn the_frame_itself_is_not_band() {
        assert_eq!(at(100.0, 100.0), None);
        assert_eq!(at(300.0, 250.0), None);
        assert_eq!(at(499.9, 399.9), None);
    }

    #[test]
    fn the_four_sides_are_just_outside_the_frame() {
        assert_eq!(at(99.0, 250.0), Some(ResizeEdge::Left));
        assert_eq!(at(500.0, 250.0), Some(ResizeEdge::Right));
        assert_eq!(at(300.0, 99.0), Some(ResizeEdge::Top));
        assert_eq!(at(300.0, 400.0), Some(ResizeEdge::Bottom));
    }

    #[test]
    fn the_band_is_band_wide_and_no_wider() {
        assert_eq!(at(f64::from(100 - BAND), 250.0), Some(ResizeEdge::Left));
        assert_eq!(at(f64::from(100 - BAND) - 0.1, 250.0), None);
        assert_eq!(at(f64::from(500 + BAND) - 0.1, 250.0), Some(ResizeEdge::Right));
        assert_eq!(at(f64::from(500 + BAND), 250.0), None);
        assert_eq!(at(300.0, f64::from(100 - BAND) - 0.1), None);
        assert_eq!(at(300.0, f64::from(400 + BAND)), None);
    }

    #[test]
    fn the_four_corners_are_squares_outside_the_frame() {
        assert_eq!(at(95.0, 95.0), Some(ResizeEdge::TopLeft));
        assert_eq!(at(503.0, 95.0), Some(ResizeEdge::TopRight));
        assert_eq!(at(95.0, 403.0), Some(ResizeEdge::BottomLeft));
        assert_eq!(at(503.0, 403.0), Some(ResizeEdge::BottomRight));
    }

    #[test]
    fn a_side_reaches_into_the_corner_along_the_edge() {
        // Just left of the frame but within CORNER of the top: the corner.
        assert_eq!(
            at(95.0, 100.0 + f64::from(CORNER) - 1.0),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(at(95.0, 100.0 + f64::from(CORNER)), Some(ResizeEdge::Left));
        assert_eq!(
            at(503.0, 400.0 - f64::from(CORNER)),
            Some(ResizeEdge::BottomRight)
        );
        assert_eq!(
            at(503.0, 400.0 - f64::from(CORNER) - 1.0),
            Some(ResizeEdge::Right)
        );
        // Likewise above the frame near a side.
        assert_eq!(
            at(100.0 + f64::from(CORNER) - 1.0, 95.0),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(at(100.0 + f64::from(CORNER), 95.0), Some(ResizeEdge::Top));
        assert_eq!(
            at(500.0 - f64::from(CORNER), 403.0),
            Some(ResizeEdge::BottomRight)
        );
    }

    #[test]
    fn far_from_the_frame_is_nothing() {
        assert_eq!(at(0.0, 0.0), None);
        assert_eq!(at(300.0, 700.0), None);
        assert_eq!(at(95.0, 700.0), None);
    }

    #[test]
    fn every_edge_has_a_cursor() {
        use ResizeEdge as E;
        for e in [
            E::Top,
            E::Bottom,
            E::Left,
            E::Right,
            E::TopLeft,
            E::TopRight,
            E::BottomLeft,
            E::BottomRight,
        ] {
            assert!(icon_for(e).is_some(), "{e:?}");
        }
        assert_eq!(icon_for(E::None), None);
        assert_eq!(icon_for(E::TopLeft), Some(CursorIcon::NwResize));
        assert_eq!(icon_for(E::Right), Some(CursorIcon::EResize));
    }
}
