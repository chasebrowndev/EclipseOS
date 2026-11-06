// SPDX-License-Identifier: AGPL-3.0-only
//! Maximize under `mode "de"` (COMP-05 §3.1, amendment C-24, ADR 0090).
//!
//! A maximized window is a floating window whose [`Placement`] is `Maximized`:
//! its rectangle is not placed but derived on every arrange from the output's
//! usable area (the output minus layer-shell exclusive zones), so a panel
//! appearing or an output resizing is followed. It is drawn square and without
//! a border, like a fullscreen window, and the placement keeps the rectangle to
//! give back. Fullscreen wins over it and leaving fullscreen returns to it.
//!
//! Quick Tile (`shell::quicktile`) is the other placement of this kind and
//! shares the restore code here: leaving either gives back the same rectangle,
//! and a drag out of either puts the window under the pointer.
//!
//! Outside `de` none of this runs: the requests are declined (C-20,
//! [`super::decline_maximize_toplevel`]). The gate is `ModeProfile::maximize`,
//! checked at each entry point, and [`clear_all`] runs when the mode is left.

use smithay::{
    desktop::{layer_map_for_output, Window},
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State,
    utils::{Logical, Point, Rectangle},
    wayland::shell::xdg::ToplevelSurface,
};

use crate::state::AbyssState;

use super::{
    anim, arrange, clamp_size, de, shrink, tiling_area,
    titlebar::{self, content_of_outer},
    usable_area,
    workspace::{Floating, Placement},
};

/// The rectangle a maximized window takes: the usable area along each set axis,
/// and `restore` (shrunk to its content) along the other. It is the window's
/// own rectangle, so the caller adds no border inset.
pub(super) fn maximized_rect(
    usable: Rectangle<i32, Logical>,
    border: i32,
    restore: Rectangle<i32, Logical>,
    h: bool,
    v: bool,
) -> Rectangle<i32, Logical> {
    let own = shrink(restore, border);
    let (x, w) = if h {
        (usable.loc.x, usable.size.w)
    } else {
        (own.loc.x, own.size.w)
    };
    let (y, ht) = if v {
        (usable.loc.y, usable.size.h)
    } else {
        (own.loc.y, own.size.h)
    };
    Rectangle::new(Point::from((x, y)), (w, ht).into())
}

/// [`maximized_rect`] held to the client's size hints, as `arrange` configures
/// it: a window with a max size is smaller than the area and reports so.
///
/// The rectangle is the frame (`bar` px of titlebar above the content), and the
/// size hints apply to the content, so the content is `bar` shorter than the
/// frame and a window held to a max size still reports it exactly.
pub(super) fn fitted_rect(
    window: &Window,
    usable: Rectangle<i32, Logical>,
    border: i32,
    restore: Rectangle<i32, Logical>,
    h: bool,
    v: bool,
    bar: i32,
) -> Rectangle<i32, Logical> {
    let mut rect = maximized_rect(usable, border, restore, h, v);
    let content = clamp_size(window, (rect.size.w, (rect.size.h - bar).max(1)).into(), false);
    rect.size = (content.w, content.h + bar).into();
    rect
}

/// Where a floating window's content sits for `f`: a maximized or fullscreen
/// window has no border inset.
pub(super) fn content_rect(f: &Floating, fullscreen: bool, border: i32, bar: i32) -> Rectangle<i32, Logical> {
    if fullscreen {
        f.rect
    } else if f.placement.is_placed() {
        content_of_outer(f.rect, 0, bar)
    } else {
        content_of_outer(f.rect, border, bar)
    }
}

/// Is `window` maximized (along any axis)? No allocation.
pub fn is_maximized(state: &AbyssState, window: &Window) -> bool {
    placement_of(state, window).is_maximized()
}

/// Is `window` maximized or Quick Tiled? Either is released by a drag.
pub fn is_placed(state: &AbyssState, window: &Window) -> bool {
    placement_of(state, window).is_placed()
}

/// The placement of `window` wherever it floats or is minimized; `Free` for a
/// window that is neither.
pub(super) fn placement_of(state: &AbyssState, window: &Window) -> Placement {
    for entry in state.outputs.iter() {
        for ws in &entry.workspaces {
            if let Some(f) = ws.floating.iter().find(|f| &f.window == window) {
                return f.placement;
            }
            if let Some(m) = ws.minimized.iter().find(|m| &m.window == window) {
                return m.placement;
            }
        }
    }
    Placement::Free
}

/// Put a placement back on a window that was re-installed (`replace_window`).
pub(super) fn restore_placement(state: &mut AbyssState, window: &Window, placement: Placement) {
    if !placement.is_placed() {
        return;
    }
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            if let Some(f) = ws.floating.iter_mut().find(|f| &f.window == window) {
                f.placement = placement;
                return;
            }
        }
    }
}

