// SPDX-License-Identifier: AGPL-3.0-only
//! Window management: placement, layout, workspaces, focus (COMP-05).

pub mod anim;
pub mod de;
#[cfg(test)]
mod de_chrome_tests;
pub mod desktop;
#[cfg(test)]
mod desktop_tests;
mod drop_guides;
pub mod edge;
pub mod focus;
pub mod layout;
pub mod maximize;
pub mod quicktile;
#[cfg(test)]
mod quicktile_tests;
pub mod rules;
pub mod snap;
pub mod titlebar;
mod toplevel;
pub mod winmenu;
pub mod winops;
pub mod workspace;

// The focus path lives in `focus`, but every existing call site says
// `shell::focus_window` (ADR 0042 makes `focus::apply_focus` the path they all
// grow into, one caller at a time).
pub use focus::{focus_surface, focus_window, refocus_topmost};
// Likewise the drag-to-tile and toplevel-map entry points.
pub use drop_guides::*;
use toplevel::has_buffer;
pub use toplevel::{handle_commit, reset_toplevel_map};

use std::cell::Cell;
use std::time::Instant;

use smithay::{
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, LayerMap,
        LayerSurface as DesktopLayerSurface, PopupKind, PopupManager, Window, WindowSurfaceType,
    },
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, Rectangle, Size, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurfaceData},
            xdg::{PopupSurface, SurfaceCachedState, XdgPopupSurfaceData, XdgToplevelSurfaceData},
        },
    },
};

use self::titlebar::{content_of_outer, outer_of_content};
use crate::{
    config::{FloatingPlacement, LayoutKind},
    input::Direction,
    protocols::standard::fractional_scale,
    shell::workspace::{Floating, Placement},
    state::AbyssState,
};

/// The `wl_surface` backing a window, xdg or X11 (COMP-07 §1).
pub fn window_surface(window: &Window) -> Option<WlSurface> {
    use smithay::desktop::WindowSurface;
    match window.underlying_surface() {
        WindowSurface::Wayland(t) => Some(t.wl_surface().clone()),
        WindowSurface::X11(s) => s.wl_surface(),
    }
}

// Read by the renderer, written here; defined in `ec-abyss-render::userdata`.
pub use ec_abyss_render::userdata::{
    layer_geometry, owner_output, tile_clip, LayerOffset, OwnerOutput, QuickTiled, TileClip,
};

fn set_owner_output(window: &Window, output: &Output) {
    let data = window.user_data();
    data.insert_if_missing(|| OwnerOutput(Cell::new(None)));
    if let Some(o) = data.get::<OwnerOutput>() {
        o.0.set(Some(output.clone()));
    }
}

fn set_quick_tiled(window: &Window, on: bool) {
    let data = window.user_data();
    data.insert_if_missing(|| QuickTiled(Cell::new(false)));
    if let Some(c) = data.get::<QuickTiled>() {
        c.0.set(on);
    }
}

fn set_tile_clip(window: &Window, rect: Option<Rectangle<i32, Logical>>) {
    let data = window.user_data();
    data.insert_if_missing(|| TileClip(Cell::new(None)));
    if let Some(c) = data.get::<TileClip>() {
        c.0.set(rect);
    }
}

/// The output that owns focus. `None` only when there is no output at all:
/// `Outputs::add` adopts focus for the first entry and `unregister` calls
/// `repair_focus`, so the focused id dangles nowhere in between (ADR 0042).
/// Callers therefore treat `None` as "no display attached", not as "unknown".
pub fn focused_output(state: &AbyssState) -> Option<Output> {
    state.outputs.focused().map(|e| e.output.clone())
}

/// The output whose logical area contains `pos`, else the focused output.
pub fn output_at(state: &AbyssState, pos: Point<f64, Logical>) -> Option<Output> {
    state
        .space
        .output_under(pos)
        .next()
        .cloned()
        .or_else(|| focused_output(state))
}

/// The handle of the output whose workspaces hold `window`.
pub fn output_of_window(state: &AbyssState, window: &Window) -> Option<u64> {
    state
        .outputs
        .iter()
        .find(|e| e.workspaces.iter().any(|ws| ws.holds(window)))
        .map(|e| e.id)
}

/// Usable area: output geometry minus layer-shell exclusive zones, with no
/// outer gap applied: the raw protocol-defined usable region, independent of
/// abyss's own gap styling (COMP-06 §3).
pub(crate) fn usable_area(state: &AbyssState, output: &Output) -> Rectangle<i32, Logical> {
    let geo = state.space.output_geometry(output).unwrap_or_default();
    let zone = accumulate_non_exclusive_zone(output, geo.size);
    let loc = Point::from((geo.loc.x + zone.loc.x, geo.loc.y + zone.loc.y));
    let size = Size::from((zone.size.w.max(1), zone.size.h.max(1)));
    Rectangle::new(loc, size)
}

/// The output's non-exclusive zone, accumulated by hand.
///
/// `LayerMap::non_exclusive_zone` cannot be used here: smithay honours a
/// non-zero exclusive zone for *any* anchor that touches an edge, including
/// two adjacent edges, two opposite edges, no edge at all, and all four (which
/// it collapses to a zero-sized zone). wlr-layer-shell only defines an
/// exclusive zone when the surface is anchored to exactly one edge, or to
/// three edges — i.e. when there is exactly one "attached edge". wlroots and
/// Mir ignore the zone for every other anchor set, and wlcs asserts it
/// (`maximized_xdg_toplevel_is_shrunk_for_exclusive_zone`, parametrized over
/// all sixteen anchor combinations). COMP-06 §3.
fn accumulate_non_exclusive_zone(
    output: &Output,
    output_size: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let mut zone = Rectangle::from_size(output_size);
    for layer in layer_map_for_output(output).layers() {
        let data = layer.cached_state();
        let ExclusiveZone::Exclusive(amount) = data.exclusive_zone else {
            continue;
        };
        let amount = amount as i32;
        let a = data.anchor;
        let (l, r, t, b) = (
            a.contains(Anchor::LEFT),
            a.contains(Anchor::RIGHT),
            a.contains(Anchor::TOP),
            a.contains(Anchor::BOTTOM),
        );
        // A vertical edge is attached when the surface spans (or spans neither
        // of) top and bottom and picks exactly one of left/right; a horizontal
        // edge, the mirror. Anything else claims nothing.
        if t == b && l != r {
            if l {
                let take = amount + data.margin.left;
                zone.loc.x += take;
                zone.size.w -= take;
            } else {
                zone.size.w -= amount + data.margin.right;
            }
        } else if l == r && t != b {
            if t {
                let take = amount + data.margin.top;
                zone.loc.y += take;
                zone.size.h -= take;
            } else {
                zone.size.h -= amount + data.margin.bottom;
            }
        }
    }
    zone
}

/// Usable tiling area: the usable area minus the outer gap.
fn tiling_area(state: &AbyssState, output: &Output) -> Rectangle<i32, Logical> {
    let area = usable_area(state, output);
    let gx = state.config.general.gaps_out;
    let gy = state.config.general.gaps_out_y();
    let loc = Point::from((area.loc.x + gx, area.loc.y + gy));
    let size = Size::from(((area.size.w - 2 * gx).max(1), (area.size.h - 2 * gy).max(1)));
    Rectangle::new(loc, size)
}

fn shrink(r: Rectangle<i32, Logical>, by: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (r.loc.x + by, r.loc.y + by).into(),
        Size::from(((r.size.w - 2 * by).max(1), (r.size.h - 2 * by).max(1))),
    )
}

/// Clamp against the client's advertised min/max. A max smaller than the area
/// always wins. A min larger than the area wins only for floating windows: a
/// tiled window is held to its tile, so a large advertised min (Electron apps
/// ask for ~940 logical px) cannot push it under its neighbour.
fn clamp_size(window: &Window, size: Size<i32, Logical>, tiled: bool) -> Size<i32, Logical> {
    let Some(toplevel) = window.toplevel() else {
        let Some(x11) = window.x11_surface() else {
            return size;
        };
        let min = x11.min_size().unwrap_or_default();
        let max = x11.max_size().unwrap_or_default();
        return clamp_to(size, min, max, tiled);
    };
    with_states(toplevel.wl_surface(), |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        let d = *guard.current();
        clamp_to(size, d.min_size, d.max_size, tiled)
    })
}

/// Pure min/max clamp behind [`clamp_size`]. A zero max component means
/// "unbounded"; `min` is ignored when `tiled`.
fn clamp_to(
    mut size: Size<i32, Logical>,
    min: Size<i32, Logical>,
    max: Size<i32, Logical>,
    tiled: bool,
) -> Size<i32, Logical> {
    if max.w > 0 {
        size.w = size.w.min(max.w);
    }
    if max.h > 0 {
        size.h = size.h.min(max.h);
    }
    if !tiled {
        size.w = size.w.max(min.w);
        size.h = size.h.max(min.h);
    }
    size.w = size.w.max(1);
    size.h = size.h.max(1);
    size
}

/// Send the client its content rectangle, activation, the `Tiled*` states for
/// `tiled` (every edge for a Radiant tile, the zone's for a Quick Tile, none
/// for a free window) and `Maximized`. An X11 window gets the rectangle and
/// activation only: it has no tiled or maximized state here (see
/// [`quicktile`]).
fn configure(
    state: &mut AbyssState,
    window: &Window,
    rect: Rectangle<i32, Logical>,
    activated: bool,
    tiled: quicktile::Edges,
    maximized: bool,
) {
    let size = rect.size;
    let Some(toplevel) = window.toplevel() else {
        // X11 windows are positioned in the X root's coordinate space, so
        // they need the whole rectangle, not just a size.
        if let Some(x11) = window.x11_surface() {
            x11.set_activated(activated).ok();
            crate::xwayland::sync_on_configure(state, x11, rect);
            let _ = x11.configure(rect);
        }
        return;
    };
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
    toplevel.with_pending_state(|s| {
        s.size = Some(size);
        if activated {
            s.states.set(State::Activated);
        } else {
            s.states.unset(State::Activated);
        }
        // Tiled states stop clients like foot snapping to whole cells; smithay
        // strips them for xdg_wm_base versions that predate them.
        for (t, on) in [
            (State::TiledLeft, tiled.left),
            (State::TiledRight, tiled.right),
            (State::TiledTop, tiled.top),
            (State::TiledBottom, tiled.bottom),
        ] {
            if on {
                s.states.set(t);
            } else {
                s.states.unset(t);
            }
        }
        // Only `de` ever sets it (C-24); elsewhere this keeps it unset.
        if maximized {
            s.states.set(State::Maximized);
        } else {
            s.states.unset(State::Maximized);
        }
    });
    toplevel.send_pending_configure();
}

/// Order an output's layer map so every surface claiming an exclusive zone is
/// arranged before the surfaces that have to avoid it (COMP-06 §3).
///
/// Smithay's `LayerMap::arrange` accumulates the non-exclusive zone in a single
/// pass over the map's insertion order, so a surface mapped before a panel is
/// laid out against the *full* output and never re-centred once the panel claims
/// its strip. wlroots and Mir avoid this by running an exclusive pass first;
/// with the map's order being the only knob smithay exposes, sorting the
/// claimants to the front is the equivalent. Relative order is otherwise
/// preserved, so stacking within a layer is unchanged.
fn order_layers(output: &Output) {
    let mut map = layer_map_for_output(output);
    let current: Vec<_> = map.layers().cloned().collect();
    let mut wanted = current.clone();
    wanted.sort_by_key(|l| !matches!(l.cached_state().exclusive_zone, ExclusiveZone::Exclusive(n) if n > 0));
    if wanted == current {
        return;
    }
    // `unmap_layer` clears the cached location, which `map_layer` requires.
    for l in &current {
        map.unmap_layer(l);
    }
    for l in &wanted {
        if let Err(err) = map.map_layer(l) {
            tracing::warn!(?err, "re-mapping layer surface while ordering");
        }
    }
}

/// A layout pass held back while a tiled window's close ghost plays out, so
/// the windows that stay do not slide into its space before it is gone.
///
/// Render-only apart from the delay: the closed window is already out of the
/// layout tree, `space` and the listing, and the neighbours keep the geometry
/// they were last arranged at (that is what hit-testing and `get_tree` see
/// and what is drawn). Layout passes in the meantime (a focus change, a bar
/// resizing as the chip leaves, a popup) still run, but leave the held
/// workspace's tiles where they are. The hold ends, and the tiles are laid
/// out in full, when the ghost is done or the held workspace's tiled set,
/// tiling area or active index changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetileHold {
    None,
    /// `unmap_window` is finishing; its own passes are the ones deferred.
    Settling,
    /// Waiting for the ghost: `output`'s workspace `ws`, holding `tiled`
    /// windows in `area`. Anything else there means the hold is stale.
    Held {
        output: u64,
        ws: usize,
        tiled: usize,
        area: Rectangle<i32, Logical>,
    },
}

/// End the hold once its ghost is done. Called before each frame is built,
/// so a ghost that is retired in that frame already sees the final layout.
/// A ghost that never draws is done after the armed-clock grace, so the hold
/// cannot outlive it.
pub fn tick_retile_hold(state: &mut AbyssState) {
    if matches!(state.retile_hold, RetileHold::Held { .. }) && !state.borders.anim.holding(Instant::now()) {
        state.retile_hold = RetileHold::None;
        arrange(state);
    }
}

/// The output whose layout closing `window` should hold: it is tiled on that
/// output's active workspace and its close ghost is playing (so animations
/// are on, and not reduced to nothing).
fn holds_retile(state: &AbyssState, window: &Window) -> Option<u64> {
    if !state.config.animations.motion()
        || !state.borders.anim.close_playing(window, Instant::now())
        || state.tile_drag.as_ref().is_some_and(|d| &d.window == window)
    {
        return None;
    }
    state
        .outputs
        .iter()
        .find(|e| e.workspaces[e.active].tiled.windows().contains(window))
        .map(|e| e.id)
}

