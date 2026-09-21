// SPDX-License-Identifier: AGPL-3.0-only
//! Window management: placement, layout, workspaces, focus (COMP-05).

pub mod focus;
pub mod layout;
pub mod rules;
pub mod workspace;

// The focus path lives in `focus`, but every existing call site says
// `shell::focus_window` (ADR 0042 makes `focus::apply_focus` the path they all
// grow into, one caller at a time).
pub use focus::{focus_surface, focus_window, refocus_topmost};

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
            wlr_layer::{Anchor, ExclusiveZone, Layer, LayerSurfaceData},
            xdg::{PopupSurface, SurfaceCachedState, XdgPopupSurfaceData, XdgToplevelSurfaceData},
        },
    },
};

use crate::{
    config::{FloatingPlacement, LayoutKind},
    input::Direction,
    protocols::standard::fractional_scale,
    shell::workspace::Floating,
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
/// outer gap applied. This is the area a maximized toplevel is constrained to
/// (COMP-06 §3): the raw protocol-defined usable region, independent of
/// abyss's own gap styling.
fn usable_area(state: &AbyssState, output: &Output) -> Rectangle<i32, Logical> {
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
    let g = state.config.general.gaps_out;
    let loc = Point::from((area.loc.x + g, area.loc.y + g));
    let size = Size::from(((area.size.w - 2 * g).max(1), (area.size.h - 2 * g).max(1)));
    Rectangle::new(loc, size)
}

fn shrink(r: Rectangle<i32, Logical>, by: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        (r.loc.x + by, r.loc.y + by).into(),
        Size::from(((r.size.w - 2 * by).max(1), (r.size.h - 2 * by).max(1))),
    )
}

/// Clamp against the client's advertised min/max, loosely: a max smaller than
/// the tile wins, a min larger than the tile wins.
fn clamp_size(window: &Window, mut size: Size<i32, Logical>) -> Size<i32, Logical> {
    let Some(toplevel) = window.toplevel() else {
        if let Some(x11) = window.x11_surface() {
            let (min, max) = (x11.min_size(), x11.max_size());
            if let Some(max) = max {
                if max.w > 0 {
                    size.w = size.w.min(max.w);
                }
                if max.h > 0 {
                    size.h = size.h.min(max.h);
                }
            }
            if let Some(min) = min {
                size.w = size.w.max(min.w);
                size.h = size.h.max(min.h);
            }
            size.w = size.w.max(1);
            size.h = size.h.max(1);
        }
        return size;
    };
    with_states(toplevel.wl_surface(), |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        let d = *guard.current();
        if d.max_size.w > 0 {
            size.w = size.w.min(d.max_size.w);
        }
        if d.max_size.h > 0 {
            size.h = size.h.min(d.max_size.h);
        }
        size.w = size.w.max(d.min_size.w).max(1);
        size.h = size.h.max(d.min_size.h).max(1);
    });
    size
}