/// Maximize `window` along the axes (`Some`), or restore it (`None`). False
/// when nothing was done: not `de`, or the window is not floating on a
/// workspace (minimized, not yet placed).
pub fn set(state: &mut AbyssState, window: &Window, axes: Option<(bool, bool)>) -> bool {
    if !state.config.mode.profile().maximize {
        return false;
    }
    // Maximize restores a rolled-up window (`winops`); restoring from maximize
    // leaves it as it is.
    if axes.is_some() {
        super::winops::unshade(state, window);
    }
    let mut done = false;
    anim::toggle(state, Some(window.clone()), |state| {
        done = set_now(state, window, axes, None);
    });
    done
}

/// The `toggle-maximize` bind (Super+PageUp under `de`).
pub fn toggle(state: &mut AbyssState) {
    let Some(window) = state.focus.clone() else { return };
    toggle_window(state, &window);
}

/// Maximize `window` if it is not, restore it if it is (the bind's body, and
/// the titlebar's maximize button, which acts on its own window).
pub fn toggle_window(state: &mut AbyssState, window: &Window) {
    let on = !is_maximized(state, window);
    set(state, window, on.then_some((true, true)));
}

/// The body of [`set`]. `pointer`: also move the restored window so that point
/// stays inside it, in the same proportion as before (a drag leaving maximize).
fn set_now(
    state: &mut AbyssState,
    window: &Window,
    axes: Option<(bool, bool)>,
    pointer: Option<Point<f64, Logical>>,
) -> bool {
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
    let area = tiling_area(state, &output);
    let border = state.config.general.border_size;
    let fullscreen = state.fullscreen.contains_key(window);
    let fullscreen_restore = state.fullscreen.get(window).copied().flatten();
    let default_rect = de::default_float_rect(state, id, area);
    let bar = titlebar::bar_height(state, window);

    let entry = state.outputs.get_mut(id).expect("just found");
    let f = entry.workspaces[ws]
        .floating
        .iter_mut()
        .find(|f| &f.window == window)
        .expect("just found");
    // Where the window is now, from its placement (a client's buffer may lag).
    let old = content_rect(f, fullscreen, border, bar);
    let axes = axes.filter(|(h, v)| *h || *v);
    let mut restored_fullscreen = None;
    match (axes, f.placement) {
        (Some((h, v)), placement) => {
            // A maximize that replaces a tile keeps the tile's restore rect, so
            // leaving it gives back the original free rectangle.
            let restore = match placement.restore_rect() {
                Some(restore) => restore,
                None if fullscreen => fullscreen_restore.unwrap_or(default_rect),
                None => f.rect,
            };
            f.placement = Placement::Maximized { h, v, restore };
            if !fullscreen {
                f.rect = fitted_rect(&f.window, usable, border, restore, h, v, bar);
            }
        }
        (None, Placement::Maximized { restore, .. } | Placement::QuickTile { restore, .. }) => {
            // The area may have moved since: keep the restored window on it.
            let mut rect = de::carried_rect(restore, Point::default(), usable);
            if let Some(p) = pointer {
                let new = content_of_outer(rect, border, bar);
                let at = |p: f64, o: i32, ol: i32, nl: i32| {
                    p - (p - f64::from(o)) * f64::from(nl) / f64::from(ol.max(1))
                };
                let x = at(p.x, old.loc.x, old.size.w, new.size.w).round() as i32;
                // A pointer on the bar stays the same distance below the bar's
                // top (the x above keeps its place along the bar); one in the
                // content keeps its proportion of the content.
                let y = if bar > 0 && p.y < f64::from(old.loc.y) {
                    let below_top = p.y - f64::from(old.loc.y - bar);
                    (p.y - below_top + f64::from(bar)).round() as i32
                } else {
                    at(p.y, old.loc.y, old.size.h, new.size.h).round() as i32
                };
                rect.loc = Point::from((x - border, y - border - bar));
            }
            f.placement = Placement::Free;
            if fullscreen {
                restored_fullscreen = Some(rect);
            } else {
                f.rect = rect;
            }
        }
        (None, Placement::Free) => {}
    }
    let placement = f.placement;
    let content = content_rect(f, fullscreen, border, bar);
    if let Some(rect) = restored_fullscreen {
        state.fullscreen.insert(window.clone(), Some(rect));
    }
    if !fullscreen {
        tell_client(window, placement, content);
    }
    arrange(state);
    true
}

/// Send the client the size and the `Maximized` state it now has.
fn tell_client(window: &Window, placement: Placement, content: Rectangle<i32, Logical>) {
    let size = clamp_size(window, content.size, false);
    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|s| {
            s.size = Some(size);
            if placement.is_fully_maximized() {
                s.states.set(State::Maximized);
            } else {
                s.states.unset(State::Maximized);
            }
        });
        toplevel.send_configure();
    } else if let Some(x11) = window.x11_surface() {
        // `_NET_WM_STATE` has one bit per axis, but smithay 0.7 can set only
        // both together, so a single-axis placement is not advertised.
        let _ = x11.set_maximized(placement.is_fully_maximized());
    }
}