/// Re-run the layout for every output's active workspace.
pub fn arrange(state: &mut AbyssState) {
    if state.retile_hold == RetileHold::Settling {
        // Still repaint: the ghost has to be drawn.
        crate::backend::damage_all(state);
        return;
    }
    state.winops.prune();
    winmenu::validate(state);
    crate::overview::validate(state);
    let ids: Vec<u64> = state.outputs.iter().map(|e| e.id).collect();
    for id in ids {
        arrange_output(state, id);
    }
    raise_override_redirect(state);
    crate::xwayland::sync_stacking(state);
    refresh_reactive_popups(state);
    crate::overview::corners::rebuild(state);
    // Layout changes are not driven by a client commit: an unmapped surface
    // sends nothing, so without this the last composite stays on screen and a
    // closed window or dismissed launcher goes on being visible until some
    // other client happens to commit a frame.
    crate::backend::damage_all(state);
}

/// Keep override-redirect X11 windows (menus, tooltips) above every managed
/// window. `arrange_output` raises each floating window, which would bury a
/// menu that mapped earlier — then it is drawn and hit-tested underneath.
fn raise_override_redirect(state: &mut AbyssState) {
    if state.xwayland.unmanaged.is_empty() {
        return;
    }
    let menus: Vec<Window> = state
        .space
        .elements()
        .filter(|w| is_unmanaged_x11(state, w))
        .cloned()
        .collect();
    for w in menus {
        state.space.raise_element(&w, false);
    }
}

/// An X11 window the client positions itself: override-redirect, or a managed
/// popup-type one (menu, tooltip) mapped outside any workspace.
pub fn is_unmanaged_x11(state: &AbyssState, window: &Window) -> bool {
    window
        .x11_surface()
        .is_some_and(|x| x.is_override_redirect() || state.xwayland.unmanaged.contains(x))
}

/// The content rectangle the layout gave a floating `window` (what its client
/// is told), as opposed to the size it has drawn so far. `None` for a window
/// that is not floating.
pub fn stored_content(state: &AbyssState, window: &Window) -> Option<Rectangle<i32, Logical>> {
    let border = state.config.general.border_size;
    let bar = titlebar::bar_height(state, window);
    let fullscreen = state.fullscreen.contains_key(window);
    state
        .outputs
        .iter()
        .flat_map(|e| e.workspaces.iter())
        .flat_map(|ws| ws.floating.iter())
        .find(|f| &f.window == window)
        .map(|f| maximize::content_rect(f, fullscreen, border, bar))
}

/// Whether `window` is floating on some workspace.
pub fn is_floating(state: &AbyssState, window: &Window) -> bool {
    state.outputs.iter().any(|e| {
        e.workspaces
            .iter()
            .any(|ws| ws.floating.iter().any(|f| &f.window == window))
    })
}

/// Lay out one output's active workspace and remap it into the space.
pub fn arrange_output(state: &mut AbyssState, id: u64) {
    if state.outputs.get(id).is_none() {
        return;
    }
    // Sticky windows follow the active workspace before anything is laid out.
    winops::sync_sticky(state, id);
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let output = entry.output.clone();
    let ws = entry.active;
    order_layers(&output);
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    // A fullscreen toplevel takes the whole output, exclusive zones included, so
    // it tracks the raw geometry rather than the usable area (COMP-05 §4).
    let full_area = state.space.output_geometry(&output).unwrap_or_default();
    let pointer = state.pointer_location;
    {
        // `Window` hashes by its stable `ObjectId`; the interior mutability
        // clippy flags lives in fields that take no part in `Hash`/`Eq`, so
        // it is sound as a key. This is smithay's own idiom.
        #[allow(clippy::mutable_key_type)]
        let fullscreen = &state.fullscreen;
        let entry = state.outputs.get_mut(id).expect("checked above");
        // A floating rectangle is absolute and was computed against the area
        // in force when it was set, so it is only stale when that area moved
        // or resized under it. Re-clamping on every pass instead would fight
        // `place_at`, which pins a window to an exact rectangle that may
        // legitimately hang off the tiling area (a client-driven move, a
        // negative coordinate, a window under the bar).
        let area_changed = entry.workspaces[ws].last_area.is_some_and(|prev| prev != area);
        entry.workspaces[ws].last_area = Some(area);
        for f in entry.workspaces[ws].floating.iter_mut() {
            if fullscreen.contains_key(&f.window) {
                f.rect = full_area;
            } else if f.placement.is_placed() {
                // Derived below, once adopted windows are in.
            } else if area_changed {
                // The area moved or resized under this window — a mode/scale
                // change, a bar fold, a reconfigured layout, or a monitor swap
                // under a stashed window (`OutputRegistry::add`/`remove`).
                // Re-clamp so it never drifts off-screen; this mirrors the
                // pointer-spawn clamp in `install` below, including the
                // `.max()` guard for a window wider/taller than the output.
                let max_x = (area.loc.x + area.size.w - f.rect.size.w).max(area.loc.x);
                let max_y = (area.loc.y + area.size.h - f.rect.size.h).max(area.loc.y);
                f.rect.loc.x = f.rect.loc.x.clamp(area.loc.x, max_x);
                f.rect.loc.y = f.rect.loc.y.clamp(area.loc.y, max_y);
            }
        }
    }

    // Adopt anything handed over by an output that went away.
    let pending: Vec<Window> = {
        let entry = state.outputs.get_mut(id).expect("checked above");
        std::mem::take(&mut entry.workspaces[ws].pending)
    };
    let stacking = de::stacking(state);
    let adopt_rect = if stacking && !pending.is_empty() {
        de::default_float_rect(state, id, area)
    } else {
        Rectangle::default()
    };
    for (i, w) in pending.into_iter().enumerate() {
        if stacking {
            // Step each adopted window along a diagonal so none hides another.
            let step = Point::from((32 * i as i32, 32 * i as i32));
            let mut rect = de::carried_rect(adopt_rect, step, area);
            // A window that was maximized stays so, filling this output, and
            // keeps the rectangle it had before (kept in the adopter's area).
            let origin = state
                .space
                .output_geometry(&output)
                .map_or(Point::default(), |g| g.loc);
            let placement = match state.de.take_carried(&w) {
                Some(Placement::Maximized { h, v, restore }) => {
                    let restore = de::carried_rect(restore, origin, area);
                    rect = restore;
                    Placement::Maximized { h, v, restore }
                }
                // A Quick Tile keeps its zone on the adopting output.
                Some(Placement::QuickTile { zone, restore }) => {
                    let restore = de::carried_rect(restore, origin, area);
                    rect = restore;
                    Placement::QuickTile { zone, restore }
                }
                _ => Placement::Free,
            };
            let entry = state.outputs.get_mut(id).expect("checked above");
            entry.workspaces[ws].floating.push(Floating {
                window: w,
                rect,
                weight: None,
                placement,
            });
        } else {
            let entry = state.outputs.get_mut(id).expect("checked above");
            entry.workspaces[ws].tiled.insert(w, None, area, pointer);
        }
    }
    if stacking {
        de::lift_stray(state, id, area);
    }
    // A maximized or Quick Tiled window fills its area of the usable area as it
    // is now, on every pass, so a panel appearing or an output resizing is
    // followed (COMP-05 §3.1).
    {
        // The frame fills the usable area, so the bar eats into the content.
        #[allow(clippy::mutable_key_type)]
        let bar_heights: Vec<(Window, i32)> = state.outputs.get(id).expect("checked above").workspaces[ws]
            .floating
            .iter()
            .filter(|f| f.placement.is_placed())
            .map(|f| (f.window.clone(), titlebar::bar_height(state, &f.window)))
            .collect();
        let bars = |w: &Window| bar_heights.iter().find(|(b, _)| b == w).map_or(0, |(_, h)| *h);
        #[allow(clippy::mutable_key_type)]
        let fullscreen = &state.fullscreen;
        let border = state.config.general.border_size;
        let gaps = (state.config.general.gaps_in, state.config.general.gaps_in_y());
        let entry = state.outputs.get_mut(id).expect("checked above");
        for f in entry.workspaces[ws].floating.iter_mut() {
            if !fullscreen.contains_key(&f.window) {
                if let Some(rect) =
                    quicktile::placed_rect(&f.window, f.placement, area, gaps, border, bars(&f.window))
                {
                    f.rect = rect;
                }
            }
        }
    }

    // A close on this workspace is still playing: leave its tiles where they
    // are, unless what is tiled here (or where) changed, which ends the hold.
    let frozen = match state.retile_hold {
        RetileHold::Held { output, .. } if output != id => false,
        RetileHold::Held {
            ws: held,
            tiled,
            area: held_area,
            ..
        } => {
            let entry = state.outputs.get(id).expect("checked above");
            let same = held == ws && held_area == area && entry.workspaces[ws].tiled.windows().len() == tiled;
            if !same {
                state.retile_hold = RetileHold::None;
            }
            same
        }
        _ => false,
    };

    let entry = state.outputs.get(id).expect("checked above");
    let kind = entry.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    let gap_x = state.config.general.gaps_in;
    let gap_y = state.config.general.gaps_in_y();
    let border = state.config.general.border_size;

    let entry = state.outputs.get_mut(id).expect("checked above");
    let tiled: Vec<(Window, Rectangle<i32, Logical>)> = match kind {
        LayoutKind::Radiant => entry.workspaces[ws].tiled.radiant(area, gap_x, gap_y),
        LayoutKind::Dwindle => entry.workspaces[ws].tiled.dwindle(area, gap_x, gap_y),
        LayoutKind::Master => entry.workspaces[ws].tiled.master(area, gap_x, gap_y),
    };
    let mut floating: Vec<(Window, Rectangle<i32, Logical>, Placement)> = entry.workspaces[ws]
        .floating
        .iter()
        .map(|f| (f.window.clone(), f.rect, f.placement))
        .collect();
    // Keep-above / keep-below: a stable sort by band, so raising a normal
    // window never crosses one (`winops`). Fullscreen is raised after this.
    if stacking && state.winops.has_bands() {
        winops::sort_by_band(&mut floating, |(w, ..)| winops::effective_band(state, w));
    }
    let focus = state.focus.clone();
    let mapping = state.xwayland.mapping.clone();

    // A tiled window being dragged keeps its leaf as a placeholder so its
    // neighbours hold still, but the grab owns where it is drawn.
    let dragged = state
        .tile_drag
        .as_ref()
        .filter(|d| !d.floating)
        .map(|d| d.window.clone());
    for (w, rect) in tiled {
        if frozen || dragged.as_ref() == Some(&w) {
            continue;
        }
        let inner = shrink(rect, border);
        let size = clamp_size(&w, inner.size, true);
        if mapping.as_ref() != Some(&w) {
            configure(
                state,
                &w,
                Rectangle::new(inner.loc, size),
                focus.as_ref() == Some(&w),
                quicktile::Edges::ALL,
                false,
            );
        }
        fractional_scale::update_window_scale(&w, &output);
        titlebar::publish(state, &w, 0, false, false);
        set_quick_tiled(&w, false);
        set_tile_clip(&w, Some(inner));
        set_owner_output(&w, &output);
        state.space.map_element(w, inner.loc, false);
    }
    for (w, rect, placement) in floating {
        // Fullscreen and maximized windows fill their area exactly; no border
        // inset. Fullscreen wins: it is not told it is also maximized. The
        // stored rect is the outer frame; the content is what is left under
        // the bar (`shell::titlebar`).
        let full = state.fullscreen.contains_key(&w);
        let bar = titlebar::bar_height(state, &w);
        titlebar::publish(state, &w, bar, placement.is_fully_maximized(), false);
        set_quick_tiled(&w, !full && matches!(placement, Placement::QuickTile { .. }));
        let inner = if full {
            rect
        } else {
            content_of_outer(rect, border, bar)
        };
        // Fullscreen ignores the client's size hints: a game floated at a
        // pinned windowed size (min == max) would otherwise be held to it.
        let size = if full {
            inner.size
        } else {
            clamp_size(&w, inner.size, false)
        };
        if mapping.as_ref() != Some(&w) {
            configure(
                state,
                &w,
                Rectangle::new(inner.loc, size),
                focus.as_ref() == Some(&w),
                match placement {
                    Placement::QuickTile { zone, .. } if !full => zone.edges(),
                    _ => quicktile::Edges::NONE,
                },
                placement.is_fully_maximized() && !full,
            );
        }
        fractional_scale::update_window_scale(&w, &output);
        set_tile_clip(&w, None);
        set_owner_output(&w, &output);
        state.space.map_element(w.clone(), inner.loc, false);
        state.space.raise_element(&w, false);
    }
    raise_fullscreen(state, id);
}

/// A fullscreen window is the top of its workspace's stack, whatever the
/// floating order says. Only its own dialogs go above it, so a file picker
/// opened from a fullscreen app stays visible (COMP-05 §4, amendment C-19).
/// Override-redirect menus are raised over everything afterwards by `arrange`.
fn raise_fullscreen(state: &mut AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let ws = &entry.workspaces[entry.active];
    let fullscreen: Vec<Window> = ws
        .floating
        .iter()
        .filter(|f| state.fullscreen.contains_key(&f.window))
        .map(|f| f.window.clone())
        .collect();
    if fullscreen.is_empty() {
        return;
    }
    let floating: Vec<Window> = ws.floating.iter().map(|f| f.window.clone()).collect();
    for full in fullscreen {
        // Its dialogs, in their own floating order.
        let dialogs: Vec<Window> = floating
            .iter()
            .filter(|w| **w != full && descends_from(state, w, &full))
            .cloned()
            .collect();
        state.space.raise_element(&full, false);
        for w in &dialogs {
            state.space.raise_element(w, false);
        }
    }
}