fn configure(window: &Window, rect: Rectangle<i32, Logical>, activated: bool) {
    let size = rect.size;
    let Some(toplevel) = window.toplevel() else {
        // X11 windows are positioned in the X root's coordinate space, so
        // they need the whole rectangle, not just a size.
        if let Some(x11) = window.x11_surface() {
            x11.set_activated(activated).ok();
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

/// Re-run the layout for every output's active workspace.
pub fn arrange(state: &mut AbyssState) {
    let ids: Vec<u64> = state.outputs.iter().map(|e| e.id).collect();
    for id in ids {
        arrange_output(state, id);
    }
    refresh_reactive_popups(state);
    // Layout changes are not driven by a client commit: an unmapped surface
    // sends nothing, so without this the last composite stays on screen and a
    // closed window or dismissed launcher goes on being visible until some
    // other client happens to commit a frame.
    crate::backend::damage_all(state);
}

/// Lay out one output's active workspace and remap it into the space.
pub fn arrange_output(state: &mut AbyssState, id: u64) {
    let Some(entry) = state.outputs.get(id) else {
        return;
    };
    let output = entry.output.clone();
    let ws = entry.active;
    order_layers(&output);
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    // A maximized toplevel tracks the usable area, so it has to be recomputed
    // on every arrange — a layer surface claiming an exclusive zone after the
    // window was maximized must still shrink it (COMP-06 §3).
    let max_area = usable_area(state, &output);
    // A fullscreen toplevel takes the whole output, exclusive zones included, so
    // it tracks the raw geometry rather than the usable area (COMP-05 §4).
    let full_area = state.space.output_geometry(&output).unwrap_or_default();
    {
        // `Window` hashes by its stable `ObjectId`; the interior mutability
        // clippy flags lives in fields that take no part in `Hash`/`Eq`, so
        // it is sound as a key. This is smithay's own idiom.
        #[allow(clippy::mutable_key_type)]
        let maximized = &state.maximized;
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
            // Fullscreen wins over maximized: a maximized window that then goes
            // fullscreen keeps its `maximized` entry so unfullscreen can restore it.
            if fullscreen.contains_key(&f.window) {
                f.rect = full_area;
            } else if maximized.contains_key(&f.window) {
                f.rect = max_area;
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
    for w in pending {
        let entry = state.outputs.get_mut(id).expect("checked above");
        entry.workspaces[ws].tiled.insert(w, None, area);
    }

    let entry = state.outputs.get(id).expect("checked above");
    let kind = entry.workspaces[ws]
        .layout
        .unwrap_or_else(|| state.config.layout_for(ws + 1));
    let gap = state.config.general.gaps_in;
    let border = state.config.general.border_size;

    let entry = state.outputs.get_mut(id).expect("checked above");
    let tiled: Vec<(Window, Rectangle<i32, Logical>)> = match kind {
        LayoutKind::Dwindle => entry.workspaces[ws].tiled.dwindle(area, gap),
        LayoutKind::Master => entry.workspaces[ws].tiled.master(area, gap),
    };
    let floating: Vec<(Window, Rectangle<i32, Logical>)> = entry.workspaces[ws]
        .floating
        .iter()
        .map(|f| (f.window.clone(), f.rect))
        .collect();
    let focus = state.focus.clone();

    for (w, rect) in tiled {
        let inner = shrink(rect, border);
        let size = clamp_size(&w, inner.size);
        configure(&w, Rectangle::new(inner.loc, size), focus.as_ref() == Some(&w));
        fractional_scale::update_window_scale(&w, &output);
        state.space.map_element(w, inner.loc, false);
    }
    for (w, rect) in floating {
        // Maximized and fullscreen windows fill their area exactly; no border inset.
        let inner = if state.fullscreen.contains_key(&w) || state.maximized.contains_key(&w) {
            rect
        } else {
            shrink(rect, border)
        };
        let size = clamp_size(&w, inner.size);
        configure(&w, Rectangle::new(inner.loc, size), focus.as_ref() == Some(&w));
        fractional_scale::update_window_scale(&w, &output);
        state.space.map_element(w.clone(), inner.loc, false);
        state.space.raise_element(&w, false);
    }
}

/// Pin a window to an exact window-geometry rectangle, floating it out of the
/// tiling layout so the next layout pass leaves it there (COMP-05 §3). This is
/// what an interactive move/resize grab and an external placement request both
/// end up calling.
pub fn place_at(state: &mut AbyssState, window: &Window, geo: Rectangle<i32, Logical>) {
    // A maximized or fullscreen window is mapped without the border inset and has
    // its rectangle recomputed on every arrange, so pinning it would be a lie.
    if state.maximized.contains_key(window) || state.fullscreen.contains_key(window) {
        return;
    }
    // `arrange_output` shrinks the stored rectangle by the border before it
    // configures and maps, so store the outer rectangle that shrinks back to
    // exactly `geo`.
    let border = state.config.general.border_size;
    let outer = Rectangle::new(
        Point::from((geo.loc.x - border, geo.loc.y - border)),
        Size::from((geo.size.w + 2 * border, geo.size.h + 2 * border)),
    );
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

/// A brand-new toplevel joins the active workspace, tiled, next to the focus.
pub fn place_new_window(state: &mut AbyssState, window: Window) {
    // Classify before the window is ever composited (COMP-02 §7). Raise-only.
    crate::render::capture::mark_sensitive(state, &window);
    crate::protocols::standard::foreign_toplevel::window_mapped(state, &window);
    // COMP-05 §4: rules decide placement before the window joins a workspace.
    let placement = rules::apply(state, &window);
    if !install(state, &window, &placement) {
        return;
    }
    let handle = state.ipc.handle_for(&window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "opened", "handle": handle}),
    );
}

/// Re-run placement once a client has finally named itself, so rules that match
/// on `app-id`/`title` still land (see `rules::Placed`). The window keeps its
/// handle and its identity — only where it sits changes.
pub fn replace_window(state: &mut AbyssState, window: &Window, placement: &rules::Placement) {
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            ws.remove(window);
        }
    }
    install(state, window, placement);
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
        tracing::warn!("no output to place a new window on; dropping it");
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
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let geometry = state.space.element_geometry(&window);
    let mode = state.config.general.floating_placement;
    let pointer = state.pointer_location;
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = placement
        .workspace
        .map(|n| (n as usize - 1).min(entry.workspaces.len() - 1))
        .unwrap_or(entry.active);
    if placement.float {
        let size = placement
            .size
            .map(Size::from)
            .or_else(|| geometry.map(|g| g.size))
            .unwrap_or_else(|| Size::from((area.size.w / 2, area.size.h / 2)));
        // A `position` rule is relative to the output's tiling area, so the
        // same rule lands in the same place on any output. Absent a rule,
        // `general.floating-placement` decides.
        let loc: Point<i32, Logical> = match placement.position {
            Some((x, y)) => (area.loc.x + x, area.loc.y + y).into(),
            None => floating_origin(mode, area, size, pointer, entry.workspaces[ws].floating.len()),
        };
        let rect = Rectangle::new(loc, size);
        entry.workspaces[ws].floating.push(Floating {
            window: window.clone(),
            rect,
        });
    } else {
        let near = state
            .focus
            .clone()
            .filter(|w| entry.workspaces[ws].tiled.contains(w));
        entry.workspaces[ws]
            .tiled
            .insert(window.clone(), near.as_ref(), area);
    }
    // A no-focus-steal window is mapped where the rule put it but never takes
    // the focus; the human is told it wants attention (COMP-05 §5).
    if placement.no_focus_steal {
        arrange(state);
        mark_urgent(state, &window);
    } else {
        state.focus = Some(window.clone());
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

/// Resolve a surface to the window that owns it, if any.
pub fn window_for_surface(state: &AbyssState, surface: &WlSurface) -> Option<Window> {
    state
        .space
        .elements()
        .find(|w| window_surface(w).as_ref() == Some(surface))
        .cloned()
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
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            ws.remove(window);
        }
    }
    state.space.unmap_elem(window);
    state.borders.remove(window);
    state.geo_loc.remove(window);
    state.maximized.remove(window);
    state.fullscreen.remove(window);
    state.urgent.retain(|w| w != window);
    crate::protocols::standard::foreign_toplevel::window_closed(window);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(
        state,
        "window",
        serde_json::json!({"change": "closed", "handle": handle}),
    );
    if state.focus.as_ref() == Some(window) {
        state.focus = None;
    }
    arrange(state);
    refocus_topmost(state);
}

/// Topmost surface at `pos`, honouring the COMP-02 §4 stacking order:
/// overlay and top layers sit above toplevels, bottom and background below.
pub fn surface_under(
    state: &AbyssState,
    pos: Point<f64, Logical>,
) -> Option<(WlSurface, Point<f64, Logical>)> {
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
                    return Some((s, (output_loc + geo.loc + p).to_f64()));
                }
            }
        }
    }
    if let Some((window, loc)) = state.space.element_under(pos) {
        if let Some((s, p)) = window.surface_under(pos - loc.to_f64(), WindowSurfaceType::ALL) {
            return Some((s, (loc + p).to_f64()));
        }
    }
    let map = layer_map_for_output(&output);
    for layer in [Layer::Bottom, Layer::Background] {
        if let Some(l) = layer_under(&map, layer, local) {
            let geo = layer_geometry(&map, l).unwrap_or_default();
            if let Some((s, p)) = l.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL) {
                return Some((s, (output_loc + geo.loc + p).to_f64()));
            }
        }
    }
    None
}