/// xdg `set_maximized` / `unset_maximized` under `de`. Before the window exists
/// (not yet placed, before the initial configure or after an unmap) the request
/// is parked in the pending state, which `map_toplevel` replays at map.
pub fn request_toplevel(state: &mut AbyssState, surface: &ToplevelSurface, on: bool) {
    // Not `toplevel_window`: a window on a hidden workspace is not in the space
    // but is placed, and maximizes like any other.
    let window = super::owned_windows(state)
        .into_iter()
        .find(|w| w.toplevel() == Some(surface));
    let Some(window) = window else {
        surface.with_pending_state(|s| {
            if on {
                s.states.set(State::Maximized);
            } else {
                s.states.unset(State::Maximized);
            }
        });
        return;
    };
    // Minimized (or pending adoption): nothing to maximize now.
    if !set(state, &window, on.then_some((true, true))) {
        super::decline_maximize_toplevel(state, surface);
    }
}

/// X11 `_NET_WM_STATE_MAXIMIZED_*` under `de`. smithay 0.7 delivers a request
/// only when both axes are in one message, so the callers pass both.
pub fn request_x11(state: &mut AbyssState, window: &Window, h: bool, v: bool, on: bool) {
    if !set(state, window, on.then_some((h, v))) {
        if let Some(x11) = window.x11_surface() {
            let _ = x11.set_maximized(false);
        }
    }
}

/// A move or resize of a maximized window by the human (Alt+drag) or its client
/// begins: unmaximize first, and put the restored window under the pointer.
pub fn release_for_drag(state: &mut AbyssState, window: &Window) {
    // Fullscreen wins: it keeps the placement for when it ends (C-19).
    if !is_placed(state, window) || state.fullscreen.contains_key(window) {
        return;
    }
    let pointer = state.pointer_location;
    set_now(state, window, None, Some(pointer));
}

/// A client or the shell pins a maximized window to an exact rectangle
/// (`place_at`, `resize_window`): it is no longer maximized. The client is told.
pub(super) fn release(state: &mut AbyssState, window: &Window) {
    if !is_placed(state, window) {
        return;
    }
    set_now(state, window, None, None);
}

/// A window of a departing output keeps its maximize through the hand-over.
pub(crate) fn note_departing(state: &mut AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    // The restore rectangle is kept relative to the departing output.
    let origin = state
        .space
        .output_geometry(&entry.output)
        .map_or(Point::default(), |g| g.loc);
    let carried: Vec<(Window, Placement)> = entry
        .workspaces
        .iter()
        .flat_map(|ws| {
            let floating = ws.floating.iter().map(|f| (f.window.clone(), f.placement));
            let minimized = ws.minimized.iter().map(|m| (m.window.clone(), m.placement));
            floating.chain(minimized)
        })
        .filter(|(_, p)| p.is_placed())
        .map(|(w, p)| (w, p.moved_by(Point::from((-origin.x, -origin.y)))))
        .collect();
    for (w, p) in carried {
        state.de.carry(w, p);
    }
}

/// Leaving `de`: every window gets its pre-maximize rectangle back and its
/// client is told, before the re-tile logic runs (`de::leave`). Runs after the
/// new mode is installed, so it does not consult the profile.
#[allow(clippy::mutable_key_type)]
pub(super) fn clear_all(state: &mut AbyssState) {
    let border = state.config.general.border_size;
    let mut told: Vec<(Window, Rectangle<i32, Logical>)> = Vec::new();
    let mut fullscreen_restores: Vec<(Window, Rectangle<i32, Logical>)> = Vec::new();
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            for f in ws.floating.iter_mut() {
                let Some(restore) = f.placement.restore_rect() else {
                    continue;
                };
                f.placement = Placement::Free;
                if state.fullscreen.contains_key(&f.window) {
                    // Fullscreen stays; what it returns to is the old rectangle.
                    fullscreen_restores.push((f.window.clone(), restore));
                } else {
                    f.rect = restore;
                    told.push((f.window.clone(), shrink(restore, border)));
                }
            }
            for m in ws.minimized.iter_mut() {
                if let Some(restore) = m.placement.restore_rect() {
                    m.placement = Placement::Free;
                    m.was_floating = Some(restore);
                    told.push((m.window.clone(), shrink(restore, border)));
                }
            }
        }
    }
    for (w, restore) in fullscreen_restores {
        state.fullscreen.insert(w, Some(restore));
    }
    for (window, content) in told {
        tell_client(&window, Placement::Free, content);
    }
    state.de.drop_carried();
}