/// The window `window` is a dialog of: its xdg parent, or the X11 window it is
/// `WM_TRANSIENT_FOR`.
fn parent_of(state: &AbyssState, window: &Window) -> Option<Window> {
    if let Some(toplevel) = window.toplevel() {
        return toplevel.parent().and_then(|p| window_for_surface(state, &p));
    }
    x11_parent(state, window)
}

/// Is `window` a dialog of `ancestor`, directly or through a chain of dialogs?
/// Bounded, so a malformed parent loop cannot hang the loop.
fn descends_from(state: &AbyssState, window: &Window, ancestor: &Window) -> bool {
    let mut cur = window.clone();
    for _ in 0..8 {
        match parent_of(state, &cur) {
            Some(p) if &p == ancestor => return true,
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}

/// Focus is moving to `window` on purpose (a click, a bind, IPC, a newly
/// mapped window that takes focus). A fullscreen window on the same workspace
/// would hide it, so that window leaves fullscreen, unless `window` is that
/// fullscreen window or one of its dialogs (COMP-05 §4, amendment C-19).
pub(crate) fn yield_fullscreen(state: &mut AbyssState, window: &Window) {
    if state.fullscreen.is_empty() {
        return;
    }
    let covering: Vec<Window> = state
        .outputs
        .iter()
        .flat_map(|e| e.workspaces.iter())
        .find(|ws| ws.holds(window))
        .map(|ws| {
            ws.floating
                .iter()
                .map(|f| f.window.clone())
                .filter(|f| {
                    state.fullscreen.contains_key(f) && f != window && !descends_from(state, window, f)
                })
                .collect()
        })
        .unwrap_or_default();
    for full in covering {
        tracing::debug!("focus moved to another window: leaving fullscreen");
        if let Some(toplevel) = full.toplevel().cloned() {
            unfullscreen_toplevel(state, &toplevel);
        } else if full.x11_surface().is_some() {
            set_x11_fullscreen(state, &full, false);
        }
    }
}

/// Pin a window to an exact window-geometry rectangle, floating it out of the
/// tiling layout so the next layout pass leaves it there (COMP-05 §3). This is
/// what an interactive move/resize grab and an external placement request both
/// end up calling.
pub fn place_at(state: &mut AbyssState, window: &Window, geo: Rectangle<i32, Logical>) {
    // A fullscreen window is mapped without the border inset and has its
    // rectangle recomputed on every arrange, so pinning it would be a lie.
    if state.fullscreen.contains_key(window) {
        return;
    }
    // Pinning a maximized window is a move or resize: it stops being maximized.
    maximize::release(state, window);
    // `arrange_output` shrinks the stored rectangle by the border before it
    // configures and maps, so store the outer rectangle that shrinks back to
    // exactly `geo`.
    let border = state.config.general.border_size;
    let bar = titlebar::bar_height(state, window);
    let mut outer = outer_of_content(geo, border, bar);
    // A titlebar must stay reachable: the frame never starts above the top of
    // the usable area of the output it lands on.
    if bar > 0 {
        if let Some(output) = output_at(state, geo.loc.to_f64()).or_else(|| focused_output(state)) {
            outer.loc.y = outer.loc.y.max(usable_area(state, &output).loc.y);
        }
    }
    let mut found = false;
    'outer: for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            if let Some(f) = ws.floating.iter_mut().find(|f| &f.window == window) {
                f.rect = outer;
                found = true;
                break 'outer;
            }
            if ws.remove(window) {
                ws.floating.push(Floating {
                    window: window.clone(),
                    rect: outer,
                    weight: None,
                    placement: Placement::Free,
                });
                found = true;
                break 'outer;
            }
        }
    }
    if found {
        arrange(state);
        // A server-side move can put a different surface under a stationary
        // pointer (COMP-04 §6).
        state.refresh_pointer_focus();
    }
}

/// Run the capture-policy classifier (COMP-02 §7, raise-only) and, when it is
/// what raised the window, record it as the `class_source` (COMP-05 §1).
fn classify(state: &mut AbyssState, window: &Window) {
    let surface = window_surface(window);
    let before = surface.as_ref().is_some_and(|s| state.sensitive.contains(s));
    crate::render::capture::mark_sensitive(state, window);
    if let Some(s) = surface.filter(|s| !before && state.sensitive.contains(s)) {
        rules::record_class_source(&s, rules::CLASS_SOURCE_CAPTURE_POLICY);
    }
}

/// The human raised `window` to `secret` from its menu. Raise-only: the set
/// is never removed from, so this lasts until the surface is gone. Withdraws
/// it from the foreign-toplevel list like any other late raise.
pub fn mark_secret(state: &mut AbyssState, window: &Window) {
    let Some(surface) = window_surface(window) else {
        return;
    };
    if state.sensitive.insert(surface.clone()) {
        rules::record_class_source(&surface, rules::CLASS_SOURCE_HUMAN);
    }
    crate::protocols::standard::foreign_toplevel::window_updated(state, window);
    crate::backend::damage_all(state);
}

/// A brand-new toplevel joins the active workspace, tiled, next to the focus.
pub fn place_new_window(state: &mut AbyssState, window: Window) {
    // Classify before the window is ever composited (COMP-02 §7). Raise-only.
    classify(state, &window);
    // COMP-05 §4: rules decide placement before the window joins a workspace.
    let placement = rules::apply(state, &window);
    // After the rules: a `sensitivity` rule must have raised the class before
    // the window is published, or never is (ADR 0094).
    crate::protocols::standard::foreign_toplevel::window_mapped(state, &window);
    if !install(state, &window, &placement) {
        return;
    }
    if de::stacking(state) {
        state.de.note_opened(&window);
    }
    if wants_first_window_maximize(state, &window, &placement) {
        maximize::set(state, &window, Some((true, true)));
    }
    let handle = state.ipc.handle_for(&window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "opened", "handle": handle}),
    );
}

/// `general.maximize-first-window`: under `de`, a plain window that is the only
/// one on its workspace opens maximized. A dialog, a window a rule sized,
/// placed or sent fullscreen, and an exempt `app_id` keep their own placement.
fn wants_first_window_maximize(state: &AbyssState, window: &Window, placement: &rules::Placement) -> bool {
    let g = &state.config.general;
    if !g.maximize_first_window
        || !state.config.mode.profile().maximize
        || placement.fullscreen
        || placement.size.is_some()
        || placement.position.is_some()
        || window.toplevel().is_some_and(|t| t.parent().is_some())
        || x11_parent(state, window).is_some()
    {
        return false;
    }
    if let (Some(app_id), _) = crate::ipc::methods::identity_of(window) {
        if g.maximize_first_window_exempt
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&app_id))
        {
            return false;
        }
    }
    state
        .outputs
        .iter()
        .find_map(|e| e.workspaces.iter().find(|ws| ws.holds(window)))
        .is_some_and(|ws| ws.all_windows().iter().all(|w| w == window))
}

/// Re-run placement once a window has settled (see `rules::Placed`), so rules
/// that match on `app-id`/`title` and the dialog/fixed-size float default
/// still land. The window keeps its handle and its identity — only where it
/// sits changes.
pub fn replace_window(state: &mut AbyssState, window: &Window, placement: &rules::Placement) {
    // A window the client positions itself is never put into the layout.
    if is_unmanaged_x11(state, window) {
        return;
    }
    // A re-placement (a rule re-matching on a changed title) is not a request
    // to move: the window stays maximized.
    let kept = maximize::placement_of(state, window);
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            ws.remove(window);
        }
    }
    install(state, window, placement);
    maximize::restore_placement(state, window, kept);
}

/// Put a window into a workspace according to `placement`. False when there is
/// no output to place it on.
fn install(state: &mut AbyssState, window: &Window, placement: &rules::Placement) -> bool {
    let window = window.clone();
    // No focused output means no outputs at all — `add` adopts focus for the
    // first entry and `unregister` repairs it, so there is no transient window
    // where an id dangles (ADR 0042). `fallback_id()` would be `None` here too,
    // so there is deliberately no fallback: there is genuinely nowhere to put
    // the window. Say so, because the window is then dropped on the floor.
    let Some(mut id) = state.outputs.focused().map(|e| e.id) else {
        tracing::warn!(
            x11 = window.x11_surface().is_some(),
            class = window.x11_surface().map(|x| x.class()),
            "no output to place a new window on; dropping it"
        );
        return false;
    };
    // An `output` rule retargets the window before anything is computed from
    // the output; an unmatched name leaves it on the focused output.
    if let Some(name) = &placement.output {
        if let Some(target) = state.outputs.iter().find(|e| {
            crate::config::glob_match(name, &e.connector) || crate::config::glob_match(name, &e.identity)
        }) {
            id = target.id;
        } else {
            tracing::warn!(output = name, "windowrule output matches no connected output");
        }
    }
    // A transient follows its parent's output and workspace unless a rule
    // said otherwise, so a dialog never lands on another head or desktop.
    let parent = x11_parent(state, &window);
    let mut parent_ws = None;
    if placement.output.is_none() {
        if let Some(p) = &parent {
            if let Some((pid, pws)) = state
                .outputs
                .iter()
                .find_map(|e| e.workspaces.iter().position(|ws| ws.holds(p)).map(|i| (e.id, i)))
            {
                id = pid;
                parent_ws = Some(pws);
            }
        }
    }
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let geometry = state.space.element_geometry(&window);
    let parent_rect = parent.as_ref().and_then(|p| state.space.element_geometry(p));
    // An X11 dialog whose parent is not the focused window must not take the
    // keyboard from whatever the human is using (COMP-05 §5).
    let steal_blocked = placement.no_focus_steal || (parent.is_some() && state.focus != parent);
    let mode = state.config.general.floating_placement;
    let border = state.config.general.border_size;
    let pointer = state.pointer_location;
    let stacking = de::stacking(state);
    let bar = titlebar::bar_height(state, &window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = placement
        .workspace
        .map(|n| (n as usize - 1).min(entry.workspaces.len() - 1))
        .or(parent_ws)
        .unwrap_or(entry.active);
    // An X11 window floats at the size it asked to map at, clamped to the
    // output, rather than at half the area (a dialog narrower than its
    // contents hides its buttons).
    let x11_request = window.x11_surface().and(state.xwayland.requested);
    let x11_min = window
        .x11_surface()
        .and_then(|x| x.min_size())
        .unwrap_or_default();
    // Under `de` nothing tiles: a `tile` rule or the default still floats.
    if placement.float || stacking {
        let size = placement
            .size
            .map(Size::from)
            .or_else(|| x11_request.map(|r| x11_outer_size(r.size, x11_min, area.size, border)))
            .or_else(|| geometry.map(|g| g.size))
            .unwrap_or_else(|| Size::from((area.size.w / 2, area.size.h / 2)));
        // The rect is the frame. A size a rule, an X11 request or the client's
        // own geometry names is the content's, so the bar goes on top of it,
        // held to the area (the default half-area is a frame already).
        let named = placement.size.is_some() || x11_request.is_some() || geometry.is_some();
        let size = framed_size(size, named, bar, area.size.h);
        // A `position` rule is relative to the output's tiling area, so the
        // same rule lands in the same place on any output. Absent a rule,
        // `general.floating-placement` decides.
        let loc: Point<i32, Logical> = match placement.position {
            Some((x, y)) => (area.loc.x + x, area.loc.y + y).into(),
            None => match parent_rect {
                // A transient dialog opens centred over its parent, kept on
                // the output.
                Some(p) => centered_in(p, size, area),
                // An X11 window opens centred, wherever the pointer is.
                // Under `de` a window that asked for a position (WM_NORMAL_HINTS
                // USPosition/PPosition) keeps it, held to the area; one that
                // did not falls through to `general.floating-placement`.
                None if stacking && x11_request.is_some() => {
                    let hinted = window
                        .x11_surface()
                        .is_some_and(|x| x.size_hints().is_some_and(|h| h.position.is_some()));
                    match x11_request {
                        Some(r) if hinted => x11_hinted_origin(r.loc, size, area),
                        _ => floating_origin(mode, area, size, pointer, entry.workspaces[ws].floating.len()),
                    }
                }
                // An X11 window opens centred, wherever the pointer is.
                None if x11_request.is_some() => centered_in(area, size, area),
                None => floating_origin(mode, area, size, pointer, entry.workspaces[ws].floating.len()),
            },
        };
        let rect = Rectangle::new(loc, size);
        entry.workspaces[ws].floating.push(Floating {
            window: window.clone(),
            rect,
            weight: None,
            placement: Placement::Free,
        });
    } else {
        let near = state
            .focus
            .clone()
            .filter(|w| entry.workspaces[ws].tiled.contains(w));
        entry.workspaces[ws]
            .tiled
            .insert(window.clone(), near.as_ref(), area, pointer);
    }
    // A no-focus-steal window is mapped where the rule put it but never takes
    // the focus; the human is told it wants attention (COMP-05 §5).
    if steal_blocked {
        arrange(state);
        mark_urgent(state, &window);
    } else {
        // Under the lock the window maps but focus stays with the locker;
        // `unlock` re-derives it.
        if !state.lock.locked {
            state.focus = Some(window.clone());
        }
        arrange(state);
        focus_window(state, &window);
    }
    // A `fullscreen` rule fires last, so the rectangle it restores to on
    // unfullscreen is the one the other rules just placed (COMP-05 §4).
    if placement.fullscreen {
        if let Some(toplevel) = window.toplevel().cloned() {
            fullscreen_toplevel(state, &toplevel, None);
        }
    }
    true
}

/// The window an X11 transient is `WM_TRANSIENT_FOR`, if the shell holds it.
fn x11_parent(state: &AbyssState, window: &Window) -> Option<Window> {
    let id = window.x11_surface()?.is_transient_for()?;
    owned_windows(state)
        .into_iter()
        .find(|w| w != window && w.x11_surface().is_some_and(|x| x.window_id() == id))
}

/// Resolve a surface to the window that owns it, if any.
pub fn window_for_surface(state: &AbyssState, surface: &WlSurface) -> Option<Window> {
    state
        .space
        .elements()
        .find(|w| window_surface(w).as_ref() == Some(surface))
        .cloned()
}

/// Every window the shell holds, each once: whatever is mapped in the space
/// (override-redirect X11 windows included) plus every workspace's tiled,
/// floating, minimized and pending windows.
///
/// `space` alone only holds the *active* workspaces. Teardown must search this
/// instead: a window destroyed while on a hidden workspace or minimized is not
/// in the space, and resolving it there leaves its leaf in the layout tree,
/// which then reserves an empty tile once that workspace is shown again.
pub fn owned_windows(state: &AbyssState) -> Vec<Window> {
    let mut out: Vec<Window> = state.space.elements().cloned().collect();
    for entry in state.outputs.iter() {
        for ws in entry.workspaces.iter() {
            for w in ws.all_windows().into_iter().chain(ws.pending.iter().cloned()) {
                if !out.contains(&w) {
                    out.push(w);
                }
            }
        }
    }
    out
}

/// COMP-05 §5: the fallback when a focus request is refused. The window stays
/// where it is; the human is told it wants attention and decides.
pub fn mark_urgent(state: &mut AbyssState, window: &Window) {
    if state.focus.as_ref() == Some(window) {
        return;
    }
    if !state.urgent.iter().any(|w| w == window) {
        state.urgent.push(window.clone());
    }
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "urgent", "handle": handle}),
    );
}