// ------------------------------------------------------ layer placement

/// An explicit position for a layer surface, over and above the arrangement
/// `LayerMap::arrange` derives from its anchors and margins.
///
/// The layer map has no notion of a position a layer surface did not ask for,
/// so the delta is kept beside it, in the surface's own user data. Nothing in
/// the protocol sets this; it exists for the harnesses and test hooks that
/// place a layer surface directly (the wlcs `position_window` hook is the only
/// caller today). An unset offset is `(0, 0)`, so every read path below stays
/// exactly the arrangement the layer map produced.
#[derive(Default)]
struct LayerOffset(std::cell::Cell<Point<i32, Logical>>);

fn layer_offset(layer: &DesktopLayerSurface) -> Point<i32, Logical> {
    layer
        .user_data()
        .get::<LayerOffset>()
        .map(|o| o.0.get())
        .unwrap_or_default()
}

/// Geometry of a mapped layer surface, including any explicit placement.
///
/// Every hit-test and render path must use this rather than
/// `LayerMap::layer_geometry`, or an explicitly placed layer surface is drawn
/// in one place and clicked in another.
pub fn layer_geometry(map: &LayerMap, layer: &DesktopLayerSurface) -> Option<Rectangle<i32, Logical>> {
    let mut geo = map.layer_geometry(layer)?;
    geo.loc += layer_offset(layer);
    Some(geo)
}

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

