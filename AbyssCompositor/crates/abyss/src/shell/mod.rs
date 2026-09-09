// SPDX-License-Identifier: AGPL-3.0-only
//! Window management: placement, layout, workspaces, focus (COMP-05).

pub mod layout;
pub mod rules;
pub mod workspace;

use smithay::{
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, LayerMap,
        LayerSurface as DesktopLayerSurface, PopupKind, Window, WindowSurfaceType,
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
    config::LayoutKind, input::Direction, protocols::standard::fractional_scale, shell::workspace::Floating,
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

/// The output that owns focus. Never `None` in practice: COMP-03 §5 guarantees
/// a fallback output exists whenever there is no connector.
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
        .find(|e| {
            e.workspaces
                .iter()
                .any(|ws| ws.windows().contains(window) || ws.pending.contains(window))
        })
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
    {
        // `Window` hashes by its stable `ObjectId`; the interior mutability
        // clippy flags lives in fields that take no part in `Hash`/`Eq`, so
        // it is sound as a key. This is smithay's own idiom.
        #[allow(clippy::mutable_key_type)]
        let maximized = &state.maximized;
        let entry = state.outputs.get_mut(id).expect("checked above");
        for f in entry.workspaces[ws].floating.iter_mut() {
            if maximized.contains_key(&f.window) {
                f.rect = max_area;
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
        // Maximized windows fill the usable area exactly; no border inset.
        let inner = if state.maximized.contains_key(&w) {
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
    let Some(mut id) = state.outputs.focused().map(|e| e.id) else {
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
        // same rule lands in the same place on any output.
        let loc = match placement.position {
            Some((x, y)) => (area.loc.x + x, area.loc.y + y).into(),
            None => (
                area.loc.x + (area.size.w - size.w).max(0) / 2,
                area.loc.y + (area.size.h - size.h).max(0) / 2,
            )
                .into(),
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

pub fn unmap_window(state: &mut AbyssState, window: &Window) {
    for entry in state.outputs.iter_mut() {
        for ws in entry.workspaces.iter_mut() {
            ws.remove(window);
        }
    }
    state.space.unmap_elem(window);
    state.borders.remove(window);
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

pub fn focus_window(state: &mut AbyssState, window: &Window) {
    let Some(surface) = window_surface(window) else {
        return;
    };
    // X11 focus is compositor-driven: activate, raise in the X stack, then
    // let the keyboard follow (COMP-07 §1).
    if let Some(x11) = window.x11_surface() {
        x11.set_activated(true).ok();
        if let Some(wm) = state.xwayland.wm.as_mut() {
            let _ = wm.raise_window(x11);
        }
        let others: Vec<Window> = state.space.elements().filter(|w| *w != window).cloned().collect();
        for w in others {
            if let Some(other) = w.x11_surface() {
                other.set_activated(false).ok();
            }
        }
    }
    state.focus = Some(window.clone());
    state.urgent.retain(|w| w != window);
    if let Some(id) = output_of_window(state, window) {
        state.outputs.set_focused(id);
    }
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, Some(surface), SERIAL_COUNTER.next_serial());
    arrange(state);
    let handle = state.ipc.handle_for(window);
    crate::ipc::emit(state, "focus", serde_json::json!({"handle": handle}));
}

pub fn focus_surface(state: &mut AbyssState, surface: Option<WlSurface>) {
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_focus(state, surface, SERIAL_COUNTER.next_serial());
}

pub fn refocus_topmost(state: &mut AbyssState) {
    let here: Vec<Window> = state
        .outputs
        .focused()
        .map(|e| e.workspace().windows())
        .unwrap_or_default();
    let top = state
        .space
        .elements()
        .rfind(|w| here.contains(w))
        .cloned()
        .or_else(|| state.space.elements().next_back().cloned());
    match top {
        Some(w) => focus_window(state, &w),
        None => {
            state.focus = None;
            focus_surface(state, None);
        }
    }
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
        if !xdg.is_initial_configure_sent() && xdg.send_configure().is_ok() {
            publish_popup_geometry(xdg);
        }
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

// ---------------------------------------------------------------- actions

pub fn close_focused(state: &mut AbyssState) {
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

pub fn toggle_floating(state: &mut AbyssState) {
    let Some(window) = state.focus.clone() else { return };
    let Some(id) = output_of_window(state, &window).or_else(|| state.outputs.focused().map(|e| e.id)) else {
        return;
    };
    let output = state.outputs.get(id).expect("just resolved").output.clone();
    layer_map_for_output(&output).arrange();
    let area = tiling_area(state, &output);
    let geometry = state.space.element_geometry(&window);
    let entry = state.outputs.get_mut(id).expect("just resolved");
    let ws = entry.active;
    if entry.workspaces[ws].tiled.contains(&window) {
        let rect = geometry
            .unwrap_or_else(|| Rectangle::new(area.loc, Size::from((area.size.w / 2, area.size.h / 2))));
        let centered = Rectangle::new(
            (
                area.loc.x + (area.size.w - rect.size.w).max(0) / 2,
                area.loc.y + (area.size.h - rect.size.h).max(0) / 2,
            )
                .into(),
            rect.size,
        );
        entry.workspaces[ws].tiled.remove(&window);
        entry.workspaces[ws].floating.push(Floating {
            window,
            rect: centered,
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
        return;
    }
    let Some(target) = neighbour(state, &from, dir) else {
        return;
    };
    let entry = state.outputs.get_mut(id).expect("just resolved");
    if entry.workspaces[ws].tiled.contains(&target) {
        entry.workspaces[ws].tiled.swap(&from, &target);
        arrange(state);
    }
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
    entry.workspaces[target].tiled.insert(window, None, area);
    state.focus = None;
    arrange(state);
    refocus_topmost(state);
    tracing::info!(workspace = idx, "window moved to workspace");
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