/// Keep a window's surface anchored when its window geometry origin moves.
///
/// `Space` positions an element by the origin of its window geometry, and a
/// toplevel with no explicit `set_window_geometry` derives that geometry from
/// the bounding box of its whole surface tree. So a client that attaches a
/// subsurface extending left of (or above) its root surface moves its own
/// geometry origin, and the window would jump by that much on screen even
/// though the client asked for nothing of the kind. Shift the stored placement
/// by the same delta instead, which leaves the surface exactly where it was.
pub(crate) fn reanchor(state: &mut AbyssState, window: &Window) {
    let geo = window.geometry();
    // An unmapped window has no geometry: smithay intersects the client's
    // window-geometry rect with the bounding box, and an unmapped bounding box
    // is empty, so `geometry()` collapses to `0x0` at the origin (see
    // smithay 0.7.0 `desktop/wayland/window.rs::geometry`). That collapse is
    // not the client moving its geometry origin, and chasing it corrupts the
    // anchor: the offset is subtracted on the unmap commit and added back on
    // the remap commit only if the window is floating for *both*, which is a
    // race against anything that maps it in between. Leave `geo_loc` holding
    // the last mapped origin so the remap commit is a no-op.
    if geo.is_empty() {
        return;
    }
    let now = geo.loc;
    let Some(prev) = state.geo_loc.insert(window.clone(), now) else {
        return;
    };
    if prev == now {
        return;
    }
    let delta = now - prev;
    let mut moved = false;
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            if let Some(f) = ws.floating.iter_mut().find(|f| &f.window == window) {
                f.rect.loc += delta;
                moved = true;
            }
        }
    }
    if moved {
        arrange(state);
    }
}

pub fn unmap_window(state: &mut AbyssState, window: &Window) {
    // A scripted drag held down on a window that goes away lets go with it.
    state.cancel_drag_hold_for(window);
    let hold = holds_retile(state, window);
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            ws.remove(window);
        }
    }
    state.space.unmap_elem(window);
    state.borders.remove(window);
    state.geo_loc.remove(window);
    state.fullscreen.remove(window);
    state.de.forget(window);
    state.bar_ui.forget(window);
    state.winops.forget(window);
    state.edge.forget(window);
    state.urgent.retain(|w| w != window);
    crate::protocols::standard::foreign_toplevel::window_closed(window);
    let handle = state.ipc.handle_for(window);
    state.ipc.forget_chip(handle);
    crate::ipc::client_strips::forget_window(state, window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "closed", "handle": handle}),
    );
    if state.focus.as_ref() == Some(window) {
        state.focus = None;
    }
    if hold.is_some() {
        state.retile_hold = RetileHold::Settling;
    }
    arrange(state);
    refocus_topmost(state);
    if let Some(id) = hold {
        state.retile_hold = match state.outputs.get(id) {
            Some(e) => {
                let output = e.output.clone();
                let ws = e.active;
                let tiled = e.workspaces[ws].tiled.windows().len();
                RetileHold::Held {
                    output: id,
                    ws,
                    tiled,
                    area: tiling_area(state, &output),
                }
            }
            None => RetileHold::None,
        };
        if state.retile_hold == RetileHold::None {
            arrange(state);
        }
    }
}

/// The size of a new floating window's frame: a named content size gets the
/// bar added on top (held to the area), a default half-area size is a frame
/// already. Pure, so the X11 request path is testable without Xwayland.
pub(crate) fn framed_size(
    size: Size<i32, Logical>,
    named: bool,
    bar: i32,
    area_h: i32,
) -> Size<i32, Logical> {
    if named && bar > 0 {
        Size::from((size.w, (size.h + bar).min(area_h.max(bar + 1))))
    } else {
        size
    }
}

/// Whether an overlay or top layer surface (or one of its popups) is at `pos`.
/// Those sit above every window and its bar (COMP-02 §4).
pub(crate) fn top_layer_at(state: &AbyssState, pos: Point<f64, Logical>) -> bool {
    let Some(output) = output_at(state, pos) else {
        return false;
    };
    let output_loc = state
        .space
        .output_geometry(&output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let local = pos - output_loc.to_f64();
    let map = layer_map_for_output(&output);
    [Layer::Overlay, Layer::Top].into_iter().any(|layer| {
        layer_under(&map, layer, local).is_some_and(|l| {
            let geo = layer_geometry(&map, l).unwrap_or_default();
            l.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL)
                .is_some()
        })
    })
}

/// Topmost surface at `pos`, honouring the COMP-02 §4 stacking order:
/// overlay and top layers sit above toplevels, bottom and background below.
pub fn surface_under(
    state: &AbyssState,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
    hit_under(state, pos).map(|(s, p, _)| (s, p))
}

/// The layer surface that owns the topmost surface at `pos`, if that surface
/// belongs to a layer surface at all (its own tree or one of its popups). A
/// toplevel stacked above a bottom/background layer hides it, exactly as
/// [`surface_under`] resolves it.
pub fn layer_at(state: &AbyssState, pos: Point<f64, Logical>) -> Option<DesktopLayerSurface> {
    hit_under(state, pos).and_then(|(_, _, l)| l)
}

/// [`surface_under`], also naming the layer surface hit, so the focus rules
/// can tell a layer from a toplevel without a second (allocating) lookup.
fn hit_under(
    state: &AbyssState,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>, Option<DesktopLayerSurface>)> {
    let output = output_at(state, pos)?;
    let output_loc = state
        .space
        .output_geometry(&output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let local = pos - output_loc.to_f64();
    {
        let map = layer_map_for_output(&output);
        for layer in [Layer::Overlay, Layer::Top] {
            if let Some(l) = layer_under(&map, layer, local) {
                let geo = layer_geometry(&map, l).unwrap_or_default();
                if let Some((s, p)) = l.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                    return Some((s, (output_loc + geo.loc + p).to_f64(), Some(l.clone())));
                }
            }
        }
    }
    // A titlebar is the compositor's: over one no client is under the pointer,
    // and nothing below it either (COMP-05 §7.1).
    if titlebar::bar_at(state, pos).is_some() {
        return None;
    }
    // The resize band just outside a decorated window is the compositor's too.
    if edge::band_at(state, pos).is_some() {
        return None;
    }
    if let Some((window, loc)) = winops::element_under(state, pos) {
        if let Some((s, p)) = window.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL) {
            return Some((s, (loc + p).to_f64(), None));
        }
    }
    let map = layer_map_for_output(&output);
    for layer in [Layer::Bottom, Layer::Background] {
        if let Some(l) = layer_under(&map, layer, local) {
            let geo = layer_geometry(&map, l).unwrap_or_default();
            if let Some((s, p)) = l.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                return Some((s, (output_loc + geo.loc + p).to_f64(), Some(l.clone())));
            }
        }
    }
    None
}

// ------------------------------------------------------ layer placement

/// Place a mapped layer surface at an explicit output-local position.
pub fn place_layer(map: &LayerMap, layer: &DesktopLayerSurface, loc: Point<i32, Logical>) {
    let Some(base) = map.layer_geometry(layer) else {
        return;
    };
    let data = layer.user_data();
    data.insert_if_missing(LayerOffset::default);
    if let Some(offset) = data.get::<LayerOffset>() {
        offset.0.set(loc - base.loc);
    }
}

/// Offset that keeps a layer surface where its *acked* size puts it.
///
/// smithay's `LayerMap::arrange` places a layer from the size the client has
/// committed to, and sends the new size as a configure. A bottom- or
/// right-anchored bar that grows therefore jumps at once, while the buffer on
/// screen is still the old one. This returns the delta from smithay's placement
/// at `placed` (the size of the configure in flight) to the placement at
/// `acked` (the size the client has acked, and so the size of the buffer it
/// will draw). Zero when the two are equal.
///
/// Mirrors smithay's per-axis formulas: a surface on the near edge (top or
/// left) keeps its origin, one on the far edge keeps its far edge, and a
/// centred one stays centred.
pub fn held_offset(
    anchor: Anchor,
    placed: Size<i32, Logical>,
    acked: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let axis = |near: bool, far: bool, placed: i32, acked: i32| {
        if near {
            0
        } else if far {
            placed - acked
        } else {
            placed / 2 - acked / 2
        }
    };
    let x = axis(
        anchor.contains(Anchor::LEFT),
        anchor.contains(Anchor::RIGHT),
        placed.w,
        acked.w,
    );
    let y = axis(
        anchor.contains(Anchor::TOP),
        anchor.contains(Anchor::BOTTOM),
        placed.h,
        acked.h,
    );
    Point::from((x, y))
}

/// Hold one layer surface at the placement of its last acked size.
///
/// Returns whether the surface moved. A layer that has never been acked is
/// left alone: its first configure carries no buffer to hold.
pub fn hold_layer_size(map: &LayerMap, layer: &DesktopLayerSurface) -> bool {
    let acked = with_states(layer.wl_surface(), |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .and_then(|d| d.lock().unwrap().last_acked.as_ref().and_then(|s| s.size))
    });
    let Some(acked) = acked else {
        return false;
    };
    // The size arrange placed at, i.e. the one in flight (or already acked).
    // Reading it through `with_pending_state` changes nothing: it only
    // materialises the server pending state, which equals the current one.
    let Some(placed) = layer.layer_surface().with_pending_state(|s| s.size) else {
        return false;
    };
    let Some(base) = map.layer_geometry(layer) else {
        return false;
    };
    let before = layer_geometry(map, layer);
    let delta = held_offset(layer.cached_state().anchor, placed, acked);
    place_layer(map, layer, base.loc + delta);
    before != layer_geometry(map, layer)
}

/// The topmost layer surface of `layer` under `point`, in output-local
/// coordinates. `LayerMap::layer_under` tests the arranged bounding box, which
/// is the wrong rectangle once a surface has been placed explicitly.
fn layer_under(map: &LayerMap, layer: Layer, point: Point<f64, Logical>) -> Option<&DesktopLayerSurface> {
    map.layers_on(layer).rev().find(|l| {
        let Some(geo) = layer_geometry(map, l) else {
            return false;
        };
        let mut bbox = l.bbox_with_popups();
        bbox.loc += geo.loc - l.bbox().loc;
        bbox.to_f64().contains(point)
    })
}

/// Constrain a popup to the output its parent sits on (COMP-06 §1).
///
/// The positioner works in coordinates relative to the parent's geometry
/// origin, so the output rectangle has to be translated into that space before
/// `get_unconstrained_geometry` can apply the client's constraint adjustment.
/// The parent may be a toplevel *or* a layer surface; both need the same
/// treatment, and only resolving the toplevel case leaves every layer-shell
/// popup placed against an unconstrained, wrongly-originated rectangle.
pub fn unconstrain_popup(state: &AbyssState, popup: &PopupSurface) {
    let kind = PopupKind::Xdg(popup.clone());
    let Ok(root) = find_popup_root_surface(&kind) else {
        return;
    };

    // (output, parent geometry origin in global coordinates)
    let resolved = state
        .space
        .elements()
        .find(|w| window_surface(w).as_ref() == Some(&root))
        .and_then(|w| {
            let geo = state.space.element_geometry(w)?;
            let output = output_of_window(state, w).and_then(|id| state.outputs.get(id))?;
            Some((output.output.clone(), geo.loc))
        })
        .or_else(|| {
            state.outputs.iter().find_map(|e| {
                let map = layer_map_for_output(&e.output);
                let layer = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)?;
                let geo = layer_geometry(&map, layer)?;
                let output_loc = state
                    .space
                    .output_geometry(&e.output)
                    .map(|g| g.loc)
                    .unwrap_or_default();
                Some((e.output.clone(), output_loc + geo.loc))
            })
        });
    let Some((output, parent_loc)) = resolved else {
        return;
    };
    let Some(output_geo) = state.space.output_geometry(&output) else {
        return;
    };

    let mut target = output_geo;
    target.loc -= get_popup_toplevel_coords(&kind);
    target.loc -= parent_loc;
    popup.with_pending_state(|s| {
        s.geometry = s.positioner.get_unconstrained_geometry(target);
    });
}