/// Marker: this window has had its map-time configure sent (see `handle_commit`).
struct MapConfigured;

/// True once `surface` has a committed buffer, i.e. the window is mapped.
fn has_buffer(surface: &WlSurface) -> bool {
    smithay::backend::renderer::utils::with_renderer_surface_state(surface, |s| s.buffer().is_some())
        .unwrap_or(false)
}

/// Send the initial configure for toplevels, layer surfaces and popups, and
/// keep the layer map arranged.
pub fn handle_commit(state: &mut AbyssState, surface: &WlSurface) {
    state.popups.commit(surface);

    let mapped = state
        .space
        .elements()
        .find(|w| window_surface(w).as_ref() == Some(surface))
        .cloned();
    if let Some(window) = mapped {
        // Title and app_id can change at any commit; the foreign-toplevel list
        // has no other notification path for them.
        crate::protocols::standard::foreign_toplevel::window_updated(&window);
        // Title-matching rules are re-evaluated on title change (COMP-05 §4).
        if let Some(placement) = rules::reevaluate(state, &window) {
            replace_window(state, &window, &placement);
        }
        if window.toplevel().is_none() {
            // X11: no configure handshake to complete on this side.
            return;
        }
        let initial_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|d| d.lock().unwrap().initial_configure_sent)
                .unwrap_or(true)
        });
        if !initial_sent {
            if let Some(t) = window.toplevel() {
                t.send_configure();
            }
        } else if window.user_data().get::<MapConfigured>().is_none() && has_buffer(surface) {
            // COMP-05 §3: a toplevel gets a configure when it maps. The commit
            // that first attaches a buffer is that moment, and it is the only
            // point at which the client learns the size and state the shell
            // actually chose for a window it has already been placed into.
            // `configure` uses `send_pending_configure`, which suppresses a
            // configure whose content is unchanged since the initial one, so
            // without this the map configure never goes out.
            window.user_data().insert_if_missing(|| MapConfigured);
            if let Some(t) = window.toplevel() {
                t.send_configure();
            }
        }
        return;
    }

    let layer_output = state.outputs.iter().map(|e| e.output.clone()).find(|o| {
        layer_map_for_output(o)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some()
    });
    if let Some(output) = layer_output {
        let mut arranged = false;
        let mut send_initial = false;
        {
            let mut map = layer_map_for_output(&output);
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                send_initial = !with_states(surface, |states| {
                    states
                        .data_map
                        .get::<LayerSurfaceData>()
                        .map(|d| d.lock().unwrap().initial_configure_sent)
                        .unwrap_or(true)
                });
                let layer = layer.clone();
                // Arrange *before* the initial configure. `new_layer_surface`
                // fires at `get_layer_surface` time, i.e. before the client has
                // set its anchor, size or exclusive zone, so the geometry from
                // that first arrange is stale. Configuring from it hands the
                // client a wrong size that it may well act on (COMP-06 §3).
                arranged = map.arrange();
                if send_initial {
                    layer.layer_surface().send_configure();
                }
            }
        }
        if arranged || send_initial {
            arrange(state);
        }
        if send_initial {
            focus_layer_if_wanted(state, surface);
        }
    }

    if let Some(popup) = state.popups.find_popup(surface) {
        let PopupKind::Xdg(ref xdg) = popup else { return };
        if !xdg.is_initial_configure_sent() && xdg.send_configure().is_err() {
            return;
        }
        // Smithay's popup post-commit hook resets `current` to the last acked
        // configure on *every* commit, so republishing only once at the
        // initial configure would be undone by the next commit (COMP-06 §4).
        publish_popup_geometry(xdg);
    }
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
        match w.underlying_surface() {
            smithay::desktop::WindowSurface::Wayland(t) => t.send_close(),
            smithay::desktop::WindowSurface::X11(x) => {
                if let Err(e) = x.close() {
                    tracing::warn!(error = %e, "could not close x11 window");
                }
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
    let was_floating = entry.workspaces[ws]
        .floating
        .iter()
        .find(|f| &f.window == window)
        .map(|f| f.rect);
    entry.workspaces[ws].remove(window);
    entry.workspaces[ws].minimized.push(workspace::Minimized {
        window: window.clone(),
        was_floating,
    });
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
    if state.focus.as_ref() == Some(window) {
        state.focus = None;
    }
    arrange(state);
    refocus_topmost(state);
    emit_minimized(state, window, true);
}

/// Bring a minimized window back to the layout it left and focus it.
pub fn unminimize_window(state: &mut AbyssState, window: &Window) {
    let Some(id) = output_of_window(state, window) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
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
        }),
        None => entry.workspaces[ws].tiled.insert(window.clone(), None, area),
    }
    arrange(state);
    // Restoring onto an inactive workspace must not steal the screen: the
    // window is in the layout, but only a switch back will show it.
    if state.outputs.get(id).is_some_and(|e| e.active == ws) {
        focus_window(state, window);
    }
    emit_minimized(state, window, false);
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