/// Publish a popup's configured geometry into its current state.
///
/// Smithay only promotes the configured geometry on the commit *after* the
/// client acks it (`PopupSurface::post_commit_hook`). A client that acks and
/// never commits again would then be hit-tested and rendered at the origin. A
/// popup's position is server-determined — there is nothing to negotiate — so
/// publish it as soon as it is configured, the way wlroots and Mir do
/// (COMP-06 §4).
pub(crate) fn publish_popup_geometry(popup: &PopupSurface) {
    let geometry = popup.with_pending_state(|s| s.geometry);
    with_states(popup.wl_surface(), |states| {
        if let Some(data) = states.data_map.get::<XdgPopupSurfaceData>() {
            data.lock().unwrap().current.geometry = geometry;
        }
    });
}

/// Reconstrain every reactive popup against its parent's current position.
///
/// `xdg_positioner.set_reactive` asks the compositor to re-run the constraint
/// adjustment whenever the parent moves or the visible area changes
/// (COMP-06 §4). Nothing in smithay does this for us, so it runs at the end of
/// every layout pass. A plain `configure` is what the protocol wants here — a
/// `repositioned` token belongs only to an explicit `xdg_popup.reposition`.
pub fn refresh_reactive_popups(state: &mut AbyssState) {
    let roots: Vec<WlSurface> = state.space.elements().filter_map(window_surface).collect();
    let mut reactive: Vec<PopupSurface> = Vec::new();
    for root in &roots {
        for (kind, _) in PopupManager::popups_for_surface(root) {
            let PopupKind::Xdg(xdg) = kind else { continue };
            if xdg.with_pending_state(|s| s.positioner.reactive) {
                reactive.push(xdg);
            }
        }
    }
    for popup in reactive {
        unconstrain_popup(state, &popup);
        if popup.send_configure().is_ok() {
            publish_popup_geometry(&popup);
        }
    }
}

/// The popup-root surface an arbitrary surface belongs to, for grab-scope tests.
fn grab_root_of(state: &AbyssState, surface: &WlSurface) -> WlSurface {
    state
        .popups
        .find_popup(surface)
        .and_then(|k| find_popup_root_surface(&k).ok())
        .or_else(|| window_for_surface(state, surface).and_then(|w| window_surface(&w)))
        .unwrap_or_else(|| surface.clone())
}

/// `xdg_popup.grab`: an explicit grab takes keyboard focus and stays up until
/// it is dismissed (COMP-06 §4). Smithay's own `PopupGrab` needs
/// `From<PopupKind>` for the seat's focus type, which the orphan rule forbids
/// for `WlSurface`, so the stack is kept here instead.
pub fn popup_grab_start(state: &mut AbyssState, popup: PopupSurface) {
    // A grab would take the keyboard from the locker.
    if state.lock.locked {
        return;
    }
    let surface = popup.wl_surface().clone();
    state.popup_grabs.push(popup);
    focus_surface(state, Some(surface));
}

/// Dismiss the whole grab stack, deepest popup first.
pub fn popup_grab_dismiss(state: &mut AbyssState) {
    let grabs = std::mem::take(&mut state.popup_grabs);
    let Some(bottom) = grabs.first() else {
        return;
    };
    // `PopupManager::dismiss_popup` walks the tree children-first, which is the
    // ordering xdg-shell mandates for `popup_done`.
    let kind = PopupKind::Xdg(bottom.clone());
    if let Ok(root) = find_popup_root_surface(&kind) {
        let _ = PopupManager::dismiss_popup(&root, &kind);
    }
    refocus_topmost(state);
}

/// A button press outside the grabbing popup's own surface tree dismisses it.
pub fn popup_grab_button_press(state: &mut AbyssState, under: Option<&WlSurface>) {
    let Some(bottom) = state.popup_grabs.first().cloned() else {
        return;
    };
    let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(bottom)) else {
        state.popup_grabs.clear();
        return;
    };
    if let Some(under) = under {
        if grab_root_of(state, under) == root {
            return;
        }
    }
    popup_grab_dismiss(state);
}

/// A popup died: hand pointer focus back while its `wl_surface` is still alive.
///
/// `wl_pointer.leave` can only be delivered to a live resource, so waiting for
/// the surface itself to go leaves the client believing the pointer is still
/// inside a surface it destroyed, and the next `enter` looks unpaired
/// (COMP-04 §6).
pub fn popup_gone(state: &mut AbyssState, popup: &PopupSurface) {
    let surface = popup.wl_surface().clone();
    let was_grab = state.popup_grabs.iter().any(|p| p.wl_surface() == &surface);
    state.popup_grabs.retain(|p| p.wl_surface() != &surface);
    if state.last_pointer_focus.as_ref().map(|(s, _)| s) == Some(&surface) {
        state.last_pointer_focus = None;
        if !state.pointer_grab_active {
            if let Some(pointer) = state.seat.get_pointer() {
                let serial = SERIAL_COUNTER.next_serial();
                let time = state.start_time.elapsed().as_millis() as u32;
                let location = state.pointer_location;
                pointer.motion(
                    state,
                    None,
                    &smithay::input::pointer::MotionEvent {
                        location,
                        serial,
                        time,
                    },
                );
                pointer.frame(state);
            }
        }
        state.refresh_pointer_focus();
    }
    if was_grab && state.popup_grabs.is_empty() {
        refocus_topmost(state);
    }
}

// ---------------------------------------------------------------- actions

/// The layer surface that currently holds the keyboard, if any. A
/// keyboard-exclusive layer surface — the launcher, a panel that has taken the
/// keyboard — is what the human is typing into, and `state.focus` still names
/// the toplevel underneath it, so anything that acts on "the focused window"
/// has to ask this first.
pub fn focused_layer(state: &AbyssState) -> Option<DesktopLayerSurface> {
    let focused = state.seat.get_keyboard()?.current_focus()?;
    let focused = smithay::wayland::seat::WaylandFocus::wl_surface(&focused)?;
    state.outputs.iter().map(|e| e.output.clone()).find_map(|o| {
        layer_map_for_output(&o)
            .layer_for_surface(&focused, WindowSurfaceType::TOPLEVEL)
            .cloned()
    })
}

pub fn close_focused(state: &mut AbyssState) {
    // While a layer surface has the keyboard it is what "close" means; closing
    // the toplevel underneath instead would be a surprise. wlr-layer-shell has
    // no close request, only `closed`: the client destroys the surface and goes.
    if let Some(layer) = focused_layer(state) {
        layer.layer_surface().send_close();
        return;
    }
    if let Some(w) = state.focus.clone() {
        close_window(state, &w);
    }
}

/// Ask `window` to close (not the focused one): the path `close_focused` and
/// the titlebar's close button share.
pub fn close_window(_state: &mut AbyssState, window: &Window) {
    match window.underlying_surface() {
        smithay::desktop::WindowSurface::Wayland(t) => t.send_close(),
        smithay::desktop::WindowSurface::X11(x) => {
            if let Err(e) = x.close() {
                tracing::warn!(error = %e, "could not close x11 window");
            }
        }
    }
}

/// Send a window away without closing it (COMP-05 §4 `minimized`). It leaves
/// the layout and the space entirely — nothing to draw, nothing to focus —
/// but stays owned by the workspace it was on, so it comes back where it left.
pub fn minimize_window(state: &mut AbyssState, window: &Window) {
    let Some(id) = output_of_window(state, window) else {
        return;
    };
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let Some(ws) = entry
        .workspaces
        .iter()
        .position(|w| w.windows().iter().any(|w| w == window))
    else {
        // Already minimized, or not placed yet. Either way there is nothing
        // to take out of the layout.
        return;
    };
    let floating = entry.workspaces[ws].floating.iter().find(|f| &f.window == window);
    let was_floating = floating.map(|f| f.rect);
    let placement = floating.map_or(Placement::Free, |f| f.placement);
    entry.workspaces[ws].remove(window);
    entry.workspaces[ws].minimized.push(workspace::Minimized {
        window: window.clone(),
        was_floating,
        placement,
        weight: None,
    });
    anim::minimize(state, window);
    state.space.unmap_elem(window);
    // A minimized window is not activated; a client that draws a focus ring
    // would otherwise keep drawing it in the taskbar's thumbnail.
    if let smithay::desktop::WindowSurface::Wayland(t) = window.underlying_surface() {
        t.with_pending_state(|s| {
            s.states.unset(
                smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated,
            );
        });
        t.send_pending_configure();
    }
    // The X11 equivalent of the xdg state above: `_NET_WM_STATE_HIDDEN` is how
    // a client learns it is iconic and, on restore, that it is shown again. A
    // Wine game that iconified itself and never hears it was restored keeps
    // its swapchain parked, which is a black window. Wine reads only the ICCCM
    // `WM_STATE`, never `_NET_WM_STATE_HIDDEN`: without the Iconic → Normal
    // transition `set_mapped` writes, it never learns it was restored.
    if let Some(x11) = window.x11_surface() {
        let _ = x11.set_suspended(true);
        let _ = x11.set_mapped(false);
    }
    if state.focus.as_ref() == Some(window) {
        state.focus = None;
    }
    arrange(state);
    refocus_topmost(state);
    emit_minimized(state, window, true);
    // A minimized fullscreen window no longer hides the bar; the bar only
    // folds on what it is told.
    focus::emit_output_state(state, id);
}

/// Bring a minimized window back to the layout it left and focus it.
pub fn unminimize_window(state: &mut AbyssState, window: &Window) {
    let Some(id) = output_of_window(state, window) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let pointer = state.pointer_location;
    let stacking = de::stacking(state);
    let default_rect = de::default_float_rect(state, id, area);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let Some(ws) = entry.workspaces.iter().position(|w| w.is_minimized(window)) else {
        return;
    };
    let i = entry.workspaces[ws]
        .minimized
        .iter()
        .position(|m| &m.window == window)
        .expect("just resolved");
    let restored = entry.workspaces[ws].minimized.remove(i);
    match restored.was_floating {
        Some(rect) => entry.workspaces[ws].floating.push(Floating {
            window: window.clone(),
            rect,
            weight: None,
            placement: restored.placement,
        }),
        None if stacking => entry.workspaces[ws].floating.push(Floating {
            window: window.clone(),
            rect: default_rect,
            weight: None,
            placement: Placement::Free,
        }),
        None => {
            entry.workspaces[ws]
                .tiled
                .insert(window.clone(), None, area, pointer);
            if let Some(wt) = restored.weight {
                entry.workspaces[ws].tiled.set_weight(window, wt);
            }
        }
    }
    if let Some(x11) = window.x11_surface() {
        let _ = x11.set_mapped(true);
        let _ = x11.set_suspended(false);
    }
    arrange(state);
    // Restoring onto an inactive workspace must not steal the screen: the
    // window is in the layout, but only a switch back will show it.
    if state.outputs.get(id).is_some_and(|e| e.active == ws) {
        focus_window(state, window);
    }
    anim::unminimize(state, window);
    emit_minimized(state, window, false);
    focus::emit_output_state(state, id);
}

fn emit_minimized(state: &mut AbyssState, window: &Window, minimized: bool) {
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({
            "change": if minimized { "minimized" } else { "unminimized" },
            "handle": handle,
        }),
    );
}

/// Is this window minimized anywhere?
pub fn is_minimized(state: &AbyssState, window: &Window) -> bool {
    state
        .outputs
        .iter()
        .any(|e| e.workspaces.iter().any(|ws| ws.is_minimized(window)))
}

pub fn set_minimized(state: &mut AbyssState, window: &Window, value: bool) {
    if value {
        minimize_window(state, window);
    } else {
        unminimize_window(state, window);
    }
}

/// Keybind form: send the focused window away.
pub fn minimize_focused(state: &mut AbyssState) {
    let Some(w) = state.focus.clone() else { return };
    minimize_window(state, &w);
}

/// Keybind form: bring back the window most recently sent away on the active
/// workspace. A minimized window holds no focus, so there is nothing for a
/// focus-relative toggle to act on — this is the other half of the pair.
pub fn unminimize_last(state: &mut AbyssState) {
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let entry = state.outputs.get(id).expect("just resolved");
    let Some(window) = entry.workspace().minimized.last().map(|m| m.window.clone()) else {
        return;
    };
    unminimize_window(state, &window);
}

/// The requested origin of an X11 window that asked for a position, held so
/// the frame stays on the area.
pub(crate) fn x11_hinted_origin(
    requested: Point<i32, Logical>,
    size: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let max_x = (area.loc.x + area.size.w - size.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - size.h).max(area.loc.y);
    (
        requested.x.clamp(area.loc.x, max_x),
        requested.y.clamp(area.loc.y, max_y),
    )
        .into()
}

/// Where a floating window lands when no `position` rule places it
/// (COMP-05 §4). One helper so a newly-mapped floating window and
/// `toggle-floating` cannot drift apart: `already` is how many windows are
/// already floating on the workspace, which only `cascade` reads.
pub(crate) fn floating_origin(
    mode: FloatingPlacement,
    area: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    pointer: Point<f64, Logical>,
    already: usize,
) -> Point<i32, Logical> {
    let max_x = (area.loc.x + area.size.w - size.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - size.h).max(area.loc.y);
    match mode {
        FloatingPlacement::Centered => (
            area.loc.x + (area.size.w - size.w).max(0) / 2,
            area.loc.y + (area.size.h - size.h).max(0) / 2,
        )
            .into(),
        FloatingPlacement::Pointer => {
            let c: Point<i32, Logical> = pointer.to_i32_round();
            (c.x.clamp(area.loc.x, max_x), c.y.clamp(area.loc.y, max_y)).into()
        }
        FloatingPlacement::Cascade => {
            const STEP: i32 = 32;
            // Wrap before the step would push the window off the area, so a
            // long-lived workspace restarts the diagonal instead of piling
            // every window into the bottom-right corner.
            let room = ((max_x - area.loc.x).min(max_y - area.loc.y) / STEP).max(1);
            let n = (already as i32 % room) * STEP;
            (area.loc.x + n, area.loc.y + n).into()
        }
    }
}