pub fn toggle_floating(state: &mut AbyssState) {
    let Some(window) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let geometry = state.space.element_geometry(&window);
    let mode = state.config.general.floating_placement;
    let pointer = state.pointer_location;
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
        entry.workspaces[ws].tiled.remove(&window);
        entry.workspaces[ws].floating.push(Floating {
            window,
            rect: Rectangle::new(loc, rect.size),
        });
    } else if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == window)
    {
        entry.workspaces[ws].floating.remove(i);
        entry.workspaces[ws].tiled.insert(window, None, area);
    }
    arrange(state);
}

/// A client's `xdg_toplevel.set_maximized` (COMP-05 §4): fill the output's
/// usable area, i.e. shrunk by any layer-shell exclusive zone, with no gap or
/// border — the raw protocol contract, not abyss's own tiling style.
pub fn maximize_toplevel(state: &mut AbyssState, surface: &smithay::wayland::shell::xdg::ToplevelSurface) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;

    let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel() == Some(surface))
        .cloned()
    else {
        return;
    };
    if state.maximized.contains_key(&window) {
        return;
    }
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = usable_area(state, &output);

    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    let restore = if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == window)
    {
        Some(entry.workspaces[ws].floating.remove(i).rect)
    } else {
        entry.workspaces[ws].tiled.remove(&window);
        None
    };
    entry.workspaces[ws].floating.push(Floating {
        window: window.clone(),
        rect: area,
    });
    state.maximized.insert(window.clone(), restore);

    surface.with_pending_state(|s| {
        s.size = Some(area.size);
        s.states.set(State::Maximized);
    });
    surface.send_configure();
    arrange(state);
}