/// The size an X11 window floats at: what it asked for, no smaller than its
/// minimum, no larger than the area.
pub(crate) fn x11_float_size(
    requested: Size<i32, Logical>,
    min: Size<i32, Logical>,
    area: Size<i32, Logical>,
) -> Size<i32, Logical> {
    let w = requested.w.max(min.w).max(1);
    let h = requested.h.max(min.h).max(1);
    Size::from((
        if area.w > 0 { w.min(area.w) } else { w },
        if area.h > 0 { h.min(area.h) } else { h },
    ))
}

/// The stored (outer) floating size for an X11 window that asked to map at
/// `requested`. `arrange_output` shrinks the stored rectangle by `border` on
/// every side before configuring, so the request is the *inner* size: storing
/// it as-is made the client a border smaller on each side at every map, and a
/// client that unmaps and remaps (Zoom) shrank and crept a little further each
/// time, because it re-requests whatever it was last configured to.
pub(crate) fn x11_outer_size(
    requested: Size<i32, Logical>,
    min: Size<i32, Logical>,
    area: Size<i32, Logical>,
    border: i32,
) -> Size<i32, Logical> {
    let room = Size::from(((area.w - 2 * border).max(0), (area.h - 2 * border).max(0)));
    let inner = x11_float_size(requested, min, room);
    Size::from((inner.w + 2 * border, inner.h + 2 * border))
}

/// `size` centred on `anchor`, then moved to lie inside `area` (the area's
/// origin wins when it does not fit).
pub(crate) fn centered_in(
    anchor: Rectangle<i32, Logical>,
    size: Size<i32, Logical>,
    area: Rectangle<i32, Logical>,
) -> Point<i32, Logical> {
    let max_x = (area.loc.x + area.size.w - size.w).max(area.loc.x);
    let max_y = (area.loc.y + area.size.h - size.h).max(area.loc.y);
    Point::from((
        (anchor.loc.x + (anchor.size.w - size.w) / 2).clamp(area.loc.x, max_x),
        (anchor.loc.y + (anchor.size.h - size.h) / 2).clamp(area.loc.y, max_y),
    ))
}

pub fn toggle_floating(state: &mut AbyssState) {
    // Floating -> tiled is the tile-only half; nothing is tiled under `de`.
    if de::stacking(state) {
        return;
    }
    let Some(window) = state.focus.clone() else { return };
    // Leave fullscreen first, or the `fullscreen` entry outlives the float and
    // the next fullscreen bind reads it as "already on".
    if state.fullscreen.contains_key(&window) {
        if let Some(toplevel) = window.toplevel().cloned() {
            unfullscreen_toplevel_now(state, &toplevel);
        } else {
            set_x11_fullscreen_now(state, &window, false);
        }
    }
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let geometry = state.space.element_geometry(&window);
    let mode = state.config.general.floating_placement;
    let pointer = state.pointer_location;
    let toggled = window.clone();
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    if entry.workspaces[ws].tiled.contains(&window) {
        let rect = geometry
            .unwrap_or_else(|| Rectangle::new(area.loc, Size::from((area.size.w / 2, area.size.h / 2))));
        let loc = floating_origin(
            mode,
            area,
            rect.size,
            pointer,
            entry.workspaces[ws].floating.len(),
        );
        // The leaf weight rides along, so re-tiling restores its priority.
        let weight = entry.workspaces[ws].tiled.weight_of(&window);
        entry.workspaces[ws].tiled.remove(&window);
        entry.workspaces[ws].floating.push(Floating {
            window,
            rect: Rectangle::new(loc, rect.size),
            weight,
            placement: Placement::Free,
        });
    } else if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == window)
    {
        let f = entry.workspaces[ws].floating.remove(i);
        entry.workspaces[ws]
            .tiled
            .insert(window.clone(), None, area, pointer);
        if let Some(w) = f.weight {
            entry.workspaces[ws].tiled.set_weight(&window, w);
        }
    }
    arrange(state);
    // `floating` matchers follow the toggle at once instead of waiting for the
    // client's next commit. The window is already placed, so this re-applies
    // property actions only (never `float`/`tile`/placement) and cannot loop.
    let _ = rules::reevaluate(state, &toggled);
}

fn toplevel_window(
    state: &AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
) -> Option<Window> {
    state
        .space
        .elements()
        .find(|w| w.toplevel() == Some(surface))
        .cloned()
}

/// `fullscreen` (COMP-02 §9) plays over each of the fullscreen requests below, and
/// over the X11 form: [`anim::toggle`] keeps the old frame, runs the change,
/// and animates from the old rectangle to the new.
/// xdg `set_maximized` / `unset_maximized` outside `mode "de"` (C-20; under
/// `de` they go to [`maximize::request_toplevel`]): there is no maximized
/// state, so both are declined. The client is answered with a configure that does not
/// carry `Maximized`; its pending size is left as the layout set it, so a
/// tiled window stays at its tile. A toplevel still owed its initial configure
/// has no `Window` yet, so only its pending state is cleaned.
pub fn decline_maximize_toplevel(
    _state: &mut AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;

    surface.with_pending_state(|s| s.states.unset(State::Maximized));
    if surface.is_initial_configure_sent() {
        surface.send_configure();
    }
}

pub fn fullscreen_toplevel(
    state: &mut AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
    target: Option<&smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
) {
    let window = toplevel_window(state, surface);
    anim::toggle(state, window, |state| {
        fullscreen_toplevel_now(state, surface, target)
    });
}

pub fn unfullscreen_toplevel(
    state: &mut AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
) {
    let window = toplevel_window(state, surface);
    anim::toggle(state, window, |state| unfullscreen_toplevel_now(state, surface));
}

/// The `toggle-fullscreen` bind: the same shell state a client request
/// reaches, so the human and the app can each end what the other started.
pub fn toggle_fullscreen(state: &mut AbyssState) {
    let Some(window) = state.focus.clone() else { return };
    let on = !state.fullscreen.contains_key(&window);
    if let Some(toplevel) = window.toplevel().cloned() {
        if on {
            fullscreen_toplevel(state, &toplevel, None);
        } else {
            unfullscreen_toplevel(state, &toplevel);
        }
    } else if window.x11_surface().is_some() {
        set_x11_fullscreen(state, &window, on);
    }
}

pub fn set_x11_fullscreen(state: &mut AbyssState, window: &Window, on: bool) {
    anim::toggle(state, Some(window.clone()), |state| {
        set_x11_fullscreen_now(state, window, on)
    });
}

/// An X11 window's title/class/transient/type property changed.
pub fn x11_properties_changed(state: &mut AbyssState, surface: &smithay::xwayland::X11Surface) {
    let Some(window) = owned_windows(state)
        .into_iter()
        .find(|w| w.x11_surface() == Some(surface) && !is_unmanaged_x11(state, w))
    else {
        return;
    };
    // Rules first: a title that matches a `sensitivity` rule must raise the
    // class before the foreign-toplevel list is told the title (ADR 0094).
    if let Some(placement) = rules::reevaluate(state, &window) {
        replace_window(state, &window, &placement);
    }
    if crate::protocols::standard::foreign_toplevel::window_updated(state, &window) {
        let handle = state.ipc.handle_for(&window);
        crate::ipc::emit(
            state,
            "window",
            serde_json::json!({"change": "title", "handle": handle}),
        );
    }
}

/// An X11 window's `_NET_WM_STATE` fullscreen request: the same shell state
/// the xdg path uses (`state.fullscreen`, which `arrange_output` tracks),
/// minus the xdg configure handshake.
fn set_x11_fullscreen_now(state: &mut AbyssState, window: &Window, on: bool) {
    let Some(x11) = window.x11_surface().cloned() else {
        return;
    };
    let Some(id) = output_of_window(state, window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    if on == state.fullscreen.contains_key(window) {
        return;
    }
    if on {
        // Fullscreen restores a rolled-up window (`winops`).
        winops::unshade(state, window);
    }
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    if on {
        let area = state.space.output_geometry(&output).unwrap_or_default();
        tracing::info!(
            class = x11.class(),
            ?area,
            "configuring x11 window for fullscreen"
        );
        let ws = home_workspace(state, id, window);
        let detached = detach_from_layout(state, id, window);
        let placement = detached.map_or(Placement::Free, |(_, p)| p);
        state.outputs.get_mut(id).expect("just resolved").workspaces[ws]
            .floating
            .push(Floating {
                window: window.clone(),
                rect: area,
                weight: None,
                placement,
            });
        state
            .fullscreen
            .insert(window.clone(), fullscreen_restore(detached));
    } else {
        let restore = state.fullscreen.remove(window);
        let area = tiling_area(state, &output);
        let pointer = state.pointer_location;
        let stacking = de::stacking(state);
        let default_rect = de::default_float_rect(state, id, area);
        let ws = home_workspace(state, id, window);
        // Maximized before fullscreen: back to it, not to the plain rectangle.
        let placement = detach_from_layout(state, id, window).map_or(Placement::Free, |(_, p)| p);
        let entry = state.outputs.get_mut(id).expect("just resolved");
        match restore.flatten() {
            Some(rect) => entry.workspaces[ws].floating.push(Floating {
                window: window.clone(),
                rect,
                weight: None,
                placement,
            }),
            None if stacking => entry.workspaces[ws].floating.push(Floating {
                window: window.clone(),
                rect: default_rect,
                weight: None,
                placement,
            }),
            None => entry.workspaces[ws]
                .tiled
                .insert(window.clone(), None, area, pointer),
        }
    }
    let _ = x11.set_fullscreen(on);
    arrange(state);
    focus::emit_output_state(state, id);
}

/// The workspace on `id` that holds `window`, else the active one. A state
/// change acts where the window lives: a background app going fullscreen must
/// not be copied onto the workspace the human is looking at.
fn home_workspace(state: &AbyssState, id: u64, window: &Window) -> usize {
    state.outputs.get(id).map_or(0, |e| {
        e.workspaces
            .iter()
            .position(|ws| ws.holds_live(window))
            .unwrap_or(e.active)
    })
}

/// Remove `window` from whatever placement it holds on `id`'s workspace that
/// holds it, returning its floating rectangle and placement if it had one and
/// `None` if it was tiled.
fn detach_from_layout(
    state: &mut AbyssState,
    id: u64,
    window: &Window,
) -> Option<(Rectangle<i32, Logical>, Placement)> {
    let ws = home_workspace(state, id, window);
    let entry = state.outputs.get_mut(id)?;
    if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == *window)
    {
        let f = entry.workspaces[ws].floating.remove(i);
        Some((f.rect, f.placement))
    } else {
        entry.workspaces[ws].tiled.remove(window);
        None
    }
}

/// What a window returns to when it leaves fullscreen: the rectangle it was
/// placed at, which for a maximized window is the one maximize will give back.
fn fullscreen_restore(
    detached: Option<(Rectangle<i32, Logical>, Placement)>,
) -> Option<Rectangle<i32, Logical>> {
    detached.map(|(rect, placement)| placement.restore_rect().unwrap_or(rect))
}

/// Does a fullscreen toplevel currently own `output`'s active workspace?
///
/// The render pass needs this to decide whether the `Top` layer (the bar) draws
/// above or below the window stack; it cannot see [`AbyssState`] itself, so the
/// answer is threaded in from the backend call sites (COMP-02 §3).
pub fn output_has_fullscreen(state: &AbyssState, output: &Output) -> bool {
    let Some(entry) = state.outputs.by_output(output) else {
        return false;
    };
    let ws = entry.active;
    entry.workspaces[ws]
        .floating
        .iter()
        .any(|f| state.fullscreen.contains_key(&f.window))
}

/// A client's `xdg_toplevel.set_fullscreen` (COMP-05 §4): fill the whole output,
/// exclusive zones included.
///
/// `target` is the client's requested output; `None` means "wherever it is now".
/// Honouring a different output moves the window there, which is what the
/// protocol asks for.
fn fullscreen_toplevel_now(
    state: &mut AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
    target: Option<&smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;

    let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel() == Some(surface))
        .cloned()
    else {
        if !surface.is_initial_configure_sent() {
            surface.with_pending_state(|s| {
                s.states.set(State::Fullscreen);
                s.fullscreen_output = target.cloned();
            });
        }
        return;
    };
    if state.fullscreen.contains_key(&window) {
        return;
    }
    // Fullscreen restores a rolled-up window (`winops`).
    winops::unshade(state, &window);
    let current = output_of_window(state, &window);
    let Some(id) = target
        .and_then(|wl| state.outputs.by_wl_output(wl).map(|e| e.id))
        .or(current)
        .or_else(|| state.outputs.focused().map(|e| e.id))
    else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    let area = state.space.output_geometry(&output).unwrap_or_default();

    // Detach from wherever it lives now, which may be a different output than
    // the one the client asked to be fullscreen on.
    // Same output: stay on the window's own workspace. Moving to another
    // output lands on that output's active one.
    let ws = match current {
        Some(from) if from == id => home_workspace(state, id, &window),
        _ => state.outputs.get(id).expect("just resolved").active,
    };
    let detached = current.and_then(|from| detach_from_layout(state, from, &window));
    let placement = detached.map_or(Placement::Free, |(_, p)| p);

    let entry = state.outputs.get_mut(id).expect("just resolved");
    entry.workspaces[ws].floating.push(Floating {
        window: window.clone(),
        rect: area,
        weight: None,
        placement,
    });
    state
        .fullscreen
        .insert(window.clone(), fullscreen_restore(detached));

    surface.with_pending_state(|s| {
        s.size = Some(area.size);
        s.states.set(State::Fullscreen);
        // Fullscreen wins over maximized; leaving it returns to maximized.
        s.states.unset(State::Maximized);
    });
    surface.send_configure();
    arrange(state);
    // hyperion hides outright under fullscreen rather than folding, and it
    // has no view of the window stack to work that out for itself.
    focus::emit_output_state(state, id);
}