/// A client's `xdg_toplevel.unset_maximized`: restore whatever placement the
/// window had before it was maximized.
pub fn unmaximize_toplevel(state: &mut AbyssState, surface: &smithay::wayland::shell::xdg::ToplevelSurface) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;

    let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel() == Some(surface))
        .cloned()
    else {
        return;
    };
    let Some(restore) = state.maximized.remove(&window) else {
        return;
    };
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        surface.with_pending_state(|s| s.states.unset(State::Maximized));
        surface.send_configure();
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);

    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == window)
    {
        entry.workspaces[ws].floating.remove(i);
    }
    let size = match restore {
        Some(rect) => {
            entry.workspaces[ws].floating.push(Floating {
                window: window.clone(),
                rect,
            });
            rect.size
        }
        None => {
            entry.workspaces[ws].tiled.insert(window.clone(), None, area);
            area.size
        }
    };

    surface.with_pending_state(|s| {
        s.size = Some(size);
        s.states.unset(State::Maximized);
    });
    surface.send_configure();
    arrange(state);
}

/// Remove `window` from whatever placement it holds on `id`'s active workspace,
/// returning its floating rectangle if it had one and `None` if it was tiled.
fn detach_from_layout(state: &mut AbyssState, id: u64, window: &Window) -> Option<Rectangle<i32, Logical>> {
    let entry = state.outputs.get_mut(id)?;
    let ws = entry.active;
    if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == *window)
    {
        Some(entry.workspaces[ws].floating.remove(i).rect)
    } else {
        entry.workspaces[ws].tiled.remove(window);
        None
    }
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
/// exclusive zones included — unlike maximize, which stops at the usable area.
///
/// `target` is the client's requested output; `None` means "wherever it is now".
/// Honouring a different output moves the window there, which is what the
/// protocol asks for.
pub fn fullscreen_toplevel(
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
        return;
    };
    if state.fullscreen.contains_key(&window) {
        return;
    }
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
    let restore = current.and_then(|from| detach_from_layout(state, from, &window));

    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    entry.workspaces[ws].floating.push(Floating {
        window: window.clone(),
        rect: area,
    });
    state.fullscreen.insert(window.clone(), restore);

    surface.with_pending_state(|s| {
        s.size = Some(area.size);
        s.states.set(State::Fullscreen);
    });
    surface.send_configure();
    arrange(state);
    // hyperion hides outright under fullscreen rather than folding, and it
    // has no view of the window stack to work that out for itself.
    focus::emit_output_state(state, id);
}

/// A client's `xdg_toplevel.unset_fullscreen`: restore whatever placement the
/// window had before. A window that was maximized when it went fullscreen drops
/// back to maximized rather than to its pre-maximize rectangle.
pub fn unfullscreen_toplevel(
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
    let still_maximized = state.maximized.contains_key(&window);
    let max_area = usable_area(state, &output);

    detach_from_layout(state, id, &window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    let size = if still_maximized {
        entry.workspaces[ws].floating.push(Floating {
            window: window.clone(),
            rect: max_area,
        });
        max_area.size
    } else {
        match restore {
            Some(rect) => {
                entry.workspaces[ws].floating.push(Floating {
                    window: window.clone(),
                    rect,
                });
                rect.size
            }
            None => {
                entry.workspaces[ws].tiled.insert(window.clone(), None, area);
                area.size
            }
        }
    };

    surface.with_pending_state(|s| {
        s.size = Some(size);
        s.states.unset(State::Fullscreen);
    });
    surface.send_configure();
    arrange(state);
    // Symmetric with `fullscreen_toplevel`: the bar comes back out of hiding.
    focus::emit_output_state(state, id);
}

pub fn toggle_layout(state: &mut AbyssState) {
    let Some(id) = state.outputs.focused().map(|e| e.id) else {
        return;
    };
    let ws = state.outputs.get(id).expect("just resolved").active;
    let fallback = state.config.layout_for(ws + 1);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let current = entry.workspaces[ws].layout.unwrap_or(fallback);
    let next = match current {
        LayoutKind::Dwindle => LayoutKind::Master,
        LayoutKind::Master => LayoutKind::Dwindle,
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

/// Swap the focused window with its neighbour in `dir` (tiled), or nudge a
/// floating window.
pub fn move_direction(state: &mut AbyssState, dir: Direction) {
    let Some(from) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &from) else {
        return;
    };
    let ws = state.outputs.get(id).expect("just resolved").active;
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if let Some(i) = entry.workspaces[ws]
        .floating
        .iter()
        .position(|f| f.window == from)
    {
        const STEP: i32 = 50;
        let r = &mut entry.workspaces[ws].floating[i].rect;
        match dir {
            Direction::Left => r.loc.x -= STEP,
            Direction::Right => r.loc.x += STEP,
            Direction::Up => r.loc.y -= STEP,
            Direction::Down => r.loc.y += STEP,
        }
        arrange(state);
        warp_pointer_to(state, &from);
        return;
    }
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
    for w in entry.workspace().windows() {
        state.space.unmap_elem(&w);
    }
    let previous = entry.active;
    state.outputs.get_mut(id).expect("just resolved").active = target;
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    // COMP-02 §9 `workspaces`: slide the arriving windows in from the side the
    // switch came from. There is no outgoing half — the old workspace's
    // windows were unmapped above and no longer exist for the render path.
    if let Some(anim) = state.config.animations.get("workspaces").cloned() {
        let width = state
            .outputs
            .get(id)
            .and_then(|e| state.space.output_geometry(&e.output))
            .map_or(0, |g| g.size.w);
        let dir = if target > previous { 1 } else { -1 };
        let windows = state
            .outputs
            .get(id)
            .map(|e| e.workspace().windows())
            .unwrap_or_default();
        state
            .borders
            .anim
            .slide(&state.space, &anim, &windows, (dir * width, 0).into());
    }
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

pub fn move_to_workspace(state: &mut AbyssState, idx: usize) {
    if !(1..=workspace::COUNT).contains(&idx) {
        return;
    }
    let target = idx - 1;
    let Some(window) = state.focus.clone() else { return };
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
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let active = entry.active;
    entry.workspaces[active].remove(&window);
    state.space.unmap_elem(&window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    entry.workspaces[target].tiled.insert(window.clone(), None, area);
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(workspace = idx, "window moved to workspace");
    // Following the window is the default: a move you cannot see is hard to
    // tell from a move that did not happen.
    if state.config.general.follow_window_to_workspace {
        switch_workspace(state, idx);
        focus::focus_window(state, &window);
        warp_pointer_to(state, &window);
    }
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

    let source_entry = state.outputs.get_mut(source_id).expect("just resolved");
    source_entry.workspaces[source_active].remove(&window);
    state.space.unmap_elem(&window);

    let target_entry = state.outputs.get_mut(target_id).expect("just resolved");
    target_entry.workspaces[target_ws]
        .tiled
        .insert(window.clone(), None, area);

    state.focus = None;
    arrange(state);
    refocus_topmost(state);
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
/// expressed as ratios by [`layout::Tree::resize`] and may not be honoured
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
    // The master layout ignores the tree's shape (ADR 0031), so a ratio has
    // nowhere to land. Say so rather than silently doing nothing.
    if tiled && kind == LayoutKind::Master {
        return Err("a tiled window cannot be resized under the master layout");
    }
    let border = state.config.general.border_size;
    let gap = state.config.general.gaps_in;
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    // Callers name the window's own size; the tree deals in tiles, which are
    // the window plus its border on each side.
    let want = (width.map(|w| w + 2 * border), height.map(|h| h + 2 * border));

    let entry = state.outputs.get_mut(id).expect("just resolved");
    let changed = if tiled {
        entry.workspaces[ws].tiled.resize(window, area, gap, want)
    } else if let Some(f) = entry.workspaces[ws]
        .floating
        .iter_mut()
        .find(|f| &f.window == window)
    {
        let before = f.rect.size;
        f.rect.size.w = want.0.unwrap_or(before.w).max(1);
        f.rect.size.h = want.1.unwrap_or(before.h).max(1);
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
    let floating: Vec<Floating> = entry.workspaces[target].floating.drain(..).collect();
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
    let dst = state.outputs.get_mut(to).expect("checked above");
    for w in tiled.into_iter().chain(pending) {
        dst.workspaces[target].tiled.insert(w, None, area);
    }
    for mut f in floating {
        // The rectangle is in global logical coordinates, so carry it across
        // by the difference between the two outputs' origins.
        f.rect.loc += dst_origin - src_origin;
        dst.workspaces[target].floating.push(f);
    }
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(from, to, workspace = idx, "workspace moved to output");
    Ok(true)
}

pub fn spawn(command: &str) {
    tracing::info!(command, "spawning");
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    // The child must not inherit our controlling terminal or process group.
    cmd.process_group(0);
    if let Err(e) = cmd.spawn() {
        tracing::error!(command, %e, "spawn failed");
    }
}

/// Give keyboard focus to a newly mapped layer surface that asks for it.
pub fn focus_layer_if_wanted(state: &mut AbyssState, surface: &WlSurface) {
    let wants = state.outputs.iter().any(|e| {
        layer_map_for_output(&e.output)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some_and(|l| l.can_receive_keyboard_focus())
    });
    if wants {
        state.focus = None;
        focus_surface(state, Some(surface.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(mode: FloatingPlacement, pointer: (f64, f64), already: usize) -> (i32, i32) {
        let area = Rectangle::new(Point::from((100, 50)), Size::from((800, 600)));
        let size = Size::from((400, 300));
        let p = floating_origin(mode, area, size, Point::from(pointer), already);
        (p.x, p.y)
    }

    #[test]
    fn centered_sits_in_the_middle_of_the_tiling_area() {
        // Offset by the area's own origin, so it centres on the output the
        // window belongs to and not on the compositor's global 0,0.
        assert_eq!(
            origin(FloatingPlacement::Centered, (0.0, 0.0), 0),
            (100 + 200, 50 + 150)
        );
    }

    #[test]
    fn pointer_clamps_so_the_window_never_opens_partly_offscreen() {
        assert_eq!(origin(FloatingPlacement::Pointer, (300.0, 200.0), 0), (300, 200));
        // Past the far edge: pulled back so the whole window fits.
        assert_eq!(
            origin(FloatingPlacement::Pointer, (10_000.0, 10_000.0), 0),
            (100 + 800 - 400, 50 + 600 - 300)
        );
        // Before the near edge (pointer on another output): pushed in.
        assert_eq!(origin(FloatingPlacement::Pointer, (-500.0, -500.0), 0), (100, 50));
    }

    #[test]
    fn cascade_steps_then_wraps_instead_of_piling_up() {
        assert_eq!(origin(FloatingPlacement::Cascade, (0.0, 0.0), 0), (100, 50));
        assert_eq!(origin(FloatingPlacement::Cascade, (0.0, 0.0), 1), (132, 82));
        // Room is (min(400, 300) / 32) = 9 steps, so the tenth window restarts
        // the diagonal rather than walking off the bottom-right.
        assert_eq!(origin(FloatingPlacement::Cascade, (0.0, 0.0), 9), (100, 50));
    }
}