/// A client's `xdg_toplevel.unset_fullscreen`: restore whatever placement the
/// window had before.
fn unfullscreen_toplevel_now(
    state: &mut AbyssState,
    surface: &smithay::wayland::shell::xdg::ToplevelSurface,
) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;

    let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel() == Some(surface))
        .cloned()
    else {
        if !surface.is_initial_configure_sent() {
            surface.with_pending_state(|s| {
                s.states.unset(State::Fullscreen);
                s.fullscreen_output = None;
            });
        }
        return;
    };
    let Some(restore) = state.fullscreen.remove(&window) else {
        return;
    };
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        surface.with_pending_state(|s| s.states.unset(State::Fullscreen));
        surface.send_configure();
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let pointer = state.pointer_location;
    let stacking = de::stacking(state);
    let default_rect = de::default_float_rect(state, id, area);

    let ws = home_workspace(state, id, &window);
    // Maximized before fullscreen: back to it, not to the plain rectangle.
    let placement = detach_from_layout(state, id, &window).map_or(Placement::Free, |(_, p)| p);
    let gaps = (state.config.general.gaps_in, state.config.general.gaps_in_y());
    let border = state.config.general.border_size;
    let bar = titlebar::bar_height(state, &window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let size = match restore {
        Some(rect) => {
            let (rect, size) = match quicktile::placed_rect(&window, placement, area, gaps, border, bar) {
                Some(r) => (r, r.size),
                None => (rect, rect.size),
            };
            entry.workspaces[ws].floating.push(Floating {
                window: window.clone(),
                rect,
                weight: None,
                placement,
            });
            size
        }
        None if stacking => {
            entry.workspaces[ws].floating.push(Floating {
                window: window.clone(),
                rect: default_rect,
                weight: None,
                placement,
            });
            default_rect.size
        }
        None => {
            entry.workspaces[ws]
                .tiled
                .insert(window.clone(), None, area, pointer);
            area.size
        }
    };

    surface.with_pending_state(|s| {
        s.size = Some(size);
        s.states.unset(State::Fullscreen);
        if placement.is_fully_maximized() {
            s.states.set(State::Maximized);
        }
    });
    surface.send_configure();
    arrange(state);
    // Symmetric with `fullscreen_toplevel`: the bar comes back out of hiding.
    focus::emit_output_state(state, id);
}

pub fn toggle_layout(state: &mut AbyssState) {
    if de::stacking(state) {
        return;
    }
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let ws = state.outputs.get(id).expect("just resolved").active;
    let fallback = state.config.layout_for(ws + 1);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let current = entry.workspaces[ws].layout.unwrap_or(fallback);
    let next = match current {
        LayoutKind::Radiant => LayoutKind::Dwindle,
        LayoutKind::Dwindle => LayoutKind::Master,
        LayoutKind::Master => LayoutKind::Radiant,
    };
    entry.workspaces[ws].layout = Some(next);
    tracing::info!(workspace = ws + 1, layout = ?next, "layout toggled");
    arrange(state);
}

fn neighbour(state: &AbyssState, from: &Window, dir: Direction) -> Option<Window> {
    let origin = state.space.element_geometry(from)?;
    let oc = (
        origin.loc.x as f64 + origin.size.w as f64 / 2.0,
        origin.loc.y as f64 + origin.size.h as f64 / 2.0,
    );
    state
        .space
        .elements()
        .filter(|w| *w != from)
        .filter_map(|w| state.space.element_geometry(w).map(|g| (w.clone(), g)))
        .filter_map(|(w, g)| {
            let c = (
                g.loc.x as f64 + g.size.w as f64 / 2.0,
                g.loc.y as f64 + g.size.h as f64 / 2.0,
            );
            let (dx, dy) = (c.0 - oc.0, c.1 - oc.1);
            let ok = match dir {
                Direction::Left => dx < -1.0 && dx.abs() >= dy.abs(),
                Direction::Right => dx > 1.0 && dx.abs() >= dy.abs(),
                Direction::Up => dy < -1.0 && dy.abs() > dx.abs(),
                Direction::Down => dy > 1.0 && dy.abs() > dx.abs(),
            };
            ok.then_some((w, dx * dx + dy * dy))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(w, _)| w)
}

pub fn focus_direction(state: &mut AbyssState, dir: Direction) {
    let Some(from) = state.focus.clone() else {
        refocus_topmost(state);
        return;
    };
    if let Some(target) = neighbour(state, &from, dir) {
        focus_window(state, &target);
    }
}

/// Swap the focused tiled window with its neighbour in `dir`. A floating
/// window is not moved here; it is dragged with a `mousebind` (ADR 0057).
pub fn move_direction(state: &mut AbyssState, dir: Direction) {
    if de::stacking(state) {
        return;
    }
    let Some(from) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &from) else {
        return;
    };
    let ws = state.outputs.get(id).expect("just resolved").active;
    let Some(target) = neighbour(state, &from, dir) else {
        return;
    };
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if entry.workspaces[ws].tiled.contains(&target) {
        entry.workspaces[ws].tiled.swap(&from, &target);
        arrange(state);
        warp_pointer_to(state, &from);
    }
}

/// The active workspace on output `id` and the layout it is using.
fn active_layout(state: &AbyssState, id: u64) -> Option<(usize, LayoutKind)> {
    let entry = state.outputs.get(id)?;
    let ws = entry.active;
    let kind = entry.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    Some((ws, kind))
}

/// Raise or lower the focused tiled window's weight in its container
/// (COMP-05 §3.1 priority). Radiant only: classic dwindle and master ignore
/// weights, and a floating window has none.
pub fn adjust_priority(state: &mut AbyssState, delta: i32) {
    if de::stacking(state) {
        return;
    }
    let Some(window) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &window) else {
        return;
    };
    let Some((ws, LayoutKind::Radiant)) = active_layout(state, id) else {
        return;
    };
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if entry.workspaces[ws].tiled.adjust_weight(&window, delta) {
        arrange(state);
    }
}

/// Put the pointer in the middle of `window`, unless the human turned that off
/// (`general.cursor-follows-moved-window`).
///
/// A keybind that moves a window to another workspace or display otherwise
/// leaves the cursor sitting on whatever is now under it, which both loses the
/// pointer and hands focus-follows-mouse a stale answer the moment the mouse
/// twitches. Warping goes through `pointer_moved`, the same path real motion
/// takes, so enter/leave, constraints and cursor-shape all stay consistent.
pub fn warp_pointer_to(state: &mut AbyssState, window: &Window) {
    if !state.config.general.cursor_follows_moved_window {
        return;
    }
    let Some(geo) = state.space.element_geometry(window) else {
        return;
    };
    let centre = Point::<f64, Logical>::from((
        geo.loc.x as f64 + geo.size.w as f64 / 2.0,
        geo.loc.y as f64 + geo.size.h as f64 / 2.0,
    ));
    let time = state.start_time.elapsed().as_millis() as u32;
    state.pointer_moved(centre, time);
}

/// 1-based workspace index.
pub fn switch_workspace(state: &mut AbyssState, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let entry = state.outputs.get(id).expect("just resolved");
    if target == entry.active {
        return;
    }
    let previous = entry.active;
    let outgoing: Vec<Window> = entry
        .workspace()
        .windows()
        .into_iter()
        .filter(|w| !winops::stays_on_switch(state, w))
        .collect();
    let output = entry.output.clone();
    // COMP-02 §9 `workspace-switch`, outgoing half: the old windows are
    // handed to the render path as ghosts before they leave the space.
    anim::leave_workspace(state, &outgoing, &output, if target > previous { 1 } else { -1 });
    for w in &outgoing {
        state.space.unmap_elem(w);
    }
    state.outputs.get_mut(id).expect("just resolved").active = target;
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    // The arriving half: slide the new windows in from the side the switch
    // came from. `slide` settles the arrivals even with the event off, so
    // they do not fire `window-open`.
    let size = state
        .outputs
        .get(id)
        .and_then(|e| state.space.output_geometry(&e.output))
        .map_or_else(Default::default, |g| g.size);
    let dir = if target > previous { 1 } else { -1 };
    let windows: Vec<Window> = state
        .outputs
        .get(id)
        .map(|e| e.workspace().windows())
        .unwrap_or_default()
        .into_iter()
        .filter(|w| !winops::stays_on_switch(state, w))
        .collect();
    state
        .borders
        .anim
        .slide(&state.space, &state.config.animations, &windows, size, dir);
    crate::ipc::emit(
        state,
        "workspace",
        serde_json::json!({"change": "switched", "output": id, "workspace": idx}),
    );
    // The new workspace may hold a fullscreen window, or none at all. Either
    // way `focus_window` above saw no output transition and emitted nothing, so
    // restate it here or hyperion folds against the old workspace.
    focus::emit_output_state(state, id);
    tracing::info!(workspace = idx, "workspace switched");
}

/// Switch the focused output to the workspace `step` away from its active one.
/// Clamped to 1..=10, not wrapped: a swipe past the last workspace does nothing
/// rather than jumping to the far end.
pub fn switch_workspace_relative(state: &mut AbyssState, step: isize) {
    let Some(active) = state.outputs.focused().map(|e| e.active) else {
        return;
    };
    let target = active as isize + 1 + step;
    if (1..=workspace::COUNT as isize).contains(&target) {
        switch_workspace(state, target as usize);
    }
}

pub fn move_to_workspace(state: &mut AbyssState, idx: usize) {
    let Some(window) = state.focus.clone() else { return };
    move_window_to_workspace(state, &window, idx);
}

/// [`move_to_workspace`] for a named window (the window menu's "Move to
/// desktop"), 1-based. A sticky window is on every workspace of its output
/// already, so it stays where it is showing (`winops::sync_sticky`).
pub fn move_window_to_workspace(state: &mut AbyssState, window: &Window, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    let window = window.clone();
    if is_minimized(state, &window) {
        move_minimized(state, &window, target);
        return;
    }
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let entry = state.outputs.get(id).expect("just resolved");
    if target == entry.active {
        return;
    }
    let output = entry.output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let pointer = state.pointer_location;
    let stacking = de::stacking(state);
    let default_rect = de::default_float_rect(state, id, area);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let active = entry.active;
    let floating = entry.workspaces[active]
        .floating
        .iter()
        .find(|f| f.window == window);
    let floating_rect = floating.map(|f| f.rect);
    // A maximized window stays maximized on the workspace it goes to.
    let placement = floating.map_or(Placement::Free, |f| f.placement);
    entry.workspaces[active].remove(&window);
    let follow = state.config.general.follow_window_to_workspace;
    let from = state.space.element_geometry(&window);
    if !follow {
        // Not followed: it leaves toward the workspace it is bound for.
        anim::send_away(state, &window, &output, if target > active { 1 } else { -1 });
    }
    state.space.unmap_elem(&window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if stacking {
        entry.workspaces[target].floating.push(Floating {
            window: window.clone(),
            rect: floating_rect.unwrap_or(default_rect),
            weight: None,
            placement,
        });
    } else {
        entry.workspaces[target]
            .tiled
            .insert(window.clone(), None, area, pointer);
    }
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(workspace = idx, "window moved to workspace");
    // Following the window is the default: a move you cannot see is hard to
    // tell from a move that did not happen.
    if follow {
        switch_workspace(state, idx);
        focus::focus_window(state, &window);
        warp_pointer_to(state, &window);
        // The switch slid it in with the rest; `carry` has it stay put while
        // the workspace moves under it.
        if let Some(from) = from {
            anim::carried(state, &window, from);
        }
    }
}

/// A minimized window changes desktop where it sits: its entry moves to the
/// target workspace's minimized list and it stays minimized, so nothing is
/// shown, switched or focused (the window menu's "Move to desktop" on a
/// taskbar chip). It reappears on that desktop when restored.
fn move_minimized(state: &mut AbyssState, window: &Window, target: usize) {
    let Some(id) = output_of_window(state, window) else {
        return;
    };
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let Some(from) = entry.workspaces.iter().position(|ws| ws.is_minimized(window)) else {
        return;
    };
    if from == target {
        return;
    }
    let i = entry.workspaces[from]
        .minimized
        .iter()
        .position(|m| &m.window == window)
        .expect("just resolved");
    let m = entry.workspaces[from].minimized.remove(i);
    entry.workspaces[target].minimized.push(m);
    tracing::info!(workspace = target + 1, "minimized window moved to workspace");
}

/// Move the focused window to display `number`'s currently active workspace
/// (ADR 0049). Composes "set output" (COMP-05 §5.1) with a read of the target
/// output's own active-workspace index — the per-output equivalent of ADR
/// 0042's workspace focus history, since a *workspace*, not a window, is what
/// needs recovering here. A no-op, loudly, when nothing has that number.
pub fn move_to_output_workspace(state: &mut AbyssState, number: u8) {
    let Some(target_id) = state.outputs.iter().find(|e| e.number == number).map(|e| e.id) else {
        tracing::warn!(number, "move-to-output-workspace: no output with that number");
        return;
    };
    let Some(window) = state.focus.clone() else { return };
    let Some(source_id) = output_of_window(state, &window) else {
        return;
    };
    let target_entry = state.outputs.get(target_id).expect("just resolved");
    let target_ws = target_entry.active;
    if source_id == target_id {
        // Already on this output; folding into `move_to_workspace` would just
        // duplicate the same-workspace short-circuit it already has.
        move_to_workspace(state, target_ws + 1);
        return;
    }
    let source_entry = state.outputs.get(source_id).expect("just resolved");
    let source_active = source_entry.active;
    let target_output = target_entry.output.clone();
    layer_map_for_output(&target_output).arrange();
    let area = tiling_area(state, &target_output);
    let pointer = state.pointer_location;

    let stacking = de::stacking(state);
    let offset = match (
        state
            .outputs
            .get(source_id)
            .and_then(|e| state.space.output_geometry(&e.output)),
        state.space.output_geometry(&target_output),
    ) {
        (Some(src), Some(dst)) => dst.loc - src.loc,
        _ => Point::default(),
    };
    let default_rect = de::default_float_rect(state, target_id, area);
    let source_entry = state.outputs.get_mut(source_id).expect("just resolved");
    let floating = source_entry.workspaces[source_active]
        .floating
        .iter()
        .find(|f| f.window == window);
    let floating_rect = floating.map(|f| f.rect);
    // Maximized stays so, and fills the target output's usable area; its
    // restore rectangle moves with it.
    let placement = floating.map_or(Placement::Free, |f| f.placement.moved_by(offset));
    source_entry.workspaces[source_active].remove(&window);
    let source_output = source_entry.output.clone();
    anim::leave_output(state, &window, &source_output, &target_output);
    state.space.unmap_elem(&window);

    let target_entry = state.outputs.get_mut(target_id).expect("just resolved");
    if stacking {
        target_entry.workspaces[target_ws].floating.push(Floating {
            window: window.clone(),
            rect: floating_rect.map_or(default_rect, |r| de::carried_rect(r, offset, area)),
            weight: None,
            placement,
        });
    } else {
        target_entry.workspaces[target_ws]
            .tiled
            .insert(window.clone(), None, area, pointer);
    }

    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    anim::arrive_on_output(state, &window);
    tracing::info!(number, workspace = target_ws + 1, "window moved to output");
    // Same rule across displays: focus moves to the target output and the
    // pointer goes with the window, so the next keystroke lands where the
    // human is looking.
    if state.config.general.follow_window_to_workspace {
        state.outputs.set_focused(target_id);
        focus::focus_window(state, &window);
        warp_pointer_to(state, &window);
    }
}

/// Resize one window to an absolute logical size (COMP-13 §2.1).
///
/// Floating and tiled are different operations and both are handled here. A
/// floating window owns its rectangle, so the new size is simply stored. A
/// tiled window owns nothing but the split ratios above it, so the size is
/// expressed as weights by [`layout::Tree::resize`] and may not be honoured
/// exactly. Errors are strings so the caller can hand them straight back over
/// the socket; nothing is mutated on an error path.
pub fn resize_window(
    state: &mut AbyssState,
    window: &Window,
    width: Option<i32>,
    height: Option<i32>,
) -> Result<bool, &'static str> {
    let Some(id) = output_of_window(state, window) else {
        return Err("window is not on any output");
    };
    if !winops::can_resize(state, window) {
        return Err("a shaded window cannot be resized");
    }
    // Resizing a maximized window ends the maximize first. Under fullscreen it
    // stays: leaving fullscreen returns to maximized.
    if !state.fullscreen.contains_key(window) {
        maximize::release(state, window);
    }
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    let entry = state.outputs.get(id).expect("just resolved");
    let Some(ws) = entry
        .workspaces
        .iter()
        .position(|w| w.windows().iter().any(|w| w == window))
    else {
        return Err("window is not on any workspace");
    };
    let tiled = entry.workspaces[ws].tiled.contains(window);
    let kind = entry.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    // Master and classic dwindle ignore the tree's shape (ADR 0031), so a
    // weight has nowhere to land. Say so rather than silently doing nothing.
    if tiled && kind == LayoutKind::Master {
        return Err("a tiled window cannot be resized under the master layout");
    }
    if tiled && kind == LayoutKind::Dwindle {
        return Err("a tiled window cannot be resized under the dwindle layout");
    }
    let border = state.config.general.border_size;
    let gap_x = state.config.general.gaps_in;
    let gap_y = state.config.general.gaps_in_y();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    // Callers name the window's own size; the tree deals in tiles, which are
    // the window plus its border on each side.
    let want = (width.map(|w| w + 2 * border), height.map(|h| h + 2 * border));
    // A floating window's rect is its frame, bar included.
    let bar = titlebar::bar_height(state, window);
    let want_floating = (want.0, want.1.map(|h| h + bar));

    let entry = state.outputs.get_mut(id).expect("just resolved");
    let changed = if tiled {
        entry.workspaces[ws]
            .tiled
            .resize(window, area, gap_x, gap_y, want)
    } else if let Some(f) = entry.workspaces[ws]
        .floating
        .iter_mut()
        .find(|f| &f.window == window)
    {
        let before = f.rect.size;
        f.rect.size.w = want_floating.0.unwrap_or(before.w).max(1);
        f.rect.size.h = want_floating.1.unwrap_or(before.h).max(1);
        f.rect.size != before
    } else {
        return Err("window is neither tiled nor floating");
    };
    if changed {
        arrange_output(state, id);
    }
    Ok(changed)
}

/// Hand one output's workspace over to another output (COMP-13 §2.1).
///
/// Every output owns its own fixed set of workspaces, so this moves the
/// *windows* of `idx` on `from` into the same index on `to` — the slot itself
/// stays where it is, which is why emptying it can never strand an output.
/// Both outputs are validated before anything moves.
pub fn move_workspace_to_output(
    state: &mut AbyssState,
    from: u64,
    idx: usize,
    to: u64,
) -> Result<bool, &'static str> {
    if !(1..=workspace::COUNT).contains(&idx) {
        return Err("workspace is out of range");
    }
    let target = idx - 1;
    if state.outputs.get(from).is_none() {
        return Err("no such source output");
    }
    match state.outputs.get(to) {
        None => return Err("no such output"),
        Some(e) if !e.enabled => return Err("target output is disabled"),
        Some(_) => {}
    }
    if from == to {
        return Ok(false);
    }
    let src_origin = state
        .outputs
        .get(from)
        .and_then(|e| state.space.output_geometry(&e.output))
        .map(|g| g.loc)
        .unwrap_or_default();
    let dst_output = state.outputs.get(to).expect("checked above").output.clone();
    let dst_origin = state
        .space
        .output_geometry(&dst_output)
        .map(|g| g.loc)
        .unwrap_or_default();
    let entry = state.outputs.get_mut(from).expect("checked above");
    let tiled = entry.workspaces[target].tiled.windows();
    let floating: Vec<Floating> = std::mem::take(&mut entry.workspaces[target].floating);
    let pending: Vec<Window> = std::mem::take(&mut entry.workspaces[target].pending);
    for w in &tiled {
        entry.workspaces[target].tiled.remove(w);
    }
    if tiled.is_empty() && floating.is_empty() && pending.is_empty() {
        return Ok(false);
    }
    for w in tiled.iter().chain(floating.iter().map(|f| &f.window)) {
        state.space.unmap_elem(w);
    }
    layer_map_for_output(&dst_output).arrange();
    let area = tiling_area(state, &dst_output);
    let pointer = state.pointer_location;
    let stacking = de::stacking(state);
    let default_rect = de::default_float_rect(state, to, area);
    let dst = state.outputs.get_mut(to).expect("checked above");
    for (i, w) in tiled.into_iter().chain(pending).enumerate() {
        if stacking {
            // Nothing tiles under `de`; step along a diagonal so none hides another.
            let step = Point::from((32 * i as i32, 32 * i as i32));
            dst.workspaces[target].floating.push(Floating {
                window: w,
                rect: de::carried_rect(default_rect, step, area),
                weight: None,
                placement: Placement::Free,
            });
        } else {
            dst.workspaces[target].tiled.insert(w, None, area, pointer);
        }
    }
    for mut f in floating {
        // The rectangle is in global logical coordinates, so carry it across
        // by the difference between the two outputs' origins.
        f.rect.loc += dst_origin - src_origin;
        f.placement = f.placement.moved_by(dst_origin - src_origin);
        dst.workspaces[target].floating.push(f);
    }
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(from, to, workspace = idx, "workspace moved to output");
    Ok(true)
}

/// Run `command` through `sh -c` on the human's behalf. In `dedicated` GPU
/// mode it carries the discrete GPU's offload environment
/// ([`AbyssState::dedicated_env`], ADR 0084), as every launch from the launcher
/// and bar does through `ec-services`.
pub fn spawn(state: &AbyssState, command: &str) {
    spawn_with(&state.dedicated_env, command);
}

/// [`spawn`] with an explicit environment (none for compositor-internal commands).
pub fn spawn_with(env: &[(&'static str, String)], command: &str) {
    tracing::info!(command, "spawning");
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    cmd.envs(env.iter().map(|(k, v)| (*k, v.as_str())));
    // The child must not inherit our controlling terminal or process group.
    cmd.process_group(0);
    if let Err(e) = cmd.spawn() {
        tracing::error!(command, %e, "spawn failed");
    }
}

/// The keyboard interactivity a layer surface had at its previous commit.
#[derive(Default)]
struct SeenInteractivity(Cell<Option<KeyboardInteractivity>>);

/// Act on a mapped layer surface changing its keyboard interactivity
/// (wlr-layer-shell `set_keyboard_interactivity`). Mapping is handled by
/// [`focus_layer_if_wanted`]; this is the same rule for a surface that asks
/// later — a bar opening its menu, say (COMP-05 §5):
///
/// - `none` to `on_demand`/`exclusive`: it takes the keyboard, as it would
///   have had it mapped that way.
/// - to `none` while holding the keyboard: it loses it, and focus goes back to
///   the focused output's workspace ([`refocus_topmost`]). The protocol says
///   the compositor "should never assign it the keyboard focus".
pub(crate) fn follow_interactivity(
    state: &mut AbyssState,
    output: &Output,
    surface: &WlSurface,
    initial: bool,
) {
    let Some(layer) = layer_map_for_output(output)
        .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
        .cloned()
    else {
        return;
    };
    let now = layer.cached_state().keyboard_interactivity;
    layer.user_data().insert_if_missing(SeenInteractivity::default);
    let Some(before) = layer
        .user_data()
        .get::<SeenInteractivity>()
        .map(|s| s.0.replace(Some(now)))
    else {
        return;
    };
    if initial || before.is_none() || before == Some(now) {
        return;
    }
    let holds = focused_layer(state).as_ref() == Some(&layer);
    match now {
        KeyboardInteractivity::None if holds => {
            tracing::debug!("layer surface gave up keyboard interactivity; refocusing");
            refocus_topmost(state);
        }
        KeyboardInteractivity::OnDemand | KeyboardInteractivity::Exclusive if !holds => {
            tracing::debug!(interactivity = ?now, "layer surface asked for the keyboard");
            focus_layer_if_wanted(state, surface);
        }
        _ => {}
    }
}

/// Give keyboard focus to a newly mapped layer surface that asks for it.
pub fn focus_layer_if_wanted(state: &mut AbyssState, surface: &WlSurface) {
    let wants = state.outputs.iter().any(|e| {
        layer_map_for_output(&e.output)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some_and(|l| l.can_receive_keyboard_focus())
    });
    if wants && !state.lock.locked {
        state.focus = None;
        focus_surface(state, Some(surface.clone()));
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod x11_float_tests {
    use super::*;

    fn sz(w: i32, h: i32) -> Size<i32, Logical> {
        Size::from((w, h))
    }

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), sz(w, h))
    }

    #[test]
    fn x11_outer_size_survives_a_remap_without_shrinking() {
        let area = sz(1920, 1000);
        let border = 2;
        // The configured (inner) size is what `arrange_output` hands the client.
        let configured =
            |outer: Size<i32, Logical>| shrink(Rectangle::new((0, 0).into(), outer), border).size;
        let mut asked = sz(1593, 764);
        for _ in 0..3 {
            let outer = x11_outer_size(asked, sz(800, 500), area, border);
            assert_eq!(configured(outer), asked, "the client gets the size it asked for");
            // The client re-requests whatever it was last configured to.
            asked = configured(outer);
        }
        // Larger than the area: the configured size fits inside it.
        let outer = x11_outer_size(sz(4000, 3000), sz(0, 0), area, border);
        assert_eq!(outer, area);
        // A pinned-size window keeps exactly its pinned size.
        let outer = x11_outer_size(sz(320, 282), sz(320, 282), area, border);
        assert_eq!(
            clamp_to(configured(outer), sz(320, 282), sz(320, 282), false),
            sz(320, 282)
        );
        assert_eq!(configured(outer), sz(320, 282));
    }

    #[test]
    fn x11_float_size_keeps_the_request_within_min_and_area() {
        let area = sz(1920, 1000);
        assert_eq!(x11_float_size(sz(700, 500), sz(0, 0), area), sz(700, 500));
        // Below the minimum: the minimum.
        assert_eq!(x11_float_size(sz(300, 200), sz(600, 400), area), sz(600, 400));
        // Larger than the output: clamped to it.
        assert_eq!(x11_float_size(sz(4000, 3000), sz(0, 0), area), sz(1920, 1000));
        // No area known: the request stands.
        assert_eq!(x11_float_size(sz(700, 500), sz(0, 0), sz(0, 0)), sz(700, 500));
    }

    #[test]
    fn centered_in_stays_on_the_area() {
        let area = r(0, 30, 1920, 1000);
        // Over the area itself.
        assert_eq!(centered_in(area, sz(600, 400), area), Point::from((660, 330)));
        // Over a parent hanging off the right edge: pulled back in.
        assert_eq!(
            centered_in(r(1700, 100, 600, 400), sz(800, 300), area),
            Point::from((1120, 150))
        );
        // Bigger than the area: its origin.
        assert_eq!(centered_in(area, sz(2000, 1200), area), Point::from((0, 30)));
    }
}
